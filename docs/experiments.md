# Experiments

Measured results, including the ideas that failed, so nobody repeats them
blind. Localization SCORE = 100 x mean nDCG@10 (`bench/README.md`); fitness
= 140 audited cases from nexus, nxlang and Nexamail; held-out = 83 audited
cases from Nexagate, NexapadV2, modeler and maxsis, kept outside this
repository and never used for tuning. Intervals are paired bootstrap 95% CIs
(`bench/compare.py`).

## Baselines (2026-09-30)

| method | fitness | held-out | note |
|---|---|---|---|
| repomap as copied from Nexus | 35.6 | 33.9 | conceptual 10.1 / 12.2 |
| plain BM25 over file text (Python) | 58.9 | 62.4 | `baselines/bm25.py` |
| Aider 0.86.2 repo map, no chat files | 20.4 | | 2 s p50 |
| query-free hot-files prior | 16.8 | 25.8 | diagnostic |

The original ranker lost to BM25 by 23.2 points (CI [-30.1, -16.2]): it only
matched query words against paths and definition names, and divided every
file's score by its size. Its reported 89.4 was on synthetic fixtures.

## Orientation (agent in the loop, `bench/orient.py`)

A fresh codex sees one ~2000-token map and names up to 5 files per task; it
runs no commands. File recall@5:

| map | fitness (96 tasks) | held-out (83 tasks) |
|---|---|---|
| plain file list | 0.463 | 0.789 |
| legacy list (entry points, then size) | 0.692 | 0.598 |
| overview v1 (no file index) | 0.636 | |
| overview v2 (with the file index) | 0.777 | 0.789 |

Overview v2 - legacy: +0.085 [+0.019, +0.156] fitness, +0.191 [+0.104,
+0.277] held-out. Overview - file list: ties when every file fits the budget
(held-out, +0.000 [-0.046, +0.049]) and wins when it does not (the nexus
repository: list 0.087, overview 0.583). Lesson: the overview only beat the
legacy list once it named real files; purposes alone lost (v1).

## Ranking (Nexus, branch `rank`; details in `docs/ranking.md`)

Kept: full-text body evidence (BM25), path and definition names as separate
fields, plural folding, explicit mentions as priority tiers, packing in rank
order. Rejected on fitness: broader stemming, graph propagation over the
import/reference graph, repository co-occurrence expansion, file-role priors.
Result 67.2 fitness, +8.3 over BM25 [+4.75, +12.12]; held-out 66.4, +32.5
over the original [+25.3, +39.7] but only +4.0 over BM25 [-0.0, +8.2].

## Ideas that did not help

- **Agent-phrased queries** (`bench/expand.py`: a model rewrites each task
  with likely identifiers and synonyms, without seeing the code): ranker +2.1
  fitness [-2.2, +6.1], +0.3 held-out [-4.0, +4.4]; BM25 gains as much
  (held-out 62.4 -> 66.1), so the margin does not grow. The meaning gap is
  not a phrasing problem.
- **Commit history as a task-to-file bridge** (`baselines/history_fusion.py`:
  BM25 between the task and the subjects of past commits touching each file,
  parent history only): alone 46.0 fitness / 38.5 held-out (a real signal, far
  above the hot-files prior), but reciprocal-rank fusion with the ranker lost
  -1.2 fitness [-5.4, +2.9] and -5.8 held-out [-10.4, -1.3]. Equal-vote fusion
  adds more noise than signal; a weighted field inside the ranker is untested.
- **Local sentence embeddings** (`baselines/embed_fusion.py`, 40-line chunks
  prefixed with the path, file = best chunk, fused with the ranker):
  bge-small-en-v1.5 fused at equal weight gave +4.1 fitness [+0.6, +7.6] but
  -1.7 held-out [-5.9, +2.4] (per repo: nxlang +8.6 and Nexamail +6.5 on
  fitness; Nexagate -4.4 and maxsis -5.1 held-out), so it is not a
  general gain. Alone it scored 64.1 fitness / 55.9 held-out. Static
  Model2Vec embeddings (potion-retrieval-32M) are ~100x faster but too weak:
  44.8 alone, -5.4 fused [-9.7, -0.9]. One chunk per file (its first 40
  lines) instead of all chunks: 40.6 alone, -10.5 fused. bge-small also
  indexes at roughly 17-100 chunks/s on this machine, minutes for one
  700-file repository. Untested: code-trained embedders.
- **Cross-encoder reranker** (`baselines/rerank.py`: bge-reranker-base over
  the ranker's top 20, each file represented by its best chunk for the task):
  reranker order 58.7 fitness, -8.6 [-13.7, -3.4]; fused with the ranker's
  order 66.6, -0.7 [-4.2, +3.0] (conceptual +6, lexical -2.3). About 6.5 s
  per query on this machine under load. Not run on held-out: no fitness gain.

## Speed and scale

Output-identical refactors (all 223 cases unchanged): rank first and render
only what fits the budget, cache path/definition term keys, take metadata
in the walker, stream the tokenizer. Paired warm latency, merge commit
0332142 vs 095c7c0 at load ~25: 683-file snapshot 50.9 -> 37.2 ms,
NexapadV2 58.2 -> 49.3 ms, Nexagate 25.1 -> 19.2 ms.

The 3,000-file shortlist stays: on a 27,057-file tree it gives cold 1.5 s,
warm 0.6-0.9 s, 188 MB; with no cap, 5.5 s, 2.5 s and 1.1 GB. Beyond 3,000
files the shortlist is chosen from paths, so a file matched only by its body
can be missed; the fix is a streaming shortlist over cached term vectors.

## End to end: the agent is the meaning layer (`bench/agent_eval.py`)

A fresh codex (no commands) names 5 files per task from the maps it is shown;
one snapshot per repository, the same for every condition, so conditions are
comparable with each other but not with the per-parent benchmark numbers.
Held-out, 83 tasks, file recall@5:

| agent sees | recall@5 |
|---|---|
| nothing but the ranker's own top 5 (no agent) | 0.692 |
| overview | 0.789 |
| task map (1000 tokens) | 0.830 |
| overview + task map | **0.868** |

Agent + task map vs the ranker's top 5: +0.138 [+0.074, +0.204]; overview +
task map vs the ranker: +0.176 [+0.109, +0.245]; adding the overview to the
task map: +0.038 [+0.004, +0.078]. The agent bridges task words to code that
no ranker change bridged, so repomap's job is to put the right candidates
and vocabulary in front of it: the overview first, then the task map.

`--compact` (one line per file, ~3x the candidates): task map alone +0.033
fitness [-0.009, +0.077], +0.024 held-out [-0.011, +0.058], pooled +0.029
[+0.001, +0.057]; with the overview first no difference (pooled +0.015
[-0.011, +0.042]). Offered as an option; the default stays full, whose
imports and callers matter for editing, which this test does not measure.

## Large C trees: the Linux kernel (2026-10-01)

`bench/cases/kernel.jsonl`: 200 Linux commits after v7.2, all scored on the
v7.2 tree (65,795 source files); gold = modified files present in 7.2.
`kernel-bare.jsonl`: the same subjects without their `subsystem:` prefix
(197), i.e. a newcomer who does not know the kernel's paths. Before this
round: repomap 70.2 / 25.6; BM25 over all 66k files (`baselines/bm25_rg.py`)
26.1 on bare (lexical 42.1, conceptual 14.7 vs repomap 55.0 / 4.6).

Overview: 36 s -> about 1 s for the overview phase (Makefile no longer a
package manifest, ancestor-walk layout); peak RSS 1,001,960 -> 80,012 kB.
Layout rows are named from `MAINTAINERS` (`fs/` | FILESYSTEMS (VFS and
infrastructure)), and mapping a subdirectory zooms in (`repomap
linux/drivers/net` names INTEL ETHERNET DRIVERS and so on), since the
MAINTAINERS file is found in an ancestor.

Rank areas first, then files inside them (`bench/proto/areas.py`): area
recall@10 0.515 on kernel-bare; files 26.6 with generic per-directory
documents, 28.4 adding MAINTAINERS names, Kconfig help (bound through
Makefile `obj-$(CONFIG_...)` rules) and READMEs, vs 25.6. Not adopted: +2.8
for a kernel-shaped pipeline, against the agent's +0.18 recall@5 from simply
reading the overview and task map.

### C and C++ definitions (`src/c_like.rs`)

Until now repomap extracted nothing from C or C++: no definitions, no
includes. The new reader is line-based like the others, with lookahead:
functions (return type on the same line or the one above, parameters over
several lines, brace on the same or next line, lock annotations such as
`__acquires(x)` and attribute groups in between, `SYSCALL_DEFINE3(name, ...)`
and `TEST(Suite, Case)` named by their first argument), C++ `Class::method`,
constructors with initializer lists, `struct/union/enum/class` with a body
(export macros such as `LLVM_ABI` skipped), typedef names, `#define`s (not
include guards) and quoted `#include`s. Only unindented lines count, so
types inside an indented `namespace` are not seen.

Against tree-sitter on random files (`bench/proto/c_defs_check.py`; files
where tree-sitter itself reports ERROR nodes are excluded):

| corpus | functions P / R | types P / R | typedefs P / R | macros P / R |
|---|---|---|---|---|
| kernel, 300 files | 0.999 / 0.997 | 1.000 / 0.995 | 1.000 / 0.889 | 0.994 / 1.000 |
| Wine dlls, 300 files | 1.000 / 1.000 | 1.000 / 0.990 | 1.000 / 0.973 | 0.962 / 1.000 |
| ICU, SPIRV-Tools, llama.cpp, 600 files | 0.949 / 0.954 | 0.795 / 0.967 | 0.844 / 0.900 | 0.953 / 1.000 |

The C++ precision understates: in a hand check of 20 "ours only" functions
and 20 types, all functions and 19 types were real definitions tree-sitter
missed (it fails on `class U_I18N_API Name`); the one error (`struct LSR
final` named `final`) is fixed. llvm was not used, being held out.

Ranking, paired against the parent commit:

| split | before | after | delta (CI95) |
|---|---|---|---|
| fitness | 67.30 | 69.25 | +1.95 [+0.67, +3.40] (nxlang, C: +6.22) |
| kernel-bare | 25.57 | 27.04 | +1.47 [+0.36, +2.71] |
| kernel | 70.16 | 71.14 | +0.98 [-0.87, +2.93] |
| held-out llvm (C++) | 32.41 | 35.23 | +2.82 [+0.85, +4.84] |
| held-out llvm-bare | 12.01 | 13.65 | +1.64 [+0.43, +3.14] |

The first held-out run crashed on 14 llvm tasks: name extraction stepped one
byte back from a non-identifier character, which splits `𐀀` in `test_𐀀(` or
`µ` in `operator""_µs(`, and a panic in one file aborted the whole map. The
kernel and the fitness repositories are ASCII, so nothing tuned on showed
it. Fixed (whole characters), with a test inserting multibyte characters at
every position of each layout; the reader then ran over 220,477 C/C++ files
on this machine (kernel, Wine, llvm, NDK, zig, /usr/include) without a
panic, and a panicking reader now costs only that file its definitions.

Which macros enter the definition-name field: all, function-like only, or
none. Scored on the cases where no variant timed out (189 kernel, 196 bare):

| macros as names | kernel | kernel-bare | fitness | kernel query RSS | overview RSS |
|---|---|---|---|---|---|
| none | 71.76 | 26.94 | 69.21 | | |
| function-like (chosen) | 71.71 | 27.18 | 69.25 | 113 MB | 478 MB* |
| all | 71.74 | 27.00 | 69.25 | 636 MB | 1,286 MB |

\* before dropping names from the overview's summary records (below). Ranking
is the same; generated register headers define tens of thousands of
constants, which cost memory and pulled `*_sh_mask.h` files into results.
Constants stay in the displayed definitions and in the body text.

Cost on the kernel: the overview had started building a definer table over
all 66k files whose result a summary cannot use (it keeps no identifier
hashes); skipping it and decoding no names in summary mode gives warm 1.66 s
/ 142 MB (cold full index 10.4 s), query 0.57 s / 114 MB, cache 551 MB.

References became case-sensitive and a bare all-caps name (`DONE`, `BE`)
no longer counts as distinctive: before, the comment word "enough" in
`fs/ext4/super.c` "referenced" zlib's `ENOUGH`, and refs lines on the kernel
were mostly such words (`BE; Helper; ReadOnly; Standard`). Scores move only
through the budget (a refs line's length changes how many files fit).

Overview: an area's example files are the largest that define the most
functions (none / 1-7 / 8+), so `drivers/gpu/` shows `amdgpu_dm.c` instead
of `nbio_7_9_0_sh_mask.h`; `main.c`/`main.cc`/`main.cpp` are entry points,
which puts `init/main.c` first on the kernel; quoted includes give C trees
central files (nxlang: `typed_hir.h` used by 22 files).

### Agent in the loop on the kernel (`bench/agent_eval.py`, 40 tasks each)

| split | ranker's top 5 | agent + task map | agent + overview + task map |
|---|---|---|---|
| kernel (subject keeps its subsystem) | 0.779 | 0.754 | 0.762 |
| kernel-bare | 0.283 | 0.315 | 0.290 |

On small repositories the agent added +0.18 over the ranker; on the kernel it
adds nothing measurable. With the subsystem named, the ranker already finds
most files (it was 0.752 on these 40 before C definitions). Without it, most
"conceptual" tasks are unanswerable from the text alone: "Propagate status
register errors" was `power: supply: max17042_battery:`, "free beacon SKB on
error" is one of a dozen wifi drivers with beacon code, and every condition
scores about 0.08 on them. kernel-bare therefore measures ambiguity more than
the map; treat its conceptual score as a floor, not a target. This test does
not cover what the overview is for on a tree this size (choosing where to
zoom or what to ask next), since the agent may not run commands.

## Definition readers for Rust, Go, TypeScript/JavaScript, Svelte, Vue (2026-10-01)

Checked against tree-sitter like the C reader (`bench/proto/defs_check.py`, 300
random files per language, none from the held-out repositories): Rust from
the cargo registry, Go from its standard library and module cache, TS from
Nexus/nxvision/websites, JS from npm packages, Svelte and Vue from websites
and Nexamail. Recall before -> precision / recall after:

| language | names | before | after |
|---|---|---|---|
| Rust | functions, types | 1.000, 0.997 | 0.982 / 1.000, 0.974 / 1.000 |
| Rust | const / static | **0.009** | 0.974 / 0.999 |
| Rust | macro_rules! | **0** | 1.000 / 1.000 |
| Go | functions, methods | **0.560** | 1.000 / 1.000 |
| Go | const / var | **0.123** | 1.000 / 0.994 |
| TypeScript | class methods | **0** | 0.986 / 0.986 |
| JavaScript | methods, values | **0**, 0.465 | 0.992 / 0.973, 1.000 / 0.928 |
| Svelte | functions, values | **0.004**, 0.002 | 1.000 / 1.000 |
| Vue | functions, values | 0.571, 0.538 | 1.000 / 0.857, 0.975 / 0.913 |

Causes: `const` was in the Rust prefix-strip list (for `const fn`), so every
`const NAME` lost its keyword; Go methods were named after the receiver
variable (`func (b *Reader) Read` -> `func b`) and grouped `const (`/`var (`
blocks were skipped; JS/TS classes were read but never their methods; and a
component's `<script>` is indented one level by formatters, while only
unindented lines counted. Rust "extras" are real items inside macros
(`cfg_if!`, `s! { pub struct ... }`), which tree-sitter does not parse. Go and
TS/JS/Svelte/Vue now have their own readers (`src/go_like.rs`,
`src/script_like.rs`); methods show their owner (`method Store.fetch`,
`func Reader.Read`).

Ranking barely moves: fitness +0.23 [-0.13, +0.64] (Nexamail +2.13); held-out
-0.22 [-1.74, +1.28] (NexapadV2 -1.31, modeler +1.40, maxsis +1.10, Nexagate
-0.11), no failed cases. Body text already carried these words; what changes
is the definitions an agent reads in the task map. Warm latency on fitness
p50 15.3 -> 17.1 ms, p95 36.2 -> 37.8 ms (load ~11).
A fresh agent reading only the task map (`agent_eval.py`, held-out 83 tasks,
both binaries in the same session): file recall@5 0.836 -> 0.837. So the fix
is correctness of what the maps say (a Svelte component used to list
nothing, a Go method was named after its receiver variable), not a measured
localization gain.

## Phage (ex-RuHealth) on the readers (2026-10-01)

Properties "the reader never panics" over symbolic UTF-8 input, harnesses in
`~/.cache/repomap-phage/` (repomap's own reader files included by `#[path]`).
With the multibyte slicing bug put back, Phage v10 returns the failing input in
0.8 s (`"Ѐ("`, `"≱("`); RuHealth needed 122 s. On the current code it proves
function detection for any 2 bytes + `(` (420 s) and the comment stripper for
any 4 bytes (5.6 s); 3 bytes + `(` gave no verdict in 25 min, and whole-reader
properties stay unknown (state limits, plus Phage gaps listed in FINDINGS.md:
`memchr` on 16+ byte strings, StrSearcher's visit bound, an undef select).
The cache codec's invariant (the summary decoder's `skip_strs`/`skip_u64s`
accept what `strs`/`u64s` accept and end at the same byte, so later fields stay
aligned) is caught when broken: a mutant skipping 4 bytes per number yields a
counterexample in 0.7 s. On the real code it and the encode/decode round trip,
like the tokenizer's term properties, end unknown on further Phage gaps (a
`Vec` allocation inside `collect`, a `HashMap` keyed by symbolic strings).
