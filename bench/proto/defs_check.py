#!/usr/bin/env python3
"""Check repomap's line-based definition readers against tree-sitter.

The companion of c_defs_check.py for Rust, Go, TypeScript/JavaScript, Svelte
and Vue. For sampled files it extracts declaration names with tree-sitter,
runs repomap's readers on the same files (the ignored
`repo_map::tests::dump_definitions` test) and reports precision and recall
of names per category:

  fn      functions (Rust: every fn item, methods included; Go: funcs and
          methods; TS/JS: top-level function declarations)
  method  TS/JS class methods
  type    structs, enums, traits, unions, aliases, interfaces, classes
  module  Rust `mod`
  value   Rust const/static; Go const/var; TS/JS top-level const/let/var
  macro   Rust macro_rules!

Usage (aider venv, which ships the grammars):
  ~/.cache/repomap-bench/aider-venv/bin/python bench/proto/defs_check.py \
      --lang rust --sample 300 ROOT [ROOT ...]
Do not point it at a held-out repository.
"""

import argparse
import json
import os
import random
import re
import subprocess
from collections import Counter, defaultdict

from grep_ast.tsl import get_parser

EXTENSIONS = {"rust": (".rs",), "go": (".go",), "ts": (".ts", ".tsx"), "js": (".js", ".mjs", ".cjs", ".jsx"),
              "svelte": (".svelte",), "vue": (".vue",)}
SKIP_PARTS = {"node_modules", "target", "dist", "build", ".git", "testdata", "heldout", "repomap-bench",
              "NexapadV2", "Nexagate", "modeler", "maxsis"}
KIND = {"fn": "fn", "async": "fn", "function": "fn", "func": "fn", "def": "fn",
        "struct": "type", "enum": "type", "trait": "type", "type": "type", "union": "type", "class": "type",
        "interface": "type", "mod": "module", "static": "value", "const": "value", "let": "value",
        "var": "value", "macro": "macro", "method": "method"}
SCRIPT = re.compile(rb"<script\b[^>]*>(.*?)</script>", re.S)
CACHE = os.path.expanduser("~/.cache/repomap-bench/proto")


def text(node, source):
    return source[node.start_byte:node.end_byte].decode("utf-8", "replace")


def name_of(node, source, field="name"):
    child = node.child_by_field_name(field)
    return text(child, source) if child is not None else None


def rust_reference(source):
    tree = get_parser("rust").parse(source)
    found = defaultdict(set)
    kinds = {"function_item": "fn", "function_signature_item": "fn", "struct_item": "type", "enum_item": "type",
             "union_item": "type", "trait_item": "type", "type_item": "type", "mod_item": "module",
             "const_item": "value", "static_item": "value", "macro_definition": "macro"}
    stack = [tree.root_node]
    while stack:
        node = stack.pop()
        if node.type in kinds:
            name = name_of(node, source)
            if name:
                found[kinds[node.type]].add(name.removeprefix("r#"))
        stack.extend(node.named_children)
    return found, tree.root_node.has_error


def go_reference(source):
    tree = get_parser("go").parse(source)
    found = defaultdict(set)
    for node in tree.root_node.named_children:
        if node.type in ("function_declaration", "method_declaration"):
            found["fn"].add(name_of(node, source))
        elif node.type == "type_declaration":
            for spec in node.named_children:
                if spec.type in ("type_spec", "type_alias"):
                    found["type"].add(name_of(spec, source))
        elif node.type in ("const_declaration", "var_declaration"):
            specs = [c for c in node.named_children if c.type.endswith("_spec")]
            specs += [s for c in node.named_children if c.type.endswith("_spec_list") for s in c.named_children]
            for spec in specs:
                for child in spec.children_by_field_name("name"):
                    found["value"].add(text(child, source))
    return found, tree.root_node.has_error


def script_reference(source, language):
    tree = get_parser(language).parse(source)
    found = defaultdict(set)
    for node in tree.root_node.named_children:
        if node.type == "export_statement":
            inner = node.child_by_field_name("declaration")
            if inner is None:
                continue
            node = inner
        kind = node.type
        if kind in ("function_declaration", "generator_function_declaration"):
            found["fn"].add(name_of(node, source))
        elif kind in ("class_declaration", "abstract_class_declaration", "class"):
            name = name_of(node, source)
            if name:
                found["type"].add(name)
            body = node.child_by_field_name("body")
            for member in body.named_children if body is not None else []:
                if member.type in ("method_definition", "abstract_method_signature"):
                    method = name_of(member, source)
                    if method and method != "constructor":
                        found["method"].add(method)
        elif kind in ("interface_declaration", "type_alias_declaration", "enum_declaration"):
            found["type"].add(name_of(node, source))
        elif kind in ("lexical_declaration", "variable_declaration"):
            for declarator in node.named_children:
                if declarator.type == "variable_declarator":
                    target = declarator.child_by_field_name("name")
                    if target is not None and target.type == "identifier":
                        found["value"].add(text(target, source))
    return found, tree.root_node.has_error


def reference(path, lang):
    source = open(path, "rb").read()
    if lang == "rust":
        return rust_reference(source)
    if lang == "go":
        return go_reference(source)
    if lang in ("svelte", "vue"):
        found, error = defaultdict(set), False
        for block in SCRIPT.findall(source):
            part, part_error = script_reference(block, "typescript")
            for kind, names in part.items():
                found[kind] |= names
            error |= part_error
        return found, error
    return script_reference(source, "tsx" if path.endswith((".tsx", ".jsx")) else "typescript")


def ours(paths, repo):
    os.makedirs(CACHE, exist_ok=True)
    listing = os.path.join(CACHE, "defs_files.txt")
    open(listing, "w").write("\n".join(paths) + "\n")
    output = subprocess.run(
        ["cargo", "test", "--release", "--quiet", "--lib", "repo_map::tests::dump_definitions", "--", "--ignored",
         "--nocapture"], cwd=repo, env={**os.environ, "REPOMAP_DUMP_FILES": listing}, capture_output=True,
        text=True, check=True).stdout
    result = {}
    for line in output.splitlines():
        if line.startswith("{"):
            row = json.loads(line)
            found = defaultdict(set)
            for definition in row["definitions"]:
                kind, _, name = definition.partition(" ")
                if kind in KIND:
                    found[KIND[kind]].add(name.split("::")[-1].split(".")[-1])
            result[row["path"]] = found
    return result


def readable(path):
    """Skip minified or generated bundles: very long average lines."""
    try:
        data = open(path, "rb").read(200_000)
    except OSError:
        return False
    lines = data.count(b"\n") + 1
    return len(data) / lines < 120


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("roots", nargs="+")
    parser.add_argument("--lang", required=True, choices=sorted(EXTENSIONS))
    parser.add_argument("--sample", type=int, default=300)
    parser.add_argument("--seed", type=int, default=7)
    parser.add_argument("--examples", type=int, default=10)
    parser.add_argument("--include-dependencies", action="store_true", help="do not skip node_modules")
    args = parser.parse_args()
    repo = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    skip = SKIP_PARTS - ({"node_modules"} if args.include_dependencies else set())
    files = []
    for root in args.roots:
        for directory, subdirectories, names in os.walk(root):
            subdirectories[:] = [d for d in subdirectories if d not in skip]
            for name in names:
                if name.endswith(EXTENSIONS[args.lang]) and not name.endswith((".d.ts", ".min.js")):
                    path = os.path.join(directory, name)
                    if os.path.getsize(path) < 1 << 20:
                        files.append(path)
    rng = random.Random(args.seed)
    files = sorted(files)
    rng.shuffle(files)
    paths = [path for path in files if readable(path)][:args.sample]
    mine = ours(paths, repo)
    tallies = defaultdict(Counter)
    misses = defaultdict(list)
    errored = 0
    for path in paths:
        expected, error = reference(path, args.lang)
        if error:
            errored += 1
            continue
        got = mine.get(path, {})
        for kind in set(expected) | set(got):
            want, have = {n for n in expected.get(kind, set()) if n}, got.get(kind, set())
            tallies[kind]["both"] += len(want & have)
            tallies[kind]["reference_only"] += len(want - have)
            tallies[kind]["ours_only"] += len(have - want)
            misses[(kind, "missed")] += [(path, n) for n in sorted(want - have)]
            misses[(kind, "extra")] += [(path, n) for n in sorted(have - want)]
    print(f"{args.lang}: {len(paths)} files sampled, {errored} skipped for tree-sitter parse errors")
    for kind in ("fn", "method", "type", "module", "value", "macro"):
        tally = tallies.get(kind)
        if not tally:
            continue
        both = tally["both"]
        precision = both / max(both + tally["ours_only"], 1)
        recall = both / max(both + tally["reference_only"], 1)
        print(f"  {kind:7} precision {precision:.3f} recall {recall:.3f}  "
              f"(both {both}, reference only {tally['reference_only']}, ours only {tally['ours_only']})")
    for (kind, what), rows in sorted(misses.items()):
        if rows and args.examples:
            print(f"\n{kind} {what} ({len(rows)}):")
            for path, name in rng.sample(rows, min(args.examples, len(rows))):
                print(f"  {name}  {path}")


if __name__ == "__main__":
    main()
