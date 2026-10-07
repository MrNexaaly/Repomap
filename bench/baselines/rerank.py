#!/usr/bin/env python3
"""Prototype: re-score repomap's top candidates with a cross-encoder.

For each case: take repomap's top 20 files, pick each file's best 40-line
chunk for the task (bge-small similarity, vectors cached by
embed_fusion.py), score (task, chunk) with bge-reranker-base (the model brain
keeps under /opt/brain/models), and reorder. Writes lookup tables for
bench/baselines/lookup.py: `rerank` (reranker order) and `rerank_fuse`
(reciprocal-rank fusion of ranker and reranker orders).

Usage: rerank.py CASES.jsonl OUT_DIR   (embed venv; REPOMAP_BINARY selects the ranker)
"""

import glob
import json
import os
import re
import sqlite3
import subprocess
import sys
import time

import numpy as np
import onnxruntime as ort
from tokenizers import Tokenizer

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from embed_fusion import Embedder, QUERY_PREFIX, chunks_of, key  # noqa: E402

RERANKER_DIR = glob.glob("/opt/brain/models/models--BAAI--bge-reranker-base/snapshots/*/")[0]
BINARY = os.environ.get("REPOMAP_BINARY", "target/release/repomap")
MAP_LINE = re.compile(r"^(\S[^|]*?) \| \d+ LOC(?: \|.*)?$")
TOP = 20


class Reranker:
    def __init__(self):
        self.tokenizer = Tokenizer.from_file(RERANKER_DIR + "tokenizer.json")
        self.tokenizer.enable_truncation(512)
        self.tokenizer.enable_padding()
        options = ort.SessionOptions()
        options.intra_op_num_threads = os.cpu_count()
        self.session = ort.InferenceSession(RERANKER_DIR + "onnx/model.onnx", options, providers=["CPUExecutionProvider"])
        self.inputs = {i.name for i in self.session.get_inputs()}

    def scores(self, query, passages, batch_size=10):
        out = []
        for start in range(0, len(passages), batch_size):
            encoded = self.tokenizer.encode_batch([(query, p) for p in passages[start:start + batch_size]])
            feed = {"input_ids": np.array([e.ids for e in encoded], dtype=np.int64),
                    "attention_mask": np.array([e.attention_mask for e in encoded], dtype=np.int64)}
            if "token_type_ids" in self.inputs:
                feed["token_type_ids"] = np.array([e.type_ids for e in encoded], dtype=np.int64)
            out.extend(self.session.run(None, feed)[0].reshape(-1).tolist())
        return out


def main():
    cases = [json.loads(line) for line in open(sys.argv[1]) if line.strip()]
    out_dir = sys.argv[2]
    os.makedirs(out_dir, exist_ok=True)
    db = sqlite3.connect(os.path.expanduser("~/.cache/repomap-bench/embed/vectors.sqlite"))
    embedder, reranker = Embedder(), Reranker()
    queries = list(embedder.embed([QUERY_PREFIX + c["query"] for c in cases]))
    tables = {"rerank": {}, "rerank_fuse": {}}
    started = time.time()
    for n, (case, q) in enumerate(zip(cases, queries)):
        output = subprocess.run([BINARY, case["snapshot"], "--query", case["query"], "--token-budget", "2048",
                                 "--max-chars", "10000000"], capture_output=True, text=True).stdout
        ranked = [m.group(1) for m in map(MAP_LINE.match, output.splitlines()) if m]
        top = ranked[:TOP]
        passages = []
        for relative in top:
            best, best_score = f"{relative}\n", -2.0
            for text in chunks_of(case["snapshot"], relative):
                row = db.execute("select e from v where k=?", (key(text),)).fetchone()
                if row is None:
                    continue
                score = float(np.frombuffer(row[0], dtype=np.float32) @ q)
                if score > best_score:
                    best, best_score = text, score
            passages.append(best)
        scores = reranker.scores(case["query"], passages) if passages else []
        reranked = [path for _, path in sorted(zip(scores, top), key=lambda item: -item[0])]
        fused = {}
        for rank, path in enumerate(top):
            fused[path] = fused.get(path, 0.0) + 1 / (60 + rank)
        for rank, path in enumerate(reranked):
            fused[path] = fused.get(path, 0.0) + 1 / (60 + rank)
        lookup = f"{case['snapshot']}\x00{case['query']}"
        tail = ranked[TOP:]
        tables["rerank"][lookup] = reranked + tail
        tables["rerank_fuse"][lookup] = sorted(fused, key=lambda p: (-fused[p], p)) + tail
        if n % 20 == 0:
            print(f"  {n}/{len(cases)} ({time.time() - started:.0f}s)", file=sys.stderr)
    for name, table in tables.items():
        json.dump(table, open(os.path.join(out_dir, f"{name}.json"), "w"))
    print(f"wrote {', '.join(tables)} to {out_dir} in {time.time() - started:.0f}s", file=sys.stderr)


if __name__ == "__main__":
    main()
