use std::collections::BTreeSet;

use repomap::repomap_ranker::{rank_and_pack, FileFact, RankContext};

#[allow(clippy::too_many_arguments)]
fn file(
    path: &str,
    symbols: &[&str],
    imports: &[&str],
    references: &[&str],
    loc: usize,
    tokens: usize,
    changed: bool,
    entrypoint: bool,
) -> FileFact {
    FileFact {
        path: path.into(),
        symbols: symbols.iter().map(|value| (*value).into()).collect(),
        imports: imports.iter().map(|value| (*value).into()).collect(),
        references: references.iter().map(|value| (*value).into()).collect(),
        loc,
        estimated_tokens: tokens,
        changed,
        entrypoint,
    }
}

fn context(query: &str, symbols: &[&str], open: &[&str], budget: usize) -> RankContext {
    RankContext {
        query: query.into(),
        mentioned_paths: Vec::new(),
        mentioned_symbols: symbols.iter().map(|value| (*value).into()).collect(),
        open_paths: open.iter().map(|value| (*value).into()).collect(),
        token_budget: budget,
    }
}

fn baseline(files: &[FileFact], budget: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..files.len()).collect();
    order.sort_by(|left, right| {
        files[*right]
            .entrypoint
            .cmp(&files[*left].entrypoint)
            .then(files[*right].loc.cmp(&files[*left].loc))
            .then(files[*left].path.cmp(&files[*right].path))
    });
    let mut remaining = budget;
    order
        .into_iter()
        .filter(|index| {
            if files[*index].estimated_tokens > remaining {
                return false;
            }
            remaining -= files[*index].estimated_tokens;
            true
        })
        .collect()
}

fn relevance(output: &[usize], oracle: &[(usize, f64)]) -> f64 {
    let selected: BTreeSet<usize> = output.iter().copied().collect();
    let possible: f64 = oracle.iter().map(|(_, weight)| *weight).sum();
    oracle
        .iter()
        .filter(|(index, _)| selected.contains(index))
        .map(|(_, weight)| *weight)
        .sum::<f64>()
        / possible
}

fn assert_contract(output: &[usize], files: &[FileFact], budget: usize) {
    let unique: BTreeSet<usize> = output.iter().copied().collect();
    assert_eq!(unique.len(), output.len(), "duplicate file indices");
    assert!(output.iter().all(|index| *index < files.len()));
    assert!(
        output
            .iter()
            .map(|index| files[*index].estimated_tokens)
            .sum::<usize>()
            <= budget
    );
}

#[test]
fn query_graph_and_coverage_ranking_beats_entrypoint_then_loc_baseline() {
    let cases = [
        (
            vec![
                file(
                    "src/payments/core.rs",
                    &["authorize_payment", "Payment"],
                    &[],
                    &[],
                    260,
                    35,
                    false,
                    false,
                ),
                file(
                    "src/payments/retry.rs",
                    &["retry_policy", "payment_backoff"],
                    &[],
                    &["authorize_payment"],
                    120,
                    24,
                    false,
                    false,
                ),
                file(
                    "src/api.rs",
                    &["submit_payment"],
                    &["src/payments/core.rs"],
                    &[],
                    180,
                    28,
                    false,
                    true,
                ),
                file(
                    "src/common.rs",
                    &["new", "Error", "Result"],
                    &[],
                    &[],
                    900,
                    46,
                    false,
                    false,
                ),
            ],
            context(
                "payment authorization retry",
                &["retry_policy"],
                &["src/api.rs"],
                87,
            ),
            vec![(0, 1.0), (1, 1.0), (2, 0.72)],
        ),
        (
            vec![
                file(
                    "schema/migrate.rs",
                    &["migrate_schema"],
                    &["schema/plan.rs"],
                    &[],
                    950,
                    90,
                    false,
                    false,
                ),
                file(
                    "schema/plan.rs",
                    &["plan_schema_change"],
                    &[],
                    &["migrate_schema"],
                    240,
                    35,
                    false,
                    false,
                ),
                file(
                    "schema/check.rs",
                    &["check_compatibility"],
                    &[],
                    &["plan_schema_change"],
                    180,
                    25,
                    false,
                    false,
                ),
                file(
                    "tools/db.rs",
                    &["schema_command"],
                    &["schema/plan.rs"],
                    &[],
                    100,
                    20,
                    false,
                    true,
                ),
                file(
                    "metrics/query.rs",
                    &["query_metrics"],
                    &[],
                    &[],
                    700,
                    30,
                    true,
                    false,
                ),
            ],
            context("schema compatibility plan", &[], &["schema/check.rs"], 80),
            vec![(1, 1.0), (2, 1.0), (3, 0.65)],
        ),
        (
            vec![
                file(
                    "src/large.rs",
                    &["new", "Result"],
                    &[],
                    &[],
                    1_500,
                    38,
                    false,
                    true,
                ),
                file(
                    "src/rare.rs",
                    &["rare_search_target"],
                    &[],
                    &[],
                    80,
                    20,
                    false,
                    false,
                ),
                file(
                    "src/common.rs",
                    &["new", "Result"],
                    &[],
                    &[],
                    500,
                    22,
                    true,
                    false,
                ),
            ],
            context("rare search target", &["rare_search_target"], &[], 40),
            vec![(1, 1.0)],
        ),
    ];

    let mut candidate_total = 0.0;
    let mut baseline_total = 0.0;
    for (files, context, oracle) in cases {
        let candidate = rank_and_pack(&files, &context);
        assert_contract(&candidate, &files, context.token_budget);
        for _ in 0..5 {
            assert_eq!(candidate, rank_and_pack(&files, &context));
        }
        candidate_total += relevance(&candidate, &oracle);
        baseline_total += relevance(&baseline(&files, context.token_budget), &oracle);
    }
    assert!(
        candidate_total > baseline_total,
        "candidate={candidate_total} baseline={baseline_total}"
    );
}

#[test]
fn zero_budget_and_missing_graph_targets_are_safe() {
    let files = vec![file(
        "src/caller.rs",
        &["call_missing"],
        &["src/does/not/exist.rs"],
        &["missing_symbol"],
        40,
        12,
        false,
        true,
    )];
    assert!(rank_and_pack(&files, &context("missing", &[], &[], 0)).is_empty());
    let output = rank_and_pack(&files, &context("missing", &[], &[], 12));
    assert_contract(&output, &files, 12);
}

#[test]
fn explicitly_mentioned_symbol_keeps_its_large_caller_ahead_of_tiny_noise() {
    let mut files = vec![
        file(
            "src/learning.rs",
            &["VerifiedLearningStore"],
            &[],
            &[],
            900,
            30,
            false,
            false,
        ),
        file(
            "src/pipelines.rs",
            &["run_pipeline"],
            &[],
            &["VerifiedLearningStore"],
            4_000,
            55,
            false,
            false,
        ),
    ];
    for index in 0..12 {
        files.push(file(
            &format!("scripts/verified-learning-{index}.ts"),
            &["smoke"],
            &[],
            &[],
            8,
            5,
            false,
            false,
        ));
    }
    let output = rank_and_pack(
        &files,
        &context(
            "verified learning persistence",
            &["VerifiedLearningStore"],
            &[],
            100,
        ),
    );
    assert_eq!(&output[..2], &[0, 1]);
    assert_contract(&output, &files, 100);
}

#[test]
fn natural_language_query_matches_camel_case_symbols_case_insensitively() {
    let files = vec![
        file(
            "src/rank.rs",
            &["RankContext"],
            &[],
            &[],
            80,
            20,
            false,
            false,
        ),
        file(
            "src/unrelated.rs",
            &["unrelated_value"],
            &[],
            &[],
            20,
            10,
            false,
            true,
        ),
    ];
    let output = rank_and_pack(&files, &context("rank context", &[], &[], 20));
    assert_eq!(output, vec![0]);
}

#[test]
fn natural_language_query_matches_file_and_directory_names() {
    let files = vec![
        file(
            "agent/verification_evidence.py",
            &[],
            &[],
            &[],
            800,
            20,
            false,
            false,
        ),
        file("ui/tiny.ts", &["tiny"], &[], &[], 2, 8, false, true),
    ];
    let output = rank_and_pack(&files, &context("verification evidence", &[], &[], 20));
    assert_eq!(output, vec![0]);
}

#[test]
fn production_path_match_precedes_a_smaller_fixture_match() {
    let files = vec![
        file(
            "agent/verification_evidence.py",
            &["VerificationEvidence"],
            &[],
            &[],
            800,
            40,
            false,
            false,
        ),
        file(
            "tests/test_verification_evidence.py",
            &["test_ledger"],
            &[],
            &[],
            20,
            8,
            false,
            false,
        ),
    ];
    let output = rank_and_pack(&files, &context("verification evidence", &[], &[], 40));
    assert_eq!(output, vec![0]);
}

#[test]
fn short_import_suffixes_resolve_unique_targets_without_breaking_ambiguous_paths() {
    let unique = vec![
        file(
            "agent/context_engine.py",
            &["ContextEngine"],
            &[],
            &[],
            400,
            18,
            false,
            false,
        ),
        file(
            "gateway/run.py",
            &["run_gateway"],
            &["context_engine.py"],
            &[],
            120,
            12,
            false,
            true,
        ),
    ];
    let mut unique_context = context("gateway", &[], &["gateway/run.py"], 30);
    unique_context.mentioned_paths.push("gateway/run.py".into());
    let unique_output = rank_and_pack(&unique, &unique_context);
    assert_eq!(unique_output, vec![1, 0]);

    let ambiguous = vec![
        unique[0].clone(),
        file(
            "plugin/context_engine.py",
            &["PluginContextEngine"],
            &[],
            &[],
            80,
            18,
            false,
            false,
        ),
        unique[1].clone(),
    ];
    let mut ambiguous_context = context("gateway", &[], &["gateway/run.py"], 30);
    ambiguous_context
        .mentioned_paths
        .push("gateway/run.py".into());
    let ambiguous_output = rank_and_pack(&ambiguous, &ambiguous_context);
    assert_eq!(ambiguous_output.first(), Some(&2));
    assert_contract(&ambiguous_output, &ambiguous, 30);
    assert_eq!(
        ambiguous_output,
        rank_and_pack(&ambiguous, &ambiguous_context)
    );
}

#[test]
fn full_text_fields_keep_explicit_tiers_and_skip_over_budget_files() {
    use repomap::repomap_ranker::{rank_and_pack_with_terms, term_counts};
    let files = vec![
        file("named.rs", &[], &[], &[], 1000, 8, false, false),
        file(
            "definition.rs",
            &["Target"],
            &[],
            &[],
            1000,
            8,
            false,
            false,
        ),
        file("caller.rs", &[], &[], &["Target"], 1000, 8, false, false),
        file("open.rs", &[], &[], &[], 1000, 8, false, false),
        file("body.rs", &[], &[], &[], 1000, 8, false, false),
    ];
    let bodies: Vec<_> = ["noise", "noise", "noise", "noise", "signature signature"]
        .map(term_counts)
        .into();
    let terms: Vec<_> = bodies.iter().map(Vec::as_slice).collect();
    let mut ctx = context("signatures", &["Target"], &["open.rs"], 40);
    ctx.mentioned_paths.push("named.rs".into());
    let order = rank_and_pack_with_terms(&files, &ctx, &terms);
    assert_eq!(order, vec![0, 1, 2, 3, 4]);
    assert_contract(&order, &files, 40);
    ctx.mentioned_paths.clear();
    ctx.mentioned_symbols.clear();
    ctx.open_paths.clear();
    assert_eq!(rank_and_pack_with_terms(&files, &ctx, &terms)[0], 4);
    // Incomplete body vectors must not panic or silently misalign files.
    assert_eq!(
        rank_and_pack_with_terms(&files, &ctx, &terms[..1]),
        rank_and_pack(&files, &ctx)
    );
    ctx.token_budget = 7;
    assert!(rank_and_pack_with_terms(&files, &ctx, &terms).is_empty());
    ctx.token_budget = 0;
    assert!(rank_and_pack_with_terms(&files, &ctx, &terms).is_empty());
    // A high-priority file that cannot fit must not block smaller candidates.
    let mut oversized = files.clone();
    oversized[0].estimated_tokens = 100;
    ctx.mentioned_paths.push("named.rs".into());
    ctx.open_paths.push("open.rs".into());
    ctx.token_budget = 16;
    let packed = rank_and_pack_with_terms(&oversized, &ctx, &terms);
    assert_eq!(packed, vec![3, 4]);
    assert_contract(&packed, &oversized, 16);
}

#[test]
fn incomplete_matching_body_vectors_use_only_structural_evidence() {
    use repomap::repomap_ranker::{rank_and_pack_with_terms, term_counts};
    let files = vec![
        file("z.rs", &[], &[], &[], 1, 8, false, false),
        file("a.rs", &[], &[], &[], 1, 8, false, false),
    ];
    let matching = term_counts("nebula nebula");
    let ctx = context("nebula", &[], &[], 8);
    let structural = rank_and_pack(&files, &ctx);
    assert_eq!(structural, vec![1]);
    assert_eq!(
        rank_and_pack_with_terms(&files, &ctx, &[&matching]),
        structural
    );
    assert_eq!(
        rank_and_pack_with_terms(&files, &ctx, &[&matching, &matching, &matching]),
        structural
    );
}

#[test]
fn full_text_changed_files_get_a_score_boost_not_just_a_tiebreaker() {
    use repomap::repomap_ranker::{rank_and_pack_with_terms, term_counts};
    let mut files = vec![
        file("a.rs", &[], &[], &[], 1, 8, false, false),
        file("z.rs", &[], &[], &[], 1, 8, false, false),
    ];
    let bodies = [term_counts("nebula"), term_counts("noise")];
    let terms: Vec<_> = bodies.iter().map(Vec::as_slice).collect();
    let ctx = context("nebula", &[], &[], 8);
    assert_eq!(rank_and_pack_with_terms(&files, &ctx, &terms), vec![0]);
    // The dirty file has no query evidence and loses both relevance and path
    // order without the boost. Equal costs isolate the changed flag.
    files[1].changed = true;
    assert_eq!(rank_and_pack_with_terms(&files, &ctx, &terms), vec![1]);
    files[1].changed = false;
    assert_eq!(rank_and_pack_with_terms(&files, &ctx, &terms), vec![0]);

    // Changed is an additive score, not a tier above strong text evidence.
    files[1].changed = true;
    files[0].path = "nebula_signature_handler.rs".into();
    let strong = context("nebula signature handler", &[], &[], 8);
    assert_eq!(rank_and_pack_with_terms(&files, &strong, &terms), vec![0]);
}

#[test]
fn full_text_changed_boost_stays_below_explicit_context_tiers() {
    use repomap::repomap_ranker::{rank_and_pack_with_terms, term_counts};
    let files = vec![
        file("named.rs", &[], &[], &[], 1, 8, false, false),
        file("definition.rs", &["Target"], &[], &[], 1, 8, false, false),
        file("caller.rs", &[], &[], &["Target"], 1, 8, false, false),
        file("open.rs", &[], &[], &[], 1, 8, false, false),
        file("dirty.rs", &[], &[], &[], 1, 8, true, false),
        file("clean.rs", &[], &[], &[], 1, 8, false, false),
    ];
    let bodies: Vec<_> = files.iter().map(|_| term_counts("noise")).collect();
    let terms: Vec<_> = bodies.iter().map(Vec::as_slice).collect();
    let mut ctx = context("missing", &["Target"], &["open.rs"], 48);
    ctx.mentioned_paths.push("named.rs".into());
    let order = rank_and_pack_with_terms(&files, &ctx, &terms);
    assert_eq!(order, vec![0, 1, 2, 3, 4, 5]);
    assert_contract(&order, &files, ctx.token_budget);
}

#[test]
fn full_text_splits_acronyms_and_ties_are_deterministic() {
    use repomap::repomap_ranker::{rank_and_pack_with_terms, term_counts};
    assert_eq!(
        term_counts("HTTPServer snake_case Signatures"),
        vec![
            ("case".into(), 1),
            ("http".into(), 1),
            ("server".into(), 1),
            ("signatures".into(), 1),
            ("snake".into(), 1)
        ]
    );
    let files = vec![
        file("z.rs", &[], &[], &[], 1, 8, false, false),
        file("a.rs", &[], &[], &[], 1, 8, false, false),
    ];
    let bodies = [term_counts("identical"), term_counts("identical")];
    let terms: Vec<_> = bodies.iter().map(Vec::as_slice).collect();
    let ctx = context("missing", &[], &[], 16);
    for _ in 0..5 {
        assert_eq!(rank_and_pack_with_terms(&files, &ctx, &terms), vec![1, 0]);
    }
}

#[test]
fn tokenizer_splits_camel_acronyms_and_unicode_like_before() {
    let terms = repomap::repomap_ranker::term_counts("parseHTTPResponse XMLHttpRequest über_Größe a1B2 x");
    let words: Vec<&str> = terms.iter().map(|(word, _)| word.as_str()).collect();
    assert_eq!(
        words,
        ["a1", "b2", "größe", "http", "parse", "request", "response", "xml", "über", "http"]
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    );
    let http = terms.iter().find(|(word, _)| word == "http").unwrap();
    assert_eq!(http.1, 2);
}
