//! Query-aware repository-map ranking invented by Nexus.
//!
//! This is independent of Aider's implementation. It combines exact path and
//! symbol evidence, inverse document frequency, bidirectional graph
//! propagation, workspace-locality signals, and deterministic marginal
//! coverage packing under a token budget.
//!
//! The recovered Invent seed needed one mechanical borrow fix before its
//! frozen and held-out evaluators could compile.

use std::collections::{BTreeSet, HashMap};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileFact {
    pub path: String,
    pub symbols: Vec<String>,
    pub imports: Vec<String>,
    pub references: Vec<String>,
    pub loc: usize,
    pub estimated_tokens: usize,
    pub changed: bool,
    pub entrypoint: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RankContext {
    pub query: String,
    pub mentioned_paths: Vec<String>,
    pub mentioned_symbols: Vec<String>,
    pub open_paths: Vec<String>,
    pub token_budget: usize,
}

struct FileInfo {
    path: String,
    path_identifiers: BTreeSet<String>,
    symbols: BTreeSet<String>,
    imports: Vec<String>,
    references: Vec<String>,
    identifiers: BTreeSet<String>,
    common_penalty: f64,
}

struct Edge {
    source: usize,
    target: usize,
    weight: f64,
}

fn normalize_path(value: &str) -> String {
    let value = value.trim().replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();

    for part in value.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            if !parts.is_empty() && parts.last().copied() != Some("..") {
                parts.pop();
            } else {
                parts.push(part);
            }
        } else {
            parts.push(part);
        }
    }

    parts.join("/")
}

fn normalize_identifier(value: &str) -> String {
    value.trim().to_lowercase()
}

fn text_tokens(value: &str) -> BTreeSet<String> {
    let mut result = BTreeSet::new();
    let mut current = String::new();
    let mut previous_was_lower_or_digit = false;

    for ch in value.chars() {
        if ch.is_alphanumeric() {
            if ch.is_uppercase() && previous_was_lower_or_digit && !current.is_empty() {
                result.insert(std::mem::take(&mut current));
            }
            current.extend(ch.to_lowercase());
            previous_was_lower_or_digit = ch.is_lowercase() || ch.is_numeric();
        } else if !current.is_empty() {
            result.insert(std::mem::take(&mut current));
            previous_was_lower_or_digit = false;
        } else {
            previous_was_lower_or_digit = false;
        }
    }

    if !current.is_empty() {
        result.insert(current);
    }

    result
}

fn add_identifier(set: &mut BTreeSet<String>, value: &str) {
    let normalized = normalize_identifier(value);
    if normalized.is_empty() {
        return;
    }

    set.insert(normalized.clone());
    for token in text_tokens(value) {
        set.insert(token);
    }
}

fn normalized_values(values: &[String]) -> Vec<String> {
    let mut result = BTreeSet::new();
    for value in values {
        let normalized = normalize_identifier(value);
        if !normalized.is_empty() {
            result.insert(normalized);
        }
    }
    result.into_iter().collect()
}

fn make_file_info(file: &FileFact) -> FileInfo {
    let mut symbols = BTreeSet::new();
    let mut identifiers = BTreeSet::new();
    let mut path_identifiers = BTreeSet::new();

    // File and directory names are first-class repository evidence. Without
    // them, a natural-language query for "verification evidence" could
    // shortlist `verification_evidence.py` in the cheap path pass and then
    // immediately lose it to tiny unrelated files during semantic packing.
    add_identifier(&mut path_identifiers, &file.path);
    identifiers.extend(path_identifiers.iter().cloned());

    for value in &file.symbols {
        let normalized = normalize_identifier(value);
        if !normalized.is_empty() {
            symbols.insert(normalized.clone());
            add_identifier(&mut identifiers, value);
        }
    }

    let imports = normalized_values(&file.imports);
    let references = normalized_values(&file.references);

    for value in &imports {
        add_identifier(&mut identifiers, value);
    }
    for value in &references {
        add_identifier(&mut identifiers, value);
    }

    FileInfo {
        path: normalize_path(&file.path),
        path_identifiers,
        symbols,
        imports,
        references,
        identifiers,
        common_penalty: 0.0,
    }
}

fn idf(identifier: &str, document_frequency: &HashMap<String, usize>, file_count: usize) -> f64 {
    let df = document_frequency.get(identifier).copied().unwrap_or(0);
    ((file_count as f64 + 1.0) / (df as f64 + 1.0)).ln() + 1.0
}

fn resolve_target(
    raw_target: &str,
    path_index: &HashMap<String, Vec<usize>>,
    suffix_path_index: &HashMap<String, Vec<usize>>,
    symbol_index: &HashMap<String, Vec<usize>>,
) -> Option<usize> {
    let path_target = normalize_path(raw_target);
    if path_target.is_empty() {
        return None;
    }

    if let Some(candidates) = path_index.get(&path_target) {
        return if candidates.len() == 1 {
            Some(candidates[0])
        } else {
            None
        };
    }

    // Most reference edges name symbols, not paths. Resolve the O(1) symbol
    // index before the suffix-path fallback; scanning every repository path
    // for every symbol made large maps unnecessarily quadratic.
    let symbol_target = normalize_identifier(raw_target);
    if let Some(candidates) = symbol_index.get(&symbol_target) {
        if candidates.len() == 1 {
            return Some(candidates[0]);
        }
    }

    suffix_path_index
        .get(&path_target)
        .filter(|candidates| candidates.len() == 1)
        .map(|candidates| candidates[0])
}

fn edge_weight(
    raw_target: &str,
    target: usize,
    infos: &[FileInfo],
    document_frequency: &HashMap<String, usize>,
) -> f64 {
    let file_count = infos.len();
    let mut distinctive = idf(
        &normalize_identifier(raw_target),
        document_frequency,
        file_count,
    );

    for token in text_tokens(raw_target) {
        distinctive = distinctive.max(idf(&token, document_frequency, file_count));
    }

    for symbol in &infos[target].symbols {
        distinctive = distinctive.max(idf(symbol, document_frequency, file_count));
    }

    let normalized = ((distinctive - 1.0) / 2.5).clamp(0.0, 1.0);
    0.36 + normalized * 0.64
}

fn build_edges(
    infos: &[FileInfo],
    path_index: &HashMap<String, Vec<usize>>,
    suffix_path_index: &HashMap<String, Vec<usize>>,
    symbol_index: &HashMap<String, Vec<usize>>,
    document_frequency: &HashMap<String, usize>,
) -> Vec<Edge> {
    let mut edge_weights: HashMap<(usize, usize), f64> = HashMap::new();

    for (source, info) in infos.iter().enumerate() {
        for target_name in info.imports.iter().chain(info.references.iter()) {
            let Some(target) =
                resolve_target(target_name, path_index, suffix_path_index, symbol_index)
            else {
                continue;
            };

            if target == source {
                continue;
            }

            let weight = edge_weight(target_name, target, infos, document_frequency);
            edge_weights
                .entry((source, target))
                .and_modify(|old| {
                    if weight > *old {
                        *old = weight;
                    }
                })
                .or_insert(weight);
        }
    }

    let mut edges: Vec<Edge> = edge_weights
        .into_iter()
        .map(|((source, target), weight)| Edge {
            source,
            target,
            weight,
        })
        .collect();

    edges.sort_by(|a, b| a.source.cmp(&b.source).then(a.target.cmp(&b.target)));

    edges
}

fn build_adjacency(file_count: usize, edges: &[Edge]) -> Vec<BTreeSet<usize>> {
    let mut adjacency = vec![BTreeSet::new(); file_count];

    for edge in edges {
        adjacency[edge.source].insert(edge.target);
        adjacency[edge.target].insert(edge.source);
    }

    adjacency
}

fn query_terms(query: &str) -> BTreeSet<String> {
    let mut terms = text_tokens(query);
    let trimmed = normalize_identifier(query);

    if !trimmed.is_empty() && !trimmed.chars().any(|ch| ch.is_whitespace()) {
        terms.insert(trimmed);
    }

    terms
}

fn fixture_like_path(path: &str) -> bool {
    path.starts_with("tests/")
        || path.contains("/tests/")
        || path.contains("/test/")
        || path.contains("/fixtures/")
        || path.contains("/examples/")
}

fn rank_scores(
    files: &[FileFact],
    infos: &mut [FileInfo],
    context: &RankContext,
    document_frequency: &HashMap<String, usize>,
) -> Vec<f64> {
    let file_count = files.len();
    let mentioned_paths: BTreeSet<String> = context
        .mentioned_paths
        .iter()
        .map(|value| normalize_path(value))
        .filter(|value| !value.is_empty())
        .collect();

    let mentioned_symbols: BTreeSet<String> = context
        .mentioned_symbols
        .iter()
        .map(|value| normalize_identifier(value))
        .filter(|value| !value.is_empty())
        .collect();

    let open_paths: BTreeSet<String> = context
        .open_paths
        .iter()
        .map(|value| normalize_path(value))
        .filter(|value| !value.is_empty())
        .collect();

    let query_terms = query_terms(&context.query);
    let mentioned_reference_counts = mentioned_symbols
        .iter()
        .map(|symbol| {
            let count = infos
                .iter()
                .filter(|info| info.references.iter().any(|reference| reference == symbol))
                .count();
            (symbol.clone(), count)
        })
        .collect::<HashMap<_, _>>();

    for info in infos.iter_mut() {
        let mut penalty = 0.0;
        for symbol in &info.symbols {
            let value = idf(symbol, document_frequency, file_count);
            penalty += (1.30 - value).max(0.0);
        }
        info.common_penalty = penalty.min(24.0);
    }

    let mut scores = Vec::with_capacity(files.len());

    for (index, file) in files.iter().enumerate() {
        let info = &infos[index];
        let mut score = 0.0;

        if mentioned_paths.contains(&info.path) {
            score += 12_000.0;
        }

        for symbol in &mentioned_symbols {
            if info.symbols.contains(symbol) {
                let value = idf(symbol, document_frequency, file_count);
                score += 8_000.0 + 450.0 * value;
            } else if mentioned_reference_counts.get(symbol) == Some(&1)
                && info.references.iter().any(|reference| reference == symbol)
            {
                // An explicitly named symbol's consumers are first-class
                // localization evidence. Graph propagation alone can lose a
                // large caller behind many tiny query-shaped files.
                let value = idf(symbol, document_frequency, file_count);
                score += 4_200.0 + 240.0 * value;
            }
        }

        for term in &query_terms {
            if info.identifiers.contains(term) {
                let value = idf(term, document_frequency, file_count);
                score += 330.0 * value;
            }

            if info.path_identifiers.contains(term) {
                let value = idf(term, document_frequency, file_count);
                score += 5_000.0 * value;
            }

            if info.symbols.contains(term) {
                let value = idf(term, document_frequency, file_count);
                score += 1_050.0 * value;
            }
        }

        if open_paths.contains(&info.path) {
            score += 1_100.0;
        }

        if file.changed {
            score += 900.0;
        }

        if file.entrypoint {
            score += 180.0;
        }

        let mut distinctive_bonus = 0.0;
        for symbol in &info.symbols {
            let value = idf(symbol, document_frequency, file_count);
            distinctive_bonus += 24.0 * value;
        }
        score += distinctive_bonus.min(600.0);

        score -= 26.0 * info.common_penalty;
        score -= 1.35 * (file.estimated_tokens as f64).sqrt();

        scores.push(score);
    }

    scores
}

fn propagate(scores: &mut [f64], edges: &[Edge]) {
    for _ in 0..4 {
        let mut forward = vec![0.0; scores.len()];
        let mut backward = vec![0.0; scores.len()];

        for edge in edges {
            let source_score = scores[edge.source].max(0.0);
            let target_score = scores[edge.target].max(0.0);

            forward[edge.target] += source_score * 0.31 * edge.weight;
            backward[edge.source] += target_score * 0.14 * edge.weight;
        }

        for index in 0..scores.len() {
            let forward_addition = forward[index].min(1_900.0);
            let backward_addition = backward[index].min(850.0);
            scores[index] += forward_addition + backward_addition;
        }
    }
}

fn tie_precedes(
    candidate: usize,
    current: usize,
    utility: f64,
    current_utility: f64,
    infos: &[FileInfo],
) -> bool {
    const EPSILON: f64 = 1e-10;

    if utility > current_utility + EPSILON {
        return true;
    }

    if (utility - current_utility).abs() <= EPSILON {
        return infos[candidate]
            .path
            .cmp(&infos[current].path)
            .then(candidate.cmp(&current))
            .is_lt();
    }

    false
}

pub fn rank_and_pack_with_terms(
    files: &[FileFact],
    context: &RankContext,
    terms: &[&[(String, u32)]],
) -> Vec<usize> {
    // Never blend partial or extra body vectors into structural evidence.
    if !terms.is_empty() && terms.len() == files.len() {
        return text_rank_and_pack(files, context, terms);
    }
    rank_and_pack(files, context)
}

pub fn rank_and_pack(files: &[FileFact], context: &RankContext) -> Vec<usize> {
    if files.is_empty() || context.token_budget == 0 {
        return Vec::new();
    }

    let mut infos: Vec<FileInfo> = files.iter().map(make_file_info).collect();

    let mut document_frequency: HashMap<String, usize> = HashMap::new();
    for info in &infos {
        for identifier in &info.identifiers {
            *document_frequency.entry(identifier.clone()).or_insert(0) += 1;
        }
    }

    let mut path_index: HashMap<String, Vec<usize>> = HashMap::new();
    let mut suffix_path_index: HashMap<String, Vec<usize>> = HashMap::new();
    let mut symbol_index: HashMap<String, Vec<usize>> = HashMap::new();

    for (index, info) in infos.iter().enumerate() {
        path_index.entry(info.path.clone()).or_default().push(index);

        // Unresolved import/reference targets often carry only a basename or
        // a short path suffix. The old resolver scanned every repository path
        // for every such edge, turning large monorepos into O(edges * files)
        // work. Index every component-aligned suffix once so resolution stays
        // proportional to total path depth instead.
        suffix_path_index
            .entry(info.path.clone())
            .or_default()
            .push(index);
        for (offset, character) in info.path.char_indices() {
            if character == '/' && offset + 1 < info.path.len() {
                suffix_path_index
                    .entry(info.path[offset + 1..].to_owned())
                    .or_default()
                    .push(index);
            }
        }

        for symbol in &info.symbols {
            symbol_index.entry(symbol.clone()).or_default().push(index);
        }
    }

    let edges = build_edges(
        &infos,
        &path_index,
        &suffix_path_index,
        &symbol_index,
        &document_frequency,
    );
    let adjacency = build_adjacency(files.len(), &edges);

    let mut scores = rank_scores(files, &mut infos, context, &document_frequency);
    propagate(&mut scores, &edges);

    let mentioned_paths: BTreeSet<String> = context
        .mentioned_paths
        .iter()
        .map(|value| normalize_path(value))
        .filter(|value| !value.is_empty())
        .collect();

    let mentioned_symbols: BTreeSet<String> = context
        .mentioned_symbols
        .iter()
        .map(|value| normalize_identifier(value))
        .filter(|value| !value.is_empty())
        .collect();

    let unique_mentioned_callers = mentioned_symbols
        .iter()
        .filter_map(|symbol| {
            let mut callers = infos
                .iter()
                .enumerate()
                .filter(|(_, info)| info.references.iter().any(|reference| reference == symbol))
                .map(|(index, _)| index);
            let caller = callers.next()?;
            callers.next().is_none().then_some(caller)
        })
        .collect::<BTreeSet<_>>();

    let open_paths: BTreeSet<String> = context
        .open_paths
        .iter()
        .map(|value| normalize_path(value))
        .filter(|value| !value.is_empty())
        .collect();

    let query_terms = query_terms(&context.query);
    let query_has_anchor = !query_terms.is_empty()
        && infos.iter().any(|info| {
            query_terms
                .iter()
                .any(|term| info.identifiers.contains(term))
        });
    let mut selected = vec![false; files.len()];
    let mut result = Vec::new();
    let mut remaining = context.token_budget;

    let mut covered_paths = BTreeSet::new();
    let mut covered_symbols = BTreeSet::new();
    let mut covered_query_terms = BTreeSet::new();
    let mut covered_distinctive = BTreeSet::new();
    let mut seen_neighborhood = BTreeSet::new();

    loop {
        let mut best: Option<usize> = None;
        let mut best_utility = f64::NEG_INFINITY;

        for index in 0..files.len() {
            if selected[index] || files[index].estimated_tokens > remaining {
                continue;
            }

            let info = &infos[index];
            let direct_query_signal = query_terms
                .iter()
                .any(|term| info.identifiers.contains(term))
                || mentioned_paths.contains(&info.path)
                || mentioned_symbols.iter().any(|symbol| {
                    info.symbols.contains(symbol)
                        || info.references.iter().any(|reference| reference == symbol)
                })
                || open_paths.contains(&info.path)
                || files[index].changed;
            let selected_graph_neighbor =
                adjacency[index].iter().any(|neighbor| selected[*neighbor]);
            if query_has_anchor && !direct_query_signal && !selected_graph_neighbor {
                continue;
            }
            let mut coverage = 0.0;

            if mentioned_paths.contains(&info.path) && !covered_paths.contains(&info.path) {
                coverage += 7_500.0;
            }

            for symbol in &mentioned_symbols {
                if info.symbols.contains(symbol) && !covered_symbols.contains(symbol) {
                    let value = idf(symbol, &document_frequency, files.len());
                    coverage += 4_600.0 + 300.0 * value;
                }
            }

            let mut query_coverage = 0.0;
            for term in &query_terms {
                if info.identifiers.contains(term) && !covered_query_terms.contains(term) {
                    query_coverage += 620.0 * idf(term, &document_frequency, files.len());
                }
            }
            coverage += query_coverage.min(4_500.0);

            let mut distinctive_coverage = 0.0;
            for symbol in &info.symbols {
                let value = idf(symbol, &document_frequency, files.len());
                if value > 1.20 && !covered_distinctive.contains(symbol) {
                    distinctive_coverage += 72.0 * value;
                }
            }
            coverage += distinctive_coverage.min(1_500.0);

            let unseen_neighbors = adjacency[index]
                .iter()
                .filter(|neighbor| !seen_neighborhood.contains(*neighbor))
                .count();

            coverage += (unseen_neighbors as f64 * 86.0).min(900.0);

            let mut utility = scores[index] + coverage;
            // Direct conversational evidence is a priority tier, not merely
            // another score divided by file size. A tiny graph neighbor must
            // never outrank a file or symbol the user named explicitly.
            if mentioned_paths.contains(&info.path) {
                utility += 1_000_000.0;
            }
            if mentioned_symbols
                .iter()
                .any(|symbol| info.symbols.contains(symbol))
            {
                utility += 750_000.0;
            }
            if unique_mentioned_callers.contains(&index) {
                utility += 250_000.0;
            }
            if open_paths.contains(&info.path) {
                utility += 100_000.0;
            }
            let path_query_hits = query_terms
                .iter()
                .filter(|term| info.path_identifiers.contains(*term))
                .count();
            if path_query_hits > 0 {
                // Query-shaped production paths are localization evidence in
                // their own right. Keep this outside the size division so a
                // tiny fixture cannot crowd out the implementation file whose
                // name directly matches two or three user terms.
                utility += path_query_hits as f64 * 25_000.0;
                if !fixture_like_path(&info.path) {
                    utility += 20_000.0;
                }
            }
            utility -= 0.055 * (files[index].estimated_tokens as f64).sqrt();
            utility -= 0.11 * info.common_penalty;

            if !utility.is_finite() {
                utility = f64::NEG_INFINITY;
            }

            if let Some(current) = best {
                if tie_precedes(index, current, utility, best_utility, &infos) {
                    best = Some(index);
                    best_utility = utility;
                }
            } else {
                best = Some(index);
                best_utility = utility;
            }
        }

        let Some(index) = best else {
            break;
        };

        selected[index] = true;
        remaining -= files[index].estimated_tokens;
        result.push(index);

        if mentioned_paths.contains(&infos[index].path) {
            covered_paths.insert(infos[index].path.clone());
        }

        for symbol in &infos[index].symbols {
            covered_symbols.insert(symbol.clone());

            let value = idf(symbol, &document_frequency, files.len());
            if value > 1.20 {
                covered_distinctive.insert(symbol.clone());
            }
        }

        for term in &query_terms {
            if infos[index].identifiers.contains(term) {
                covered_query_terms.insert(term.clone());
            }
        }

        seen_neighborhood.insert(index);
        for neighbor in &adjacency[index] {
            seen_neighborhood.insert(*neighbor);
        }
    }

    result
}

/// Full-text frequencies, retaining repeated evidence and splitting acronyms.
/// Streams the text with one character of lookahead, so a large file never
/// becomes a `Vec<char>` four times its size.
pub fn term_counts(text: &str) -> Vec<(String, u32)> {
    let mut counts = HashMap::<String, u32>::new();
    let mut word = String::new();
    // Single characters are dropped as noise, counted in characters so a
    // one-letter non-ASCII word is treated like an ASCII one.
    let finish = |word: &mut String, counts: &mut HashMap<String, u32>| {
        if word.chars().nth(1).is_some() {
            *counts.entry(std::mem::take(word)).or_default() += 1;
        }
        word.clear();
    };
    let mut characters = text.chars().peekable();
    let mut previous: Option<char> = None;
    while let Some(ch) = characters.next() {
        if ch.is_alphanumeric() {
            let next = characters.peek().copied();
            if ch.is_uppercase()
                && (previous.is_some_and(|c| c.is_lowercase() || c.is_numeric())
                    || (previous.is_some_and(|c| c.is_uppercase())
                        && next.is_some_and(|c| c.is_lowercase())))
            {
                finish(&mut word, &mut counts);
            }
            word.extend(ch.to_lowercase());
        } else {
            finish(&mut word, &mut counts);
        }
        previous = Some(ch);
    }
    finish(&mut word, &mut counts);
    let mut terms: Vec<_> = counts.into_iter().collect();
    terms.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    terms
}
fn text_rank_and_pack(
    files: &[FileFact],
    context: &RankContext,
    bodies: &[&[(String, u32)]],
) -> Vec<usize> {
    let hashed: Vec<_> = bodies.iter().map(|body| hash_terms(body)).collect();
    let paths: Vec<_> = files
        .iter()
        .map(|file| hash_terms(&term_counts(&file.path)))
        .collect();
    let symbols: Vec<_> = files
        .iter()
        .map(|file| hash_terms(&term_counts(&file.symbols.join(" "))))
        .collect();
    let fields: Vec<_> = (0..files.len())
        .map(|i| TermFields {
            body: &hashed[i],
            path: &paths[i],
            symbols: &symbols[i],
        })
        .collect();
    rank_and_pack_with_hashed_terms(files, context, &fields)
}

/// Stable compact keys for cached full-text frequencies. Not a security hash.
fn term_key(term: &str) -> u64 {
    term.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    })
}
/// Sorted, unique term keys. Two words whose keys collide are counted as one
/// term (the codec requires strictly increasing keys).
pub(crate) fn hash_terms(terms: &[(String, u32)]) -> Vec<(u64, u32)> {
    let mut result: Vec<(u64, u32)> = terms.iter().map(|(t, n)| (term_key(t), *n)).collect();
    result.sort_unstable_by_key(|&(key, _)| key);
    result.dedup_by(|later, kept| {
        if later.0 == kept.0 {
            kept.1 = kept.1.saturating_add(later.1);
            true
        } else {
            false
        }
    });
    result
}

/// Cached term vectors for one file: body text, path and definition names.
#[derive(Clone, Copy)]
pub(crate) struct TermFields<'a> {
    pub body: &'a [(u64, u32)],
    pub path: &'a [(u64, u32)],
    pub symbols: &'a [(u64, u32)],
}
pub(crate) fn rank_and_pack_with_hashed_terms(
    files: &[FileFact],
    context: &RankContext,
    fields: &[TermFields],
) -> Vec<usize> {
    if files.is_empty() || context.token_budget == 0 {
        return Vec::new();
    }
    let mut remaining = context.token_budget;
    rank_with_hashed_terms(files, context, fields)
        .into_iter()
        .filter(|&i| {
            if files[i].estimated_tokens > remaining {
                false
            } else {
                remaining -= files[i].estimated_tokens;
                true
            }
        })
        .collect()
}

/// Every file index, best first: explicit-context tiers, then text score.
/// It reads only `path`, `symbols`, `references` (for callers of mentioned
/// symbols) and `changed` from each fact, so a caller may pack lazily and
/// render only the files that fit (as `repo_map_with_context` does).
pub(crate) fn rank_with_hashed_terms(
    files: &[FileFact],
    context: &RankContext,
    fields: &[TermFields],
) -> Vec<usize> {
    if files.is_empty() {
        return Vec::new();
    }
    let query = term_counts(&context.query);
    // Borrow cached term vectors: only query terms need a posting list.
    let lengths: Vec<f64> = fields
        .iter()
        .map(|field| {
            field.body.iter().map(|(_, n)| f64::from(*n)).sum::<f64>()
                + 3.0 * field.path.iter().map(|(_, n)| f64::from(*n)).sum::<f64>()
        })
        .collect();
    let average = (lengths.iter().sum::<f64>() / files.len() as f64).max(1.0);
    let count = |v: &[(u64, u32)], key: u64| {
        v.binary_search_by_key(&key, |&(k, _)| k)
            .ok()
            .map_or(0.0, |p| f64::from(v[p].1))
    };
    let mut scores = vec![0.0; files.len()];
    for (term, _) in &query {
        // Singular and plural match each other both ways ("dog"/"dogs");
        // words ending in "ss" ("class") are not plurals.
        let alternate = if term.chars().count() >= 4 && term.ends_with('s') && !term.ends_with("ss")
        {
            term[..term.len() - 1].to_owned()
        } else {
            format!("{term}s")
        };
        let key = term_key(term);
        let alt_key = term_key(&alternate);
        let postings: Vec<_> = fields
            .iter()
            .enumerate()
            .filter_map(|(i, field)| {
                let tf = count(field.body, key)
                    + count(field.body, alt_key)
                    + 3.0 * (count(field.path, key) + count(field.path, alt_key));
                (tf > 0.0).then_some((i, tf))
            })
            .collect();
        let df = postings.len() as f64;
        let idf = (1.0 + (files.len() as f64 - df + 0.5) / (df + 0.5)).ln();
        for (i, tf) in postings {
            scores[i] += idf * tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * lengths[i] / average));
        }
        // Independent short fields: body length must not erase filename/definition evidence.
        for (i, field) in fields.iter().enumerate() {
            if count(field.path, key) + count(field.path, alt_key) > 0.0 {
                scores[i] += 2.0 * idf;
            }
            if count(field.symbols, key) + count(field.symbols, alt_key) > 0.0 {
                scores[i] += idf;
            }
        }
    }
    // Dirty workspace files are useful evidence even without a lexical hit.
    // Two BM25-scale points beat a weak body hit but remain an additive prior:
    // strong text evidence can win, and explicit context tiers stay above it.
    for (score, file) in scores.iter_mut().zip(files) {
        if file.changed {
            *score += 2.0;
        }
    }
    let paths: BTreeSet<_> = context
        .mentioned_paths
        .iter()
        .map(|p| normalize_path(p))
        .collect();
    let open: BTreeSet<_> = context
        .open_paths
        .iter()
        .map(|p| normalize_path(p))
        .collect();
    let symbols: BTreeSet<_> = context
        .mentioned_symbols
        .iter()
        .map(|p| normalize_identifier(p))
        .collect();
    let callers: BTreeSet<_> = symbols
        .iter()
        .filter_map(|symbol| {
            let mut owners = files.iter().enumerate().filter(|(_, f)| {
                f.references
                    .iter()
                    .any(|r| normalize_identifier(r) == *symbol)
            });
            let (i, _) = owners.next()?;
            owners.next().is_none().then_some(i)
        })
        .collect();
    let tier = |i: usize| {
        let f = &files[i];
        if paths.contains(&normalize_path(&f.path)) {
            4
        } else if f
            .symbols
            .iter()
            .any(|s| symbols.contains(&normalize_identifier(s)))
        {
            3
        } else if callers.contains(&i) {
            2
        } else if open.contains(&normalize_path(&f.path)) {
            1
        } else {
            0
        }
    };
    let tiers: Vec<_> = (0..files.len()).map(tier).collect();
    let mut order: Vec<_> = (0..files.len()).collect();
    order.sort_unstable_by(|&a, &b| {
        tiers[b]
            .cmp(&tiers[a])
            .then_with(|| scores[b].total_cmp(&scores[a]))
            // Equal evidence (above all, a query nothing matches) falls back
            // to structure: entry points, then larger files, as the legacy
            // list orders them, instead of alphabetical order.
            .then_with(|| files[b].entrypoint.cmp(&files[a].entrypoint))
            .then_with(|| files[b].loc.cmp(&files[a].loc))
            .then_with(|| files[a].path.cmp(&files[b].path))
            .then(a.cmp(&b))
    });
    order
}
