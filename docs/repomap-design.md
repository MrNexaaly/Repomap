# Nexus query-aware repository map

Nexus uses a repository map for broad codebase questions and dedicated
ripgrep-backed `grep` and `glob` tools for exact follow-up searches.

## Design

The native Rust map collects file paths, LOC, definitions, local imports, and
distinctive symbol references. Its ranker combines:

- hard priority tiers for paths and symbols explicitly mentioned by the user;
- open-file and changed-file locality;
- inverse-document-frequency weighting for distinctive identifiers;
- bounded bidirectional propagation across caller/definer and import links;
- retention and priority for a unique caller of an explicitly named symbol,
  even when a large file has more than 64 other distinctive references;
- deterministic marginal-coverage packing under a token budget;
- production-path priority when a fixture and implementation match the same query;
- deterministic path tie-breaking, duplicate prevention, and budget checks;
- per-process file analysis caching keyed by root, path, mtime, and size.

Large query-aware maps use a deterministic two-stage boundary. When a
repository has more than 3,000 source files, path mentions, open/changed files,
query terms, source locality, and shallow entry points select a 3,000-file
analysis pool. The rendered map states both the indexed and total counts. Exact
`grep`, LSP, or ranged reads remain required before a code claim. Within that
pool, component-aligned path-suffix indexes avoid scanning every file for every
unresolved import, and compact identifier hashes avoid retaining millions of
owned strings solely for caller discovery.

Calling `repomap` without query context retains the previous deterministic
entrypoint-then-LOC ordering for compatibility. Query-aware calls accept
`query`, `mentioned_paths`, `mentioned_symbols`, `open_paths`, and
`token_budget`.

The repository's executable `./repomap` delegates to the `nexus-tools`
`repomap` binary, so terminal users and the Nexus tool action now execute the
same parser, ranker, renderer, and cache path. Its repeatable CLI equivalents
are `--mentioned-path`, `--mentioned-symbol`, and `--open-path`; `--query`,
`--token-budget`, and the legacy `--max-chars` flag are also supported.

## Relationship to Aider

The design was informed by Aider's current
[`repomap.py`](https://github.com/Aider-AI/aider/blob/main/aider/repomap.py), but
the Rust implementation and scoring are independent.

| Area | Aider | Nexus |
| --- | --- | --- |
| Structure | Tree-sitter definition/reference tags with a lexer fallback | Lightweight multi-language structural extraction plus exact ripgrep follow-up |
| Ranking | Mention-personalized graph ranking | Exact-priority tiers, IDF, open/changed locality, and bounded bidirectional graph propagation |
| Budget | Ranked tag/tree rendering under a token limit | Deterministic marginal-relevance and coverage packing |
| Cache | Persistent tag cache | Process-local per-file mtime/size cache |
| Exact lookup | Separate search flow | First-class bounded `grep` and `glob` tools in the same agent loop |

This does **not** establish that Nexus universally outperforms Aider. Aider has
stronger parser coverage through Tree-sitter. Nexus's measured advantage below
is against the old Nexus entrypoint-then-LOC baseline on frozen synthetic
fixtures.

## Replayable evidence

The native Invent run produced separate frozen and held-out evaluators. After
one mechanical Rust borrow repair and two integration improvements, the
production ranker measured:

| Gate | Result |
| --- | ---: |
| Frozen composite score | 89.383789 |
| Old Nexus baseline coverage | 0.623213 |
| Independent held-out score | 74.156785 |

Replay both generated evaluators against the production source with:

```bash
CARGO_TARGET_DIR=/tmp/nexus-rust-target CARGO_INCREMENTAL=0 \
  cargo test --manifest-path rust/Cargo.toml \
  -p nexus-tools --test repomap_invent_evidence
```

On the dirty Nexus repository used during the original implementation, an
optimized probe put the explicitly mentioned ranker first and measured about
310 ms cold and 121–128 ms after the per-file cache was warm.

A later large-monorepo dogfood used the 7,881-source-file Hermes Agent checkout
at `801466765b8908fcfae78338fd034609b2c42ff9`. The debug launcher exceeded 60
seconds before the suffix-index, compact-identifier, shortlist, and packing
changes. The same root query then completed in about 7 seconds, reported that
3,000 of 7,881 source files were indexed, and ranked
`agent/context_compressor.py` plus `agent/verification_evidence.py` first. This
is one local before/after observation, not a cross-machine latency guarantee or
proof of semantic completeness.

A subsequent verified-learning dogfood query put the ledger definition first
but omitted its 4,000-line pipeline caller because that file's explicitly named
reference fell beyond the 64-reference render cap. Prioritizing explicit
references fixed the real query. An initial all-callers boost was rejected when
the held-out score fell to 71.819123; limiting the priority tier to unique
explicit callers produced the 89.383789 frozen score above and restored the
held-out score exactly to 74.156785.
