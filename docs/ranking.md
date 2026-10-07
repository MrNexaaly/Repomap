# Ranking research

## Contract and diagnosis

Work is confined to the `rank` worktree. Frozen benchmark and rendering are unchanged. Only fitness snapshots are used; the external held-out split is not inspected.

Primary metric: SCORE (100 × mean nDCG@10). Baseline 35.6394, five identical runs. Warm p95 median 148.6 ms (range 142.1–150). Target: SCORE >=69, paired interval above BM25, conceptual improvement; warm p95 <=40 ms; live nexus rank <=15 ms; no regression at budgets 1024/4096; release tests pass.

Baseline zero-score analysis (receipt 00000089): 27 cases have query words in gold bodies but not paths; 27 have path evidence but lose in scoring/packing. This is a heuristic classification, not proof that each cause is sufficient. Source inspection confirms bodies are absent from scoring and path-hit priority and repeated score/token packing dominate.

CPU profile (receipt 00000057): 1,844,444,968 instructions overall; rank_and_pack self 529,242,257 (28.7%), libc called by rank_and_pack 173,038,335 (9.4%), string replacement 219,648,666 (11.9%), logarithms 59,394,464 (3.2%). Profile includes cold parsing; it is not wall time or quality attribution.

## Falsifiable hypotheses (before changes)

- H6: remove the token divisor only. Predict higher quality on large gold files; unchanged repeated-pick complexity.
- H1: cached full-body term counts with BM25 evidence. Predict substantial conceptual recovery versus structural-only scoring.
- H2: identifier splitting and conservative stemming. Predict recovery for inflection variants, possibly false matches.
- H3: bounded language-local graph propagation atop body scores. Predict conceptual improvement; reject if ambiguity dilutes hits.
- H4: repository-only term co-occurrence expansion. Predict conceptual recovery without lexical regression; reject if generic neighbors dilute queries.
- H5: mild implementation role prior, disabled by explicit test intent. Predict reduced fixture noise without preventing test queries.
- H7: cached sorted term vectors and a sparse query accumulation plus single rank-order packing pass. Predict warm p95 and rank-phase reduction by removing repeated set scans and logs; cache costs remain in measured end-to-end time.

The no-cache.rs rule overrides the suggested edit there: SourceRecord and its codec live in repo_map.rs, so additions can be persisted without changing cache.rs.

## Implemented mechanism

Implementation commit: `e92741a` on `rank`. No new crate, model, runtime network access, or repository-specific rules were added.

- At parse time, tokenize full source text, including comments. Split camel case, acronyms and snake case; lowercase Unicode letters. Keep repeated term frequencies.
- Persist sorted compact term keys and counts in SourceRecord's existing cache encoding. The stable keys are non-security hashes. Borrow these vectors during ranking instead of cloning strings. The codec rejects truncated or unsorted records.
- Score body and path frequencies with BM25, which rewards matching words but limits the value of repetitions and accounts for file length. Path frequencies have weight 3. Add independent short-field evidence: 2 times term importance for a matching path and 1 times for a matching definition. This prevents a long body from erasing filename or definition evidence.
- Match conservative singular/plural alternatives. This is not a general stemmer: verify/verification remains unresolved.
- Build postings only for query terms using binary search in sorted cached vectors. This is not a persistent repository-wide inverted index: ranking still scans files for each query term.
- Preserve the explicit tiers: mentioned path, mentioned definition, unique caller, open path, then text score. Break ties by path and index. Sort once, then pack in rank order, skipping files that do not fit the remaining estimated token budget. Do not divide relevance by rendered size.
- The native query map uses this full-text route. The structural public route remains available when body vectors are absent or incomplete. Legacy query-free rendering is unchanged.

The text route does not use graph propagation, co-occurrence expansion, or role priors. That avoids the measured quality losses of those prototypes; it does not repair every bare-name graph edge elsewhere in the project.

## One-change experiments and verdicts

Numbers below are fitness SCORE points unless stated otherwise. Artifacts and executable variants are retained under `target/rank-research/`. Results describe these prototypes on fitness, not all possible versions of the hypotheses.

| Experiment | A -> B | Verdict |
|---|---|---|
| H6, remove token divisor | baseline 35.6394 -> h6 52.7541 | Keep the principle: large files should not lose just for costing more tokens. This alone does not remove repeated greedy work. |
| H1, add body evidence | h6 52.7541 -> h1 59.8495 | Confirmed; five paired runs gave +13.4%. Conceptual SCORE rose from 17.0488 to 29.5268. |
| Full-text BM25 route | bm25fast 59.7527 | Different scoring design, not a one-change attribution against h1. Provides the parent for the following ablations. |
| H2, broader normalization prototype | bm25fast 59.7527 -> h2 57.9514 | Reject: conceptual rose to 57.9314 but lexical fell to 57.9575. |
| H3, graph propagation prototype | bm25fast 59.7527 -> h3 53.7942 | Reject: conceptual 52.2432 and lexical 54.2726. |
| H4, repository co-occurrence prototype | bm25fast 59.7527 -> h4 59.6394 | Reject: conceptual stayed 55.2126; lexical fell to 61.0047. The predicted meaning-layer benefit was not observed. |
| H5, role prior prototype | bm25fast 59.7527 -> h5 59.3671 | Reject: conceptual 55.2361, lexical 60.6411. |
| Independent path field | bm25fast 59.7527 -> pathfield 65.4365 | Keep: paired +9.5%; conceptual 50.6484, lexical 69.9973. |
| Independent definition field | pathfield 65.4365 -> symbolfield 67.0971 | Keep: paired +2.5%; conceptual 53.6877, lexical 71.2327. |
| Coverage alternative | coverage 64.2766 | Reject versus the short-field design. |
| Length alternative | length 65.1817 | Reject versus the short-field design. |
| Borrowed cached vectors | borrowed 67.0971 | Preserve quality while avoiding body-vector string copies. |
| H7 compact cached keys | borrowed -> compact; SCORE stays 67.0971 | Keep: five-pair warm p95 comparison -24.5%, interval [-28.3%, -20.8%]. |
| Conservative plural matching | compact 67.0971 -> plural 67.2119 | Keep: five-pair SCORE +0.17%. Conceptual falls slightly to 53.4825, lexical rises to 71.4462. |
| Weaker length normalization | plural -> length04 | Reject: five-pair SCORE -2.4%. |
| ASCII tokenizer shortcut | plural -> ascii | Reverted: warm p95 interval [-32.7%, +12.5%] crosses zero; no established speed benefit. |

Two-pair quality ablations were used for the deterministic SCORE experiments H2-H5 and the independent short fields. These repeats establish repeatability on the fixed cases, not uncertainty across unseen tasks. Timing choices used five pairs. The overall BM25 comparison below uses case-level uncertainty instead of repeated deterministic totals.

Evidence: receipts `00000157` (H1), `00000233` (H2), `00000251` (H5), `00000269` (H3), `00000287` (H4), `00000305` (path), `00000323` (definition), `00000624` (compact), `00000663` (plural), `00000767` (length04), `00000815` (ASCII). All references use session prefix `receipt://rust-1213700-1790767501690807777/`. Aggregation of saved per-case files is in `00000988`.

## Where the time went

Warm baseline CPU profile (`00000178`): 1,065,087,903 executed instructions. rank_and_pack self cost is 529,496,822 (49.7%), its libc cost 154,505,755 (14.5%), logs 59,394,464 (5.6%), token BTree insertion 39,075,412 (3.7%). Most baseline instructions therefore occur in ranking, repeated matching and allocation rather than walking.

Warm final profile (`00000875`): 218,059,474 instructions. The difference is 847,028,429 instructions (computed in `00001003`); this is not a wall-time measurement. Remaining costs include read_records 28,635,014 (13.1%), term_counts 15,192,209 (7.0%), character-string extension 10,539,671 (4.8%), and cache u64 decoding 8,633,392 (4.0%), plus allocation and freeing. Single-sort packing removes repeated greedy selection, while compact borrowed body vectors remove string-heavy body processing. Those mechanisms explain the large change in instruction mix, but their individual shares of the total improvement are not separately established.

Final-versus-baseline warm p95 comparison (`00000872`), five pairs: 151 ms -> 46.3 ms; relative change -69.0%, paired interval [-70.1%, -68.0%]. Cache decoding, short-field tokenization, allocation, walking and process overhead remain in end-to-end timing; they have not been moved outside the evaluator.

## Final fitness evidence and acceptance

`python3 bench/run.py --split fitness --quiet --warmup` on the unchanged audited cases (case hash `f305ecb63501d2f1`), depth 20, budget 2048. All 140 cases complete without failures. Only parent snapshots are indexed.

| Group | SCORE |
|---|---:|
| All | 67.2119 |
| Conceptual | 53.4825 |
| Lexical | 71.4462 |
| nexamail | 68.0803 |
| nexus | 68.3032 |
| nxlang | 64.7506 |

Aggregation: `00000988`. Baseline conceptual was 10.1047 and lexical 43.5146. BM25 conceptual is 53.4478, lexical 60.5398, all 58.8681 (`00001003`). Conceptual improvement versus BM25 is only 0.0347 points; no meaningful conceptual advantage over BM25 is established.

Five-pair final-versus-original SCORE comparison (`00000941`): 35.6394 -> 67.2119, paired gain 31.5725 points, +88.6%. Repeated SCORE is exact; the zero-width repeat interval is not evidence about unseen cases.

Actual `bench/compare.py` output against the supplied BM25 case file (`00000959`):

```text
delta nDCG@10 x100: +8.34  CI95 [+4.75, +12.12]  n=140
wins=63 ties=54 losses=23
  nexamail     n= 16 delta=-0.04
  nexus        n= 82 delta=+14.52
  nxlang       n= 42 delta=-0.52
```

Warm latency (`00000962`): five runs after warmup, p95 median 47.2 ms, range 44–47.9 ms, noise ±3.3%, mean interval [44.774, 48.586] ms. Earlier final timing samples were noisier (`00000850`); do not treat a favorable isolated run as acceptance. Live nexus rank (`00000965`), query `verify signature handling`: median 9.77 ms, range 9.54–10.7 ms over five warm runs. This establishes the live rank threshold for this query, not every query.

| Check | Result |
|---|---|
| SCORE >=69 | **Missed**: 67.2119, gap 1.7881 points (`00000988`). |
| BM25 paired interval excludes zero | Passed overall; gains are concentrated in nexus. |
| Conceptual improved | Large gain versus original; essentially tied with BM25. |
| Warm p95 <=40 ms | **Missed**: median 47.2 ms. |
| Live rank <=15 ms | Passed for the measured query. |
| Budget 1024 | 64.9506 versus original 35.3390: no baseline regression. |
| Budget 4096 | 67.2959 versus original 35.6394: no baseline regression. |
| Release invariants | `cargo test --release` passes (`00000959`). |

Budget evidence is aggregated in `00000988`; fresh final budget runs are in `00000959`. A smaller budget does lower the new ranker's score relative to budget 2048; the no-regression comparison here is against the original ranker at the same budget, not a claim of budget-independent quality.

Tests add full-text explicit-tier ordering, singular/plural body evidence, zero and too-small budgets, partial-vector fallback, acronym splitting, deterministic ties, cache round-trip/truncation and body-only cache invalidation. Existing ranking assertions were not weakened (`00001006`). The source rendering function and frozen benchmark, walk, cache.rs and dependency files are unchanged. Existing structural-route tests do not by themselves prove the full-text route; the new body and tier tests exercise it directly.

## Reproduction and risks

Build and test only in this worktree:

```sh
cargo test --release
cargo build --release
python3 bench/run.py --split fitness --quiet --warmup --json target/rank-research/final.json
python3 bench/compare.py /home/zero/Dev/Nexaaly/repomap/.orders/base-bm25.json target/rank-research/final.json
python3 bench/run.py --split fitness --quiet --warmup --budget 1024
python3 bench/run.py --split fitness --quiet --warmup --budget 4096
REPOMAP_TIMING=1 target/release/repomap /home/zero/systemhelper/nexus --query 'verify signature handling' >/dev/null
```

For matched baseline comparisons, add `--binary target/rank-research/baseline` to the evaluator. Prototype executables and per-case results are local build artifacts, not tracked source. Receipts retain raw measurement paths and commands. A fresh build needs cache warmup before timing; cold index-build costs are not claimed to meet the warm threshold.

Open risks: the quality and end-to-end speed bars are not met. The remaining quality gap is not causally attributed to a single fix; broad stemming, expansion and graph variants did not close it. Short-field boosts improve mostly lexical nexus cases; other repos slightly lose against BM25. Only fitness was tuned; the external held-out split was not inspected. Stable compact hashes have a small nonzero collision risk. Estimated token budgets are not a guarantee for every model's tokenizer. The structural fallback retains its older graph behavior, and lack of graph propagation in the full-text route may miss useful callers when none is explicitly named. The measured cache/tokenization/allocation costs are plausible further speed targets, not verified future improvements.

This is a measured research result with acceptance misses, not a claim that the entire bar has been reached. The review follow-up below supersedes the earlier timing acceptance results.

## Review follow-up: dirty workspace evidence and formatting

The full-text route now adds **2.0 score points** for `FileFact.changed`, before sorting within the existing explicit-context tiers. This is a real additive boost on the new BM25 scale, not the old structural scale's +900 and not a tie-breaker or a new priority tier. A weak body-only match can lose to dirty workspace evidence; strong multi-term text evidence can still win. Mentioned paths, mentioned definitions, unique callers and open paths retain their priority. The structural fallback already had its +900 boost and is unchanged.

Two ranker regression tests and one native query-map test failed before the fix (`00001532`) and pass afterward. They cover changing only the flag, equal costs, an unfavorable path tie, weak versus strong text evidence, all explicit tiers, and Git unstaged/staged edits. The native test also warms the source cache, commits an edit, then edits and restores it: dirty status is refreshed rather than persisted as stale ranking evidence. These tests establish the behavior, not an optimal boost weight or a general relevance gain in dirty trees. Fitness snapshots have no `.git`, so their scores cannot measure this signal's usefulness.

`cargo fmt` was run. Its unrelated changes in `src/main.rs`, `src/overview.rs`, and the prohibited `src/cache.rs`/`src/walk.rs` were restored. Formatting changes are retained only in the ranking implementation, its integration and its tests. `cargo fmt --check` passed immediately after formatting the whole crate (`00001541`); the scoped `rustfmt --check --edition 2021 src/repomap_ranker.rs src/repo_map.rs tests/repomap_ranker.rs` and `git diff --check` pass after restoring the other files (`00001560`). The final checkout is not claimed to pass a whole-crate formatting check: those unrelated pre-existing formatting differences remain. `cargo test --release` passes all 32 tests; no existing assertions were weakened.

Fresh final measurements:

- Five warm fitness runs (`00001570`): SCORE **67.2119** every time, warm p95 median **145.2 ms**, range **129.4–151.4 ms**, noise **6.3%**. This does not meet the 40 ms bar.
- Five alternating pairs against the retained pre-review `continuation-base` (`00001579`): identical SCORE; p95 medians **145.5 -> 143.4 ms**, mean paired difference **+0.28 ms**, 95% interval **[-9.09, +9.65] ms**. No timing change is established. Both binaries are much slower in this session than the historical samples above; the cause is not diagnosed, and the earlier timing numbers cannot establish current acceptance.
- Live nexus rank, `verify signature handling`, five warm runs (`00001582`): median **20.38 ms**, range **15.68–42.90 ms**, noise **54.6%**. Current samples do not meet the 15 ms bar; the previous pass is historical only.
- Fresh budgets (`00001576`): **64.9506** at 1024 and **67.2959** at 4096, unchanged and above the original same-budget baselines. All 140 cases complete without failures.
- Fresh per-kind/per-repo aggregation (`00001600`): conceptual **53.4825**, lexical **71.4462**; nexamail **68.0803**, nexus **68.3032**, nxlang **64.7506**. Fresh BM25 comparison (`00001576`): **+8.34 points**, CI95 **[+4.75, +12.12]**, **63 wins / 54 ties / 23 losses**.

All receipt references in this follow-up use prefix `receipt://rust-1213700-1790767501690807777/`. The follow-up fixes the two review issues without changing the approved text-ranking design. SCORE >=69 and both current timing targets remain unmet; no external held-out performance is claimed.
