#!/usr/bin/env python3
"""Score a repository mapper on mined localization cases.

For each case the mapper sees the parent snapshot and the task text (the
commit subject); the files that commit modified are the relevant set. Every
method is scored on the first --depth unique paths it outputs, so a longer
list buys nothing. A run that exits non-zero or times out scores 0.

Output: `case <id>: <nDCG@10>` per case, grouped tables, latency, run
metadata, and `SCORE: <100 * mean nDCG@10>` (micro average over cases).

Adapters:
  repomap  run the repomap binary and read file order from its map
  paths    run --cmd (placeholders {dir} {query} {budget}); one path per line
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import random
import re
import shlex
import statistics
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

HERE = Path(__file__).resolve().parent
MAP_LINE = re.compile(r"^(\S[^|]*?) \| \d+ LOC(?: \|.*)?$")


def command_for(arguments, case) -> list[str]:
    if arguments.adapter == "repomap":
        return [
            arguments.binary, case["snapshot"], "--query", case["query"],
            "--token-budget", str(arguments.budget), "--max-chars", "10000000",
        ]
    values = {"dir": case["snapshot"], "query": case["query"], "budget": str(arguments.budget)}
    return [part.format(**values) for part in shlex.split(arguments.cmd)]


def canonical(path: str, snapshot: str) -> str:
    path = path.strip().replace("\\", "/")
    if os.path.isabs(path):
        path = os.path.relpath(path, snapshot)
    path = os.path.normpath(path).replace("\\", "/")
    return path[2:] if path.startswith("./") else path


def ranked_paths(adapter: str, output: str, snapshot: str, depth: int) -> list[str]:
    seen, order = set(), []
    for line in output.splitlines():
        if adapter == "repomap":
            match = MAP_LINE.match(line)
            path = match.group(1) if match else None
        else:
            path = line.strip() or None
        if path:
            path = canonical(path, snapshot)
            if path not in seen:
                seen.add(path)
                order.append(path)
        if len(order) >= depth:
            break
    return order


def metrics(order: list[str], gold: list[str]) -> dict:
    gold_set = set(gold)
    ranks = [index + 1 for index, path in enumerate(order) if path in gold_set]
    dcg = sum(1 / math.log2(rank + 1) for rank in ranks if rank <= 10)
    ideal = sum(1 / math.log2(rank + 1) for rank in range(1, min(len(gold_set), 10) + 1))
    recall = lambda k: sum(rank <= k for rank in ranks) / len(gold_set)
    return {
        "ndcg10": dcg / ideal,
        "r1": recall(1),
        "r5": recall(5),
        "r10": recall(10),
        "r_depth": len(ranks) / len(gold_set),
        "mrr": 1 / ranks[0] if ranks else 0.0,
        "ranks": ranks,
    }


def execute(arguments, case):
    command = command_for(arguments, case)
    started = time.perf_counter()
    try:
        result = subprocess.run(command, capture_output=True, text=True, timeout=arguments.timeout)
    except subprocess.TimeoutExpired:
        return None, time.perf_counter() - started, "timeout"
    elapsed = time.perf_counter() - started
    if result.returncode != 0:
        return None, elapsed, f"exit {result.returncode}: {result.stderr.strip()[:160]}"
    return result.stdout, elapsed, None


def run_case(arguments, case) -> dict:
    if arguments.warmup:
        execute(arguments, case)
    output, elapsed, error = execute(arguments, case)
    order = ranked_paths(arguments.adapter, output, case["snapshot"], arguments.depth) if output is not None else []
    scores = metrics(order, case["gold"])
    if error:
        print(f"failure: {case['id']}: {error}", file=sys.stderr)
    return {
        **scores,
        "id": case["id"], "repo": case["repo"], "kind": case["kind"],
        "tests_only": case.get("tests_only", False), "seconds": elapsed,
        "failed": error is not None, "order": order,
    }


def mean(rows, key):
    return sum(row[key] for row in rows) / len(rows) if rows else 0.0


def percentile(values, fraction):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(fraction * len(ordered)))]


def bootstrap_ci(values, rounds=2000, seed=7):
    generator = random.Random(seed)
    means = sorted(
        sum(generator.choice(values) for _ in values) / len(values) for _ in range(rounds)
    )
    return means[int(0.025 * rounds)], means[int(0.975 * rounds)]


def sha256(path: str) -> str:
    try:
        return hashlib.sha256(Path(path).read_bytes()).hexdigest()[:16]
    except OSError:
        return "unreadable"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--split", default="fitness", help="a name under bench/cases/ or a path to a .jsonl")
    parser.add_argument("--adapter", choices=["repomap", "paths"], default="repomap")
    parser.add_argument("--binary", default=str(HERE.parent / "target/release/repomap"))
    parser.add_argument("--cmd", help="command template for --adapter paths")
    parser.add_argument("--budget", type=int, default=2048, help="map token budget")
    parser.add_argument("--depth", type=int, default=20, help="paths scored per case, for every method")
    parser.add_argument("--jobs", type=int, default=1, help=">1 is faster but makes latency noisy")
    parser.add_argument("--timeout", type=float, default=120)
    parser.add_argument("--warmup", action="store_true", help="run each case once untimed first")
    parser.add_argument("--max-p95-ms", type=float, help="exit 3 when p95 latency exceeds this")
    parser.add_argument("--json", help="write per-case results here")
    parser.add_argument("--quiet", action="store_true", help="omit per-case lines")
    arguments = parser.parse_args()
    if arguments.adapter == "paths" and not arguments.cmd:
        parser.error("--adapter paths needs --cmd")

    source = Path(arguments.split)
    if not source.suffix:
        source = HERE / "cases" / f"{arguments.split}.jsonl"
    cases = [json.loads(line) for line in source.read_text().splitlines() if line.strip()]

    with ThreadPoolExecutor(max_workers=arguments.jobs) as pool:
        rows = list(pool.map(lambda case: run_case(arguments, case), cases))

    if not arguments.quiet:
        for row in rows:
            print(f"case {row['id']}: {row['ndcg10']:.4f}")
    groups = [("all", rows)]
    groups += [(f"repo={name}", [r for r in rows if r["repo"] == name]) for name in sorted({r["repo"] for r in rows})]
    groups += [(f"kind={name}", [r for r in rows if r["kind"] == name]) for name in ("lexical", "conceptual")]
    groups += [("tests_only", [r for r in rows if r["tests_only"]])]
    print(f"{'group':22} {'n':>4} {'nDCG@10':>8} {'R@1':>6} {'R@5':>6} {'R@10':>6} {'MRR':>6} {'R@depth':>7}")
    for name, group in groups:
        if group:
            print(
                f"{name:22} {len(group):4} {mean(group, 'ndcg10'):8.4f} {mean(group, 'r1'):6.3f} "
                f"{mean(group, 'r5'):6.3f} {mean(group, 'r10'):6.3f} {mean(group, 'mrr'):6.3f} "
                f"{mean(group, 'r_depth'):7.3f}"
            )
    repos = sorted({r["repo"] for r in rows})
    macro = sum(mean([r for r in rows if r["repo"] == name], "ndcg10") for name in repos) / len(repos)
    low, high = bootstrap_ci([row["ndcg10"] for row in rows])
    seconds = [row["seconds"] for row in rows]
    p95 = 1000 * percentile(seconds, 0.95)
    failures = sum(row["failed"] for row in rows)
    print(
        f"latency_ms p50={1000 * statistics.median(seconds):.1f} p95={p95:.1f} "
        f"max={1000 * max(seconds):.1f} total_s={sum(seconds):.1f} warmup={arguments.warmup} jobs={arguments.jobs}"
    )
    print(
        f"# cases={source.name}:{sha256(str(source))} n={len(rows)} failures={failures} depth={arguments.depth} "
        f"budget={arguments.budget} adapter={arguments.adapter} "
        + (f"binary={sha256(arguments.binary)}" if arguments.adapter == "repomap" else f"cmd={arguments.cmd!r}")
    )
    print(f"MACRO: {100 * macro:.4f}  CI95: [{100 * low:.2f}, {100 * high:.2f}]")
    if arguments.json:
        Path(arguments.json).write_text(json.dumps(rows, indent=1))
    print(f"SCORE: {100 * mean(rows, 'ndcg10'):.4f}")
    if arguments.max_p95_ms is not None and p95 > arguments.max_p95_ms:
        print(f"latency gate failed: p95 {p95:.1f} ms > {arguments.max_p95_ms} ms", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    sys.exit(main())
