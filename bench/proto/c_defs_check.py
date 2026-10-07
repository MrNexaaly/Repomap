#!/usr/bin/env python3
"""Check repomap's line-based C/C++ definition reader against tree-sitter.

Samples C/C++ files under the given roots, extracts file-scope definitions
with tree-sitter (functions with a body, struct/union/enum/class with a
body, typedef names, #define names), runs repomap's reader on the same
files (the ignored `c_like::tests::dump` test) and reports precision and
recall per kind, with examples of each kind of miss.

Tree-sitter is the reference, not the truth: kernel macros confuse it, so a
file whose tree has ERROR nodes is reported separately.

Usage (aider venv, which ships the grammars):
  ~/.cache/repomap-bench/aider-venv/bin/python bench/proto/c_defs_check.py \
      --sample 300 ROOT [ROOT ...]
Do not point it at the held-out repository.
"""

import argparse
import json
import os
import random
import re
import subprocess
from collections import Counter, defaultdict

from grep_ast.tsl import get_parser

C_EXT = (".c", ".h")
CPP_EXT = (".cc", ".cpp", ".hpp")
DESCEND = {"translation_unit", "preproc_if", "preproc_ifdef", "preproc_else", "preproc_elif",
           "preproc_elifdef", "linkage_specification", "declaration_list", "namespace_definition",
           "template_declaration", "declaration", "type_definition"}
MACRO = re.compile(r"^[A-Z][A-Z0-9_]+$")
CACHE = os.path.expanduser("~/.cache/repomap-bench/proto")


def text(node, source):
    return source[node.start_byte:node.end_byte].decode("utf-8", "replace")


def declarator_name(node, source):
    """Innermost name inside a (pointer/function/array/...) declarator."""
    while node is not None:
        if node.type in ("identifier", "type_identifier", "field_identifier", "qualified_identifier",
                         "destructor_name", "operator_name", "template_function", "primitive_type"):
            return node
        inner = node.child_by_field_name("declarator")
        if inner is None:
            inner = next((c for c in node.named_children if c.type.endswith("declarator") or c.type in (
                "identifier", "type_identifier", "qualified_identifier", "destructor_name")), None)
        node = inner
    return None


def simple(name):
    return name.rsplit("::", 1)[-1].lstrip("~").split("<", 1)[0].strip()


def reference(path):
    source = open(path, "rb").read()
    parser = get_parser("cpp" if path.endswith(CPP_EXT) or "/icu" in path or "SPIRV" in path or "llama" in path else "c")
    tree = parser.parse(source)
    found = defaultdict(set)
    guards = set(re.findall(rb"^\s*#\s*ifndef\s+(\w+)", source, re.M))
    errors = 0

    def visit(node):
        nonlocal errors
        for child in node.named_children:
            kind = child.type
            if kind == "ERROR":
                errors += 1
                continue
            if kind == "function_definition":
                function = declarator_name(child.child_by_field_name("declarator"), source)
                if function is not None:
                    name = simple(text(function, source))
                    if MACRO.match(name):
                        params = child.child_by_field_name("declarator")
                        match = re.search(r"\(\s*([A-Za-z_]\w*)", text(params, source)) if params else None
                        name = match.group(1) if match else ""
                    if name and not name.startswith("operator"):
                        found["fn"].add(name)
                continue
            if kind in ("struct_specifier", "union_specifier", "enum_specifier", "class_specifier"):
                name, body = child.child_by_field_name("name"), child.child_by_field_name("body")
                if name is not None and body is not None:
                    found["type"].add(simple(text(name, source)))
                continue
            if kind == "type_definition":
                declarator = child.child_by_field_name("declarator")
                name = declarator_name(declarator, source) if declarator else None
                if name is not None:
                    found["typedef"].add(text(name, source))
                visit(child)
                continue
            if kind in ("preproc_def", "preproc_function_def"):
                name = text(child.child_by_field_name("name"), source)
                if not (kind == "preproc_def" and child.child_by_field_name("value") is None
                        and name.encode() in guards):
                    found["define"].add(name)
                visit(child)
                continue
            if kind in DESCEND:
                visit(child)

    visit(tree.root_node)
    return found, errors


def ours(paths, repo):
    os.makedirs(CACHE, exist_ok=True)
    listing = os.path.join(CACHE, "c_defs_files.txt")
    open(listing, "w").write("\n".join(paths) + "\n")
    output = subprocess.run(
        ["cargo", "test", "--release", "--quiet", "--lib", "c_like::tests::dump", "--", "--ignored", "--nocapture"],
        cwd=repo, env={**os.environ, "C_LIKE_FILES": listing}, capture_output=True, text=True, check=True).stdout
    result = {}
    for line in output.splitlines():
        if line.startswith("{"):
            row = json.loads(line)
            found = defaultdict(set)
            for definition in row["definitions"] + row["macros"]:
                kind, _, name = definition.partition(" ")
                kind = "type" if kind in ("struct", "union", "enum", "class") else kind
                found[kind].add(simple(name))
            result[row["path"]] = found
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("roots", nargs="+")
    parser.add_argument("--sample", type=int, default=300)
    parser.add_argument("--seed", type=int, default=7)
    parser.add_argument("--examples", type=int, default=12)
    args = parser.parse_args()
    repo = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    rng = random.Random(args.seed)
    paths = []
    for root in args.roots:
        files = sorted(os.path.join(d, f) for d, _, fs in os.walk(root) for f in fs
                       if f.endswith(C_EXT + CPP_EXT) and os.path.getsize(os.path.join(d, f)) < 1 << 20)
        paths += rng.sample(files, min(args.sample, len(files)))
    mine = ours(paths, repo)
    counts = {clean: defaultdict(Counter) for clean in (True, False)}
    misses = defaultdict(list)
    for path in paths:
        expected, errors = reference(path)
        got = mine.get(path, {})
        for kind in ("fn", "type", "typedef", "define"):
            want, have = expected.get(kind, set()), got.get(kind, set())
            tally = counts[errors == 0][kind]
            tally["both"] += len(want & have)
            tally["reference_only"] += len(want - have)
            tally["ours_only"] += len(have - want)
            if errors == 0:
                misses[(kind, "missed")] += [(path, name) for name in sorted(want - have)]
                misses[(kind, "extra")] += [(path, name) for name in sorted(have - want)]
    for clean in (True, False):
        print("files without parse errors" if clean else "files where tree-sitter reported ERROR nodes")
        for kind, tally in counts[clean].items():
            both = tally["both"]
            precision = both / max(both + tally["ours_only"], 1)
            recall = both / max(both + tally["reference_only"], 1)
            print(f"  {kind:8} precision {precision:.3f} recall {recall:.3f}  "
                  f"(both {both}, reference only {tally['reference_only']}, ours only {tally['ours_only']})")
    for (kind, what), rows in sorted(misses.items()):
        if rows:
            print(f"\n{kind} {what} ({len(rows)}):")
            for path, name in rng.sample(rows, min(args.examples, len(rows))):
                print(f"  {name}  {path}")


if __name__ == "__main__":
    main()
