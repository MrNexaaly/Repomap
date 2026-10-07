#!/usr/bin/env python3
"""Simulate an agent phrasing its repomap query: expand each task, blind.

A live agent calling repomap_query writes the query itself and can use the
words code tends to use. This asks one model (codex, read-only, from an empty
directory, told to run no commands) to rewrite every task as a code-search
query WITHOUT seeing the repository: keep the task's words, add likely
identifier and file-name words and synonyms. The output is a case file with
`query` replaced (the original kept as `task`), so bench/run.py can compare
original vs agent-phrased queries for any mapper.

Usage: expand.py CASES.jsonl OUT.jsonl
"""

import json
import subprocess
import sys
import tempfile
from pathlib import Path

SCHEMA = {
    "type": "object",
    "additionalProperties": False,
    "required": ["queries"],
    "properties": {
        "queries": {
            "type": "array",
            "items": {
                "type": "object",
                "additionalProperties": False,
                "required": ["id", "query"],
                "properties": {"id": {"type": "string"}, "query": {"type": "string"}},
            },
        }
    },
}

PROMPT = """You are an AI coding agent about to work on each task below in a codebase you
have not seen. Before opening anything you call a code-search tool that ranks
files by how well their paths, definitions and text match your query.

For each task write that query: keep the task's own words, then add the words
the code most likely uses for it: probable identifiers (split into words),
module or file-name words, and close synonyms. At most 20 words, no
punctuation beyond spaces. Do not run any commands or read any files; you
cannot see the repository.

Tasks (JSON):
{tasks}

Your final message must be the JSON object only: {{"queries": [{{"id": ..., "query": ...}}]}},
one entry per task id.
"""


def main() -> int:
    source, target = Path(sys.argv[1]), Path(sys.argv[2])
    cases = [json.loads(line) for line in source.read_text().splitlines() if line.strip()]
    work = Path(tempfile.mkdtemp(prefix="expand-", dir=str(Path.home() / ".cache" / "repomap-bench")))
    (work / "schema.json").write_text(json.dumps(SCHEMA))
    prompt = PROMPT.format(tasks=json.dumps([{"id": c["id"], "task": c["query"]} for c in cases], indent=1))
    with open(work / "events.jsonl", "w") as sink:
        subprocess.run(
            ["codex", "exec", "--skip-git-repo-check", "--json", "-s", "read-only", "-C", str(work),
             "--output-schema", str(work / "schema.json"), "-o", str(work / "answer.json"), prompt],
            stdin=subprocess.DEVNULL, stdout=sink, stderr=subprocess.STDOUT, timeout=3600,
        )
    commands = sum('"command_execution"' in line for line in open(work / "events.jsonl"))
    expanded = {item["id"]: item["query"] for item in json.loads((work / "answer.json").read_text())["queries"]}
    missing = [c["id"] for c in cases if c["id"] not in expanded]
    if missing or commands:
        sys.exit(f"expansion invalid: {len(missing)} ids missing, {commands} commands run (see {work})")
    with open(target, "w") as sink:
        for case in cases:
            sink.write(json.dumps({**case, "task": case["query"], "query": expanded[case["id"]]}) + "\n")
    print(f"wrote {len(cases)} agent-phrased cases to {target} (0 commands run; log {work})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
