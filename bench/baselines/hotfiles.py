#!/usr/bin/env python3
"""Diagnostic: how far a query-free "hot files" prior gets on a case file.

For each case, rank files by how often they are gold in the OTHER cases of the
same repository (leave-one-out). It never reads the query or the code, so any
mapper scoring near it may be exploiting label frequency, not understanding.

Usage: hotfiles.py CASES.jsonl  -> SCORE: <100 * mean nDCG@10>
"""

import json
import math
import sys
from collections import Counter


def main():
    cases = [json.loads(line) for line in open(sys.argv[1]) if line.strip()]
    total = 0.0
    for case in cases:
        counts = Counter(
            g for other in cases if other["repo"] == case["repo"] and other["id"] != case["id"] for g in other["gold"]
        )
        order = [path for path, _ in counts.most_common(20)]
        gold = set(case["gold"])
        dcg = sum(1 / math.log2(i + 2) for i, path in enumerate(order[:10]) if path in gold)
        ideal = sum(1 / math.log2(i + 2) for i in range(min(len(gold), 10)))
        total += dcg / ideal
    print(f"SCORE: {100 * total / len(cases):.4f}")


if __name__ == "__main__":
    main()
