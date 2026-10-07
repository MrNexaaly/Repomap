#!/usr/bin/env python3
"""Mine localization cases from git history: commit subject -> files it modified.

Each case is evaluated against a snapshot of the commit's PARENT, so the text
the commit added cannot leak into the index. Snapshots hold only the files a
mapper can use (source extensions plus manifests/READMEs/.gitignore) and live
under ~/.cache/repomap-bench (never /tmp: it is a quota'd tmpfs).
"""

from __future__ import annotations

import argparse
import fnmatch
import hashlib
import json
import re
import subprocess
import sys
import tarfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
CACHE = Path.home() / ".cache" / "repomap-bench"

# Mirrors SOURCE_EXTENSIONS in src/repo_map.rs: a gold file the mapper can
# never index would only measure the extension list.
SOURCE_EXTENSIONS = {
    "rs", "ts", "tsx", "py", "js", "jsx", "mjs", "cjs", "go", "java", "kt", "kts", "swift", "rb",
    "php", "cs", "c", "h", "cc", "cpp", "hpp", "sh", "html", "css", "scss", "svelte", "vue",
}
EXTRA_FILES = ["README*", "Cargo.toml", "package.json", "go.mod", "pyproject.toml", ".gitignore"]
SKIP_SUBJECT = re.compile(
    r"^(merge|revert|bump|release|version|wip|fmt|format|lint|typo|initial commit|deps\b)",
    re.IGNORECASE,
)
# Only real conventional-commit types are dropped. "test"/"docs" stay as words
# because they state the task's intent, and a subsystem prefix such as
# "hpack:" or "Webmail:" is part of the task description, not noise.
CONVENTIONAL = re.compile(
    r"^(?:feat|fix|chore|refactor|perf|style|build|ci|bench)(?:\(([^)]*)\))?!?:\s*"
)
# A single file may be the answer to at most this many cases per repository,
# so a mapper cannot score by always listing a repository's hottest files.
MAX_CASES_PER_GOLD_FILE = 6
STOP_TOKENS = {"src", "lib", "mod", "index", "main", "the", "and", "for", "with", "from", "into"}


def git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(repo), *args], check=True, capture_output=True, text=True
    ).stdout


PULL_REQUEST = re.compile(r"\s*\(#\d+\)\s*$")


def clean_query(subject: str) -> str:
    """Drop a conventional-commit type but keep its scope as ordinary words,
    so "fix(app): x" and "App: x" carry the same hint. A trailing pull-request
    number "(#1234)" is noise and goes."""
    subject = PULL_REQUEST.sub("", subject)
    match = CONVENTIONAL.match(subject)
    if not match:
        return subject.strip()
    scope = match.group(1)
    rest = subject[match.end():].strip()
    return f"{scope}: {rest}" if scope else rest


def words(text: str) -> set[str]:
    """Lowercase word tokens with snake_case, kebab-case and camelCase split."""
    parts = re.findall(r"[A-Z]+(?![a-z])|[A-Z]?[a-z]+|[0-9]+", text)
    return {part.lower() for part in parts if len(part) >= 3} - STOP_TOKENS


def test_path(path: str) -> bool:
    lowered = path.lower()
    name = Path(lowered).name
    return (
        "/tests/" in f"/{lowered}" or "/test/" in f"/{lowered}" or "__tests__" in lowered
        or name.startswith("test_") or ".test." in name or ".spec." in name or name.endswith("_test.go")
    )


def source_path(path: str) -> bool:
    return Path(path).suffix.lstrip(".").lower() in SOURCE_EXTENSIONS


def commits(repo: Path, revisions: str | None = None):
    log = git(repo, "log", "--no-merges", "--no-renames", "--format=%x00%H %P%x01%s", "--name-status",
              *([revisions] if revisions else []))
    for block in log.split("\x00")[1:]:
        header, _, body = block.partition("\n")
        shas, _, subject = header.partition("\x01")
        parts = shas.split()
        if len(parts) != 2:
            continue  # root commit: no parent snapshot
        changes = [line.split("\t", 1) for line in body.strip().splitlines() if "\t" in line]
        yield parts[0], parts[1], subject.strip(), changes


def eligible(query: str, changes: list[list[str]]) -> list[str] | None:
    if len(query.split()) < 4 or SKIP_SUBJECT.match(query):
        return None
    if not changes or len(changes) > 15:
        return None
    gold = sorted(path for status, path in changes if status == "M" and source_path(path))
    if not 1 <= len(gold) <= 5:
        return None
    return gold


def snapshot(repo: Path, name: str, sha: str, root: Path) -> Path:
    destination = root / name / sha
    done = destination / ".repomap-bench-complete"
    if done.exists():
        return destination
    destination.mkdir(parents=True, exist_ok=True)
    # git archive refuses a pathspec that matches nothing, so stream the whole
    # tree and keep only the files a mapper can use.
    archive = subprocess.Popen(
        ["git", "-C", str(repo), "archive", "--format=tar", sha], stdout=subprocess.PIPE
    )
    with tarfile.open(fileobj=archive.stdout, mode="r|") as stream:
        for member in stream:
            name = Path(member.name).name
            if member.isfile() and (
                source_path(member.name) or any(fnmatch.fnmatch(name, glob) for glob in EXTRA_FILES)
            ):
                stream.extract(member, destination, filter="data")
    if archive.wait() != 0:
        raise RuntimeError(f"git archive failed for {repo} {sha}")
    done.touch()
    return destination


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", default=str(HERE / "repos.json"))
    parser.add_argument("--out", default=str(HERE / "cases" / "fitness.jsonl"))
    parser.add_argument("--snap", default=str(CACHE / "snap"), help="snapshot directory")
    arguments = parser.parse_args()

    config = json.loads(Path(arguments.config).read_text())
    cases_out = []
    for entry in config["repos"]:
        repo = Path(entry["path"]).expanduser()
        # Fixed-snapshot mode (huge trees): every task is a commit made after
        # an existing checkout, and its gold files must exist in that tree.
        # The tree predates every task, so nothing a task added can leak in.
        fixed = Path(entry["snapshot"]).expanduser() if entry.get("snapshot") else None
        found = []
        for sha, parent, subject, changes in commits(repo, entry.get("range")):
            query = clean_query(subject)
            gold = eligible(query, changes)
            if gold is None:
                continue
            if fixed is not None and not all((fixed / g).is_file() for g in gold):
                continue
            # "lexical": the query shares a word with a gold file's path (it
            # names the file or its module). "conceptual": no shared word, so
            # the mapper must bridge task words to code it has not been told.
            kind = "lexical" if words(query) & set().union(*(words(g) for g in gold)) else "conceptual"
            order = hashlib.sha256(f"{entry['name']}:{sha}".encode()).hexdigest()
            found.append((order, {
                "id": f"{entry['name']}-{sha[:10]}",
                "repo": entry["name"],
                "commit": sha,
                "parent": parent,
                "query": query,
                "gold": gold,
                "kind": kind,
                "tests_only": all(test_path(g) for g in gold),
                "added_files": sum(status == "A" for status, _ in changes),
            }))
        found.sort(key=lambda item: item[0])
        chosen, per_file = [], {}
        for _, case in found:
            if len(chosen) >= entry["max_cases"]:
                break
            if any(per_file.get(g, 0) >= MAX_CASES_PER_GOLD_FILE for g in case["gold"]):
                continue
            for g in case["gold"]:
                per_file[g] = per_file.get(g, 0) + 1
            chosen.append(case)
        for case in chosen:
            case["snapshot"] = str(fixed) if fixed is not None else str(snapshot(repo, entry["name"], case["parent"], Path(arguments.snap)))
        cases_out.extend(chosen)
        conceptual = sum(case["kind"] == "conceptual" for case in chosen)
        print(f"{entry['name']:10} eligible={len(found):4} chosen={len(chosen):3} conceptual={conceptual}", file=sys.stderr)

    out = Path(arguments.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text("".join(json.dumps(case) + "\n" for case in cases_out))
    print(f"wrote {len(cases_out)} cases to {out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
