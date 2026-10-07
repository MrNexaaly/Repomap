#!/usr/bin/env python3
"""Paired comparison of two run.py --json results on the same cases.

Usage: compare.py BASE.json CANDIDATE.json
Prints the mean nDCG@10 difference (candidate - base) with a paired bootstrap
95% interval, per-repository deltas, and win/tie/loss counts. A difference
whose interval includes 0 is not a demonstrated improvement.
"""

import json
import random
import sys


def main():
    base = {row["id"]: row for row in json.load(open(sys.argv[1]))}
    candidate = {row["id"]: row for row in json.load(open(sys.argv[2]))}
    shared = sorted(base.keys() & candidate.keys())
    if len(shared) != len(base) or len(shared) != len(candidate):
        print(f"warning: comparing {len(shared)} shared of {len(base)}/{len(candidate)} cases", file=sys.stderr)
    deltas = [candidate[i]["ndcg10"] - base[i]["ndcg10"] for i in shared]
    generator = random.Random(11)
    means = sorted(sum(generator.choice(deltas) for _ in deltas) / len(deltas) for _ in range(4000))
    print(
        f"delta nDCG@10 x100: {100 * sum(deltas) / len(deltas):+.2f}  "
        f"CI95 [{100 * means[100]:+.2f}, {100 * means[3899]:+.2f}]  n={len(deltas)}"
    )
    print(
        f"wins={sum(d > 1e-9 for d in deltas)} ties={sum(abs(d) <= 1e-9 for d in deltas)} "
        f"losses={sum(d < -1e-9 for d in deltas)}"
    )
    for repo in sorted({base[i]["repo"] for i in shared}):
        ids = [i for i in shared if base[i]["repo"] == repo]
        values = [candidate[i]["ndcg10"] - base[i]["ndcg10"] for i in ids]
        print(f"  {repo:12} n={len(ids):3} delta={100 * sum(values) / len(values):+.2f}")


if __name__ == "__main__":
    main()
