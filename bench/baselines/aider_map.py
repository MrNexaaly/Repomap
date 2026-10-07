#!/home/zero/.cache/repomap-bench/aider-venv/bin/python
"""Aider 0.86.2's personalized repository ranking, without a chat or model.

Usage: ~/.cache/repomap-bench/aider-venv/bin/python -B aider_map.py DIR QUERY [BUDGET]
For repeatable upstream set iteration, prefix the interpreter with
``env PYTHONHASHSEED=0`` (also works inside bench/run.py's --cmd).

BUDGET is accepted for the paths adapter protocol (default 2048), but does
not truncate the ranking: this exposes up to 50 files, not a rendered,
token-budgeted map. get_ranked_tags does not use a model or token counter.
Only Aider's parser, tag cache and graph ranker run; no LLM/network calls.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import re
import sys
from collections import defaultdict
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

# Avoid creating bytecode caches when imported for verification, too.
sys.dont_write_bytecode = True

# Exactly bench/mine.py's SOURCE_EXTENSIONS; manifests/READMEs are not source.
SOURCE_EXTENSIONS = {
    "rs", "ts", "tsx", "py", "js", "jsx", "mjs", "cjs", "go", "java", "kt", "kts", "swift", "rb",
    "php", "cs", "c", "h", "cc", "cpp", "hpp", "sh", "html", "css", "scss", "svelte", "vue",
}


def source_files(root: Path) -> list[str]:
    files = []
    for directory, subdirectories, names in os.walk(root):
        subdirectories[:] = sorted(
            name for name in subdirectories
            if not name.startswith(".") and name not in ("target", "node_modules")
        )
        for name in sorted(names):
            path = Path(directory) / name
            if path.suffix.lstrip(".").lower() in SOURCE_EXTENSIONS and path.is_file():
                files.append(str(path))
    return files


def query_mentions(query: str, relative_files: list[str]) -> tuple[set[str], set[str]]:
    """Replicate base_coder.Coder's mention methods with an empty chat.

    Calling Coder would import chat/model infrastructure. These are its
    get_cur_message_text, get_ident_mentions, get_file_mentions and
    get_ident_filename_matches operations, including case/punctuation rules.
    With no chat/read-only files, all source files are addable and there are
    no existing basenames to exclude.
    """
    content = query + "\n"  # get_cur_message_text appends a newline per message.
    mentioned_idents = set(re.split(r"\W+", content))

    words = set(content.split())
    words = {word.rstrip(",.!;:?") for word in words}
    words = {word.strip("\"'`*_") for word in words}
    normalized_words = {word.replace("\\", "/") for word in words}

    mentioned_fnames = set()
    fname_to_rel_fnames = defaultdict(list)
    for rel_fname in relative_files:
        if rel_fname.replace("\\", "/") in normalized_words:
            mentioned_fnames.add(rel_fname)
        fname = os.path.basename(rel_fname)
        if "/" in fname or "\\" in fname or "." in fname or "_" in fname or "-" in fname:
            fname_to_rel_fnames[fname].append(rel_fname)
    for fname, rel_fnames in fname_to_rel_fnames.items():
        if len(rel_fnames) == 1 and fname in words:
            mentioned_fnames.add(rel_fnames[0])

    all_fnames = defaultdict(set)
    for fname in relative_files:
        if not fname or fname == ".":
            continue
        try:
            base = Path(fname).stem.lower()
            if len(base) >= 5:
                all_fnames[base].add(fname)
        except ValueError:
            continue
    for ident in mentioned_idents:
        if len(ident) >= 5:
            mentioned_fnames.update(all_fnames[ident.lower()])

    return mentioned_fnames, mentioned_idents


class QuietIO:
    """Only RepoMap's IO surface, with Aider's default strict UTF-8 reads."""

    def read_text(self, filename):
        try:
            with open(filename, encoding="utf-8") as source:
                return source.read()
        except (OSError, UnicodeError):
            return None

    def tool_output(self, *args, **kwargs):
        pass

    tool_warning = tool_output
    tool_error = tool_output


def ranked_paths(root: Path, query: str, budget: int) -> list[str]:
    other_fnames = source_files(root)
    if not other_fnames:
        return []  # Aider's ranker divides by the number of input files.
    relative_files = [Path(fname).relative_to(root).as_posix() for fname in other_fnames]
    mentioned_fnames, mentioned_idents = query_mentions(query, relative_files)

    # Suppress upstream prints (e.g. unsupported parsers) and tqdm scan bars.
    # Exceptions still propagate after these stream contexts are restored.
    with open(os.devnull, "w") as quiet, redirect_stdout(quiet), redirect_stderr(quiet):
        from aider import __version__
        from aider.repomap import RepoMap

        if __version__ != "0.86.2":
            raise RuntimeError(f"Expected aider-chat 0.86.2, found {__version__}")

        class ExternalCacheRepoMap(RepoMap):
            def __init__(self, **kwargs):
                # load_tags_cache/tags_cache_error join root / TAGS_CACHE_DIR;
                # an absolute value redirects both paths, including recovery.
                # Keep the real source root for relative paths/personalization.
                key = hashlib.sha256(os.fsencode(root)).hexdigest()
                self.TAGS_CACHE_DIR = str(
                    Path.home() / ".cache" / "repomap-bench" / "aider-map"
                    / key / RepoMap.TAGS_CACHE_DIR
                )
                super().__init__(**kwargs)

        repo_map = ExternalCacheRepoMap(root=str(root), map_tokens=budget, io=QuietIO())
        try:
            # This is exactly the ranking used before Aider packs its map.
            # First file appearance preserves rank; to_tree sorts tags by
            # filename, so rendered order would be alphabetical, not ranked.
            # get_ranked_tags already appends (fname,) entries without tags:
            # first by graph rank, then its unscored set iteration order.
            # Preserve that entire tail rather than inventing another order.
            tags = repo_map.get_ranked_tags(
                chat_fnames=set(),
                other_fnames=other_fnames,
                mentioned_fnames=mentioned_fnames,
                mentioned_idents=mentioned_idents,
            )
        finally:
            if hasattr(repo_map.TAGS_CACHE, "close"):
                repo_map.TAGS_CACHE.close()

    return list(dict.fromkeys(tag[0].replace("\\", "/") for tag in tags))[:50]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("dir", type=Path)
    parser.add_argument("query")
    parser.add_argument("budget", type=int, nargs="?", default=2048)
    args = parser.parse_args()
    root = args.dir.expanduser().resolve()
    if not root.is_dir():
        parser.error(f"not a directory: {root}")
    for path in ranked_paths(root, args.query, args.budget):
        print(path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
