# Repomap

### Find the right files before you edit.

**A fast, local codebase map for AI agents.** Give Claude, Codex or Nexus the layout, entry points and relevant symbols in a small context budget. Works as a Rust CLI or an MCP server.

**19–50 ms warm queries** on tested 250–700-file repositories. A task map of the **65,795-file Linux kernel took 0.57 s and 114 MB RAM** in the recorded run.

## Why it earns its place

- **Understand the project:** an overview with architecture, build/test commands and an index of real files.
- **Find the implementation:** task-ranked files with definitions, imports and cross-file references.
- **Spend context deliberately:** choose a token budget; parse results are cached locally. No hosted model required.

## How it compares

File-ranking score on **83 held-out tasks**, higher is better:

| Method | nDCG@10 × 100 |
| --- | ---: |
| Previous ranker | 33.9 |
| Plain BM25 text search | 62.4 |
| **Repomap** | **66.4** |

In a separate agent evaluation on the same 83 tasks, a fresh agent given the overview and task map found **86.8% of required files in its top five**, versus **69.2% for the ranker alone**.

Recorded release benchmarks, September–October 2026. Warm-cache timings; paired small-repo runs used a shared host at load ≈25. Kernel timings describe that workload, not every large repository. [Methods, conditions and results →](docs/experiments.md)

## Try it

```sh
cargo build --release
target/release/repomap .
target/release/repomap . --query "where is authentication checked?" --token-budget 2000
target/release/repomap mcp
```

Start with the overview, then ask for the task map. [CLI and benchmark guide →](bench/README.md)
