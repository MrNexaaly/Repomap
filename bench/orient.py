#!/usr/bin/env python3
"""Does a map orient a new agent? Agent-in-the-loop test.

Per repository, a fresh agent (codex, read-only, told not to run commands)
sees ONE map of a snapshot and a list of real tasks (commit subjects), and
names up to 5 files it would open first for each. Scored against the files
each commit modified: file recall@5 and directory hit@5 (a named file shares
the directory of a gold file). Conditions are given the same size.

  tree      the plain sorted list of source files (what ls/glob gives)
  legacy    repomap without a query (entrypoint-then-size list)
  overview  repomap --overview

Usage: orient.py [--cases bench/cases/fitness.jsonl] [--conditions tree,legacy,overview]
Results: bench/results/orient-<split>.json (+ a table on stdout).
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

HERE = Path(__file__).resolve().parent
BINARY = Path(os.environ.get("REPOMAP_BINARY", HERE.parent / "target/release/repomap"))
BUDGET_TOKENS = 2000
BUDGET_CHARS = BUDGET_TOKENS * 4
MAX_TASKS = 40

SCHEMA = {
    "type": "object",
    "additionalProperties": False,
    "required": ["answers"],
    "properties": {
        "answers": {
            "type": "array",
            "items": {
                "type": "object",
                "additionalProperties": False,
                "required": ["id", "files"],
                "properties": {
                    "id": {"type": "string"},
                    "files": {"type": "array", "items": {"type": "string"}, "maxItems": 5},
                },
            },
        }
    },
}

PROMPT = """You are an engineer who has never seen this repository. All you have is
the map below. Do not run any commands or read any files: answer from the map
alone. For each task, name up to 5 existing files (paths relative to the
repository root) you would open first to do it, best first. If the map does
not list a file you need, infer its path from the layout shown.

=== MAP ===
{map}
=== END MAP ===

Tasks (JSON):
{tasks}

Your final message must be the JSON object only: {{"answers": [{{"id": ..., "files": [...]}}]}},
one entry per task id.
"""


def snapshot_files(root: str) -> list[str]:
    listed = subprocess.run(["rg", "--files", root], capture_output=True, text=True).stdout.split("\n")
    exts = {"rs", "ts", "tsx", "py", "js", "jsx", "mjs", "cjs", "go", "java", "kt", "kts", "swift", "rb",
            "php", "cs", "c", "h", "cc", "cpp", "hpp", "sh", "html", "css", "scss", "svelte", "vue"}
    return sorted(os.path.relpath(p, root) for p in listed if p and p.rsplit(".", 1)[-1].lower() in exts)


def render(condition: str, root: str) -> str:
    env = {**os.environ, "REPOMAP_CACHE": "off"}
    if condition == "tree":
        text = "\n".join(snapshot_files(root))
    elif condition == "legacy":
        text = subprocess.run([str(BINARY), root, "--list", "--max-chars", str(BUDGET_CHARS)], capture_output=True, text=True, env=env).stdout
    else:
        text = subprocess.run([str(BINARY), root, "--overview", "--token-budget", str(BUDGET_TOKENS)], capture_output=True, text=True, env=env).stdout
    text = text.replace(root.rstrip("/") + "/", "").replace(root.rstrip("/"), ".")
    if len(text) > BUDGET_CHARS:
        text = text[:BUDGET_CHARS] + "\n... (truncated to the same size as the other maps)"
    return text


def ask(condition: str, repo: str, root: str, tasks: list[dict], workdir: Path) -> dict:
    prompt = PROMPT.format(map=render(condition, root), tasks=json.dumps([{"id": t["id"], "task": t["query"]} for t in tasks], indent=1))
    stem = workdir / f"{repo}-{condition}"
    (stem.with_suffix(".prompt.md")).write_text(prompt)
    schema = stem.with_suffix(".schema.json")
    schema.write_text(json.dumps(SCHEMA))
    answer = stem.with_suffix(".answer.json")
    events = stem.with_suffix(".events.jsonl")
    empty = tempfile.mkdtemp(prefix="orient-", dir=str(workdir))
    with open(events, "w") as sink:
        subprocess.run(
            ["codex", "exec", "--skip-git-repo-check", "--json", "-s", "read-only", "-C", empty,
             "--output-schema", str(schema), "-o", str(answer), prompt],
            stdin=subprocess.DEVNULL, stdout=sink, stderr=subprocess.STDOUT, timeout=1800,
        )
    commands = sum('"command_execution"' in line for line in open(events))
    try:
        answers = {a["id"]: a["files"] for a in json.loads(answer.read_text())["answers"]}
    except (OSError, ValueError, KeyError):
        answers = {}
    return {"condition": condition, "repo": repo, "answers": answers, "commands_run": commands, "map_chars": len(prompt)}


def score(files: list[str], gold: list[str]) -> tuple[float, float]:
    files = [f.strip().lstrip("./") for f in files][:5]
    recall = sum(g in files for g in gold) / len(gold)
    gold_dirs = {os.path.dirname(g) for g in gold}
    dir_hit = float(any(os.path.dirname(f) in gold_dirs for f in files))
    return recall, dir_hit


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--cases", default=str(HERE / "cases" / "fitness.jsonl"))
    parser.add_argument("--conditions", default="tree,legacy,overview")
    parser.add_argument("--out", default=str(HERE / "results"))
    arguments = parser.parse_args()
    cases = [json.loads(line) for line in open(arguments.cases) if line.strip()]
    out = Path(arguments.out)
    out.mkdir(parents=True, exist_ok=True)
    workdir = Path.home() / ".cache" / "repomap-bench" / "orient" / Path(arguments.cases).stem
    workdir.mkdir(parents=True, exist_ok=True)

    jobs = []
    for repo in sorted({c["repo"] for c in cases}):
        mine = [c for c in cases if c["repo"] == repo]
        # One snapshot per repository: the one in which the most tasks' gold
        # files all exist, so every task shown is answerable from that tree.
        listings = {root: set(snapshot_files(root)) for root in sorted({c["snapshot"] for c in mine})}
        root = max(listings, key=lambda r: (sum(all(g in listings[r] for g in c["gold"]) for c in mine), r))
        tasks = [c for c in mine if all(g in listings[root] for g in c["gold"])][:MAX_TASKS]
        for condition in arguments.conditions.split(","):
            jobs.append((condition, repo, root, tasks))

    with ThreadPoolExecutor(max_workers=3) as pool:
        results = list(pool.map(lambda job: ask(*job, workdir), jobs))

    rows = []
    for result, (_, _, _, tasks) in zip(results, jobs):
        for task in tasks:
            recall, dir_hit = score(result["answers"].get(task["id"], []), task["gold"])
            rows.append({"condition": result["condition"], "repo": result["repo"], "id": task["id"],
                         "kind": task["kind"], "recall5": recall, "dir_hit5": dir_hit})
    (out / f"orient-{Path(arguments.cases).stem}.json").write_text(json.dumps({"results": results, "rows": rows}, indent=1))

    print(f"{'condition':10} {'repo':10} {'n':>3} {'file R@5':>9} {'dir hit@5':>10} {'answered':>9} {'cmds':>5}")
    for result, (condition, repo, _, tasks) in zip(results, jobs):
        mine = [r for r in rows if r["condition"] == condition and r["repo"] == repo]
        print(f"{condition:10} {repo:10} {len(mine):3} {sum(r['recall5'] for r in mine) / len(mine):9.3f} "
              f"{sum(r['dir_hit5'] for r in mine) / len(mine):10.3f} {len(result['answers']):9} {result['commands_run']:5}")
    for condition in arguments.conditions.split(","):
        mine = [r for r in rows if r["condition"] == condition]
        print(f"{condition:10} {'ALL':10} {len(mine):3} {sum(r['recall5'] for r in mine) / len(mine):9.3f} "
              f"{sum(r['dir_hit5'] for r in mine) / len(mine):10.3f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
