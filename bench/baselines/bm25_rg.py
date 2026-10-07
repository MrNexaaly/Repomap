#!/usr/bin/env python3
"""BM25 for huge trees: one ripgrep count per query term, file size as length.

Usage: bm25_rg.py DIR QUERY -> ranked relative paths (top 50).
Same tokenization as bm25.py; ripgrep applies the tree's ignore files. An
approximation of plain BM25 (line counts as term frequency, bytes as
document length) that stays usable on a 66,000-file kernel.
"""

import math
import os
import re
import subprocess
import sys

EXT_GLOB = "*.{rs,ts,tsx,py,js,jsx,mjs,cjs,go,java,kt,kts,swift,rb,php,cs,c,h,cc,cpp,hpp,sh,html,css,scss,svelte,vue}"
WORD = re.compile(r"[A-Za-z][a-z]+|[A-Z]+(?![a-z])|[0-9]+")


def main():
    root, query = sys.argv[1], sys.argv[2]
    terms = sorted({w.lower() for w in WORD.findall(query) if len(w) > 1})
    counts = {}
    for term in terms:
        out = subprocess.run(["rg", "-c", "-i", "--no-messages", "-g", EXT_GLOB, term, root],
                             capture_output=True, text=True).stdout
        per_file = {}
        for line in out.splitlines():
            path, _, n = line.rpartition(":")
            if n.isdigit():
                per_file[os.path.relpath(path, root)] = int(n)
        counts[term] = per_file
    candidates = set().union(*counts.values()) if counts else set()
    if not candidates:
        return
    sizes = {p: max(os.path.getsize(os.path.join(root, p)), 1) for p in candidates}
    total_files = int(subprocess.run(f"rg --files -g '{EXT_GLOB}' {root} | wc -l", shell=True, capture_output=True, text=True).stdout or 1)
    average = sum(sizes.values()) / len(sizes)
    scores = {}
    for term, per_file in counts.items():
        df = len(per_file)
        idf = math.log(1 + (total_files - df + 0.5) / (df + 0.5))
        for path, tf in per_file.items():
            scores[path] = scores.get(path, 0.0) + idf * tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * sizes[path] / average))
    for path in sorted(scores, key=lambda p: (-scores[p], p))[:50]:
        print(path)


if __name__ == "__main__":
    main()
