//! Orientation map: what a new agent reads first in an unfamiliar repository.
//!
//! The query map answers "which files matter for this task"; the overview
//! answers "what is this and where do things live": purpose from the README,
//! stack by lines of code, build and test commands from the manifests, a
//! layout of packages or directories with each one's stated purpose, entry
//! points, and the files the rest of the code depends on most. Everything is
//! derived from the tree at call time; nothing is configured per repository.

use crate::{
    bound_tool_output_to, failure,
    repo_map::{read_overview_records, render_record, resolve_import, PhaseTimer, SourceRecord},
    ToolResult,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::Path,
};

/// Manifests that make a directory a package. A Makefile does not: projects
/// such as the Linux kernel keep one in every directory, and treating each as
/// a package turned the layout into thousands of rows. Its targets are still
/// listed for the root (see `build_commands`).
const PACKAGE_MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "setup.py",
    "Package.swift",
    "build.gradle.kts",
    "build.gradle",
];
const README_NAMES: &[&str] = &[
    "README.md",
    "README",
    "readme.md",
    "Readme.md",
    "README.rst",
    "README.txt",
];
/// Files whose leading doc comment usually states a module's purpose.
const MODULE_ROOTS: &[&str] = &[
    "lib.rs",
    "main.rs",
    "mod.rs",
    "index.ts",
    "index.tsx",
    "index.js",
    "__init__.py",
    "main.go",
    "main.py",
    "app.py",
    "main.ts",
    "main.swift",
    "Main.kt",
];
const LAYOUT_ROWS: usize = 14;

fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

fn language(path: &str) -> &'static str {
    match path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "rs" => "Rust",
        "ts" | "tsx" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "py" => "Python",
        "go" => "Go",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "swift" => "Swift",
        "rb" => "Ruby",
        "php" => "PHP",
        "cs" => "C#",
        "c" | "h" => "C",
        "cc" | "cpp" | "hpp" => "C++",
        "sh" => "Shell",
        "html" => "HTML",
        "css" | "scss" => "CSS",
        "svelte" => "Svelte",
        "vue" => "Vue",
        _ => "other",
    }
}

fn thousands(value: usize) -> String {
    if value >= 10_000 {
        format!("{}k", value / 1000)
    } else if value >= 1000 {
        format!("{:.1}k", value as f64 / 1000.0)
    } else {
        value.to_string()
    }
}

fn test_like(path: &str) -> bool {
    let lowered = format!("/{}", path.to_ascii_lowercase());
    let name = lowered.rsplit('/').next().unwrap_or("");
    [
        "/tests/",
        "/test/",
        "/__tests__/",
        "/fixtures/",
        "/testdata/",
        "/benches/",
        "/examples/",
    ]
    .iter()
    .any(|part| lowered.contains(part))
        || name.starts_with("test_")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.ends_with("_test.go")
        || name.ends_with("_tests.rs")
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(directory, _)| directory)
}

fn join(directory: &str, name: &str) -> String {
    if directory.is_empty() {
        name.to_owned()
    } else {
        format!("{directory}/{name}")
    }
}

/// Strip inline markdown: links keep their text, emphasis and code marks go.
fn plain_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match (after.find("]("), after.find(')')) {
            (Some(close), Some(end)) if close < end => {
                out.push_str(&after[..close]);
                rest = &after[end + 1..];
            }
            _ => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.replace("**", "").replace('`', "").replace("__", "")
}

fn first_sentence(text: &str, limit: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut = collapsed
        .char_indices()
        .find(|(index, character)| {
            *character == '.' && collapsed[index + 1..].starts_with(' ') && *index > 12
        })
        .map_or(collapsed.as_str(), |(index, _)| &collapsed[..=index]);
    if cut.chars().count() <= limit {
        cut.to_owned()
    } else {
        let mut short: String = cut.chars().take(limit.saturating_sub(3)).collect();
        short.push_str("...");
        short
    }
}

/// The README's title and first prose paragraph.
fn readme_purpose(root: &Path) -> Option<(Option<String>, String)> {
    let text = README_NAMES
        .iter()
        .find_map(|name| fs::read_to_string(root.join(name)).ok())?;
    let mut title = None;
    let mut paragraph = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        if let Some(heading) = trimmed.strip_prefix("# ") {
            if title.is_none() && paragraph.is_empty() {
                title = Some(plain_markdown(heading).trim().to_owned());
            }
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        let decorative = trimmed.starts_with('#')
            || trimmed.starts_with("[![")
            || trimmed.starts_with("![")
            || trimmed.starts_with('<')
            || trimmed.starts_with('|')
            || trimmed.starts_with("---")
            || trimmed.starts_with("===");
        if trimmed.is_empty() || decorative {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        paragraph.push(plain_markdown(trimmed));
    }
    let body = paragraph.join(" ");
    (!body.is_empty() || title.is_some()).then(|| (title, first_sentence(&body, 360)))
}

/// A manifest's own one-line description, when it has one.
fn manifest_description(directory: &Path) -> Option<String> {
    if let Ok(text) = fs::read_to_string(directory.join("package.json")) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(description) = value.get("description").and_then(|v| v.as_str()) {
                if !description.trim().is_empty() {
                    return Some(first_sentence(description, 110));
                }
            }
        }
    }
    for name in ["Cargo.toml", "pyproject.toml"] {
        if let Ok(text) = fs::read_to_string(directory.join(name)) {
            for line in text.lines() {
                if let Some(value) = line.trim().strip_prefix("description") {
                    let value = value
                        .trim_start()
                        .trim_start_matches('=')
                        .trim()
                        .trim_matches('"');
                    if !value.is_empty() {
                        return Some(first_sentence(value, 110));
                    }
                }
            }
        }
    }
    None
}

/// The leading doc comment of a source file: `//!` (Rust), a module
/// docstring (Python), `// Package` (Go) or a leading comment block (C-like).
fn module_doc(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("#!"))
        .peekable();
    let mut doc = Vec::new();
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    if extension == "py" {
        while lines
            .peek()
            .is_some_and(|line| line.is_empty() || line.starts_with('#'))
        {
            lines.next();
        }
        let first = lines.next()?;
        let quote = ["\"\"\"", "'''"]
            .into_iter()
            .find(|quote| first.starts_with(quote))?;
        let rest = &first[3..];
        if let Some(end) = rest.find(quote) {
            doc.push(rest[..end].to_owned());
        } else {
            doc.push(rest.to_owned());
            for line in lines {
                if let Some(end) = line.find(quote) {
                    doc.push(line[..end].to_owned());
                    break;
                }
                if line.is_empty() && doc.iter().any(|part| !part.is_empty()) {
                    break;
                }
                doc.push(line.to_owned());
            }
        }
    } else {
        for line in lines {
            let content = line
                .strip_prefix("//!")
                .or_else(|| line.strip_prefix("///"))
                .or_else(|| line.strip_prefix("//"))
                .or_else(|| line.strip_prefix("/**"))
                .or_else(|| line.strip_prefix("/*"))
                .or_else(|| line.strip_prefix("*/").map(|_| ""))
                .or_else(|| line.strip_prefix('*'));
            match content {
                Some(content) => {
                    let content = content.trim().trim_end_matches("*/").trim();
                    if content.is_empty() && !doc.is_empty() {
                        break;
                    }
                    let noise = content.to_ascii_lowercase();
                    if !content.is_empty()
                        && !noise.starts_with("copyright")
                        && !noise.starts_with("spdx")
                        && !noise.starts_with("eslint")
                        && !noise.starts_with("@ts-")
                        && !noise.starts_with("prettier")
                    {
                        doc.push(content.to_owned());
                    }
                }
                None if line.is_empty() && doc.is_empty() => continue,
                None => break,
            }
        }
    }
    let joined = plain_markdown(&doc.join(" "));
    let joined = joined.trim();
    (joined.len() >= 8).then(|| first_sentence(joined, 110))
}

/// Named areas from a MAINTAINERS file in the Linux format (also used by
/// U-Boot, QEMU, Zephyr and others): each section's title and the directory
/// subtrees its `F:` patterns cover. It is the project's own map of what lives
/// where, so it names layout rows before any doc-comment guess.
struct Maintainers {
    /// Directory (relative to the MAINTAINERS file) -> titles of sections
    /// whose `F:` pattern covers that whole subtree, in file order.
    by_directory: HashMap<String, Vec<String>>,
    /// The mapped root's path relative to the MAINTAINERS file's directory
    /// ("" when they are the same), so a zoomed call still finds its areas.
    offset: String,
}

impl Maintainers {
    /// Looks in `root` and up to six ancestors, so `repomap linux/drivers/net`
    /// uses the tree's top-level MAINTAINERS.
    fn find(root: &Path) -> Option<Self> {
        let root = fs::canonicalize(root).ok()?;
        let owner = root
            .ancestors()
            .take(7)
            .find(|directory| directory.join("MAINTAINERS").is_file())?;
        let text = fs::read_to_string(owner.join("MAINTAINERS")).ok()?;
        let mut by_directory: HashMap<String, Vec<String>> = HashMap::new();
        let mut title: Option<String> = None;
        for line in text.lines() {
            if line.trim().is_empty() {
                title = None;
                continue;
            }
            let tagged = line.len() > 2
                && line.as_bytes()[1] == b':'
                && line.as_bytes()[0].is_ascii_uppercase();
            if !tagged {
                if title.is_none() {
                    title = Some(line.trim().to_owned());
                }
                continue;
            }
            let (Some(title), Some(pattern)) = (title.as_ref(), line.strip_prefix("F:")) else {
                continue;
            };
            // A subtree pattern: "fs/ext4/" or "fs/ext4/*". File patterns and
            // catch-alls ("*", "*/") name no single directory.
            let pattern = pattern.trim();
            let directory = pattern
                .strip_suffix("/*")
                .or_else(|| pattern.strip_suffix('/'))
                .filter(|directory| !directory.is_empty() && !directory.contains(['*', '?', '[']));
            if let Some(directory) = directory {
                let titles = by_directory.entry(directory.to_owned()).or_default();
                if !titles.contains(title) {
                    titles.push(title.clone());
                }
            }
        }
        if by_directory.is_empty() {
            return None;
        }
        let offset = root
            .strip_prefix(owner)
            .ok()?
            .to_string_lossy()
            .replace('\\', "/");
        Some(Self {
            by_directory,
            offset,
        })
    }

    fn full(&self, directory: &str) -> String {
        match (self.offset.is_empty(), directory.is_empty()) {
            (true, _) => directory.to_owned(),
            (false, true) => self.offset.clone(),
            (false, false) => format!("{}/{directory}", self.offset),
        }
    }

    /// The most specific section covering all of `directory`.
    fn covering(&self, directory: &str) -> Option<&str> {
        let mut current = self.full(directory);
        loop {
            if let Some(titles) = self.by_directory.get(&current) {
                return titles.first().map(String::as_str);
            }
            if current.is_empty() {
                return None;
            }
            current = parent(&current).to_owned();
        }
    }

    /// Titles of the named areas inside `directory` holding the most of its
    /// files, largest first.
    fn inside(&self, directory: &str, files: &[&SourceRecord], limit: usize) -> Vec<String> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for record in files {
            let mut current = self.full(parent(&record.relative));
            let stop = self.full(directory);
            while current.len() > stop.len() {
                if let Some(titles) = self.by_directory.get(&current) {
                    if let Some(title) = titles.first() {
                        *counts.entry(title.as_str()).or_default() += 1;
                    }
                    break;
                }
                current = parent(&current).to_owned();
            }
        }
        let mut ranked: Vec<(&str, usize)> = counts.into_iter().collect();
        ranked.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(right.0)));
        ranked
            .into_iter()
            .take(limit)
            .map(|(title, _)| title.to_owned())
            .collect()
    }
}

fn directory_purpose(root: &Path, directory: &str, files: &[&SourceRecord]) -> Option<String> {
    let absolute = root.join(directory);
    if let Some(description) = manifest_description(&absolute) {
        return Some(description);
    }
    // A module-root file directly in the directory or its src/, then any file
    // at the shallowest depth, in size order.
    let mut candidates: Vec<&SourceRecord> = files
        .iter()
        .copied()
        .filter(|record| {
            let name = record.relative.rsplit('/').next().unwrap_or("");
            let home = parent(&record.relative);
            MODULE_ROOTS.contains(&name) && (home == directory || home == join(directory, "src"))
        })
        .collect();
    candidates.sort_by_key(|record| record.relative.len());
    for record in candidates {
        if let Some(doc) = module_doc(&root.join(&record.relative)) {
            return Some(doc);
        }
    }
    let readme = README_NAMES
        .iter()
        .find_map(|name| fs::read_to_string(absolute.join(name)).ok());
    if let Some(text) = readme {
        let paragraph: Vec<&str> = text
            .lines()
            .map(str::trim)
            .skip_while(|line| {
                line.is_empty()
                    || line.starts_with('#')
                    || line.starts_with('<')
                    || line.starts_with("[!")
                    || line.starts_with("![")
                    || line.starts_with('|')
                    || line.starts_with("```")
            })
            .take_while(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with('|'))
            .collect();
        if !paragraph.is_empty() {
            return Some(first_sentence(&plain_markdown(&paragraph.join(" ")), 110));
        }
    }
    None
}

fn package_runner(directory: &Path) -> &'static str {
    if directory.join("pnpm-lock.yaml").exists() {
        "pnpm"
    } else if directory.join("yarn.lock").exists() {
        "yarn"
    } else if directory.join("bun.lockb").exists() || directory.join("bun.lock").exists() {
        "bun"
    } else {
        "npm"
    }
}

/// Build and test commands a manifest declares, in the directory it lives in.
fn build_commands(root: &Path, directory: &str) -> Vec<String> {
    let absolute = root.join(directory);
    let place = if directory.is_empty() {
        ".".to_owned()
    } else {
        format!("{directory}/")
    };
    let mut rows = Vec::new();
    if let Ok(text) = fs::read_to_string(absolute.join("Cargo.toml")) {
        let workspace = text.contains("[workspace]");
        let in_ancestor_workspace = !workspace
            && Path::new(directory).ancestors().skip(1).any(|ancestor| {
                fs::read_to_string(root.join(ancestor).join("Cargo.toml"))
                    .is_ok_and(|text| text.contains("[workspace]"))
            });
        if !in_ancestor_workspace {
            let kind = if workspace {
                "Rust workspace"
            } else {
                "Rust crate"
            };
            rows.push(format!("{place} ({kind}): `cargo build` · `cargo test`"));
        }
    }
    if let Ok(text) = fs::read_to_string(absolute.join("package.json")) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            let runner = package_runner(&absolute);
            let scripts = value.get("scripts").and_then(|v| v.as_object());
            let chosen: Vec<String> = [
                "dev",
                "start",
                "build",
                "test",
                "check",
                "typecheck",
                "lint",
            ]
            .iter()
            .filter(|name| scripts.is_some_and(|scripts| scripts.contains_key(**name)))
            .map(|name| format!("`{runner} run {name}`"))
            .collect();
            if !chosen.is_empty() {
                rows.push(format!(
                    "{place} (package.json, {runner}): {}",
                    chosen.join(" · ")
                ));
            }
        }
    }
    if absolute.join("go.mod").exists() {
        rows.push(format!(
            "{place} (Go module): `go build ./...` · `go test ./...`"
        ));
    }
    if absolute.join("pyproject.toml").exists() || absolute.join("setup.py").exists() {
        let runner = if absolute.join("uv.lock").exists() {
            "uv run "
        } else {
            ""
        };
        rows.push(format!("{place} (Python): `{runner}pytest`"));
    }
    if absolute.join("Package.swift").exists() {
        rows.push(format!(
            "{place} (Swift package): `swift build` · `swift test`"
        ));
    }
    if absolute.join("gradlew").exists() {
        rows.push(format!(
            "{place} (Gradle): `./gradlew build` · `./gradlew test`"
        ));
    }
    if let Ok(text) = fs::read_to_string(absolute.join("Makefile")) {
        // Unique public targets, conventional ones first. Internal targets
        // (a leading underscore) and pattern rules are not for a newcomer.
        const CONVENTIONAL: &[&str] = &[
            "help", "all", "build", "test", "check", "lint", "install", "clean",
        ];
        let mut seen = BTreeSet::new();
        let mut names: Vec<&str> = text
            .lines()
            .filter_map(|line| {
                let (name, _) = line.split_once(':')?;
                (!name.is_empty()
                    && !name.starts_with(['.', '\t', ' ', '#', '_'])
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                .then_some(name)
            })
            .filter(|name| seen.insert(*name))
            .collect();
        names.sort_by_key(|name| {
            CONVENTIONAL
                .iter()
                .position(|known| known == name)
                .unwrap_or(usize::MAX)
        });
        let targets: Vec<String> = names
            .into_iter()
            .take(6)
            .map(|name| format!("`make {name}`"))
            .collect();
        if !targets.is_empty() {
            rows.push(format!("{place} (Makefile): {}", targets.join(" · ")));
        }
    }
    rows
}

struct Group {
    directory: String,
    files: usize,
    loc: usize,
}

/// Rows for the layout: packages when the repository has several, otherwise
/// directories, split top-down until no single row dominates.
fn layout_groups(records: &[SourceRecord], package_roots: &BTreeSet<String>) -> Vec<Group> {
    let mut totals: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut add = |key: String, loc: usize| {
        let entry = totals.entry(key).or_default();
        entry.0 += 1;
        entry.1 += loc;
    };
    let nested_packages = package_roots.iter().filter(|dir| !dir.is_empty()).count();
    if nested_packages >= 2 && records.len() <= 3_000 {
        for record in records {
            // The deepest enclosing package: walk up the file's own
            // directories instead of testing every package root.
            let mut directory = parent(&record.relative);
            let owner = loop {
                if directory.is_empty() {
                    break record
                        .relative
                        .split('/')
                        .next()
                        .filter(|_| record.relative.contains('/'))
                        .unwrap_or("")
                        .to_owned();
                }
                if package_roots.contains(directory) {
                    break directory.to_owned();
                }
                directory = parent(directory);
            };
            add(owner, record.loc);
        }
    } else {
        // Directory mode: start at top-level entries, then repeatedly expand
        // the largest row holding over a quarter of the code into its biggest
        // subdirectories, as many as the row budget allows. The parent row
        // keeps whatever was not expanded.
        let top = |relative: &str| {
            relative
                .split('/')
                .next()
                .filter(|_| relative.contains('/'))
                .unwrap_or("")
                .to_owned()
        };
        let mut prefixes: BTreeSet<String> =
            records.iter().map(|record| top(&record.relative)).collect();
        let total_loc: usize = records
            .iter()
            .map(|record| record.loc)
            .sum::<usize>()
            .max(1);
        let owner_of = |prefixes: &BTreeSet<String>, relative: &str| {
            prefixes
                .iter()
                .filter(|prefix| prefix.is_empty() || relative.starts_with(&format!("{prefix}/")))
                .max_by_key(|prefix| prefix.len())
                .cloned()
                .unwrap_or_default()
        };
        // Preserve all top-level areas while reserving room to zoom into
        // dominant ones. A wide root must not disable subdivision entirely.
        let row_limit = LAYOUT_ROWS.max(prefixes.len() + 8);
        while prefixes.len() < row_limit {
            let mut sizes: BTreeMap<String, usize> = BTreeMap::new();
            for record in records {
                *sizes
                    .entry(owner_of(&prefixes, &record.relative))
                    .or_default() += record.loc;
            }
            let mut expansion = None;
            let mut rows: Vec<(&String, &usize)> = sizes
                .iter()
                .filter(|(prefix, _)| !prefix.is_empty())
                .collect();
            rows.sort_by(|left, right| right.1.cmp(left.1));
            for (prefix, loc) in rows {
                if *loc * 4 < total_loc {
                    break;
                }
                let mut children: BTreeMap<String, usize> = BTreeMap::new();
                for record in records {
                    if owner_of(&prefixes, &record.relative) != *prefix {
                        continue;
                    }
                    if let Some((child, _)) = record
                        .relative
                        .strip_prefix(&format!("{prefix}/"))
                        .and_then(|rest| rest.split_once('/'))
                    {
                        *children.entry(format!("{prefix}/{child}")).or_default() += record.loc;
                    }
                }
                if !children.is_empty() {
                    expansion = Some(children);
                    break;
                }
            }
            let Some(children) = expansion else {
                break;
            };
            let mut children: Vec<(String, usize)> = children.into_iter().collect();
            children.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
            let slots = (row_limit - prefixes.len()).max(1);
            prefixes.extend(children.into_iter().take(slots).map(|(child, _)| child));
        }
        for record in records {
            add(owner_of(&prefixes, &record.relative), record.loc);
        }
    }
    let mut groups: Vec<Group> = totals
        .into_iter()
        .map(|(directory, (files, loc))| Group {
            directory,
            files,
            loc,
        })
        .collect();
    groups.sort_by(|left, right| {
        right
            .loc
            .cmp(&left.loc)
            .then(left.directory.cmp(&right.directory))
    });
    groups
}

fn entry_point(relative: &str, package_roots: &BTreeSet<String>) -> bool {
    let name = relative.rsplit('/').next().unwrap_or("");
    if matches!(language(relative), "CSS" | "HTML" | "other") || name.ends_with(".d.ts") {
        return false;
    }
    let home = parent(relative);
    let at_package = package_roots.contains(home)
        || (home == "src" || home.ends_with("/src")) && package_roots.contains(parent(home));
    let stem = name.split('.').next().unwrap_or("");
    (name == "main.rs" || home.ends_with("src/bin"))
        || name == "main.go"
        || matches!(name, "main.c" | "main.cc" | "main.cpp")
        || name == "__main__.py"
        || name == "manage.py"
        || (at_package && ["main", "cli", "server", "app", "index"].contains(&stem))
}

/// Languages whose files can call into each other by name.
fn language_family(path: &str) -> &'static str {
    match language(path) {
        "TypeScript" | "JavaScript" | "Svelte" | "Vue" => "web",
        "C" | "C++" => "c",
        "Java" | "Kotlin" => "jvm",
        other => other,
    }
}

const TYPE_KINDS: &[&str] = &[
    "struct",
    "union",
    "enum",
    "trait",
    "type",
    "typedef",
    "class",
    "interface",
    "protocol",
];

/// Files other files depend on most: distinct files that import them, or that
/// name a type (struct, class, trait...) they uniquely define in the same
/// language family. Plain function names are left out on purpose: `decode`
/// or `unwrap` defined in one file would otherwise collect every caller of an
/// unrelated method with the same name, across languages.
fn central_files(records: &[SourceRecord]) -> Vec<(usize, usize)> {
    let paths: HashSet<String> = records
        .iter()
        .map(|record| record.relative.clone())
        .collect();
    let index_of: HashMap<&str, usize> = records
        .iter()
        .enumerate()
        .map(|(index, record)| (record.relative.as_str(), index))
        .collect();
    let mut definers: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut types: HashSet<(usize, &str)> = HashSet::new();
    // Without references (a summary of a huge tree) only imports count.
    let referenced = records.iter().any(|record| !record.references.is_empty());
    for (index, record) in records.iter().enumerate().filter(|_| referenced) {
        for symbol in &record.symbols {
            definers.entry(symbol.as_str()).or_default().push(index);
        }
        for definition in &record.definitions {
            if let Some((kind, name)) = definition.split_once(' ') {
                if TYPE_KINDS.contains(&kind) {
                    types.insert((index, name));
                }
            }
        }
    }
    let mut users: Vec<HashSet<usize>> = vec![HashSet::new(); records.len()];
    for (index, record) in records.iter().enumerate() {
        let family = language_family(&record.relative);
        for symbol in &record.references {
            if let Some([owner]) = definers.get(symbol.as_str()).map(Vec::as_slice) {
                if *owner != index
                    && types.contains(&(*owner, symbol.as_str()))
                    && language_family(&records[*owner].relative) == family
                {
                    users[*owner].insert(index);
                }
            }
        }
        for import in &record.imports {
            let target = resolve_import(record, import, &paths);
            if let Some(&owner) = index_of.get(target.as_str()) {
                if owner != index {
                    users[owner].insert(index);
                }
            }
        }
    }
    let mut ranked: Vec<(usize, usize)> = users
        .iter()
        .enumerate()
        .filter(|(index, set)| set.len() >= 2 && !test_like(&records[*index].relative))
        .map(|(index, set)| (index, set.len()))
        .collect();
    ranked.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then(records[left.0].relative.cmp(&records[right.0].relative))
    });
    ranked
}

const FUNCTION_KINDS: &[&str] = &["fn", "function", "def", "func", "fun"];

/// How much code a file defines: 0 for none (data tables, constant or enum
/// headers), 1 for a few functions, 2 for eight or more.
fn substantive(record: &SourceRecord) -> u8 {
    let functions = record
        .definitions
        .iter()
        .filter(|definition| {
            definition
                .split_once(' ')
                .is_some_and(|(kind, _)| FUNCTION_KINDS.contains(&kind))
        })
        .count();
    match functions {
        0 => 0,
        1..=7 => 1,
        _ => 2,
    }
}

/// Every source file, grouped by directory, largest first within each
/// directory and at most `cap` per directory: "src/imap/ (45, .rs): session
/// selected wire lex +41". Names are listed without their directory, and
/// without the extension when a directory holds one kind, so the whole index
/// costs a few tokens per file.
fn file_index(records: &[SourceRecord], cap: usize) -> String {
    let mut directories: BTreeMap<&str, Vec<&SourceRecord>> = BTreeMap::new();
    for record in records {
        directories
            .entry(parent(&record.relative))
            .or_default()
            .push(record);
    }
    let mut rows: Vec<(&str, Vec<&SourceRecord>)> = directories.into_iter().collect();
    rows.sort_by(|left, right| {
        let size =
            |files: &Vec<&SourceRecord>| files.iter().map(|record| record.loc).sum::<usize>();
        size(&right.1).cmp(&size(&left.1)).then(left.0.cmp(right.0))
    });
    let mut out = String::new();
    for (directory, mut files) in rows {
        files.sort_by(|left, right| {
            right
                .loc
                .cmp(&left.loc)
                .then(left.relative.cmp(&right.relative))
        });
        let extensions: BTreeSet<&str> = files
            .iter()
            .map(|record| {
                record
                    .relative
                    .rsplit_once('.')
                    .map_or("", |(_, extension)| extension)
            })
            .collect();
        let shared = (extensions.len() == 1).then(|| *extensions.iter().next().unwrap_or(&""));
        let names: Vec<&str> = files
            .iter()
            .take(cap)
            .map(|record| {
                let name = record
                    .relative
                    .rsplit('/')
                    .next()
                    .unwrap_or(&record.relative);
                match shared {
                    Some(extension) if !extension.is_empty() => {
                        name.strip_suffix(&format!(".{extension}")).unwrap_or(name)
                    }
                    _ => name,
                }
            })
            .collect();
        let label = if directory.is_empty() {
            ".".to_owned()
        } else {
            format!("{directory}/")
        };
        let kind = shared
            .filter(|extension| !extension.is_empty())
            .map_or(String::new(), |extension| format!(", .{extension}"));
        out.push_str(&format!(
            "{label} ({}{kind}): {}",
            files.len(),
            names.join(" ")
        ));
        if files.len() > cap {
            out.push_str(&format!(" +{}", files.len() - cap));
        }
        out.push('\n');
    }
    out
}

/// Adds whole rows until the next one would exceed the budget.
struct Budget {
    output: String,
    remaining: usize,
}

impl Budget {
    fn push(&mut self, text: &str) -> bool {
        let cost = estimate_tokens(text);
        if cost > self.remaining {
            return false;
        }
        self.output.push_str(text);
        self.remaining -= cost;
        true
    }
}

pub fn overview(root: &Path, token_budget: usize, max_chars: usize) -> ToolResult {
    let mut timer = PhaseTimer::start();
    let (records, _) = read_overview_records(root, &mut timer);
    if records.is_empty() {
        return failure("repomap: no source files found");
    }
    let maintainers = Maintainers::find(root);
    let name = fs::canonicalize(root)
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| root.display().to_string());
    let total_loc: usize = records.iter().map(|record| record.loc).sum();

    // Package roots: the root plus every directory above a source file that
    // holds a manifest.
    let mut directories: BTreeSet<String> = BTreeSet::new();
    for record in &records {
        let mut directory = parent(&record.relative);
        loop {
            if !directories.insert(directory.to_owned()) {
                break;
            }
            if directory.is_empty() {
                break;
            }
            directory = parent(directory);
        }
    }
    let package_roots: BTreeSet<String> = directories
        .into_iter()
        .filter(|directory| {
            PACKAGE_MANIFESTS
                .iter()
                .any(|manifest| root.join(directory).join(manifest).is_file())
        })
        .collect();

    let mut budget = Budget {
        output: String::new(),
        remaining: token_budget,
    };
    budget.push(&format!(
        "# {name}: repository overview ({} source files, {} lines)\n\
         Generated by repomap for an agent starting work here. Sections: purpose, stack, build and test, \
         layout, entry points, central files. For a specific task, run repomap again with a query to rank \
         the files for that task.\n",
        records.len(),
        thousands(total_loc)
    ));

    let mut purpose = String::from("\n## Purpose\n");
    match readme_purpose(root) {
        Some((title, body)) => {
            if let Some(title) = title {
                purpose.push_str(&format!("{title}: "));
            }
            purpose.push_str(if body.is_empty() {
                "(README has no summary paragraph)"
            } else {
                &body
            });
            purpose.push('\n');
        }
        None => purpose.push_str("(no README)\n"),
    }
    budget.push(&purpose);

    let mut languages: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for record in &records {
        let entry = languages.entry(language(&record.relative)).or_default();
        entry.0 += 1;
        entry.1 += record.loc;
    }
    let mut languages: Vec<_> = languages.into_iter().collect();
    languages.sort_by(|left, right| right.1 .1.cmp(&left.1 .1));
    let stack = languages
        .iter()
        .take(6)
        .map(|(language, (files, loc))| {
            format!(
                "{language} {}% ({files} files)",
                loc * 100 / total_loc.max(1)
            )
        })
        .collect::<Vec<_>>()
        .join(" · ");
    budget.push(&format!("\n## Stack\n{stack}\n"));

    let commands: Vec<String> = std::iter::once(String::new())
        .chain(
            package_roots
                .iter()
                .filter(|directory| !directory.is_empty())
                .cloned(),
        )
        .flat_map(|directory| build_commands(root, &directory))
        .collect();
    if !commands.is_empty() && budget.push("\n## Build and test\n") {
        for command in commands.iter().take(8) {
            if !budget.push(&format!("- {command}\n")) {
                break;
            }
        }
        if commands.len() > 8 {
            budget.push(&format!("- ... {} more manifests\n", commands.len() - 8));
        }
    }

    timer.mark("ov-head");
    let groups = layout_groups(&records, &package_roots);
    timer.mark("ov-groups");
    if budget.push("\n## Layout (directory | files | lines | purpose | largest files)\n") {
        for group in &groups {
            let files: Vec<&SourceRecord> = records
                .iter()
                .filter(|record| {
                    group.directory.is_empty() && !record.relative.contains('/')
                        || record
                            .relative
                            .starts_with(&format!("{}/", group.directory))
                            && !groups.iter().any(|child| {
                                child.directory.len() > group.directory.len()
                                    && child
                                        .directory
                                        .starts_with(&format!("{}/", group.directory))
                                    && record
                                        .relative
                                        .starts_with(&format!("{}/", child.directory))
                            })
                })
                .collect();
            let named = maintainers.as_ref().and_then(|map| {
                map.covering(&group.directory)
                    .map(str::to_owned)
                    .or_else(|| {
                        let inside = map.inside(&group.directory, &files, 4);
                        (!inside.is_empty()).then(|| format!("areas: {}", inside.join(", ")))
                    })
            });
            let purpose = named
                .or_else(|| directory_purpose(root, &group.directory, &files))
                .unwrap_or_default();
            let label = if group.directory.is_empty() {
                "(top level)".to_owned()
            } else {
                format!("{}/", group.directory)
            };
            // The biggest files that define the most code, as paths relative
            // to the row, so an agent has concrete names to reach for.
            // Generated tables (a register header of 20,000 `#define`s, an
            // enum header) come last.
            let mut largest = files.clone();
            largest.sort_by(|left, right| {
                substantive(right)
                    .cmp(&substantive(left))
                    .then(right.loc.cmp(&left.loc))
                    .then(left.relative.cmp(&right.relative))
            });
            let examples = largest
                .iter()
                .take(4)
                .map(|record| {
                    record
                        .relative
                        .strip_prefix(&format!("{}/", group.directory))
                        .unwrap_or(&record.relative)
                        .to_owned()
                })
                .collect::<Vec<_>>()
                .join(", ");
            let row = format!(
                "{label} | {} | {} | {purpose}{}e.g. {examples}\n",
                group.files,
                thousands(group.loc),
                if purpose.is_empty() { "" } else { " | " }
            );
            if !budget.push(&row) {
                break;
            }
        }
    }

    let mut entries: Vec<&SourceRecord> = records
        .iter()
        .filter(|record| {
            !test_like(&record.relative) && entry_point(&record.relative, &package_roots)
        })
        .collect();
    entries.sort_by_key(|record| {
        (
            record.relative.matches('/').count(),
            std::cmp::Reverse(record.loc),
        )
    });
    if !entries.is_empty() && budget.push("\n## Entry points\n") {
        for record in entries.iter().take(10) {
            let defs = record
                .definitions
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ");
            let row = format!(
                "{} | {} LOC{}\n",
                record.relative,
                record.loc,
                if defs.is_empty() {
                    String::new()
                } else {
                    format!(" | {defs}")
                }
            );
            if !budget.push(&row) {
                break;
            }
        }
    }

    timer.mark("ov-layout");
    let central = central_files(&records);
    timer.mark("ov-central");
    if !central.is_empty() && budget.push("\n## Central files (used by the most other files)\n") {
        for (index, users) in central.iter().take(8) {
            let section = render_record(&records[*index], false);
            let mut lines = section.lines();
            let header = lines.next().unwrap_or("");
            let mut row = format!("{header} | used by {users} files");
            if let Some(defs) = lines.find(|line| line.trim_start().starts_with("defs:")) {
                let names: Vec<&str> = defs
                    .trim_start()
                    .trim_start_matches("defs: ")
                    .split("; ")
                    .filter(|definition| {
                        TYPE_KINDS
                            .iter()
                            .any(|kind| definition.starts_with(&format!("{kind} ")))
                    })
                    .take(6)
                    .collect();
                if !names.is_empty() {
                    row.push_str(&format!(" | {}", names.join("; ")));
                }
            }
            row.push('\n');
            if !budget.push(&row) {
                break;
            }
        }
    }
    let next = format!(
        "\nNext: repomap {} --query \"<the task>\" ranks the files for that task; repomap {}/<directory> maps one area in more detail.\n",
        root.display(),
        root.display()
    );
    let header = "\n## Files (directory (count, extension): names, largest first; +N not shown)\n";
    let available = budget
        .remaining
        .saturating_sub(estimate_tokens(&next) + estimate_tokens(header));
    let most = records.len();
    if available > 40 {
        let (mut low, mut high) = (1usize, most);
        while low < high {
            let middle = (low + high).div_ceil(2);
            if estimate_tokens(&file_index(&records, middle)) <= available {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        let mut index = file_index(&records, low);
        if estimate_tokens(&index) > available {
            // Even one name per directory does not fit: keep whole rows only.
            let mut kept = String::new();
            for line in index.lines() {
                if estimate_tokens(&kept) + estimate_tokens(line) + 1 > available {
                    break;
                }
                kept.push_str(line);
                kept.push('\n');
            }
            index = kept;
        }
        if !index.is_empty() {
            budget.push(header);
            budget.push(&index);
        }
    }
    budget.push(&next);
    timer.mark("overview");
    let mut result = bound_tool_output_to(&budget.output, max_chars.max(1));
    result.exit_code = Some(0);
    result
}
