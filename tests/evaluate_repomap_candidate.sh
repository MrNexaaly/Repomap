#!/usr/bin/env bash
set -euo pipefail

mode=${1:-fitness}
candidate=${2:-repomap_ranker.rs}
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)

case "$mode" in
  fitness)
    fixture="$script_dir/fixtures/repomap_ranker_fitness.rs"
    ;;
  heldout|held-out)
    fixture="$script_dir/fixtures/repomap_ranker_heldout.rs"
    ;;
  *)
    printf 'usage: %s fitness|heldout [candidate.rs]\n' "$0" >&2
    exit 2
    ;;
esac

if [[ ! -f "$candidate" ]]; then
  printf 'repomap evaluator: candidate is missing: %s\n' "$candidate" >&2
  exit 2
fi
candidate_path=$(realpath -- "$candidate")

temporary_source=$(mktemp -p "$PWD" --suffix=.rs .repomap-evaluator-XXXXXX)
temporary_binary=$(mktemp "${TMPDIR:-/tmp}/repomap-evaluator-XXXXXX")
cleanup() {
  rm -f -- "$temporary_source" "$temporary_binary"
}
trap cleanup EXIT

awk -v candidate="$candidate_path" '
  /^#\[path = "\.\.\/\.\.\/src\/repomap_ranker\.rs"\]$/ {
    print "#[path = \"" candidate "\"]"
    next
  }
  { print }
' "$fixture" > "$temporary_source"
rustc --crate-name repomap_candidate_evaluator --edition 2021 -Awarnings -O \
  "$temporary_source" -o "$temporary_binary"
"$temporary_binary"
