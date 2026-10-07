use std::collections::BTreeSet;
use std::process;

#[path = "../../src/repomap_ranker.rs"]
mod solution;
use solution::{FileFact, RankContext};

struct HeldCase {
    files: Vec<FileFact>,
    context: RankContext,
    relevant: Vec<(usize, f64)>,
}

fn abort(message: &str) -> ! {
    eprintln!("{}", message);
    process::exit(1);
}

fn make_file(path: &str, symbols: &[&str], imports: &[&str], references: &[&str], loc: usize, tokens: usize, changed: bool, entrypoint: bool) -> FileFact {
    FileFact {
        path: path.into(),
        symbols: symbols.iter().map(|x| (*x).into()).collect(),
        imports: imports.iter().map(|x| (*x).into()).collect(),
        references: references.iter().map(|x| (*x).into()).collect(),
        loc,
        estimated_tokens: tokens,
        changed,
        entrypoint,
    }
}

fn make_context(query: &str, paths: &[&str], symbols: &[&str], open: &[&str], budget: usize) -> RankContext {
    RankContext {
        query: query.into(),
        mentioned_paths: paths.iter().map(|x| (*x).into()).collect(),
        mentioned_symbols: symbols.iter().map(|x| (*x).into()).collect(),
        open_paths: open.iter().map(|x| (*x).into()).collect(),
        token_budget: budget,
    }
}

fn held_out_cases() -> Vec<HeldCase> {
    vec![
        HeldCase {
            files: vec![
                make_file("packages/notify/dispatch.rs", &["dispatch_webhook", "DeliveryAttempt"], &["packages/notify/retry.rs", "packages/net/http.rs"], &[], 220, 34, false, true),
                make_file("packages/notify/retry.rs", &["retry_webhook", "compute_backoff"], &[], &["dispatch_webhook"], 150, 25, false, false),
                make_file("packages/net/http.rs", &["send_webhook", "http_post"], &[], &[], 180, 27, false, false),
                make_file("packages/notify/log.rs", &["record_delivery"], &[], &[], 100, 20, false, false),
                make_file("packages/shared/text.rs", &["format", "trim", "new"], &[], &[], 900, 50, false, false),
            ],
            context: make_context("webhook delivery backoff", &[], &["dispatch_webhook"], &["packages/notify/dispatch.rs"], 80),
            relevant: vec![(0, 1.0), (1, 0.9), (2, 0.72), (3, 0.45)],
        },
        HeldCase {
            files: vec![
                make_file("apps/checkout/submit.rs", &["submit_order", "checkout_request"], &[], &["reserve_inventory"], 170, 28, false, true),
                make_file("libs/stock/reservation.rs", &["reserve_inventory", "Reservation"], &["libs/ledger/append.rs"], &["append_stock_ledger"], 190, 30, false, false),
                make_file("libs/ledger/append.rs", &["append_stock_ledger", "LedgerRecord"], &[], &[], 140, 24, false, false),
                make_file("apps/admin/reconcile.rs", &["reconcile_stock"], &[], &["reserve_inventory"], 210, 27, false, true),
                make_file("libs/stock/format.rs", &["format_quantity", "new"], &[], &[], 500, 34, false, false),
            ],
            context: make_context("inventory reservation", &[], &["reserve_inventory"], &[], 72),
            relevant: vec![(0, 0.88), (1, 1.0), (2, 0.78), (3, 0.42)],
        },
        HeldCase {
            files: vec![
                make_file("services/search/query_plan.rs", &["build_query_plan", "QueryPlan"], &["services/search/filters.rs"], &[], 260, 39, true, false),
                make_file("services/search/filters.rs", &["apply_facets", "FacetFilter"], &[], &["build_query_plan"], 130, 23, false, false),
                make_file("services/search/index_reader.rs", &["read_index", "IndexSegment"], &[], &[], 240, 35, false, false),
                make_file("services/metrics/query.rs", &["query_metrics"], &[], &[], 600, 48, true, false),
                make_file("bin/searchd.rs", &["main", "serve_search"], &["services/search/query_plan.rs"], &[], 95, 21, false, true),
            ],
            context: make_context("facet filtering search plan", &[], &[], &["services/search/filters.rs"], 75),
            relevant: vec![(0, 1.0), (1, 0.94), (2, 0.48), (4, 0.55)],
        },
        HeldCase {
            files: vec![
                make_file("engine/schema/migrate.rs", &["migrate_schema", "SchemaDelta", "read_schema"], &["engine/schema/plan.rs"], &[], 900, 165, false, false),
                make_file("engine/schema/plan.rs", &["plan_schema_change", "SchemaPlan"], &[], &["migrate_schema"], 240, 41, false, false),
                make_file("engine/schema/check.rs", &["check_compatibility", "SchemaWarning"], &[], &["plan_schema_change"], 180, 30, false, false),
                make_file("tools/dbtool.rs", &["main", "schema_command"], &["engine/schema/plan.rs"], &[], 120, 23, false, true),
                make_file("engine/common/result.rs", &["Result", "Error", "new"], &[], &[], 800, 44, false, false),
            ],
            context: make_context("database schema upgrade", &[], &["migrate_schema"], &[], 100),
            relevant: vec![(0, 0.95), (1, 1.0), (2, 0.76), (3, 0.58)],
        },
        HeldCase {
            files: vec![
                make_file("zero_case/one.rs", &["one"], &[], &[], 10, 7, false, false),
                make_file("zero_case/two.rs", &["two"], &[], &[], 20, 8, true, true),
            ],
            context: make_context("anything", &[], &[], &[], 0),
            relevant: vec![],
        },
    ]
}

fn check_result(result: &[usize], files: &[FileFact], budget: usize) {
    let mut unique = BTreeSet::new();
    let mut consumed = 0usize;
    for &index in result {
        if index >= files.len() { abort("held-out result contains an invalid index"); }
        if !unique.insert(index) { abort("held-out result contains a duplicate"); }
        consumed = consumed.checked_add(files[index].estimated_tokens).unwrap_or_else(|| abort("held-out token arithmetic overflow"));
    }
    if consumed > budget { abort("held-out result exceeds its budget"); }
}

fn relevance(result: &[usize], expected: &[(usize, f64)]) -> f64 {
    let chosen: BTreeSet<usize> = result.iter().copied().collect();
    let possible: f64 = expected.iter().map(|(_, weight)| *weight).sum();
    if possible == 0.0 { return 1.0; }
    expected.iter().filter(|(index, _)| chosen.contains(index)).map(|(_, weight)| *weight).sum::<f64>() / possible
}

fn main() {
    let cases = held_out_cases();
    let mut total = 0.0;
    for case in &cases {
        let first = solution::rank_and_pack(&case.files, &case.context);
        check_result(&first, &case.files, case.context.token_budget);
        for _ in 0..4 {
            let next = solution::rank_and_pack(&case.files, &case.context);
            if next != first { abort("held-out determinism check failed"); }
        }
        if case.context.token_budget == 0 && !first.is_empty() { abort("held-out zero-budget check failed"); }
        total += relevance(&first, &case.relevant);
    }
    let score = 100.0 * total / cases.len() as f64;
    if !score.is_finite() { abort("held-out score was not finite"); }
    println!("SCORE: {:.6}", score);
}
