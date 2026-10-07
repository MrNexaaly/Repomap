#!/usr/bin/env python3
"""Drop cases an independent reviewer labeled `mismatch`.

Usage: audit.py CASES.jsonl LABELS.json
LABELS.json is {"labels": [{"id", "label": "fair"|"mismatch", "reason"}]}.
Writes CASES.jsonl in place with only `fair` cases and keeps the labels next
to it (CASES.labels.json) so every exclusion stays inspectable. Refuses to
run when any case is unlabeled or labeled twice.
"""

import json
import sys
from collections import Counter
from pathlib import Path


def main():
    cases_path, labels_path = Path(sys.argv[1]), Path(sys.argv[2])
    cases = [json.loads(line) for line in cases_path.read_text().splitlines() if line.strip()]
    labels = json.loads(labels_path.read_text())["labels"]
    counts = Counter(label["id"] for label in labels)
    ids = {case["id"] for case in cases}
    problems = sorted(i for i in ids if counts[i] != 1) + sorted(i for i in counts if i not in ids)
    if problems:
        sys.exit(f"labels do not cover the cases exactly once: {problems[:10]}")
    verdict = {label["id"]: label for label in labels}
    kept = [case for case in cases if verdict[case["id"]]["label"] == "fair"]
    dropped = [verdict[case["id"]] for case in cases if verdict[case["id"]]["label"] != "fair"]
    cases_path.write_text("".join(json.dumps(case) + "\n" for case in kept))
    cases_path.with_suffix(".labels.json").write_text(json.dumps({"labels": labels}, indent=1) + "\n")
    print(f"kept {len(kept)} of {len(cases)}; dropped {len(dropped)}")
    for label in dropped:
        print(f"  {label['id']}: {label['reason']}")


if __name__ == "__main__":
    main()
