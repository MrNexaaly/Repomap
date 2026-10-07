# repomap

Token-budgeted maps of a codebase for AI agents (Claude, codex, Nexus, any
MCP client). Two maps:

- **Overview** (`repomap DIR`): what a new agent reads first. Purpose (from
  the README), stack, build and test commands (from manifests), layout with
  each part's stated purpose, entry points, the files the rest of the code
  depends on most, and an index of every file. About 2000 tokens.
- **Task map** (`repomap DIR --query "..."`): the files to open for one task,
  best first, each with its definitions, local imports and cross-file
  references, packed into a token budget. Add `--mentioned-path`,
  `--mentioned-symbol`, `--open-path` for context an agent already has.

`repomap mcp` serves both as MCP tools (`repomap_overview`, `repomap_query`)
over stdio, for protocol 2026-07-28 and the older initialize handshake.
`--list` prints the legacy entry-points-then-size list. `REPOMAP_TIMING=1`
prints per-phase times; `REPOMAP_CACHE=off` disables the parse cache.

Files come from ripgrep's ignore rules (`.gitignore`, `.ignore`,
`.repomapignore`); directories that declare themselves generated
(`CACHEDIR.TAG`, `CMakeCache.txt`, `pyvenv.cfg`) are skipped. Parsed files
are cached under `~/.cache/repomap`, keyed by size and modification time.

## How good it is (2026-09-30)

Measured with `bench/` (see `bench/README.md` and `docs/experiments.md`):
on repositories never used for tuning, the task map scores 66.4 nDCG@10 x100
(the version it replaced: 33.9; plain BM25: 62.4), and a fresh agent that
read only the overview named a right file in its top 5 for 79% of real
tasks (the old list: 60%). Warm runs take 19-50 ms on 250-700-file repos.
Its main gap: tasks phrased in words the code never uses match only about
as well as full-text search.

Large C trees work too (2026-10-01): on the Linux kernel (65,795 files) the
overview names each area from `MAINTAINERS` and zooms when given a
subdirectory (warm 1.7 s, 142 MB); a task map takes 0.6 s and 114 MB. C and
C++ files list their functions, types, macros and includes (checked against
tree-sitter: 0.999 precision, 0.997 recall for kernel functions).

## Development

```bash
cargo test --release
python3 bench/run.py --split fitness --quiet   # SCORE = 100 x mean nDCG@10
```

A held-out split of other repositories lives outside this repository on
purpose; never tune on it.
