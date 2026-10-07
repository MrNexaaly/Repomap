#!/usr/bin/env python3
"""Prototype: does the repository's own commit history bridge task words to files?

Each file gets a "history document": the subjects of past commits that
modified it, taken ONLY from the snapshot's parent commit and its ancestors
(what a live tool can read from .git; nothing after the task leaks in). The
task is matched against those documents with BM25, and that ranking is fused
with repomap's own order by reciprocal rank fusion.

Usage: history_fusion.py DIR QUERY BUDGET [--weight W] [--history-only]
DIR must be a benchmark snapshot (.../<repo name>/<parent sha>); the repo
path comes from bench/repos.json or the held-out config.
"""

import json
import math
import os
import re
import subprocess
import sys
from collections import Counter, defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))
BINARY = os.environ.get("REPOMAP_BINARY", os.path.join(HERE, "..", "..", "target", "release", "repomap"))
CONFIGS = [os.path.join(HERE, "..", "repos.json"), os.path.expanduser("~/.cache/repomap-bench/heldout/repos.json")]
WORD = re.compile(r"[A-Z]+(?![a-z])|[A-Z]?[a-z]+|[0-9]+")
MAP_LINE = re.compile(r"^(\S[^|]*?) \| \d+ LOC(?: \|.*)?$")


def words(text):
    out = []
    for w in WORD.findall(text):
        w = w.lower()
        if len(w) > 1:
            out.append(w[:-1] if len(w) > 4 and w.endswith("s") and not w.endswith("ss") else w)
    return out


def repo_path(name):
    for config in CONFIGS:
        try:
            for repo in json.load(open(config))["repos"]:
                if repo["name"] == name:
                    return repo["path"]
        except OSError:
            pass
    sys.exit(f"unknown repo {name}")


def history_docs(repo, parent, cache_dir):
    cache = os.path.join(cache_dir, f"{os.path.basename(repo)}-{parent}.json")
    if os.path.exists(cache):
        return json.load(open(cache))
    log = subprocess.run(
        ["git", "-C", repo, "log", "--no-merges", "--format=%x00%s", "--name-only", "-n", "3000", parent],
        capture_output=True, text=True, check=True,
    ).stdout
    docs = defaultdict(list)
    for block in log.split("\x00")[1:]:
        subject, _, names = block.partition("\n")
        for name in names.split():
            docs[name].append(subject)
    os.makedirs(cache_dir, exist_ok=True)
    json.dump(docs, open(cache, "w"))
    return docs


def main():
    root, query, budget = sys.argv[1], sys.argv[2], sys.argv[3]
    weight = float(sys.argv[sys.argv.index("--weight") + 1]) if "--weight" in sys.argv else 1.0
    history_only = "--history-only" in sys.argv
    name, parent = os.path.basename(os.path.dirname(root.rstrip("/"))), os.path.basename(root.rstrip("/"))
    docs = history_docs(repo_path(name), parent, os.path.expanduser("~/.cache/repomap-bench/history"))
    present = {os.path.relpath(os.path.join(d, f), root) for d, _, fs in os.walk(root) for f in fs}
    bags = {path: Counter(words(" ".join(subjects))) for path, subjects in docs.items() if path in present}
    terms = set(words(query))
    scores = {}
    if bags:
        lengths = {p: sum(c.values()) for p, c in bags.items()}
        average = sum(lengths.values()) / len(lengths)
        df = Counter(t for c in bags.values() for t in c)
        n = len(bags)
        for path, counts in bags.items():
            s = 0.0
            for t in terms:
                tf = counts.get(t, 0)
                if tf:
                    idf = math.log(1 + (n - df[t] + 0.5) / (df[t] + 0.5))
                    s += idf * tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * lengths[path] / average))
            if s > 0:
                scores[path] = s
    history = sorted(scores, key=lambda p: (-scores[p], p))
    if history_only:
        print("\n".join(history[:50]))
        return
    output = subprocess.run([BINARY, root, "--query", query, "--token-budget", budget, "--max-chars", "10000000"],
                            capture_output=True, text=True).stdout
    mapped = [m.group(1) for m in map(MAP_LINE.match, output.splitlines()) if m]
    fused = defaultdict(float)
    for rank, path in enumerate(mapped):
        fused[path] += 1 / (60 + rank)
    for rank, path in enumerate(history[:50]):
        fused[path] += weight / (60 + rank)
    print("\n".join(sorted(fused, key=lambda p: (-fused[p], p))[:50]))


if __name__ == "__main__":
    main()
