#!/usr/bin/env python3
"""End to end: how well does an agent localize with repomap's maps?

Like orient.py (a fresh codex, read-only, told to run no commands, names up to
5 files per task), but each task shows its own map:

  taskmap   repomap --query for that task (TASK_BUDGET tokens)
  both      the repository overview, then that task map
  compact / both-compact   the same with --compact task maps (one line per file)

Compare with orient.py's `overview` (overview only) and with the ranker's own
top 5 (bench/run.py R@5) to see what the agent adds on top of the tool.
One snapshot per repository, as in orient.py, so all conditions see the
same tree.

Usage: agent_eval.py [--cases ...] [--conditions taskmap,both] [--out DIR]
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from orient import SCHEMA, render, score, snapshot_files  # noqa: E402

HERE = Path(__file__).resolve().parent
BINARY = Path(os.environ.get("REPOMAP_BINARY", HERE.parent / "target/release/repomap"))
TASK_BUDGET = 1000
MAX_TASKS = 40

PROMPT = """You are an engineer who has never seen this repository. For each task below
you get a map produced by a code-map tool{overview_note}. Do not run any
commands or read any files: answer from the maps alone. For each task, name
up to 5 existing files (paths relative to the repository root) you would open
first to do it, best first.
{overview}
{tasks}

Your final message must be the JSON object only: {{"answers": [{{"id": ..., "files": [...]}}]}},
one entry per task id.
"""


def task_map(root: str, query: str, compact: bool = False) -> str:
    env = {**os.environ, "REPOMAP_CACHE": "off"}
    text = subprocess.run([str(BINARY), root, "--query", query, "--token-budget", str(TASK_BUDGET)]
                          + (["--compact"] if compact else []), capture_output=True, text=True, env=env).stdout
    return text.replace(root.rstrip("/") + "/", "")


def ask(condition, repo, root, tasks, workdir):
    overview = ""
    if condition in ("both", "both-compact"):
        overview = "\n=== REPOSITORY OVERVIEW ===\n" + render("overview", root) + "\n=== END OVERVIEW ===\n"
    compact = condition.endswith("compact")
    blocks = [f"=== TASK {t['id']} ===\n{t['query']}\n--- task map ---\n{task_map(root, t['query'], compact)}" for t in tasks]
    prompt = PROMPT.format(
        overview_note=" (and, once, an overview of the whole repository)" if overview else "",
        overview=overview,
        tasks="\n".join(blocks),
    )
    stem = workdir / f"{repo}-{condition}"
    stem.with_suffix(".prompt.md").write_text(prompt)
    schema = stem.with_suffix(".schema.json")
    schema.write_text(json.dumps(SCHEMA))
    answer = stem.with_suffix(".answer.json")
    events = stem.with_suffix(".events.jsonl")
    # A finished answer for the identical prompt is reused, not re-asked.
    if not answer.exists():
        empty = tempfile.mkdtemp(prefix="agent-", dir=str(workdir))
        # The prompt goes on stdin ("-"): one argument is capped at 128 KiB.
        with open(events, "w") as sink:
            subprocess.run(["codex", "exec", "--skip-git-repo-check", "--json", "-s", "read-only", "-C", empty,
                            "--output-schema", str(schema), "-o", str(answer), "-"],
                           input=prompt, text=True, stdout=sink, stderr=subprocess.STDOUT, timeout=3600)
    commands = sum('"command_execution"' in line for line in open(events))
    try:
        answers = {a["id"]: a["files"] for a in json.loads(answer.read_text())["answers"]}
    except (OSError, ValueError, KeyError):
        answers = {}
    return {"condition": condition, "repo": repo, "answers": answers, "commands_run": commands, "prompt_chars": len(prompt)}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--cases", default=str(HERE / "cases" / "fitness.jsonl"))
    parser.add_argument("--conditions", default="taskmap,both")
    parser.add_argument("--out", default=str(HERE / "results"))
    arguments = parser.parse_args()
    cases = [json.loads(line) for line in open(arguments.cases) if line.strip()]
    out = Path(arguments.out)
    out.mkdir(parents=True, exist_ok=True)
    workdir = Path.home() / ".cache" / "repomap-bench" / "agent" / Path(arguments.cases).stem
    workdir.mkdir(parents=True, exist_ok=True)
    jobs = []
    for repo in sorted({c["repo"] for c in cases}):
        mine = [c for c in cases if c["repo"] == repo]
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
    (out / f"agent-{Path(arguments.cases).stem}.json").write_text(json.dumps({"results": results, "rows": rows}, indent=1))
    print(f"{'condition':10} {'repo':10} {'n':>3} {'file R@5':>9} {'dir hit@5':>10} {'answered':>9} {'cmds':>5}")
    for result, (condition, repo, _, tasks) in zip(results, jobs):
        mine = [r for r in rows if r["condition"] == condition and r["repo"] == repo]
        print(f"{condition:10} {repo:10} {len(mine):3} {sum(r['recall5'] for r in mine) / len(mine):9.3f} "
              f"{sum(r['dir_hit5'] for r in mine) / len(mine):10.3f} {len(result['answers']):9} {result['commands_run']:5}")
    for condition in arguments.conditions.split(","):
        mine = [r for r in rows if r["condition"] == condition]
        print(f"{condition:10} {'ALL':10} {len(mine):3} {sum(r['recall5'] for r in mine) / len(mine):9.3f} "
              f"{sum(r['dir_hit5'] for r in mine) / len(mine):10.3f}")


if __name__ == "__main__":
    main()
