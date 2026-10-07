//! Source-file discovery.
//!
//! Uses ripgrep's `ignore` walker, so `.gitignore`, `.ignore`, git's exclude
//! files and the global gitignore apply exactly as they do for `rg`, even in a
//! tree without a `.git` directory. Hidden directories are skipped (that alone
//! drops `.git`, `.svelte-kit`, `.cxx`, `.gradle`, `.venv`, agent state).
//! Beyond ignore files, a directory is skipped when it declares itself build
//! output or a cache, so nothing here needs a list of every tool's folder name.

use ignore::{WalkBuilder, WalkState};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

pub(crate) const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "py", "js", "jsx", "mjs", "cjs", "go", "java", "kt", "kts", "swift", "rb",
    "php", "cs", "c", "h", "cc", "cpp", "hpp", "sh", "html", "css", "scss", "svelte", "vue",
];

/// Files whose presence marks their directory as generated: the Cache Directory
/// Tagging spec (cargo `target/`, many caches), CMake build trees, and Python
/// virtual environments.
const GENERATED_MARKERS: &[&str] = &["CACHEDIR.TAG", "CMakeCache.txt", "pyvenv.cfg"];

/// Third-party dependency trees. They are normally ignored; these names are
/// skipped even when a repository forgets to.
const DEPENDENCY_DIRECTORIES: &[&str] = &["node_modules", "bower_components", "__pycache__"];

/// Per-repository extra ignore file, same syntax as `.gitignore`.
pub(crate) const IGNORE_FILE: &str = ".repomapignore";

/// Larger files are generated or vendored in practice (amalgamations, bundles)
/// and cost more to parse than any map could use.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

pub(crate) fn is_source(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            SOURCE_EXTENSIONS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
}

fn keep_directory(path: &Path) -> bool {
    let name = path.file_name().and_then(|value| value.to_str()).unwrap_or("");
    !DEPENDENCY_DIRECTORIES.contains(&name)
        && !GENERATED_MARKERS
            .iter()
            .any(|marker| path.join(marker).exists())
}

/// Every mappable source file under `root`, sorted.
#[cfg(test)]
pub(crate) fn source_files(root: &Path) -> Vec<PathBuf> {
    source_files_with_metadata(root)
        .into_iter()
        .map(|(path, _)| path)
        .collect()
}

/// A file's cache fingerprint: modification time (nanoseconds) and size.
pub(crate) type Fingerprint = Option<(u64, u64)>;

/// Every mappable source file under `root` with its fingerprint, sorted. The
/// walker's threads take each file's metadata while they visit it, so the
/// caller needs no second, sequential stat pass.
pub(crate) fn source_files_with_metadata(root: &Path) -> Vec<(PathBuf, Fingerprint)> {
    let threads = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(4)
        .min(8);
    let mut builder = WalkBuilder::new(root);
    builder
        .require_git(false)
        .add_custom_ignore_filename(IGNORE_FILE)
        .max_filesize(Some(MAX_FILE_BYTES))
        .threads(threads)
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry.file_type().is_some_and(|kind| kind.is_dir())
                || keep_directory(entry.path())
        });

    let found = Mutex::new(Vec::new());
    builder.build_parallel().run(|| {
        let mut batch = Batch {
            found: &found,
            local: Vec::new(),
        };
        Box::new(move |entry| {
            if let Ok(entry) = entry {
                if entry.file_type().is_some_and(|kind| kind.is_file()) && is_source(entry.path()) {
                    let fingerprint = entry.metadata().ok().map(|metadata| {
                        (
                            metadata.modified().map(crate::cache::nanos).unwrap_or(0),
                            metadata.len(),
                        )
                    });
                    batch.local.push((entry.into_path(), fingerprint));
                    if batch.local.len() >= 256 {
                        batch.flush();
                    }
                }
            }
            WalkState::Continue
        })
    });
    let mut files = found
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    files.sort();
    files
}

/// A walker thread's pending paths. The walker drops each visitor when its
/// thread finishes, so the final partial batch is flushed on drop.
struct Batch<'a> {
    found: &'a Mutex<Vec<(PathBuf, Fingerprint)>>,
    local: Vec<(PathBuf, Fingerprint)>,
}

impl Batch<'_> {
    fn flush(&mut self) {
        self.found
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .append(&mut self.local);
    }
}

impl Drop for Batch<'_> {
    fn drop(&mut self) {
        self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &Path, relative: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "fn f() {}\n").unwrap();
    }

    fn listed(root: &Path) -> Vec<String> {
        source_files(root)
            .iter()
            .map(|path| path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"))
            .collect()
    }

    #[test]
    fn ignore_files_apply_without_a_git_directory() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::write(root.join(".gitignore"), "generated/\n*.gen.ts\n").unwrap();
        fs::write(root.join(IGNORE_FILE), "fixtures/huge/\n").unwrap();
        for file in [
            "src/main.rs",
            "site/src/app.ts",
            "build/tool.rs",
            "generated/api.rs",
            "src/schema.gen.ts",
            "fixtures/huge/blob.rs",
            "fixtures/small.rs",
        ] {
            write(root, file);
        }
        assert_eq!(
            listed(root),
            ["build/tool.rs", "fixtures/small.rs", "site/src/app.ts", "src/main.rs"]
        );
    }

    #[test]
    fn self_declared_build_output_and_dependencies_are_skipped() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        for file in [
            "src/lib.rs",
            "target/debug/build/out.rs",
            "app/.cxx/Debug/x86_64/_deps/llama/convert.py",
            "cmake-out/gen.c",
            "env/lib/site.py",
            "web/node_modules/pkg/index.js",
            ".svelte-kit/output/server.js",
            "src/notes.md",
        ] {
            write(root, file);
        }
        fs::write(root.join("target/CACHEDIR.TAG"), "Signature: 8a477f597d28d172789f06886806bc55").unwrap();
        fs::write(root.join("cmake-out/CMakeCache.txt"), "").unwrap();
        fs::write(root.join("env/pyvenv.cfg"), "home = /usr/bin").unwrap();
        assert_eq!(listed(root), ["src/lib.rs"]);
    }
}
