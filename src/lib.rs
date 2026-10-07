//! repomap: token-budgeted maps of a codebase for AI agents.
//!
//! `overview` orients an agent new to a repository; `repo_map_with_context`
//! ranks files for a specific task; `repo_map` is the legacy query-free list.
//! Started as a copy of the Nexus mapper (`systemhelper/nexus`, 785c4c9).

mod c_like;
mod cache;
mod go_like;
pub mod mcp;
mod overview;
mod repo_map;
pub mod repomap_ranker;
mod script_like;
mod walk;

pub use overview::overview;
pub use repo_map::{repo_map, repo_map_with_context, repo_map_with_detail, Detail};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolResult {
    pub ok: bool,
    pub output: String,
    pub truncated: bool,
    pub exit_code: Option<i32>,
}

pub(crate) fn bound_tool_output_to(output: &str, max_chars: usize) -> ToolResult {
    let count = output.chars().count();
    if count <= max_chars {
        return ToolResult {
            ok: true,
            output: output.into(),
            truncated: false,
            exit_code: None,
        };
    }
    let marker =
        |omitted| format!("\n\n... [{omitted} chars omitted; showing head and tail] ...\n\n");
    // The omitted count changes the marker width at decimal boundaries. Keep
    // recalculating until the marker and retained-content budget agree, rather
    // than stopping one character early when 99_999 becomes 100_000.
    let mut budget = max_chars;
    let mark = loop {
        let mark = marker(count - budget);
        let next_budget = max_chars.saturating_sub(mark.chars().count());
        if next_budget == budget {
            break mark;
        }
        budget = next_budget;
    };
    if mark.chars().count() > max_chars {
        return ToolResult {
            ok: true,
            output: output.chars().take(max_chars).collect(),
            truncated: true,
            exit_code: None,
        };
    }
    let head = budget.div_ceil(2);
    let tail = budget - head;
    let mut bounded: String = output.chars().take(head).collect();
    bounded.push_str(&mark);
    if tail > 0 {
        let suffix: String = output
            .chars()
            .rev()
            .take(tail)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        bounded.push_str(&suffix);
    }
    ToolResult {
        ok: true,
        output: bounded,
        truncated: true,
        exit_code: None,
    }
}

pub(crate) fn failure(output: impl Into<String>) -> ToolResult {
    ToolResult {
        ok: false,
        output: output.into(),
        truncated: false,
        exit_code: None,
    }
}
