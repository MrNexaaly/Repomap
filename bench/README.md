# repomap localization benchmark

Question it answers: **given a task description, does the map put the files a
developer must open near the top?**

## Cases

`mine.py` turns git history into cases: the task is a commit subject, the
answer is the set of existing source files that commit modified. The mapper
sees a snapshot of the **parent** commit (source files, READMEs, manifests,
`.gitignore`), so nothing the commit added can leak into the index.

Filters: 1 to 5 modified source files, at most 15 changed files, a subject of
at least 4 words, no merges, reverts or version bumps. A file may be the
answer to at most 6 cases per repository, so listing a repository's hottest
files cannot score well (`baselines/hotfiles.py` measures that prior; it gets
16.7 on the fitness split). Each case is labeled `lexical` when the task
shares a word with a gold file's path and `conceptual` when it does not.
`audit.py` drops cases an independent reviewer judged mismatched (the diff
does not do what the subject says).

Snapshots live under `~/.cache/repomap-bench/snap/` (not `/tmp`, which is a
quota'd tmpfs on this machine).

## Splits

- **fitness** (`cases/fitness.jsonl`, from `repos.json`): what an optimizer
  may look at and tune on.
- **held-out**: different repositories, kept outside this repository on
  purpose and used only to verify a finished change. Do not go looking for it;
  a result tuned on it would be worthless.

## Running

```bash
cargo build --release
python3 bench/run.py --split fitness --quiet            # SCORE = 100 * mean nDCG@10
python3 bench/run.py --split fitness --quiet --warmup   # warm-cache latency
python3 bench/run.py --split fitness --json /tmp/a.json # per-case order and ranks
python3 bench/compare.py base.json candidate.json       # paired delta + 95% CI
REPOMAP_TIMING=1 target/release/repomap DIR --query "..." >/dev/null  # phase times
```

Every method is scored on its first 20 paths (`--depth`), so printing more
buys nothing; a run that exits non-zero or times out scores 0. The score is
deterministic, so one run is exact; latency is not, so compare it with
`--warmup` and several runs. A rebuilt binary starts with a cold cache (the
cache is salted with the binary), so the first pass after a build is cold.

## Baselines

- `baselines/bm25.py`: plain BM25 over full file text and path (Python).
- `baselines/aider_map.py`: Aider 0.86.2's repo-map ranking with the query's
  mentions and no chat files (its map is designed to be personalized by chat
  files, so treat its number as "Aider's map without a conversation").
- `baselines/hotfiles.py`: query-free label-frequency prior (diagnostic).
