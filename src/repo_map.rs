use crate::{
    bound_tool_output_to,
    cache::{self, codec},
    failure,
    repomap_ranker::{FileFact, RankContext},
    walk, ToolResult,
};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
};

const LEGACY_ENTRY_POINTS: &[&str] = &["main.rs", "lib.rs", "index.ts"];
const RUST_PREFIXES: &[&str] = &["pub ", "async ", "unsafe ", "default "];
const COMMON_GRAPH_SYMBOLS: &[&str] = &[
    "actual", "boolean", "call", "changed", "clone", "close", "collect", "config", "context",
    "data", "default", "error", "expected", "file", "filter", "from", "get", "index", "input",
    "into", "item", "iter", "main", "map", "meta", "name", "new", "none", "number", "open",
    "option", "output", "path", "read", "result", "run", "set", "some", "state", "status",
    "string", "test", "tool", "unknown", "url", "value", "write",
];
/// Query-aware maps are consumed under a small output budget. Parsing every
/// source file in a very large monorepo first is both slow and misleading: the
/// mapper can spend a minute indexing thousands of unrelated integrations to
/// return a few dozen core files. Path evidence provides a deterministic
/// first-stage shortlist; symbol/import ranking then runs inside that pool.
const MAX_QUERY_SOURCE_FILES: usize = 3_000;

#[derive(Clone, Debug)]
pub(crate) struct SourceRecord {
    pub(crate) relative: String,
    pub(crate) loc: usize,
    pub(crate) definitions: Vec<String>,
    pub(crate) symbols: Vec<String>,
    pub(crate) imports: Vec<String>,
    pub(crate) identifier_hashes: Vec<u64>,
    /// Hashed term counts of the body, the path and the definition names
    /// (sorted, unique keys), so ranking never re-tokenizes a file.
    pub(crate) terms: Vec<(u64, u32)>,
    pub(crate) path_terms: Vec<(u64, u32)>,
    pub(crate) symbol_terms: Vec<(u64, u32)>,
    pub(crate) references: Vec<String>,
    pub(crate) changed: bool,
    pub(crate) entrypoint: bool,
}

impl SourceRecord {
    /// Cache payload. `relative` comes from the cache key, and `changed` and
    /// `references` are recomputed on every map, so none of them is stored.
    /// Any field added to the record must be added here and in `decode`.
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(256);
        codec::put_u64(&mut out, self.loc as u64);
        codec::put_strs(&mut out, &self.definitions);
        codec::put_strs(&mut out, &self.symbols);
        codec::put_strs(&mut out, &self.imports);
        codec::put_u64s(&mut out, &self.identifier_hashes);
        codec::put_u64(&mut out, u64::from(self.entrypoint));
        for terms in [&self.terms, &self.path_terms, &self.symbol_terms] {
            codec::put_u64s(
                &mut out,
                &terms
                    .iter()
                    .flat_map(|&(term, count)| [term, u64::from(count)])
                    .collect::<Vec<_>>(),
            );
        }
        out
    }

    /// Decode one cached term vector: key/count pairs with strictly
    /// increasing keys and counts that fit a u32.
    fn decode_terms(reader: &mut codec::Reader) -> Option<Vec<(u64, u32)>> {
        let packed = reader.u64s()?;
        if packed.len() % 2 != 0 {
            return None;
        }
        let terms = packed
            .chunks_exact(2)
            .map(|pair| Some((pair[0], u32::try_from(pair[1]).ok()?)))
            .collect::<Option<Vec<_>>>()?;
        terms
            .windows(2)
            .all(|pair| pair[0].0 < pair[1].0)
            .then_some(terms)
    }

    #[cfg(test)]
    fn decode(relative: &str, bytes: &[u8]) -> Option<Self> {
        Self::decode_mode(relative, bytes, false)
    }

    fn decode_mode(relative: &str, bytes: &[u8], summary: bool) -> Option<Self> {
        let mut reader = codec::Reader::new(bytes);
        let record = Self {
            relative: relative.to_owned(),
            loc: usize::try_from(reader.u64()?).ok()?,
            definitions: reader.strs()?,
            symbols: if summary { reader.skip_strs()?; Vec::new() } else { reader.strs()? },
            imports: reader.strs()?,
            identifier_hashes: if summary { reader.skip_u64s()?; Vec::new() } else { reader.u64s()? },
            entrypoint: reader.u64()? != 0,
            terms: if summary { reader.skip_u64s()?; Vec::new() } else { Self::decode_terms(&mut reader)? },
            path_terms: if summary { reader.skip_u64s()?; Vec::new() } else { Self::decode_terms(&mut reader)? },
            symbol_terms: if summary { reader.skip_u64s()?; Vec::new() } else { Self::decode_terms(&mut reader)? },
            references: Vec::new(),
            changed: false,
        };
        reader.finished().then_some(record)
    }
}

pub(crate) fn normalized_path(path: &Path) -> String {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => parts.push(value.to_string_lossy().into_owned()),
            Component::ParentDir => {
                parts.pop();
            }
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    parts.join("/")
}

fn compact(text: &str, limit: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= limit {
        collapsed
    } else {
        let mut cut: String = collapsed.chars().take(limit.saturating_sub(3)).collect();
        cut.push_str("...");
        cut
    }
}

fn strip_rust_prefixes(mut line: &str) -> &str {
    loop {
        let before = line;
        if let Some(rest) = line.strip_prefix("pub(") {
            if let Some(end) = rest.find(") ") {
                line = &rest[end + 2..];
            }
        }
        for prefix in RUST_PREFIXES {
            if let Some(rest) = line.strip_prefix(prefix) {
                line = rest;
            }
        }
        // `const fn` and `extern "C" fn` qualify a function; a bare `const`
        // or `static` is an item of its own and must stay.
        if let Some(rest) = line.strip_prefix("const ") {
            if ["fn ", "unsafe ", "async ", "extern "].iter().any(|next| rest.starts_with(next)) {
                line = rest;
            }
        }
        if let Some(rest) = line.strip_prefix("extern ") {
            let rest = match rest.strip_prefix('"') {
                Some(abi) => abi.split_once('"').map_or("", |(_, after)| after).trim_start(),
                None => rest,
            };
            if rest.starts_with("fn ") || rest.starts_with("unsafe ") {
                line = rest;
            }
        }
        if line == before {
            return line;
        }
    }
}

fn symbol_name(text: &str) -> &str {
    text.split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .find(|part| !part.is_empty())
        .unwrap_or("")
}

fn add_definition(
    definitions: &mut Vec<String>,
    symbols: &mut Vec<String>,
    kind: &str,
    rest: &str,
) {
    let name = symbol_name(rest);
    if name.is_empty() {
        return;
    }
    definitions.push(format!("{kind} {name}"));
    symbols.push(name.to_owned());
}

const IDENTIFIER_HASH_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const IDENTIFIER_HASH_PRIME: u64 = 0x0000_0100_0000_01b3;

fn hash_identifier_bytes(bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes
        .into_iter()
        .fold(IDENTIFIER_HASH_OFFSET, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(IDENTIFIER_HASH_PRIME)
        })
}

fn identifier_hash(value: &str) -> u64 {
    hash_identifier_bytes(value.bytes())
}

/// Compact identifier membership used only for caller/definer discovery.
/// Case-sensitive, as identifiers are: folding case let the English word
/// "enough" in a comment reference a C macro `ENOUGH`.
///
/// Keeping every identifier as an owned `String` in every source record made
/// a large Python monorepo consume hundreds of MiB and spend most of its time
/// allocating tree nodes. A sorted vector of stable 64-bit hashes preserves
/// the equality lookup this phase needs while remaining cache-friendly.
fn identifier_hashes(text: &str) -> Vec<u64> {
    let mut identifiers = Vec::new();
    let mut current_hash = IDENTIFIER_HASH_OFFSET;
    let mut current_len = 0usize;
    let finish = |identifiers: &mut Vec<u64>, hash: &mut u64, len: &mut usize| {
        if (2..=128).contains(len) {
            identifiers.push(*hash);
        }
        *hash = IDENTIFIER_HASH_OFFSET;
        *len = 0;
    };
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'_' {
            current_hash = (current_hash ^ u64::from(byte))
                .wrapping_mul(IDENTIFIER_HASH_PRIME);
            current_len = current_len.saturating_add(1);
        } else if current_len > 0 {
            finish(&mut identifiers, &mut current_hash, &mut current_len);
        }
    }
    if current_len > 0 {
        finish(&mut identifiers, &mut current_hash, &mut current_len);
    }
    identifiers.sort_unstable();
    identifiers.dedup();
    identifiers
}

fn distinctive_graph_symbol(symbol: &str) -> bool {
    let normalized = symbol.to_ascii_lowercase();
    if COMMON_GRAPH_SYMBOLS.contains(&normalized.as_str()) {
        return false;
    }
    if !symbol.chars().any(|character| character.is_ascii_lowercase()) {
        // An all-caps name is distinctive only as SCREAMING_CASE: a bare
        // `DONE` or `BE` is as likely a word in a comment as a reference.
        return symbol.contains('_') && symbol.len() >= 4;
    }
    normalized.len() >= 6
        || symbol.contains('_')
        || symbol
            .chars()
            .skip(1)
            .any(|character| character.is_ascii_uppercase())
}

pub(crate) fn import_literal(line: &str) -> Option<String> {
    for marker in [
        " from \"",
        " from '",
        "import \"",
        "import '",
        "require(\"",
        "require('",
    ] {
        let Some(at) = line.find(marker) else {
            continue;
        };
        let rest = &line[at + marker.len()..];
        let quote = if marker.ends_with('\"') { '"' } else { '\'' };
        let target = rest.split(quote).next().unwrap_or("").trim();
        if !target.is_empty() {
            return Some(target.to_owned());
        }
    }
    None
}

/// One file's definitions before ordering and truncation.
#[derive(Default)]
struct Extracted {
    definitions: Vec<String>,
    symbols: Vec<String>,
    imports: Vec<String>,
    /// Listed after the other definitions (C/C++ `#define`s).
    macros: Vec<String>,
}

fn extract(extension: &str, text: &str) -> Extracted {
    if crate::c_like::EXTENSIONS.contains(&extension) {
        let found = crate::c_like::parse(text);
        return Extracted {
            definitions: found.definitions,
            symbols: found.symbols,
            imports: found.imports,
            macros: found.macros,
        };
    }
    let found = match extension {
        "go" => Some(crate::go_like::parse(text)),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => Some(crate::script_like::parse(text, false)),
        "svelte" | "vue" => Some(crate::script_like::parse(text, true)),
        _ => None,
    };
    if let Some(found) = found {
        return Extracted {
            definitions: found.definitions,
            symbols: found.symbols,
            imports: found.imports,
            macros: found.macros,
        };
    }
    let mut definitions = Vec::new();
    let mut symbols = Vec::new();
    let mut imports = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        match extension {
            "rs" => {
                let stripped = strip_rust_prefixes(trimmed);
                let mut found = false;
                for (prefix, kind) in [
                    ("async fn ", "fn"),
                    ("fn ", "fn"),
                    ("struct ", "struct"),
                    ("enum ", "enum"),
                    ("trait ", "trait"),
                    ("type ", "type"),
                    ("union ", "union"),
                    ("mod ", "mod"),
                    ("static mut ", "static"),
                    ("static ", "static"),
                    ("const ", "const"),
                    ("macro_rules! ", "macro"),
                ] {
                    if let Some(rest) = stripped.strip_prefix(prefix) {
                        if symbol_name(rest) != "_" {
                            add_definition(&mut definitions, &mut symbols, kind, rest);
                        }
                        found = true;
                        break;
                    }
                }
                if !found {
                    if let Some(rest) = stripped.strip_prefix("impl ") {
                        let header = rest
                            .split(['{', '}'])
                            .next()
                            .unwrap_or("")
                            .split(" where ")
                            .next()
                            .unwrap_or("")
                            .trim();
                        definitions.push(compact(
                            &if header.is_empty() {
                                "impl".to_owned()
                            } else {
                                format!("impl {header}")
                            },
                            120,
                        ));
                    }
                }
                for prefix in [
                    "use crate::",
                    "pub use crate::",
                    "use super::",
                    "use self::",
                ] {
                    if let Some(rest) = trimmed.strip_prefix(prefix) {
                        imports.push(compact(rest.trim_end_matches(';'), 160));
                        break;
                    }
                }
            }
            "py" => {
                for (prefix, kind) in [("async def ", "def"), ("def ", "def"), ("class ", "class")]
                {
                    if let Some(rest) = trimmed.strip_prefix(prefix) {
                        add_definition(&mut definitions, &mut symbols, kind, rest);
                        break;
                    }
                }
                for prefix in ["from ", "import "] {
                    if let Some(rest) = trimmed.strip_prefix(prefix) {
                        imports.push(
                            rest.split_whitespace()
                                .next()
                                .unwrap_or("")
                                .replace('.', "/"),
                        );
                        break;
                    }
                }
            }
            "java" | "kt" | "kts" | "swift" | "cs" => {
                let stripped = ["public ", "private ", "protected ", "internal ", "open "]
                    .iter()
                    .fold(trimmed, |value, prefix| {
                        value.strip_prefix(prefix).unwrap_or(value)
                    });
                for (prefix, kind) in [
                    ("class ", "class"),
                    ("interface ", "interface"),
                    ("enum ", "enum"),
                    ("struct ", "struct"),
                    ("protocol ", "protocol"),
                    ("fun ", "fun"),
                    ("func ", "func"),
                ] {
                    if let Some(rest) = stripped.strip_prefix(prefix) {
                        add_definition(&mut definitions, &mut symbols, kind, rest);
                        break;
                    }
                }
            }
            "rb" => {
                for (prefix, kind) in [("def ", "def"), ("class ", "class"), ("module ", "module")]
                {
                    if let Some(rest) = trimmed.strip_prefix(prefix) {
                        add_definition(&mut definitions, &mut symbols, kind, rest);
                        break;
                    }
                }
            }
            "php" => {
                let stripped = trimmed.strip_prefix("public ").unwrap_or(trimmed);
                for (prefix, kind) in [
                    ("function ", "function"),
                    ("class ", "class"),
                    ("interface ", "interface"),
                    ("trait ", "trait"),
                ] {
                    if let Some(rest) = stripped.strip_prefix(prefix) {
                        add_definition(&mut definitions, &mut symbols, kind, rest);
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    Extracted { definitions, symbols, imports, macros: Vec::new() }
}

fn parse_source(relative: String, extension: String, text: &str, changed: bool) -> SourceRecord {
    let Extracted { mut definitions, mut symbols, mut imports, mut macros } = extract(&extension, text);
    definitions.sort();
    definitions.dedup();
    macros.sort();
    macros.dedup();
    definitions.extend(macros);
    definitions.truncate(24);
    symbols.sort();
    symbols.dedup();
    imports.sort();
    imports.dedup();
    imports.truncate(24);

    let file_name = Path::new(&relative)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let entrypoint = LEGACY_ENTRY_POINTS.contains(&file_name);
    let hashed = |text: &str| {
        crate::repomap_ranker::hash_terms(&crate::repomap_ranker::term_counts(text))
    };
    SourceRecord {
        loc: text.lines().count(),
        definitions,
        path_terms: hashed(&relative),
        symbol_terms: hashed(&symbols.join(" ")),
        symbols,
        imports,
        identifier_hashes: identifier_hashes(text),
        terms: hashed(text),
        relative,
        references: Vec::new(),
        changed,
        entrypoint,
    }
}

fn context_path_terms(context: &RankContext) -> BTreeSet<String> {
    context
        .query
        .split(|character: char| !character.is_ascii_alphanumeric())
        .chain(
            context.mentioned_symbols.iter().flat_map(|value| {
                value.split(|character: char| !character.is_ascii_alphanumeric())
            }),
        )
        .map(str::to_ascii_lowercase)
        .filter(|term| term.len() > 1)
        .collect()
}

fn context_source_files(
    root: &Path,
    files: Vec<PathBuf>,
    changed: &HashSet<String>,
    context: Option<&RankContext>,
) -> (Vec<PathBuf>, usize) {
    let total = files.len();
    let Some(context) = context else {
        return (files, total);
    };
    if files.len() <= MAX_QUERY_SOURCE_FILES {
        return (files, total);
    }

    let terms = context_path_terms(context);
    // Area evidence is added only after its isolated cost/quality experiment.
    let mentioned_paths = context
        .mentioned_paths
        .iter()
        .map(|value| normalized_path(Path::new(value)).to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>();
    let open_paths = context
        .open_paths
        .iter()
        .map(|value| normalized_path(Path::new(value)).to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>();
    let mut ranked = files
        .into_iter()
        .map(|path| {
            let relative = path
                .strip_prefix(root)
                .map(normalized_path)
                .unwrap_or_else(|_| normalized_path(&path));
            let lower = relative.to_ascii_lowercase();
            let path_terms = lower
                .split(|character: char| !character.is_ascii_alphanumeric())
                .filter(|value| !value.is_empty())
                .collect::<BTreeSet<_>>();
            let direct_mention = mentioned_paths
                .iter()
                .any(|value| lower == *value || lower.ends_with(&format!("/{value}")));
            let open = open_paths
                .iter()
                .any(|value| lower == *value || lower.ends_with(&format!("/{value}")));
            let term_hits = terms
                .iter()
                .filter(|term| path_terms.contains(term.as_str()) || lower.contains(term.as_str()))
                .count() as i64;
            let file_name = Path::new(&relative)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("");
            let depth = relative.bytes().filter(|byte| *byte == b'/').count() as i64;
            let test_like = lower.contains("/test/")
                || lower.contains("/tests/")
                || lower.contains("/fixtures/")
                || lower.contains("/examples/")
                || lower.contains("/docs/");
            let score = i64::from(direct_mention) * 1_000_000
                + i64::from(open) * 800_000
                + i64::from(changed.contains(&relative)) * 600_000
                + term_hits * 12_000

                + i64::from(LEGACY_ENTRY_POINTS.contains(&file_name)) * 800
                + i64::from(lower.contains("/src/") || lower.starts_with("src/")) * 240
                + i64::from(lower.contains("/agent/") || lower.starts_with("agent/")) * 180
                - i64::from(test_like && term_hits == 0) * 320
                - depth.min(40) * 4;
            (score, relative, path)
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    (
        ranked
            .into_iter()
            .take(MAX_QUERY_SOURCE_FILES)
            .map(|(_, _, path)| path)
            .collect(),
        total,
    )
}

fn git_paths(root: &Path, args: &[&str]) -> Option<HashSet<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    output.status.success().then(|| {
        output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
            .filter_map(|entry| std::str::from_utf8(entry).ok())
            .map(|path| path.replace('\\', "/"))
            .collect()
    })
}

/// Files that differ from HEAD, relative to `root`: tracked files with
/// changes plus files the walker found that git does not track. Both git
/// calls read the index rather than scanning the tree, so unignored build
/// output (a CMake tree, say) costs nothing, and `--relative` keeps paths
/// correct when `root` is a subdirectory of the repository.
fn changed_paths(root: &Path, walked: &[String]) -> HashSet<String> {
    let Some(tracked) = git_paths(root, &["ls-files", "-z"]) else {
        return HashSet::new();
    };
    let mut changed =
        git_paths(root, &["diff", "--name-only", "--relative", "-z", "HEAD"]).unwrap_or_default();
    changed.extend(
        walked
            .iter()
            .filter(|relative| !tracked.contains(relative.as_str()))
            .cloned(),
    );
    changed
}

pub(crate) fn resolve_import(
    source: &SourceRecord,
    target: &str,
    paths: &HashSet<String>,
) -> String {
    let target = target.replace('\\', "/");
    if paths.contains(&target) {
        return target;
    }

    let mut bases = Vec::new();
    if target.starts_with('.') {
        let parent = Path::new(&source.relative)
            .parent()
            .unwrap_or_else(|| Path::new(""));
        bases.push(normalized_path(&parent.join(&target)));
    } else if let Some(rest) = target.strip_prefix("crate::") {
        bases.push(format!("src/{}", rest.replace("::", "/")));
    } else {
        bases.push(target.replace("::", "/"));
    }

    for base in bases {
        if paths.contains(&base) {
            return base;
        }
        let candidates = [
            base.clone(),
            format!("{base}.rs"),
            format!("{base}.ts"),
            format!("{base}.tsx"),
            format!("{base}.js"),
            format!("{base}.py"),
            format!("{base}/mod.rs"),
            format!("{base}/index.ts"),
            format!("{base}/index.tsx"),
            format!("{base}/index.js"),
        ];
        if let Some(path) = candidates
            .into_iter()
            .find(|candidate| paths.contains(candidate))
        {
            return path;
        }
    }

    target
}

/// Files that uniquely define each distinctive symbol, for finding the
/// symbols a file references from elsewhere.
struct Definers {
    owners: HashMap<u64, Vec<(usize, String)>>,
}

impl Definers {
    fn new(records: &[SourceRecord]) -> Self {
        let mut owners: HashMap<u64, Vec<(usize, String)>> = HashMap::new();
        for (index, record) in records.iter().enumerate() {
            for symbol in &record.symbols {
                if distinctive_graph_symbol(symbol) {
                    owners
                        .entry(identifier_hash(symbol))
                        .or_default()
                        .push((index, symbol.clone()));
                }
            }
        }
        Self { owners }
    }

    /// Distinctive symbols record `index` uses that exactly one other file
    /// defines: mentioned symbols first, then the rest, at most 64.
    fn references(&self, record: &SourceRecord, index: usize, mentioned: &HashSet<u64>) -> Vec<String> {
        let mut priority_references = BTreeSet::new();
        let mut references = BTreeSet::new();
        for identifier_hash in &record.identifier_hashes {
            let Some(matches) = self.owners.get(identifier_hash) else {
                continue;
            };
            if matches.len() == 1 && matches[0].0 != index {
                if mentioned.contains(identifier_hash) {
                    priority_references.insert(matches[0].1.clone());
                } else {
                    references.insert(matches[0].1.clone());
                }
            }
        }
        priority_references
            .into_iter()
            .chain(references)
            .take(64)
            .collect()
    }
}

fn mentioned_hashes(context: Option<&RankContext>) -> HashSet<u64> {
    context
        .into_iter()
        .flat_map(|context| context.mentioned_symbols.iter())
        .map(|symbol| identifier_hash(symbol))
        .collect()
}

fn add_references(records: &mut [SourceRecord], context: Option<&RankContext>) {
    let definers = Definers::new(records);
    let mentioned = mentioned_hashes(context);
    let references: Vec<Vec<String>> = records
        .iter()
        .enumerate()
        .map(|(index, record)| definers.references(record, index, &mentioned))
        .collect();
    for (record, references) in records.iter_mut().zip(references) {
        record.references = references;
    }
}

/// `REPOMAP_TIMING=1` prints each phase's wall time to stderr.
pub(crate) struct PhaseTimer {
    enabled: bool,
    last: std::time::Instant,
}

impl PhaseTimer {
    pub(crate) fn start() -> Self {
        Self {
            enabled: std::env::var_os("REPOMAP_TIMING").is_some(),
            last: std::time::Instant::now(),
        }
    }

    pub(crate) fn mark(&mut self, phase: &str) {
        if self.enabled {
            let now = std::time::Instant::now();
            eprintln!(
                "repomap timing: {phase:<10} {:>8.2} ms",
                (now - self.last).as_secs_f64() * 1e3
            );
            self.last = now;
        }
    }
}

fn read_source(path: &Path, relative: String) -> Option<SourceRecord> {
    let text = fs::read_to_string(path).ok()?;
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    // The definition readers are heuristics over arbitrary text; one that
    // panics on a strange file costs that file its definitions, not the map.
    let parsed = std::panic::catch_unwind(|| parse_source(relative.clone(), extension, &text, false));
    Some(parsed.unwrap_or_else(|_| parse_source(relative, String::new(), &text, false)))
}

/// Parse cache misses on every core. Results keep their input positions.
fn parse_parallel(misses: &[(usize, PathBuf, String)]) -> Vec<(usize, SourceRecord)> {
    let workers = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(4)
        .min(16)
        .min(misses.len().div_ceil(16))
        .max(1);
    let next = AtomicUsize::new(0);
    let parsed = Mutex::new(Vec::with_capacity(misses.len()));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                let mut local = Vec::new();
                loop {
                    let at = next.fetch_add(1, Ordering::Relaxed);
                    let Some((index, path, relative)) = misses.get(at) else {
                        break;
                    };
                    if let Some(record) = read_source(path, relative.clone()) {
                        local.push((*index, record));
                    }
                }
                parsed
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .append(&mut local);
            });
        }
    });
    parsed
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Every source record under `root`. With `all_references`, each record's
/// cross-file references are filled in too (the overview and the legacy list
/// need them for every file); the task map fills them only for the files it
/// shows.
pub(crate) fn read_records(
    root: &Path,
    context: Option<&RankContext>,
    timer: &mut PhaseTimer,
    all_references: bool,
) -> (Vec<SourceRecord>, usize) {
    read_records_mode(root, context, timer, all_references, false)
}

pub(crate) fn read_overview_records(root: &Path, timer: &mut PhaseTimer) -> (Vec<SourceRecord>, usize) {
    read_records_mode(root, None, timer, true, true)
}

fn read_records_mode(root: &Path, context: Option<&RankContext>, timer: &mut PhaseTimer,
    all_references: bool, overview: bool) -> (Vec<SourceRecord>, usize) {
    let walked_files = walk::source_files_with_metadata(root);
    let summary = overview && walked_files.len() > MAX_QUERY_SOURCE_FILES;
    timer.mark("walk");
    let fingerprints_by_path: HashMap<PathBuf, walk::Fingerprint> = walked_files.iter().cloned().collect();
    let files: Vec<PathBuf> = walked_files.into_iter().map(|(path, _)| path).collect();
    let walked: Vec<String> = files
        .iter()
        .map(|path| {
            path.strip_prefix(root)
                .map(normalized_path)
                .unwrap_or_else(|_| normalized_path(path))
        })
        .collect();
    let changed = changed_paths(root, &walked);
    timer.mark("git");
    let (source_files, total_source_files) = context_source_files(root, files, &changed, context);
    let root_key = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut store = cache::open(&root_key);
    timer.mark("load");

    let mut slots: Vec<Option<SourceRecord>> = Vec::with_capacity(source_files.len());
    let mut fingerprints = Vec::with_capacity(source_files.len());
    let mut misses = Vec::new();
    for path in source_files {
        let relative = path
            .strip_prefix(root)
            .map(normalized_path)
            .unwrap_or_else(|_| normalized_path(&path));
        let fingerprint = fingerprints_by_path.get(&path).copied().flatten();
        let hit = fingerprint.and_then(|(modified, len)| {
            store
                .get(&relative, modified, len)
                .and_then(|payload| SourceRecord::decode_mode(&relative, &payload, summary))
        });
        if hit.is_none() {
            misses.push((slots.len(), path, relative));
        }
        slots.push(hit);
        fingerprints.push(fingerprint);
    }

    timer.mark("stat");
    for batch in misses.chunks(128) {
    let parsed = parse_parallel(batch);
    for (index, mut record) in parsed {
        if let Some((modified, len)) = fingerprints[index] {
            store.insert(
                record.relative.clone(),
                cache::Entry {
                    modified,
                    len,
                    payload: record.encode(),
                },
            );
        }
        if summary {
            record.terms.clear(); record.terms.shrink_to_fit();
            record.path_terms.clear(); record.path_terms.shrink_to_fit();
            record.symbol_terms.clear(); record.symbol_terms.shrink_to_fit();
            record.identifier_hashes.clear(); record.identifier_hashes.shrink_to_fit();
            record.symbols = Vec::new();
        }
        slots[index] = Some(record);
    }
    }
    timer.mark("parse");

    let mut records: Vec<SourceRecord> = slots.into_iter().flatten().collect();
    if records.len() == total_source_files {
        let present: HashSet<&str> = records
            .iter()
            .map(|record| record.relative.as_str())
            .collect();
        store.retain(|relative| present.contains(relative));
    }
    store.save();
    drop(store);
    timer.mark("save");
    for record in &mut records {
        record.changed = changed.contains(&record.relative);
    }
    // A summary keeps no identifier hashes, so no reference could be found.
    if all_references && !summary {
        add_references(&mut records, context);
        timer.mark("refs");
    }
    (records, total_source_files)
}

/// One line per file: path, size and up to eight definition names. About a
/// third the size of a full section, so a budget shows ~3x the candidates.
fn render_compact(record: &SourceRecord) -> String {
    let names = record
        .definitions
        .iter()
        .filter_map(|definition| definition.split_once(' ').map(|(_, name)| name))
        .filter(|name| !name.is_empty())
        .take(8)
        .collect::<Vec<_>>()
        .join(" ");
    if names.is_empty() {
        format!("{} | {} LOC\n", record.relative, record.loc)
    } else {
        format!("{} | {} LOC | {names}\n", record.relative, record.loc)
    }
}

pub(crate) fn render_record(record: &SourceRecord, include_references: bool) -> String {
    let mut output = format!("{} | {} LOC\n", record.relative, record.loc);
    if !record.definitions.is_empty() {
        let limit = if include_references { 18 } else { 24 };
        output.push_str(&format!(
            "  defs: {}\n",
            record
                .definitions
                .iter()
                .take(limit)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    if !record.imports.is_empty() {
        let limit = if include_references { 12 } else { 24 };
        output.push_str(&format!(
            "  imports: {}\n",
            record
                .imports
                .iter()
                .take(limit)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    if include_references && !record.references.is_empty() {
        output.push_str(&format!(
            "  refs: {}\n",
            record
                .references
                .iter()
                .take(16)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    output
}

fn finish_output(output: String, max_chars: usize) -> ToolResult {
    if output.is_empty() {
        return ToolResult {
            ok: true,
            output: "No files fit the repo-map token budget.".into(),
            truncated: false,
            exit_code: Some(0),
        };
    }
    let mut result = bound_tool_output_to(&output, max_chars.max(1));
    result.exit_code = Some(0);
    result
}

/// Legacy deterministic structural map used when no query context is supplied.
///
/// Keeping this entrypoint preserves the old entrypoint-then-LOC ordering for
/// callers that do not yet provide a query.
pub fn repo_map(root: &Path, max_chars: usize) -> ToolResult {
    let mut timer = PhaseTimer::start();
    let (mut records, _) = read_records(root, None, &mut timer, true);
    if records.is_empty() {
        return failure("repomap: no source files found");
    }
    records.sort_by(|left, right| {
        right
            .entrypoint
            .cmp(&left.entrypoint)
            .then(right.loc.cmp(&left.loc))
            .then(left.relative.cmp(&right.relative))
    });
    let output = records
        .iter()
        .map(|record| render_record(record, false))
        .collect();
    finish_output(output, max_chars)
}

/// Query-aware structural map.
///
/// The caller supplies paths/symbols already mentioned in the conversation,
/// currently open files, and a map-token budget. Ranking then combines those
/// signals with cached full-text BM25 evidence and independent path and
/// definition fields, then packs in deterministic priority and score order.
/// How much of each ranked file the task map shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detail {
    /// Definitions, local imports and cross-file references per file.
    Full,
    /// One line per file (path, size, definition names): more candidates
    /// in the same budget, for an agent to choose among.
    Compact,
}

pub fn repo_map_with_context(root: &Path, max_chars: usize, context: &RankContext) -> ToolResult {
    repo_map_with_detail(root, max_chars, context, Detail::Full)
}

pub fn repo_map_with_detail(
    root: &Path,
    max_chars: usize,
    context: &RankContext,
    detail: Detail,
) -> ToolResult {
    let mut timer = PhaseTimer::start();
    let (mut records, total_source_files) = read_records(root, Some(context), &mut timer, false);
    if records.is_empty() {
        return failure("repomap: no source files found");
    }

    // Ranking reads cached term vectors, names, the dirty flag and, for the
    // "unique caller" tier, which files use an explicitly mentioned symbol.
    // Full reference lists and rendered sections are built afterwards, only
    // for the files the budget reaches.
    let definers = Definers::new(&records);
    let mentioned = mentioned_hashes(Some(context));
    let facts: Vec<FileFact> = records
        .iter()
        .enumerate()
        .map(|(index, record)| FileFact {
            path: record.relative.clone(),
            symbols: record.symbols.clone(),
            imports: Vec::new(),
            references: if mentioned.is_empty() {
                Vec::new()
            } else {
                definers
                    .references(record, index, &mentioned)
                    .into_iter()
                    .filter(|symbol| mentioned.contains(&identifier_hash(symbol)))
                    .collect()
            },
            loc: record.loc,
            estimated_tokens: 0,
            changed: record.changed,
            entrypoint: record.entrypoint,
        })
        .collect();
    timer.mark("facts");
    let terms = records
        .iter()
        .map(|record| crate::repomap_ranker::TermFields {
            body: &record.terms,
            path: &record.path_terms,
            symbols: &record.symbol_terms,
        })
        .collect::<Vec<_>>();
    let order = if context.token_budget == 0 {
        Vec::new()
    } else {
        crate::repomap_ranker::rank_with_hashed_terms(&facts, context, &terms)
    };
    timer.mark("rank");

    // Pack in rank order, skipping a file whose section does not fit. A
    // section is never shorter than its header line, so a file whose header
    // alone does not fit is skipped without building its references.
    let mut remaining = context.token_budget;
    let mut output = String::new();
    for index in order {
        let record = &records[index];
        let header = format!("{} | {} LOC\n", record.relative, record.loc);
        if header.chars().count().div_ceil(4).max(1) > remaining {
            continue;
        }
        let section = if detail == Detail::Compact {
            render_compact(record)
        } else {
            let references = definers.references(record, index, &mentioned);
            records[index].references = references;
            render_record(&records[index], true)
        };
        let tokens = section.chars().count().div_ceil(4).max(1);
        if tokens > remaining {
            continue;
        }
        remaining -= tokens;
        output.push_str(&section);
    }
    timer.mark("pack");
    if records.len() < total_source_files {
        output.push_str(&format!(
            "... [query shortlist indexed {} of {} source files; exact reads/searches are still required]\n",
            records.len(), total_source_files
        ));
    }
    finish_output(output, max_chars)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn context(query: &str, budget: usize) -> RankContext {
        RankContext {
            query: query.into(),
            mentioned_paths: Vec::new(),
            mentioned_symbols: Vec::new(),
            open_paths: Vec::new(),
            token_budget: budget,
        }
    }

    #[test]
    fn body_only_query_survives_warm_cache_and_invalidates_on_edit() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "pub fn unrelated() {}\n").unwrap();
        fs::write(
            dir.path().join("z.rs"),
            "// nebula signatures\npub fn handler() {}\n",
        )
        .unwrap();
        let ctx = context("nebula signature", 100);
        let cold = repo_map_with_context(dir.path(), 12000, &ctx);
        let warm = repo_map_with_context(dir.path(), 12000, &ctx);
        assert!(cold.ok && warm.ok);
        assert!(cold.output.starts_with("z.rs |"), "{}", cold.output);
        assert_eq!(cold.output, warm.output);
        fs::write(
            dir.path().join("a.rs"),
            "// nebula signatures nebula signatures\npub fn unrelated() {}\n",
        )
        .unwrap();
        fs::write(dir.path().join("z.rs"), "pub fn handler() {}\n").unwrap();
        let edited = repo_map_with_context(dir.path(), 12000, &ctx);
        assert!(edited.output.starts_with("a.rs |"), "{}", edited.output);
    }

    #[test]
    fn query_map_refreshes_changed_file_boost_on_warm_cache_hits() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(root)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        fs::write(root.join("a.rs"), "// nebula\npub fn alpha() {}\n").unwrap();
        fs::write(root.join("z.rs"), "// noise\npub fn zeta() {}\n").unwrap();
        git(&["add", "a.rs", "z.rs"]);
        git(&["commit", "-qm", "base"]);
        let ctx = context("nebula", 100);
        let first_path = || {
            let map = repo_map_with_context(root, 12000, &ctx);
            assert!(map.ok, "{}", map.output);
            map.output
                .lines()
                .next()
                .unwrap()
                .split(" | ")
                .next()
                .unwrap()
                .to_owned()
        };
        assert_eq!(first_path(), "a.rs");
        fs::write(root.join("z.rs"), "// noise revised\npub fn zeta() {}\n").unwrap();
        assert_eq!(first_path(), "z.rs"); // Unstaged edit.
        assert_eq!(first_path(), "z.rs"); // Warm source cache.
        git(&["add", "z.rs"]);
        assert_eq!(first_path(), "z.rs"); // Staged edit, unchanged source.
        git(&["commit", "-qm", "edited"]);
        assert_eq!(first_path(), "a.rs"); // Clean status must not stay cached.
        fs::write(
            root.join("z.rs"),
            "// noise revised again\npub fn zeta() {}\n",
        )
        .unwrap();
        assert_eq!(first_path(), "z.rs");
        git(&["restore", "z.rs"]);
        assert_eq!(first_path(), "a.rs");
    }

    #[test]
    fn legacy_map_keeps_entrypoint_first_and_excludes_private_state() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("src")).unwrap();
        fs::create_dir_all(directory.path().join(".nexus/sessions")).unwrap();
        fs::write(
            directory.path().join("main.rs"),
            "mod helper;\nfn main() {}\n",
        )
        .unwrap();
        fs::write(
            directory.path().join("src/helper.rs"),
            "pub struct Helper;\nimpl Helper {\n    pub fn nested_method() {}\n}\n",
        )
        .unwrap();
        fs::write(
            directory.path().join(".nexus/sessions/leak.rs"),
            "pub fn secret_history() {}\n",
        )
        .unwrap();

        let map = repo_map(directory.path(), 12_000);
        assert!(map.ok, "{}", map.output);
        assert!(map.output.starts_with("main.rs |"));
        assert!(map.output.contains("fn nested_method"));
        assert!(!map.output.contains("secret_history"));
    }

    #[test]
    fn query_map_follows_unique_symbol_references_under_budget() {
        let directory = tempfile::tempdir().unwrap();
        let mut definition = fs::File::create(directory.path().join("payments.rs")).unwrap();
        writeln!(definition, "pub fn authorize_payment() {{}}").unwrap();
        let mut caller = fs::File::create(directory.path().join("checkout.rs")).unwrap();
        writeln!(caller, "pub fn submit_order() {{ authorize_payment(); }}").unwrap();
        fs::write(
            directory.path().join("large.rs"),
            "pub fn unrelated() {}\n".repeat(400),
        )
        .unwrap();

        let mut query = context("authorize payment submit order", 35);
        query.mentioned_symbols.push("authorize_payment".into());
        let map = repo_map_with_context(directory.path(), 12_000, &query);
        assert!(map.ok, "{}", map.output);
        assert!(map.output.contains("payments.rs"));
        assert!(map.output.contains("checkout.rs"));
        assert!(!map.output.contains("large.rs"));
    }

    #[test]
    fn mentioned_symbol_callers_survive_the_reference_cap() {
        let directory = tempfile::tempdir().unwrap();
        let mut definitions = String::new();
        let mut calls = String::from("pub fn call_everything() {\n");
        for index in 0..70 {
            definitions.push_str(&format!("pub fn alpha_symbol_{index:02}() {{}}\n"));
            calls.push_str(&format!("    alpha_symbol_{index:02}();\n"));
        }
        definitions.push_str("pub fn zzzz_critical_symbol() {}\n");
        calls.push_str("    zzzz_critical_symbol();\n}\n");
        fs::write(directory.path().join("owners.rs"), definitions).unwrap();
        fs::write(directory.path().join("caller.rs"), calls).unwrap();

        let mut query = context("zzzz critical symbol", 500);
        query.mentioned_symbols.push("zzzz_critical_symbol".into());
        let map = repo_map_with_context(directory.path(), 12_000, &query);
        assert!(map.ok, "{}", map.output);
        assert!(map.output.contains("owners.rs"), "{}", map.output);
        assert!(map.output.contains("caller.rs"), "{}", map.output);
        assert!(map.output.contains("refs: zzzz_critical_symbol"));
    }

    /// Verification hook for bench/proto/defs_check.py: every listed file's
    /// definitions before truncation, as JSON lines.
    /// `REPOMAP_DUMP_FILES=list cargo test --release --lib dump_definitions -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn dump_definitions() {
        let list = std::env::var("REPOMAP_DUMP_FILES").expect("REPOMAP_DUMP_FILES names a file of paths");
        for path in fs::read_to_string(list).unwrap().lines() {
            let text = String::from_utf8_lossy(&fs::read(path).unwrap_or_default()).into_owned();
            let extension = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
            let found = extract(&extension, &text);
            let definitions = found.definitions.iter().chain(&found.macros).map(|value| format!("{value:?}")).collect::<Vec<_>>();
            println!("{{\"path\":{path:?},\"definitions\":[{}]}}", definitions.join(","));
        }
    }

    #[test]
    fn rust_items_include_constants_statics_extern_functions_and_macros() {
        let found = extract(
            "rs",
            "pub const MAX: usize = 3;\npub(crate) static mut COUNTER: u32 = 0;\nconst _: () = ();\n\
             pub const fn new() -> Self {}\npub unsafe extern \"C\" fn hook() {}\nextern crate alloc;\n\
             macro_rules! ensure {\n    () => {};\n}\npub union Bits { a: u32 }\n",
        );
        assert_eq!(
            found.definitions,
            ["const MAX", "static COUNTER", "fn new", "fn hook", "macro ensure", "union Bits"]
        );
    }

    #[test]
    fn c_definitions_and_case_sensitive_references() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("table.h"),
            "#define TABLE_SIZE(n) ((n) * 2)\n#define DONE(x) (x)\nstruct code_table {\n\tint size;\n};\n",
        )
        .unwrap();
        fs::write(
            directory.path().join("user.c"),
            "#include \"table.h\"\nint decode(struct code_table *t)\n{\n\treturn TABLE_SIZE(t->size) + DONE(1);\n}\n",
        )
        .unwrap();
        fs::write(
            directory.path().join("prose.c"),
            "/* We are done: see table_size and the Code_table notes. */\nint unrelated(void)\n{\n\treturn 0;\n}\n",
        )
        .unwrap();

        let map = repo_map_with_context(directory.path(), 12_000, &context("decode table", 2_000));
        assert!(map.ok, "{}", map.output);
        // Each file's block: its header line plus the indented lines below.
        let block = |name: &str| {
            let mut lines = map.output.lines().skip_while(|line| !line.starts_with(&format!("{name} |")));
            let header = lines.next().unwrap_or_else(|| panic!("{name} missing: {}", map.output));
            std::iter::once(header).chain(lines.take_while(|line| line.starts_with("  "))).collect::<Vec<_>>().join("\n")
        };
        let user = block("user.c");
        assert!(user.contains("defs: fn decode"), "{user}");
        assert!(user.contains("imports: ./table.h"), "{user}");
        // A bare all-caps macro (DONE) is not a distinctive reference.
        assert!(user.contains("refs: TABLE_SIZE; code_table\n") || user.ends_with("refs: TABLE_SIZE; code_table"), "{user}");
        let table = block("table.h");
        assert!(table.contains("defs: struct code_table; define DONE; define TABLE_SIZE"), "{table}");
        // Words in a comment match no identifier of another case.
        let prose = block("prose.c");
        assert!(!prose.contains("refs:"), "{prose}");
    }

    #[test]
    fn zero_budget_returns_no_ranked_files() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("lib.rs"), "pub fn target() {}\n").unwrap();
        let map = repo_map_with_context(directory.path(), 12_000, &context("target", 0));
        assert_eq!(map.output, "No files fit the repo-map token budget.");
    }

    #[test]
    fn large_query_shortlist_keeps_direct_paths_and_reports_total() {
        let root = Path::new("/workspace");
        let mut files = (0..=MAX_QUERY_SOURCE_FILES)
            .map(|index| root.join(format!("optional/integration-{index:04}/handler.py")))
            .collect::<Vec<_>>();
        let target = root.join("agent/context_compressor.py");
        files.push(target.clone());
        let context = RankContext {
            query: "context compression".into(),
            mentioned_paths: vec!["Agent/Context_Compressor.py".into()],
            mentioned_symbols: Vec::new(),
            open_paths: Vec::new(),
            token_budget: 100,
        };
        let (selected, total) = context_source_files(root, files, &HashSet::new(), Some(&context));
        assert_eq!(total, MAX_QUERY_SOURCE_FILES + 2);
        assert_eq!(selected.len(), MAX_QUERY_SOURCE_FILES);
        assert!(selected.contains(&target));
    }

    #[test]
    fn record_round_trips_through_the_cache_encoding() {
        let record = parse_source(
            "src/pay.rs".into(),
            "rs".into(),
            "use crate::billing::Plan;\npub fn authorize_payment() {}\npub struct Ledger;\n",
            false,
        );
        let decoded = SourceRecord::decode("src/pay.rs", &record.encode()).unwrap();
        assert_eq!(decoded.relative, record.relative);
        assert_eq!(decoded.loc, record.loc);
        assert_eq!(decoded.definitions, record.definitions);
        assert_eq!(decoded.symbols, record.symbols);
        assert_eq!(decoded.imports, record.imports);
        assert_eq!(decoded.identifier_hashes, record.identifier_hashes);
        assert_eq!(decoded.terms, record.terms);
        assert_eq!(decoded.path_terms, record.path_terms);
        assert_eq!(decoded.symbol_terms, record.symbol_terms);
        assert!(!record.path_terms.is_empty() && !record.symbol_terms.is_empty());
        assert_eq!(decoded.entrypoint, record.entrypoint);
        let mut truncated = record.encode();
        truncated.pop();
        assert!(SourceRecord::decode("src/pay.rs", &truncated).is_none());
    }

    #[test]
    fn changed_files_are_relative_to_a_subdirectory_root() {
        let directory = tempfile::tempdir().unwrap();
        let repo = directory.path();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .output()
                .unwrap();
            assert!(
                status.status.success(),
                "{}",
                String::from_utf8_lossy(&status.stderr)
            );
        };
        git(&["init", "-q"]);
        fs::create_dir_all(repo.join("app/src")).unwrap();
        fs::write(repo.join("app/src/kept.rs"), "pub fn kept() {}\n").unwrap();
        fs::write(repo.join("app/src/edited.rs"), "pub fn edited() {}\n").unwrap();
        fs::write(repo.join("top.rs"), "pub fn top() {}\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        fs::write(repo.join("app/src/edited.rs"), "pub fn edited_again() {}\n").unwrap();
        fs::write(repo.join("app/src/new.rs"), "pub fn fresh() {}\n").unwrap();
        fs::write(repo.join("top.rs"), "pub fn top_changed() {}\n").unwrap();

        let root = repo.join("app");
        let walked = ["src/edited.rs", "src/kept.rs", "src/new.rs"].map(String::from);
        let changed = changed_paths(&root, &walked);
        assert_eq!(
            changed.into_iter().collect::<BTreeSet<_>>(),
            ["src/edited.rs", "src/new.rs"].map(String::from).into()
        );
    }

    #[test]
    fn source_cache_invalidates_after_a_file_change() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("lib.rs");
        fs::write(&path, "pub fn old_symbol() {}\n").unwrap();
        let first = repo_map(directory.path(), 12_000);
        assert!(first.output.contains("old_symbol"));

        fs::write(&path, "pub fn replacement_symbol_with_longer_name() {}\n").unwrap();
        let second = repo_map(directory.path(), 12_000);
        assert!(second
            .output
            .contains("replacement_symbol_with_longer_name"));
        assert!(!second.output.contains("old_symbol"));
    }
}
