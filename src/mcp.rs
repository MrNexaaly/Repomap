//! `repomap mcp`: the maps as Model Context Protocol tools over stdio, so
//! every agent (Claude, codex, Nexus, anything that speaks MCP) calls the same
//! implementation. Newline-delimited JSON-RPC 2.0, per the MCP stdio transport.

use crate::{overview, repo_map_with_detail, repomap_ranker::RankContext, Detail, ToolResult};
use serde_json::{json, Value};
use std::{
    io::{self, BufRead, Write},
    path::PathBuf,
};

/// Dual-era: modern revisions carry the version in every request's `_meta`
/// and are served statelessly; legacy revisions open with `initialize`.
const MODERN_VERSIONS: &[&str] = &["2026-07-28"];
const LEGACY_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;
const MAX_CHARS: usize = 60_000;

const INSTRUCTIONS: &str = "repomap maps a codebase for you in one call, so you do not spend \
turns on ls/grep/reading files blind. Starting work in a repository you have not mapped in this \
conversation: call repomap_overview first (what it is, how to build and test, layout, entry \
points, core files). Before working on a specific task or looking for where something is \
implemented: call repomap_query with the task in your own words, plus any file paths or symbol \
names already mentioned; open its top files first. Both return ranked evidence, not proof: \
confirm with an exact search or a read before claiming how code works.";

fn tools() -> Value {
    let dir = json!({
        "type": "string",
        "description": "Absolute path of the repository or directory to map. Defaults to the server's working directory."
    });
    let budget = json!({
        "type": "integer",
        "minimum": 100,
        "description": "Approximate size of the map in tokens."
    });
    let strings = |description: &str| json!({"type": "array", "items": {"type": "string"}, "description": description});
    json!([
        {
            "name": "repomap_overview",
            "title": "Repository overview",
            "description": "Orientation map for a repository you are new to: its purpose (from the README), \
languages, build and test commands (from manifests), layout of packages/directories with each one's \
stated purpose, entry points, and the files the rest of the code depends on most. Call it first in an \
unfamiliar codebase. Default budget 2000 tokens; typically under 100 ms.",
            "inputSchema": {
                "type": "object",
                "properties": {"dir": dir, "token_budget": budget},
                "additionalProperties": false
            },
            "annotations": {"readOnlyHint": true, "idempotentHint": true, "openWorldHint": false}
        },
        {
            "name": "repomap_query",
            "title": "Files for a task",
            "description": "Ranks the repository's files for a task or question and returns the top ones, best \
first, each with its definitions, local imports and cross-file references, packed into a token budget. \
Call it before grepping for where something lives or before starting a change. Pass the task in plain \
words; add paths/symbols already mentioned so they are prioritized. Default budget 3000 tokens.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "dir": dir,
                    "query": {"type": "string", "description": "The task or question, in plain words."},
                    "mentioned_paths": strings("File paths already mentioned in the conversation."),
                    "mentioned_symbols": strings("Function, type or variable names already mentioned."),
                    "open_paths": strings("Files currently open or being edited."),
                    "detail": {
                        "type": "string",
                        "enum": ["full", "compact"],
                        "description": "full (default): definitions, imports and references per file. compact: one line per file (path, size, definition names), about three times as many candidates in the same budget; measured slightly better for finding the right files when you have not read the overview."
                    },
                    "token_budget": budget
                },
                "required": ["query"],
                "additionalProperties": false
            },
            "annotations": {"readOnlyHint": true, "idempotentHint": true, "openWorldHint": false}
        }
    ])
}

fn text_result(result: ToolResult) -> Value {
    json!({
        "content": [{"type": "text", "text": result.output}],
        "isError": !result.ok
    })
}

fn string_list(arguments: &Value, key: &str) -> Result<Vec<String>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| value.as_str().map(str::to_owned).ok_or(format!("{key} must contain only strings")))
            .collect(),
        Some(_) => Err(format!("{key} must be an array of strings")),
    }
}

fn call_tool(name: &str, arguments: &Value) -> Result<Value, String> {
    let dir = match arguments.get("dir") {
        None | Some(Value::Null) => std::env::current_dir().map_err(|error| error.to_string())?,
        Some(Value::String(dir)) => PathBuf::from(dir),
        Some(_) => return Err("dir must be a string".into()),
    };
    if !dir.is_dir() {
        return Ok(text_result(ToolResult {
            ok: false,
            output: format!("repomap: not a directory: {}", dir.display()),
            truncated: false,
            exit_code: None,
        }));
    }
    let budget = match arguments.get("token_budget") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or("token_budget must be a non-negative integer")?,
        ),
    };
    match name {
        "repomap_overview" => Ok(text_result(overview(&dir, budget.unwrap_or(2000), MAX_CHARS))),
        "repomap_query" => {
            let query = arguments
                .get("query")
                .and_then(Value::as_str)
                .ok_or("query is required and must be a string")?;
            let context = RankContext {
                query: query.to_owned(),
                mentioned_paths: string_list(arguments, "mentioned_paths")?,
                mentioned_symbols: string_list(arguments, "mentioned_symbols")?,
                open_paths: string_list(arguments, "open_paths")?,
                token_budget: budget.unwrap_or(3000),
            };
            let detail = match arguments.get("detail").and_then(Value::as_str) {
                None | Some("full") => Detail::Full,
                Some("compact") => Detail::Compact,
                Some(other) => return Err(format!("detail must be full or compact, not {other}")),
            };
            Ok(text_result(repo_map_with_detail(&dir, MAX_CHARS, &context, detail)))
        }
        other => Err(format!("unknown tool: {other}")),
    }
}

enum Failure {
    Error(i64, String),
    UnsupportedVersion(String),
}

fn handle(request: &Value) -> Option<Value> {
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(Value::Null);
    let modern = params.pointer("/_meta").and_then(|meta| meta.get(VERSION_KEY)).and_then(Value::as_str);
    if let Some(version) = modern {
        if !MODERN_VERSIONS.contains(&version) && !LEGACY_VERSIONS.contains(&version) {
            return id.map(|id| reply(id, Err(Failure::UnsupportedVersion(version.to_owned())), false));
        }
    }
    let modern = modern.is_some_and(|version| MODERN_VERSIONS.contains(&version));
    let outcome: Result<Value, Failure> = match method {
        "initialize" => {
            let requested = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
            let version = LEGACY_VERSIONS
                .iter()
                .find(|known| **known == requested)
                .unwrap_or(&LEGACY_VERSIONS[0]);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "repomap", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS
            }))
        }
        "server/discover" => Ok(json!({
            "supportedVersions": MODERN_VERSIONS.iter().chain(LEGACY_VERSIONS).collect::<Vec<_>>(),
            "capabilities": {"tools": {}},
            "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "repomap", "version": env!("CARGO_PKG_VERSION")}},
            "instructions": INSTRUCTIONS
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools()})),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            call_tool(name, &arguments).map_err(|message| Failure::Error(-32602, message))
        }
        _ if method.starts_with("notifications/") => return None,
        _ => Err(Failure::Error(-32601, format!("method not found: {method}"))),
    };
    // Notifications (no id) never get a response, even on error.
    Some(reply(id?, outcome, modern || method == "server/discover"))
}

fn reply(id: Value, outcome: Result<Value, Failure>, modern: bool) -> Value {
    match outcome {
        Ok(mut result) => {
            if modern {
                result["resultType"] = json!("complete");
            }
            json!({"jsonrpc": "2.0", "id": id, "result": result})
        }
        Err(Failure::Error(code, message)) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
        }
        Err(Failure::UnsupportedVersion(requested)) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": UNSUPPORTED_PROTOCOL_VERSION,
                "message": "Unsupported protocol version",
                "data": {
                    "supported": MODERN_VERSIONS.iter().chain(LEGACY_VERSIONS).collect::<Vec<_>>(),
                    "requested": requested
                }
            }
        }),
    }
}

/// Serve MCP on stdin/stdout until stdin closes.
pub fn serve() -> io::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(batch)) => {
                let replies: Vec<Value> = batch.iter().filter_map(handle).collect();
                (!replies.is_empty()).then(|| Value::Array(replies))
            }
            Ok(request) => handle(&request),
            Err(error) => Some(json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": {"code": -32700, "message": format!("parse error: {error}")}
            })),
        };
        if let Some(reply) = reply {
            serde_json::to_writer(&mut stdout, &reply)?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_lists_tools_and_answers_calls() {
        let init = handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}})).unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        assert!(init["result"]["instructions"].as_str().unwrap().contains("repomap_overview"));
        assert!(handle(&json!({"jsonrpc":"2.0","method":"notifications/initialized"})).is_none());

        let list = handle(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).unwrap();
        let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|tool| tool["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["repomap_overview", "repomap_query"]);

        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("payments.rs"), "pub fn authorize_payment() {}\n").unwrap();
        let dir = directory.path().to_str().unwrap();
        let call = handle(&json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"repomap_query","arguments":{"dir":dir,"query":"authorize payment"}}})).unwrap();
        assert_eq!(call["result"]["isError"], false);
        assert!(call["result"]["content"][0]["text"].as_str().unwrap().contains("payments.rs"));

        let overview = handle(&json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"repomap_overview","arguments":{"dir":dir}}})).unwrap();
        assert!(overview["result"]["content"][0]["text"].as_str().unwrap().contains("repository overview"));

        let missing = handle(&json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"repomap_query","arguments":{"dir":dir}}})).unwrap();
        assert_eq!(missing["error"]["code"], -32602);
        let unknown = handle(&json!({"jsonrpc":"2.0","id":6,"method":"nope"})).unwrap();
        assert_eq!(unknown["error"]["code"], -32601);
    }

    #[test]
    fn modern_requests_are_stateless_and_versioned() {
        let meta = |version: &str| json!({"_meta": {VERSION_KEY: version, "io.modelcontextprotocol/clientInfo": {"name": "t", "version": "0"}}});
        let discover = handle(&json!({"jsonrpc":"2.0","id":"d","method":"server/discover","params":meta("2026-07-28")})).unwrap();
        assert_eq!(discover["result"]["resultType"], "complete");
        assert_eq!(discover["result"]["supportedVersions"][0], "2026-07-28");
        assert_eq!(discover["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "repomap");

        let list = handle(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":meta("2026-07-28")})).unwrap();
        assert_eq!(list["result"]["resultType"], "complete");
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 2);

        let unknown = handle(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":meta("1900-01-01")})).unwrap();
        assert_eq!(unknown["error"]["code"], UNSUPPORTED_PROTOCOL_VERSION);
        assert_eq!(unknown["error"]["data"]["requested"], "1900-01-01");
        assert_eq!(unknown["error"]["data"]["supported"][0], "2026-07-28");

        let legacy = handle(&json!({"jsonrpc":"2.0","id":3,"method":"tools/list"})).unwrap();
        assert!(legacy["result"].get("resultType").is_none());
    }
}
