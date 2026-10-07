#!/usr/bin/env python3
"""Reference baseline: BM25 over each file's full text plus its path.

Usage: bm25.py DIR QUERY  -> ranked relative paths, one per line.
Deliberately plain (identifier splitting, lowercase, no stemming) so it
measures what full-text evidence alone buys.
"""

import math
import os
import re
import sys
from collections import Counter

EXTENSIONS = {
    "rs", "ts", "tsx", "py", "js", "jsx", "mjs", "cjs", "go", "java", "kt", "kts", "swift", "rb",
    "php", "cs", "c", "h", "cc", "cpp", "hpp", "sh", "html", "css", "scss", "svelte", "vue",
}
WORD = re.compile(r"[A-Za-z][a-z]+|[A-Z]+(?![a-z])|[0-9]+")


def tokens(text):
    return [w.lower() for w in WORD.findall(text) if len(w) > 1]


def main():
    root, query = sys.argv[1], sys.argv[2]
    docs = {}
    for directory, subdirectories, files in os.walk(root):
        subdirectories[:] = [d for d in subdirectories if not d.startswith(".") and d not in ("target", "node_modules")]
        for name in files:
            if name.rsplit(".", 1)[-1].lower() in EXTENSIONS:
                path = os.path.join(directory, name)
                relative = os.path.relpath(path, root)
                try:
                    text = open(path, encoding="utf-8", errors="ignore").read()
                except OSError:
                    continue
                docs[relative] = Counter(tokens(text) + tokens(relative) * 3)
    if not docs:
        return
    lengths = {path: sum(counts.values()) for path, counts in docs.items()}
    average = sum(lengths.values()) / len(lengths)
    frequency = Counter(term for counts in docs.values() for term in counts)
    terms = set(tokens(query))
    k1, b, n = 1.2, 0.75, len(docs)
    scores = {}
    for path, counts in docs.items():
        score = 0.0
        for term in terms:
            tf = counts.get(term, 0)
            if tf:
                idf = math.log(1 + (n - frequency[term] + 0.5) / (frequency[term] + 0.5))
                score += idf * tf * (k1 + 1) / (tf + k1 * (1 - b + b * lengths[path] / average))
        scores[path] = score
    for path in sorted(scores, key=lambda p: (-scores[p], p))[:50]:
        print(path)


if __name__ == "__main__":
    main()
