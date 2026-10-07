#!/usr/bin/env python3
"""Make a "bare" variant of a case file: drop leading subsystem prefixes.

Kernel-style subjects start with the path they touch ("KVM: arm64: ...",
"drm/xe/multi_queue: ...", "hwmon: (sht3x) ..."). A newcomer asking "where is
X" does not know that prefix, so the bare variant keeps only the description.

Usage: strip_prefix.py IN.jsonl OUT.jsonl
"""

import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from mine import STOP_TOKENS, words  # noqa: E402

PREFIX = re.compile(r"^(?:[^\s:]+(?: [^\s:]+)?:\s+|\([^)]*\)\s+|\[[^\]]*\]\s*)")

out = []
for case in map(json.loads, open(sys.argv[1])):
    bare = case["query"]
    while (stripped := PREFIX.sub("", bare, count=1)) != bare:
        bare = stripped
    if len(bare.split()) < 3:
        continue
    kind = "lexical" if words(bare) & set().union(*(words(g) for g in case["gold"])) else "conceptual"
    out.append({**case, "task_with_prefix": case["query"], "query": bare, "kind": kind})
Path(sys.argv[2]).write_text("".join(json.dumps(c) + "\n" for c in out))
print(f"{len(out)} bare cases -> {sys.argv[2]}")
