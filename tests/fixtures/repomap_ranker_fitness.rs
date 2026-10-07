use std::collections::BTreeSet;
use std::process;

#[path = "../../src/repomap_ranker.rs"]
mod candidate;
use candidate::{FileFact, RankContext};

struct Case {
    files: Vec<FileFact>,
    context: RankContext,
    oracle: Vec<(usize, f64)>,
}

fn fail(message: &str) -> ! {
    eprintln!("{}", message);
    process::exit(1);
}

fn file(path: &str, symbols: &[&str], imports: &[&str], references: &[&str], loc: usize, tokens: usize, changed: bool, entrypoint: bool) -> FileFact {
    FileFact {
        path: path.to_string(),
        symbols: symbols.iter().map(|x| (*x).to_string()).collect(),
        imports: imports.iter().map(|x| (*x).to_string()).collect(),
        references: references.iter().map(|x| (*x).to_string()).collect(),
        loc,
        estimated_tokens: tokens,
        changed,
        entrypoint,
    }
}

fn context(query: &str, paths: &[&str], symbols: &[&str], open: &[&str], budget: usize) -> RankContext {
    RankContext {
        query: query.to_string(),
        mentioned_paths: paths.iter().map(|x| (*x).to_string()).collect(),
        mentioned_symbols: symbols.iter().map(|x| (*x).to_string()).collect(),
        open_paths: open.iter().map(|x| (*x).to_string()).collect(),
        token_budget: budget,
    }
}

fn fixtures() -> Vec<Case> {
    vec![
        Case {
            files: vec![
                file("src/accounts/login.rs", &["login_user", "validate_credentials"], &["src/common/errors.rs"], &[], 90, 34, false, false),
                file("src/accounts/password.rs", &["reset_password", "validate_credentials"], &["src/common/errors.rs"], &[], 70, 24, false, false),
                file("src/ui/login_view.rs", &["login_form", "login_user"], &[], &["login_user"], 55, 22, false, true),
                file("src/common/errors.rs", &["Error", "Result", "new"], &[], &[], 120, 20, false, false),
            ],
            context: context("login user authentication", &[], &["login_user"], &["src/ui/login_view.rs"], 62),
            oracle: vec![(0, 1.0), (2, 0.82), (1, 0.35)],
        },
        Case {
            files: vec![
                file("src/web/report_handler.rs", &["report_endpoint", "render_report"], &["src/report/service.rs"], &["generate_report"], 120, 29, false, true),
                file("src/report/service.rs", &["generate_report", "assemble_report"], &["src/report/query.rs"], &["fetch_report_rows"], 180, 31, false, false),
                file("src/report/query.rs", &["fetch_report_rows", "load_report_rows"], &["src/db/connection.rs"], &[], 110, 25, false, false),
                file("src/cli/report.rs", &["report_command"], &[], &["generate_report"], 80, 22, false, true),
                file("src/db/connection.rs", &["open_connection", "query"], &[], &[], 200, 28, false, false),
            ],
            context: context("export report", &["src/web/report_handler.rs"], &["generate_report"], &[], 95),
            oracle: vec![(0, 1.0), (1, 0.95), (2, 0.70), (3, 0.62)],
        },
        Case {
            files: vec![
                file("src/editor/buffer.rs", &["Buffer", "replace_selection"], &["src/editor/cursor.rs"], &[], 240, 44, true, false),
                file("src/editor/cursor.rs", &["Cursor", "move_cursor"], &[], &[], 80, 20, false, false),
                file("src/editor/parser.rs", &["parse_document", "SyntaxNode"], &[], &["Cursor"], 190, 36, true, false),
                file("src/worker/index.rs", &["rebuild_index"], &[], &[], 400, 50, true, false),
                file("src/main.rs", &["main"], &["src/editor/buffer.rs"], &[], 60, 18, false, true),
            ],
            context: context("cursor movement in editor", &[], &["Cursor"], &["src/editor/buffer.rs"], 66),
            oracle: vec![(0, 1.0), (1, 0.92), (2, 0.58), (4, 0.34)],
        },
        Case {
            files: vec![
                file("src/compiler/manifest.rs", &["compile_manifest", "ManifestAst", "parse_manifest"], &["src/compiler/diagnostic.rs"], &[], 160, 32, false, false),
                file("src/compiler/diagnostic.rs", &["Error", "Result", "new", "emit"], &[], &[], 300, 36, false, false),
                file("src/parser/generic.rs", &["parse", "new", "Error"], &[], &[], 500, 48, false, false),
                file("src/bin/build.rs", &["main", "build"], &["src/compiler/manifest.rs"], &["compile_manifest"], 90, 24, false, true),
                file("src/config/defaults.rs", &["default", "load"], &[], &[], 210, 26, false, false),
            ],
            context: context("compile manifest", &[], &["compile_manifest"], &[], 70),
            oracle: vec![(0, 1.0), (3, 0.78)],
        },
        Case {
            files: vec![
                file("src/feature/giant_feature.rs", &["compile_manifest", "feature_entry", "unrelated_helper"], &[], &[], 3000, 180, false, true),
                file("src/compiler/manifest_core.rs", &["compile_manifest", "ManifestAst"], &[], &[], 260, 42, false, false),
                file("src/compiler/manifest_emit.rs", &["emit_manifest", "ManifestOutput"], &["src/compiler/manifest_core.rs"], &[], 180, 31, false, false),
                file("src/docs/compiler.md", &["compile_manifest", "usage"], &[], &[], 90, 20, false, false),
                file("src/main.rs", &["main"], &["src/compiler/manifest_core.rs"], &[], 80, 22, false, true),
            ],
            context: context("manifest compiler", &[], &["compile_manifest"], &[], 100),
            oracle: vec![(1, 1.0), (2, 0.76), (4, 0.45)],
        },
        Case {
            files: vec![
                file("src/payments/core.rs", &["authorize_payment", "Payment"], &["src/payments/retry.rs"], &[], 260, 38, false, false),
                file("src/payments/retry.rs", &["retry_policy", "backoff_payment"], &[], &["authorize_payment"], 120, 24, false, false),
                file("src/audit/events.rs", &["record_payment_audit", "AuditEvent"], &[], &[], 130, 23, false, false),
                file("src/payments/api.rs", &["payment_endpoint"], &["src/payments/core.rs", "src/audit/events.rs"], &[], 200, 34, false, true),
                file("src/util/string.rs", &["format", "trim", "new"], &[], &[], 800, 45, false, false),
            ],
            context: context("payment retry audit", &[], &["retry_policy"], &[], 82),
            oracle: vec![(0, 0.86), (1, 1.0), (2, 0.82), (3, 0.68)],
        },
        Case {
            files: vec![
                file("src/ghost/caller.rs", &["call_missing"], &[], &["src/no/such/module.rs"], 100, 24, false, true),
                file("src/ghost/reference.rs", &["reference_missing"], &[], &["missing_symbol_xyz"], 100, 22, false, false),
                file("src/search/target.rs", &["rare_search_target"], &[], &[], 90, 21, false, false),
                file("src/common/mod.rs", &["new", "Error", "Result"], &[], &[], 400, 35, false, false),
            ],
            context: context("rare search target", &[], &["rare_search_target"], &[], 45),
            oracle: vec![(2, 1.0)],
        },
        Case {
            files: vec![
                file("src/zero/a.rs", &["zero_a"], &[], &[], 20, 10, false, true),
                file("src/zero/b.rs", &["zero_b"], &[], &[], 20, 10, true, false),
            ],
            context: context("zero budget", &[], &[], &[], 0),
            oracle: vec![],
        },
    ]
}

fn baseline(files: &[FileFact], budget: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..files.len()).collect();
    order.sort_by(|a, b| {
        files[*b].entrypoint.cmp(&files[*a].entrypoint)
            .then(files[*b].loc.cmp(&files[*a].loc))
            .then(files[*a].path.cmp(&files[*b].path))
            .then(a.cmp(b))
    });
    let mut result = Vec::new();
    let mut left = budget;
    for index in order {
        if files[index].estimated_tokens <= left {
            left -= files[index].estimated_tokens;
            result.push(index);
        }
    }
    result
}

fn validate(output: &[usize], files: &[FileFact], budget: usize) -> usize {
    let mut seen = BTreeSet::new();
    let mut used = 0usize;
    for &index in output {
        if index >= files.len() {
            fail("candidate returned an out-of-range index");
        }
        if !seen.insert(index) {
            fail("candidate returned duplicate indices");
        }
        used = used.checked_add(files[index].estimated_tokens).unwrap_or_else(|| fail("token sum overflow"));
    }
    if used > budget {
        fail("candidate exceeded the token budget");
    }
    used
}

fn oracle_score(output: &[usize], oracle: &[(usize, f64)]) -> f64 {
    let chosen: BTreeSet<usize> = output.iter().copied().collect();
    let total: f64 = oracle.iter().map(|(_, weight)| *weight).sum();
    if total == 0.0 { return 1.0; }
    oracle.iter().filter(|(index, _)| chosen.contains(index)).map(|(_, weight)| *weight).sum::<f64>() / total
}

fn main() {
    let cases = fixtures();
    let mut candidate_total = 0.0;
    let mut baseline_total = 0.0;
    for case in &cases {
        let first = candidate::rank_and_pack(&case.files, &case.context);
        for _ in 0..3 {
            let again = candidate::rank_and_pack(&case.files, &case.context);
            if again != first { fail("candidate is nondeterministic"); }
        }
        validate(&first, &case.files, case.context.token_budget);
        if case.context.token_budget == 0 && !first.is_empty() {
            fail("zero-budget case was not empty");
        }
        let base = baseline(&case.files, case.context.token_budget);
        validate(&base, &case.files, case.context.token_budget);
        candidate_total += oracle_score(&first, &case.oracle);
        baseline_total += oracle_score(&base, &case.oracle);
    }
    let n = cases.len() as f64;
    let mean = candidate_total / n;
    let base_mean = baseline_total / n;
    let score = 100.0 * mean + 25.0 * (mean - base_mean);
    if !score.is_finite() { fail("non-finite evaluator score"); }
    println!("SCORE: {:.6}", score);
}
