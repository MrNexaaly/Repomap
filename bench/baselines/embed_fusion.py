#!/usr/bin/env python3
"""Prototype: do local sentence embeddings add the "meaning" repomap lacks?

For every case snapshot, files are cut into 40-line chunks (prefixed with the
path), embedded once by content hash with bge-small-en-v1.5 (the model brain
already keeps under /opt/brain/models), and a task is scored against each
file's best chunk. The embedding ranking is fused with repomap's own order by
weighted reciprocal rank fusion. Writes, per variant, a lookup file that
bench/baselines/lookup.py serves to bench/run.py.

Usage: embed_fusion.py CASES.jsonl OUT_DIR   (run with the embed venv)
"""

import hashlib
import json
import os
import re
import sqlite3
import subprocess
import sys
import time

import numpy as np
import glob

import onnxruntime as ort
from tokenizers import Tokenizer

MODEL_DIR = glob.glob("/opt/brain/models/models--Xenova--bge-small-en-v1.5/snapshots/*/")[0]


class StaticEmbedder:
    """Model2Vec static embeddings: a token lookup table, mean pooled."""

    def __init__(self, name):
        from model2vec import StaticModel
        self.model = StaticModel.from_pretrained(name)

    def embed(self, texts, batch_size=1024):
        for start in range(0, len(texts), batch_size):
            vectors = self.model.encode(texts[start:start + batch_size])
            vectors = vectors / np.maximum(np.linalg.norm(vectors, axis=1, keepdims=True), 1e-9)
            yield from vectors


class Embedder:
    """bge-small-en-v1.5 straight from brain's ONNX export: CLS pooling, L2 norm."""

    def __init__(self):
        self.tokenizer = Tokenizer.from_file(MODEL_DIR + "tokenizer.json")
        self.tokenizer.enable_truncation(256)
        self.tokenizer.enable_padding()
        options = ort.SessionOptions()
        options.intra_op_num_threads = os.cpu_count()
        self.session = ort.InferenceSession(MODEL_DIR + "onnx/model.onnx", options, providers=["CPUExecutionProvider"])
        self.inputs = {i.name for i in self.session.get_inputs()}

    def embed(self, texts, batch_size=64):
        for start in range(0, len(texts), batch_size):
            encoded = self.tokenizer.encode_batch(texts[start:start + batch_size])
            feed = {
                "input_ids": np.array([e.ids for e in encoded], dtype=np.int64),
                "attention_mask": np.array([e.attention_mask for e in encoded], dtype=np.int64),
            }
            if "token_type_ids" in self.inputs:
                feed["token_type_ids"] = np.array([e.type_ids for e in encoded], dtype=np.int64)
            hidden = self.session.run(None, feed)[0][:, 0, :]
            hidden /= np.linalg.norm(hidden, axis=1, keepdims=True)
            yield from hidden

EXTS = {"rs", "ts", "tsx", "py", "js", "jsx", "mjs", "cjs", "go", "java", "kt", "kts", "swift", "rb",
        "php", "cs", "c", "h", "cc", "cpp", "hpp", "sh", "html", "css", "scss", "svelte", "vue"}
BINARY = os.environ.get("REPOMAP_BINARY", "target/release/repomap")
MAP_LINE = re.compile(r"^(\S[^|]*?) \| \d+ LOC(?: \|.*)?$")
QUERY_PREFIX = "Represent this sentence for searching relevant passages: "
LINES = 40


def files_of(root):
    out = subprocess.run(["rg", "--files", root], capture_output=True, text=True).stdout.split("\n")
    return sorted(os.path.relpath(p, root) for p in out if p and p.rsplit(".", 1)[-1].lower() in EXTS)


def chunks_of(root, relative):
    try:
        lines = open(os.path.join(root, relative), encoding="utf-8", errors="ignore").read().splitlines()
    except OSError:
        return []
    chunks = [f"{relative}\n" + "\n".join(lines[i:i + LINES]) for i in range(0, max(len(lines), 1), LINES)]
    # CHUNK_MODE=first: one vector per file (its first chunk), ~10x fewer.
    return chunks[:1] if os.environ.get("CHUNK_MODE") == "first" else chunks


def key(text):
    return hashlib.sha1(text.encode()).hexdigest()


def main():
    cases = [json.loads(line) for line in open(sys.argv[1]) if line.strip()]
    out_dir = sys.argv[2]
    os.makedirs(out_dir, exist_ok=True)
    model_name = os.environ.get("EMBED_MODEL", "bge")
    suffix = "" if model_name == "bge" else "-" + model_name.replace("/", "_")
    db = sqlite3.connect(os.path.expanduser(f"~/.cache/repomap-bench/embed/vectors{suffix}.sqlite"))
    db.execute("create table if not exists v (k text primary key, e blob)")
    model = Embedder() if model_name == "bge" else StaticEmbedder(model_name)

    snapshots = sorted({c["snapshot"] for c in cases})
    per_snapshot = {}
    pending = {}
    for root in snapshots:
        entries = []
        for relative in files_of(root):
            for text in chunks_of(root, relative):
                k = key(text)
                entries.append((relative, k))
                pending.setdefault(k, text)
        per_snapshot[root] = entries
    known = {row[0] for row in db.execute("select k from v")}
    todo = [(k, t) for k, t in pending.items() if k not in known]
    print(f"{len(snapshots)} snapshots, {len(pending)} unique chunks, {len(todo)} to embed", file=sys.stderr)
    started = time.time()
    for start in range(0, len(todo), 2048):
        batch = todo[start:start + 2048]
        vectors = list(model.embed([t for _, t in batch]))
        db.executemany("insert or replace into v values (?, ?)",
                       [(k, np.asarray(v, dtype=np.float32).tobytes()) for (k, _), v in zip(batch, vectors)])
        db.commit()
        print(f"  embedded {start + len(batch)}/{len(todo)} ({time.time() - started:.0f}s)", file=sys.stderr)

    vectors = {}
    def vector(k):
        if k not in vectors:
            vectors[k] = np.frombuffer(db.execute("select e from v where k=?", (k,)).fetchone()[0], dtype=np.float32)
        return vectors[k]

    prefix = QUERY_PREFIX if model_name == "bge" else ""
    queries = list(model.embed([prefix + c["query"] for c in cases]))
    lookups = {name: {} for name in ["embed", "fuse0.5", "fuse1", "fuse2"]}
    for case, q in zip(cases, queries):
        q = np.asarray(q, dtype=np.float32)
        best = {}
        for relative, k in per_snapshot[case["snapshot"]]:
            s = float(vector(k) @ q)
            if s > best.get(relative, -1.0):
                best[relative] = s
        embedded = sorted(best, key=lambda p: (-best[p], p))[:50]
        mapped_out = subprocess.run([BINARY, case["snapshot"], "--query", case["query"], "--token-budget", "2048",
                                     "--max-chars", "10000000"], capture_output=True, text=True).stdout
        mapped = [m.group(1) for m in map(MAP_LINE.match, mapped_out.splitlines()) if m]
        lookup_key = f"{case['snapshot']}\x00{case['query']}"
        lookups["embed"][lookup_key] = embedded
        for name, weight in [("fuse0.5", 0.5), ("fuse1", 1.0), ("fuse2", 2.0)]:
            fused = {}
            for rank, path in enumerate(mapped):
                fused[path] = fused.get(path, 0.0) + 1 / (60 + rank)
            for rank, path in enumerate(embedded):
                fused[path] = fused.get(path, 0.0) + weight / (60 + rank)
            lookups[name][lookup_key] = sorted(fused, key=lambda p: (-fused[p], p))[:50]
    for name, table in lookups.items():
        json.dump(table, open(os.path.join(out_dir, f"{name}.json"), "w"))
    print(f"wrote {', '.join(lookups)} to {out_dir}", file=sys.stderr)


if __name__ == "__main__":
    main()
