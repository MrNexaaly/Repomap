#!/usr/bin/env python3
"""Prototype: rank DIRECTORIES first, then files inside the best ones.

Question: for a newcomer's task ("power down on startup RX gain failure"),
does a per-directory document find the right area of a huge tree when a
per-file ranker does not? Two kinds of directory document:

  generic   words used by the directory's own files (each file counts once
            per word, so one huge generated file cannot dominate)
  meta      what the repository says about the directory: MAINTAINERS
            section names and keywords (via F: prefixes), Kconfig prompts
            and help (via the directory's Makefile obj-$(CONFIG_...) rules),
            README text

Usage: areas.py build ROOT          (once; cached under ~/.cache/repomap-bench/proto)
       areas.py eval ROOT CASES.jsonl [--k 10] [--meta-weight W]
"""

import bisect
import json
import math
import os
import pickle
import re
import subprocess
import sys
from collections import Counter, defaultdict

EXT_GLOB = "*.{rs,ts,tsx,py,js,jsx,mjs,cjs,go,java,kt,kts,swift,rb,php,cs,c,h,cc,cpp,hpp,sh,html,css,scss,svelte,vue}"
WORD = re.compile(r"[A-Za-z][a-z]+|[A-Z]+(?![a-z])|[0-9]+")
CACHE = os.path.expanduser("~/.cache/repomap-bench/proto")


def words(text):
    out = []
    for w in WORD.findall(text):
        w = w.lower()
        if len(w) > 1:
            out.append(w[:-1] if len(w) > 4 and w.endswith("s") and not w.endswith("ss") else w)
    return out


def parent(path):
    return path.rsplit("/", 1)[0] if "/" in path else ""


def cache_file(root, kind):
    return os.path.join(CACHE, f"{root.strip('/').replace('/', '_')}.{kind}.pickle")


def source_files(root):
    out = subprocess.run(["rg", "--files", "-g", EXT_GLOB, root], capture_output=True, text=True).stdout
    return sorted(os.path.relpath(p, root) for p in out.splitlines() if p)


def maintainers(root):
    """(title words, keyword words, [directory prefixes]) per section."""
    path = os.path.join(root, "MAINTAINERS")
    if not os.path.exists(path):
        return []
    sections, current = [], None
    for line in open(path, encoding="utf-8", errors="ignore"):
        line = line.rstrip("\n")
        if not line.strip():
            current = None
            continue
        if current is None:
            if re.match(r"^[A-Z]:\s", line):
                continue
            current = {"title": line.strip(), "keywords": [], "prefixes": []}
            sections.append(current)
            continue
        tag, _, value = line.partition(":")
        value = value.strip()
        if tag == "F":
            literal = re.split(r"[*?\[]", value, maxsplit=1)[0]
            prefix = literal if literal.endswith("/") else parent(literal) + "/" if "/" in literal else ""
            current["prefixes"].append(prefix.rstrip("/"))
        elif tag in ("K", "N"):
            current["keywords"].append(re.sub(r"\\[bwsd]|[\^\$\(\)\|\[\]\\\.\*\+\?\{\}]", " ", value))
    return [s for s in sections if s["prefixes"]]


def kconfig_texts(root, files_by_dir):
    """CONFIG symbol -> prompt + help text, from every Kconfig* file."""
    texts = {}
    out = subprocess.run(["rg", "--files", "-g", "Kconfig*", root], capture_output=True, text=True).stdout
    for path in out.splitlines():
        symbol, body, in_help, help_indent = None, [], False, None
        for line in open(path, encoding="utf-8", errors="ignore"):
            stripped = line.strip()
            match = re.match(r"^(?:menu)?config\s+(\w+)", stripped)
            if match:
                if symbol:
                    texts[symbol] = " ".join(body)
                symbol, body, in_help = match.group(1), [], False
                continue
            if symbol is None:
                continue
            if in_help:
                indent = len(line) - len(line.lstrip())
                if stripped and help_indent is None:
                    help_indent = indent
                if stripped and indent < (help_indent or 1):
                    in_help = False
                else:
                    body.append(stripped)
                    continue
            prompt = re.match(r'^(?:bool|tristate|prompt|string|int|hex|def_bool|def_tristate)\s+"([^"]*)"', stripped)
            if prompt:
                body.append(prompt.group(1))
            elif stripped in ("help", "---help---"):
                in_help, help_indent = True, None
            elif re.match(r"^(?:config|menuconfig|menu|endmenu|choice|endchoice|if|endif|source|comment)\b", stripped):
                if symbol:
                    texts[symbol] = " ".join(body)
                symbol = None
        if symbol:
            texts[symbol] = " ".join(body)
    return texts


def makefile_symbols(root, directories):
    """directory -> CONFIG symbols whose objects that directory builds."""
    bound = defaultdict(set)
    for directory in directories:
        for name in ("Makefile", "Kbuild"):
            path = os.path.join(root, directory, name)
            if not os.path.exists(path):
                continue
            text = open(path, encoding="utf-8", errors="ignore").read().replace("\\\n", " ")
            for line in text.splitlines():
                for symbol in re.findall(r"\$\(CONFIG_(\w+)\)", line):
                    bound[directory].add(symbol)
                    # obj-$(CONFIG_X) += sub/ also describes that subdirectory
                    for sub in re.findall(r"([\w\-]+)/", line.split("=", 1)[-1]):
                        bound[f"{directory}/{sub}" if directory else sub].add(symbol)
    return bound


def build(root):
    os.makedirs(CACHE, exist_ok=True)
    files = source_files(root)
    generic = defaultdict(Counter)
    file_count = Counter()
    for n, relative in enumerate(files):
        try:
            text = open(os.path.join(root, relative), encoding="utf-8", errors="ignore").read(1 << 20)
        except OSError:
            continue
        directory = parent(relative)
        generic[directory].update(set(words(text)) | set(words(relative)))
        file_count[directory] += 1
        if n % 10000 == 0:
            print(f"  tokenized {n}/{len(files)}", file=sys.stderr)
    directories = sorted(generic)
    meta = defaultdict(list)
    for section in maintainers(root):
        text = section["title"] + " " + " ".join(section["keywords"])
        for prefix in set(section["prefixes"]):
            start = bisect.bisect_left(directories, prefix)
            for directory in directories[start:]:
                if directory != prefix and not directory.startswith(prefix + "/") and prefix:
                    break
                meta[directory].append(text)
    kconfig = kconfig_texts(root, None)
    for directory, symbols in makefile_symbols(root, directories).items():
        for symbol in symbols:
            if symbol in kconfig:
                meta[directory].append(kconfig[symbol])
    for directory in directories:
        for name in ("README", "README.md", "README.rst"):
            path = os.path.join(root, directory, name)
            if os.path.exists(path):
                meta[directory].append(open(path, encoding="utf-8", errors="ignore").read(1500))
    meta_counts = {d: Counter(words(" ".join(texts))) for d, texts in meta.items()}
    data = {"files": files, "generic": dict(generic), "file_count": dict(file_count), "meta": meta_counts}
    pickle.dump(data, open(cache_file(root, "areas"), "wb"))
    print(f"{len(files)} files, {len(directories)} directories, {len(meta_counts)} with metadata", file=sys.stderr)


class BM25:
    def __init__(self, docs):
        self.docs = docs
        self.lengths = {d: sum(c.values()) for d, c in docs.items()}
        self.average = sum(self.lengths.values()) / max(len(self.lengths), 1)
        self.df = Counter(t for c in docs.values() for t in c)
        self.n = len(docs)

    def scores(self, terms):
        out = defaultdict(float)
        for term in set(terms):
            df = self.df.get(term, 0)
            if not df:
                continue
            idf = math.log(1 + (self.n - df + 0.5) / (df + 0.5))
            for doc, counts in self.docs.items():
                tf = counts.get(term)
                if tf:
                    out[doc] += idf * tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * self.lengths[doc] / self.average))
        return out


def ndcg10(order, gold):
    gold = set(gold)
    dcg = sum(1 / math.log2(i + 2) for i, p in enumerate(order[:10]) if p in gold)
    ideal = sum(1 / math.log2(i + 2) for i in range(min(len(gold), 10)))
    return dcg / ideal


def evaluate(root, cases_path, k, meta_weight):
    data = pickle.load(open(cache_file(root, "areas"), "rb"))
    generic, meta = BM25(data["generic"]), BM25(data["meta"])
    by_dir = defaultdict(list)
    for relative in data["files"]:
        by_dir[parent(relative)].append(relative)
    cases = [json.loads(line) for line in open(cases_path)]
    recall = {1: 0.0, 5: 0.0, 10: 0.0, 30: 0.0}
    total = 0.0
    for case in cases:
        terms = words(case["query"])
        scores = generic.scores(terms)
        for directory, value in meta.scores(terms).items():
            scores[directory] += meta_weight * value
        areas = sorted(scores, key=lambda d: (-scores[d], d))
        gold_dirs = {parent(g) for g in case["gold"]}
        for cut in recall:
            recall[cut] += len(gold_dirs & set(areas[:cut])) / len(gold_dirs)
        # Files inside the best k areas, ranked by BM25 over their text plus
        # a bounded share of their area's score.
        candidates = [f for d in areas[:k] for f in by_dir[d]]
        docs = {}
        for relative in candidates:
            try:
                docs[relative] = Counter(words(open(os.path.join(root, relative), encoding="utf-8", errors="ignore").read(1 << 20)) + words(relative) * 3)
            except OSError:
                pass
        file_scores = BM25(docs).scores(terms) if docs else {}
        top_area = scores[areas[0]] if areas else 1.0
        final = {f: file_scores.get(f, 0.0) + 2.0 * scores[parent(f)] / max(top_area, 1e-9) for f in docs}
        order = sorted(final, key=lambda f: (-final[f], f))
        total += ndcg10(order, case["gold"])
    n = len(cases)
    print(f"k={k} meta_weight={meta_weight}: area recall@1 {recall[1]/n:.3f} @5 {recall[5]/n:.3f} @10 {recall[10]/n:.3f} @30 {recall[30]/n:.3f}  file nDCG@10 x100 {100*total/n:.2f}")


if __name__ == "__main__":
    if sys.argv[1] == "build":
        build(sys.argv[2])
    else:
        k = int(sys.argv[sys.argv.index("--k") + 1]) if "--k" in sys.argv else 10
        w = float(sys.argv[sys.argv.index("--meta-weight") + 1]) if "--meta-weight" in sys.argv else 1.0
        evaluate(sys.argv[2], sys.argv[3], k, w)
