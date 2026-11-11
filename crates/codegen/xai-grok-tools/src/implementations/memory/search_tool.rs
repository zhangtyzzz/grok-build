//! `memory_search` tool — new architecture (`Tool` trait).

use std::sync::Arc;

use super::types::MemorySearchInput;
use crate::script_value::{MemorySearchHit, MemorySearchValue};
use crate::types::memory_backend::{MemoryBackend, format_staleness_note};
use crate::types::output::{TextOutput, ToolOutput};
use crate::types::tool::{ToolKind, ToolNamespace};

#[derive(Debug, Default)]
pub struct MemorySearchImpl;

impl crate::types::tool_metadata::ToolMetadata for MemorySearchImpl {
    fn script_output_schema(&self) -> Option<serde_json::Value> {
        Some(crate::script_value::schema_of::<
            crate::script_value::MemorySearchValue,
        >())
    }
    fn kind(&self) -> ToolKind {
        ToolKind::MemorySearch
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::GrokBuild
    }

    fn description_template(&self) -> &str {
        "Search cross-session memory for relevant knowledge chunks. Returns ranked results \
         from global, workspace, and session memory files.\n\n\
         Use this proactively when:\n\
         - A question references prior work, decisions, or context you don't have\n\
         - You need project conventions, coding patterns, or user preferences\n\
         - The user mentions something discussed or decided in a previous session\n\
         - Starting work in an unfamiliar part of the codebase\n\
         - After compaction when prior context may have been lost\n\n\
         Memory is historical context, not automatically the current plan. Verify recalled facts \
         against live sources before relying on them."
    }
}

impl xai_tool_runtime::Tool for MemorySearchImpl {
    type Args = MemorySearchInput;
    type Output = ToolOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new("memory_search").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(
            "memory_search",
            crate::types::tool_metadata::ToolMetadata::sanitized_description_template(self),
        )
    }

    fn capabilities(&self) -> xai_tool_protocol::ToolCapabilities {
        xai_tool_protocol::ToolCapabilities {
            is_read_only: true,
            tool_scope: Some(xai_tool_protocol::ToolScope::Read),
            ..Default::default()
        }
    }

    async fn run(
        &self,
        ctx: xai_tool_runtime::ToolCallContext,
        input: MemorySearchInput,
    ) -> Result<ToolOutput, xai_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;
        let Some(memory) = resources
            .lock()
            .await
            .get::<Arc<dyn MemoryBackend>>()
            .cloned()
        else {
            return Ok(ToolOutput::Text(TextOutput::failure(
                "Memory is not enabled. Use --experimental-memory to enable.",
            )));
        };
        let max_results = input
            .max_results
            .unwrap_or_else(|| memory.default_search_max_results());
        let min_score = input
            .min_score
            .unwrap_or_else(|| memory.default_search_min_score());
        tracing::info!(target: crate::types::memory_backend::MEMORY_LOG_TARGET, max_results, "MEMORY_SEARCH: invoked");
        let results = memory
            .search(&input.query, max_results, min_score)
            .await
            .map_err(|e| {
                xai_tool_runtime::ToolError::execution(
                    xai_tool_protocol::ToolId::new("memory_search").expect("valid"),
                    format!("memory search failed: {e}"),
                )
            })?;
        tracing::info!(target: crate::types::memory_backend::MEMORY_LOG_TARGET, results = results.len(), "MEMORY_SEARCH: complete");
        if results.is_empty() {
            return Ok(ToolOutput::Text(TextOutput::with_data(
                "No memory results found for query.",
                serde_json::json!({ "results": [] }),
            )));
        }
        let mut output = format!("Found {} memory result(s):\n", results.len());
        for (i, r) in results.iter().enumerate() {
            let staleness = format_staleness_note(&r.source, r.created_at);
            output.push_str(&format!(
                "\n### Result {} (score: {:.2}, source: {})\n**File:** {} (lines {}-{})\n{}```\n{}\n```\n",
                i + 1, r.score, r.source, r.path, r.start_line, r.end_line, staleness, r.snippet,
            ));
        }
        let data = MemorySearchValue {
            results: results
                .iter()
                .map(|r| MemorySearchHit {
                    path: r.path.clone(),
                    start_line: r.start_line + 1,
                    end_line: r.end_line,
                    score: r.score,
                    source: r.source.clone(),
                    snippet: r.snippet.clone(),
                })
                .collect(),
        };
        Ok(ToolOutput::Text(match serde_json::to_value(data) {
            Ok(data) => TextOutput::with_data(output, data),
            Err(_) => output.into(),
        }))
    }
}
