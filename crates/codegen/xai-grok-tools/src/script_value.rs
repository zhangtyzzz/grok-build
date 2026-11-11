//! Values Code Mode scripts receive from tool calls.
//!
//! A nested call resolves to one JSON value in the shape its tool declares
//! (`catalog.describe(name).returns`), or rejects with the tool's failure text.
//! This is a second projection of the same [`ToolOutput`] the model-facing text
//! comes from ([`ToolOutput::to_prompt_format`]), which stays unchanged.
//!
//! Every tool declares a schema: its `Tool::Output` type implements
//! [`ScriptSchema`], or (when that type is the untyped [`ToolOutput`]) the tool
//! overrides `ToolMetadata::script_output_schema`. The registry tests check that
//! no registered tool is left without one and that each projection matches the
//! schema of the tools that produce it.
use crate::implementations::grok_build::lsp::LspToolOutput;
use crate::implementations::grok_build::monitor::types::MonitorOutput;
use crate::implementations::grok_build::scheduler::create::SchedulerCreateOutput;
use crate::implementations::grok_build::scheduler::delete::SchedulerDeleteOutput;
use crate::implementations::grok_build::scheduler::list::SchedulerListOutput;
use crate::implementations::grok_build::send_feedback::SendFeedbackOutput;
use crate::implementations::grok_build::send_subagent_message::{
    SendSubagentMessageDisposition, SendSubagentMessageOutput,
};
use crate::implementations::grok_build::todo::{TodoPriority, TodoStatus};
use crate::implementations::grok_build::update_goal::UpdateGoalOutput;
use crate::implementations::grok_build::workflow::WorkflowToolOutput;
use crate::implementations::opencode::glob::GlobOutput;
use crate::implementations::skills::skill::SkillOutput;
use crate::types::output::{
    ApplyPatchOutput, AskUserQuestionOutput, BashToolOutput, CodexGrepFilesOutput,
    EnterPlanModeOutput, ExitPlanModeOutput, GrepFileSummary, GrepSearchOutput, KillTaskOutput,
    ListDirEntry, ListDirOutput, MCPOutputDetails, MediaGenOutput, ReadFileOutput,
    SearchReplaceOutput, TaskOutputOutput, TodoWriteOutput, ToolOutput, WebFetchOutput,
    WebSearchOutput,
};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};

/// A script's view of one call: the declared value, or the failure text the call rejects with.
pub type ScriptResult = Result<Value, String>;

/// JSON Schema of the value scripts receive from a tool whose `Tool::Output` is `Self`.
pub trait ScriptSchema {
    /// `None` only for [`ToolOutput`]: tools returning it declare their schema through
    /// `ToolMetadata::script_output_schema`.
    fn script_schema() -> Option<Value>;
}

/// Inline JSON Schema for `T`, in the same form as tool input schemas.
pub fn schema_of<T: JsonSchema + 'static>() -> Value {
    crate::registry::types::generate_schema_cached::<T>()
}

/// Schema any JSON value satisfies, for runtime-registered tools that declare nothing.
pub fn any_value_schema() -> Value {
    json!({})
}

/// Plain text, for tools whose result is prose (LSP answers, feedback receipts).
#[derive(Debug, Serialize, JsonSchema)]
pub struct TextValue {
    pub text: String,
}

/// A message-only result (plan mode and question tools).
#[derive(Debug, Serialize, JsonSchema)]
pub struct MessageValue {
    pub message: String,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReadFileValue {
    /// File text without line-number prefixes.
    Text {
        path: String,
        content: String,
        /// 1-based number of the first line in `content`, when known.
        start_line: Option<usize>,
        /// Lines in `content`.
        line_count: usize,
        /// Lines in the whole file.
        total_lines: usize,
    },
    /// An image; it is attached to the `execute` result for you to see.
    Image { mime_type: String },
    /// Rendered PDF pages; they are attached to the `execute` result.
    Pdf {
        pages: Vec<usize>,
        total_pages: usize,
        file_size: usize,
    },
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListDirValue {
    /// Absolute path of the listed directory.
    pub path: String,
    /// Entries this call returned, relative to `path`.
    pub entries: Vec<ListDirEntry>,
    /// More entries exist than `entries` holds.
    pub truncated: bool,
    /// The formatted listing the tool shows you directly.
    pub listing: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GrepMatchValue {
    pub path: String,
    pub line_number: usize,
    pub line: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GrepValue {
    /// Matched lines (content mode); empty in the file-list and count modes.
    pub matches: Vec<GrepMatchValue>,
    /// Every file with a match; `count` is set in count and content modes.
    pub files: Vec<GrepFileSummary>,
    pub match_count: usize,
    /// Results stopped at the head limit or the search timed out.
    pub truncated: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EditLocation {
    /// 1-based line where the replaced text began.
    pub old_line: usize,
    /// 1-based line where the new text begins.
    pub new_line: usize,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EditValue {
    /// Absolute path of the written file.
    pub path: String,
    /// Each replacement made, in file order.
    pub edits: Vec<EditLocation>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BashValue {
    /// The command finished (a nonzero `exit_code` is not a rejection).
    Completed {
        exit_code: i32,
        /// Combined stdout/stderr without ANSI escapes.
        output: String,
        /// `output` was cut; the full output is in `output_file`.
        truncated: bool,
        total_bytes: usize,
        output_file: String,
        timed_out: bool,
        signal: Option<String>,
        /// Working directory after the command.
        cwd: String,
    },
    /// The command runs in the background; poll `task_id` with the task output tool.
    Background {
        task_id: String,
        task_type: String,
        output_file: String,
        command: String,
        pid: Option<u32>,
    },
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TodoValueItem {
    pub content: String,
    pub status: TodoStatus,
    pub priority: TodoPriority,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TodoValue {
    pub todos: Vec<TodoValueItem>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct WebSearchValue {
    pub query: String,
    /// The search answer text.
    pub content: String,
    pub citations: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct WebFetchValue {
    /// Final URL after redirects.
    pub url: String,
    /// Page content as markdown, or the raw text for non-HTML.
    pub content: String,
    pub content_type: String,
    pub status_code: u16,
    pub bytes: usize,
    /// File holding the complete content when it was too large to inline.
    pub output_file: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TaskValue {
    pub task_id: String,
    pub command: String,
    /// `running`, `completed`, `failed`, `cancelled` or `timed_out`.
    pub status: String,
    pub exit_code: Option<i32>,
    pub duration_secs: f64,
    pub output: String,
    pub output_file: String,
    /// `output` was cut; the full output is in `output_file`.
    pub truncated: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskOutputValue {
    /// One task.
    Task(TaskValue),
    /// Several tasks waited on together.
    Multi {
        mode: String,
        tasks: Vec<TaskValue>,
        summary: String,
    },
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct KillTaskValue {
    pub task_id: String,
    pub outcome: String,
    pub message: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SkillValue {
    pub name: String,
    /// The skill's instructions.
    pub content: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PatchedFile {
    pub path: String,
    /// `added`, `modified`, `deleted` or `moved`.
    pub action: String,
    pub move_to: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ApplyPatchValue {
    pub files: Vec<PatchedFile>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PathsValue {
    pub paths: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GlobValue {
    /// Matching absolute paths, most recently modified first.
    pub paths: Vec<String>,
    /// Matches before the result limit.
    pub total_count: usize,
    pub truncated: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubagentValue {
    /// The subagent finished.
    Completed {
        subagent_id: String,
        subagent_type: String,
        /// The subagent's final answer.
        output: String,
        tool_calls: u32,
        turns: u32,
        duration_ms: u64,
        worktree_path: Option<String>,
    },
    /// The subagent runs in the background (requested, or moved there after the
    /// wait budget); its result arrives later.
    Background {
        subagent_id: String,
        auto_backgrounded: bool,
    },
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SubagentMessageValue {
    /// Set when delivery was confirmed.
    pub message_id: Option<String>,
    /// `false` when admission could not be confirmed either way.
    pub confirmed: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct MediaValue {
    /// Absolute path of the saved file; `null` when only uploaded.
    pub path: Option<String>,
    pub filename: String,
    pub session_folder: String,
    pub uploaded_url: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct MemorySearchHit {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub score: f64,
    pub source: String,
    pub snippet: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct MemorySearchValue {
    pub results: Vec<MemorySearchHit>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct MemoryGetValue {
    pub path: String,
    pub content: String,
    pub start_line: usize,
    pub line_count: usize,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ToolSearchValue {
    pub result_count: usize,
    pub content: String,
}

/// MCP results. `data` is the decoded payload; `text` keeps the raw text blocks.
#[derive(Debug, Serialize, JsonSchema)]
pub struct McpValue {
    /// The result's text blocks joined by newlines.
    pub text: String,
    /// `structuredContent` when the server sent it; otherwise the text parsed as
    /// JSON (one document, or an array when every line is a JSON document);
    /// otherwise `null`.
    pub data: Value,
}

/// Schema of [`McpValue`] with `data` narrowed to the server's `outputSchema`.
pub fn mcp_value_schema(output_schema: Option<&Value>) -> Value {
    let mut schema = schema_of::<McpValue>();
    if let Some(output) = output_schema.filter(|s| s.is_object())
        && let Some(data) = schema.pointer_mut("/properties/data")
    {
        let description = data.get("description").cloned();
        *data = json!({"anyOf": [output, {"type": "null"}]});
        if let (Some(description), Some(object)) = (description, data.as_object_mut()) {
            object.insert("description".to_owned(), description);
        }
    }
    schema
}

/// The server `outputSchema` inside a schema built by [`mcp_value_schema`].
pub fn mcp_output_schema(value_schema: &Value) -> Option<Value> {
    value_schema
        .pointer("/properties/data/anyOf/0")
        .filter(|s| s.is_object())
        .cloned()
}

/// `structuredContent`, else the text as one JSON document, else JSON Lines, else `null`.
fn mcp_data(text: &str, structured: Option<&Value>) -> Value {
    if let Some(structured) = structured {
        return structured.clone();
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Value::Null;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return value;
    }
    let lines: Vec<&str> = trimmed.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() > 1 {
        let parsed: Result<Vec<Value>, _> = lines.iter().map(|l| serde_json::from_str(l)).collect();
        if let Ok(values) = parsed {
            return Value::Array(values);
        }
    }
    Value::Null
}

fn to_value<T: Serialize>(value: T) -> ScriptResult {
    serde_json::to_value(value).map_err(|e| format!("Code Mode value serialization failed: {e}"))
}

fn path_string(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

impl ToolOutput {
    /// The value a Code Mode script receives for this output, or the failure text
    /// its call rejects with. The match is exhaustive so every new variant decides.
    pub fn script_value(&self) -> ScriptResult {
        let fail = || Err(self.to_prompt_format());
        match self {
            ToolOutput::ReadFile(read) => match read {
                ReadFileOutput::FileContent(file) => to_value(ReadFileValue::Text {
                    path: path_string(&file.absolute_path),
                    content: file.raw_output.clone(),
                    start_line: file.start_line,
                    line_count: file.raw_output.lines().count(),
                    total_lines: file.total_lines,
                }),
                ReadFileOutput::ImageContent(image) => to_value(ReadFileValue::Image {
                    mime_type: image.mime_type.clone(),
                }),
                ReadFileOutput::PdfPageImages(pdf) => to_value(ReadFileValue::Pdf {
                    pages: pdf.pages.iter().map(|p| p.page_number).collect(),
                    total_pages: pdf.total_pages,
                    file_size: pdf.file_size,
                }),
                ReadFileOutput::FileNotFound(_)
                | ReadFileOutput::IsADirectory(_)
                | ReadFileOutput::PermissionDenied(_)
                | ReadFileOutput::FileTooLarge(_)
                | ReadFileOutput::FileReadError(_)
                | ReadFileOutput::ImageSizeError(_) => fail(),
            },
            ToolOutput::ListDir(list) => match list {
                ListDirOutput::Content(content) => to_value(ListDirValue {
                    path: path_string(&content.absolute_root_path),
                    entries: content.entries.clone(),
                    truncated: content.truncated,
                    listing: content.content.clone(),
                }),
                ListDirOutput::NotFound(_)
                | ListDirOutput::IsAFile(_)
                | ListDirOutput::NotADirectory(_)
                | ListDirOutput::PermissionDenied(_)
                | ListDirOutput::Error(_) => fail(),
            },
            ToolOutput::GrepSearch(grep) => grep_value(grep).map_or_else(fail, to_value),
            ToolOutput::SearchReplace(edit) => match edit {
                SearchReplaceOutput::EditsApplied(applied) => to_value(EditValue {
                    path: path_string(&applied.absolute_path),
                    edits: applied
                        .edits
                        .details
                        .iter()
                        .map(|d| EditLocation {
                            old_line: d.old_line,
                            new_line: d.new_line,
                        })
                        .collect(),
                }),
                SearchReplaceOutput::FileAlreadyExists(_)
                | SearchReplaceOutput::MultipleMatchesFound(_)
                | SearchReplaceOutput::NoMatchesFound(_)
                | SearchReplaceOutput::InvalidInput(_)
                | SearchReplaceOutput::FileNotFound(_)
                | SearchReplaceOutput::FilenameTooLong(_) => fail(),
            },
            ToolOutput::Bash(bash) => to_value(BashValue::Completed {
                exit_code: bash.exit_code,
                output: strip_ansi_escapes::strip_str(String::from_utf8_lossy(&bash.output)),
                truncated: bash.truncated,
                total_bytes: bash.total_bytes,
                output_file: bash.output_file.clone(),
                timed_out: bash.timed_out,
                signal: bash.signal.clone(),
                cwd: bash.current_dir.clone(),
            }),
            ToolOutput::BackgroundTaskStarted(bg) => to_value(BashValue::Background {
                task_id: bg.task_id.clone(),
                task_type: bg.task_type.clone(),
                output_file: bg.output_file.clone(),
                command: bg.command.clone(),
                pid: bg.pid,
            }),
            ToolOutput::Todo(todo) => match todo {
                TodoWriteOutput::TodosUpdated(success) => to_value(TodoValue {
                    todos: success
                        .todos
                        .iter()
                        .map(|t| TodoValueItem {
                            content: t.content.clone(),
                            status: t.status,
                            priority: t.priority,
                        })
                        .collect(),
                }),
                TodoWriteOutput::DuplicateId(_) | TodoWriteOutput::InvalidArgument(_) => fail(),
            },
            ToolOutput::WebSearch(search) => to_value(WebSearchValue {
                query: search.query.clone(),
                content: search.content.clone(),
                citations: search.citations.clone(),
            }),
            ToolOutput::WebFetch(fetch) => match fetch {
                WebFetchOutput::Content(page) => to_value(WebFetchValue {
                    url: page.url.clone(),
                    content: page.content.clone(),
                    content_type: page.content_type.clone(),
                    status_code: page.status_code,
                    bytes: page.bytes,
                    output_file: page.output_location.as_ref().map(|l| l.file_path.clone()),
                }),
                WebFetchOutput::DomainNotAllowed(_)
                | WebFetchOutput::CrossHostRedirect { .. }
                | WebFetchOutput::Error { .. } => fail(),
            },
            ToolOutput::MCP(mcp) => {
                if mcp.is_error {
                    return fail();
                }
                let full = match mcp.output() {
                    MCPOutputDetails::OkayOutput(text) => text.as_str(),
                    MCPOutputDetails::Error(_) => return fail(),
                };
                // Older producers keep only the joined rendering.
                let text = mcp
                    .text_blocks
                    .as_deref()
                    .filter(|t| !t.is_empty() || mcp.structured_content.is_some())
                    .unwrap_or(full);
                to_value(McpValue {
                    text: text.to_owned(),
                    data: mcp_data(text, mcp.structured_content.as_ref()),
                })
            }
            ToolOutput::TaskOutput(task) => match task {
                TaskOutputOutput::Result(result) => {
                    to_value(TaskOutputValue::Task(task_value(result)))
                }
                TaskOutputOutput::MultiResult(multi) => to_value(TaskOutputValue::Multi {
                    mode: multi.mode.clone(),
                    tasks: multi.results.iter().map(task_value).collect(),
                    summary: multi.summary.clone(),
                }),
                TaskOutputOutput::TaskNotFound(_) => fail(),
            },
            ToolOutput::KillTask(kill) => match kill {
                KillTaskOutput::Result(result) => to_value(KillTaskValue {
                    task_id: result.task_id.clone(),
                    outcome: result.outcome.clone(),
                    message: result.message.clone(),
                }),
                KillTaskOutput::TaskNotFound(_) => fail(),
            },
            ToolOutput::Skill(skill) if skill.success => to_value(SkillValue {
                name: skill.skill_name.clone(),
                content: skill
                    .skill_message
                    .clone()
                    .unwrap_or_else(|| skill.tool_result.clone()),
            }),
            ToolOutput::Skill(_) => fail(),
            ToolOutput::ApplyPatch(patch) => match patch {
                ApplyPatchOutput::Success { files, .. } => to_value(ApplyPatchValue {
                    files: files
                        .iter()
                        .map(|f| PatchedFile {
                            path: path_string(&f.path),
                            action: f.action.clone(),
                            move_to: f.move_to.as_deref().map(path_string),
                        })
                        .collect(),
                }),
                ApplyPatchOutput::ParseError(_)
                | ApplyPatchOutput::ApplicationError(_)
                | ApplyPatchOutput::EmptyPatch(_) => fail(),
            },
            ToolOutput::CodexGrepFiles(files) => match files {
                CodexGrepFilesOutput::Matches { content, .. } => to_value(PathsValue {
                    paths: content
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(str::to_owned)
                        .collect(),
                }),
                CodexGrepFilesOutput::NoMatches(_) => to_value(PathsValue { paths: Vec::new() }),
                CodexGrepFilesOutput::Error(_) => fail(),
            },
            ToolOutput::SearchTool(search) => to_value(ToolSearchValue {
                result_count: search.result_count,
                content: search.content.clone(),
            }),
            ToolOutput::SubagentCompleted(sub) => to_value(SubagentValue::Completed {
                subagent_id: sub.subagent_id.clone(),
                subagent_type: sub.subagent_type.clone(),
                output: sub.output.clone(),
                tool_calls: sub.tool_calls,
                turns: sub.turns,
                duration_ms: sub.duration_ms,
                worktree_path: sub.worktree_path.clone(),
            }),
            ToolOutput::EnterPlanMode(EnterPlanModeOutput::Entered { message, .. })
            | ToolOutput::ExitPlanMode(
                ExitPlanModeOutput::PlanReady { message, .. }
                | ExitPlanModeOutput::EmptyPlan { message, .. },
            )
            | ToolOutput::AskUserQuestion(
                AskUserQuestionOutput::QuestionsSent { message, .. }
                | AskUserQuestionOutput::UserAnswered { message },
            ) => to_value(MessageValue {
                message: message.clone(),
            }),
            ToolOutput::SendSubagentMessage(sent) => match (sent.disposition(), sent) {
                (_, SendSubagentMessageOutput::Accepted { message_id }) => {
                    to_value(SubagentMessageValue {
                        message_id: Some(message_id.clone()),
                        confirmed: true,
                    })
                }
                (SendSubagentMessageDisposition::Unconfirmed, _) => {
                    to_value(SubagentMessageValue {
                        message_id: None,
                        confirmed: false,
                    })
                }
                _ => fail(),
            },
            ToolOutput::Monitor(monitor) => to_value(monitor),
            ToolOutput::SchedulerCreate(created) => to_value(created),
            ToolOutput::SchedulerDelete(deleted) if deleted.success => to_value(deleted),
            ToolOutput::SchedulerDelete(_) => fail(),
            ToolOutput::SchedulerList(list) => to_value(list),
            ToolOutput::UpdateGoal(goal) if goal.success => to_value(goal),
            ToolOutput::UpdateGoal(_) => fail(),
            ToolOutput::Workflow(run) => to_value(run),
            ToolOutput::Dynamic(dynamic) => Ok(dynamic.value.clone()),
            ToolOutput::Text(text) if text.failed => fail(),
            ToolOutput::Text(text) => match &text.data {
                Some(data) => Ok(data.clone()),
                None => to_value(TextValue {
                    text: text.text.clone(),
                }),
            },
            ToolOutput::ImageGen(media)
            | ToolOutput::ImageToVideo(media)
            | ToolOutput::ReferenceToVideo(media)
            | ToolOutput::ImageEdit(media) => to_value(media_value(media)),
        }
    }
}

/// `None` for a failed search: rg errors, and a timeout that found nothing.
fn grep_value(grep: &GrepSearchOutput) -> Option<GrepValue> {
    let found = !grep.file_matches.is_empty() || !grep.files.is_empty();
    if grep.exit_code > 1 || (grep.exit_code < 0 && !found) {
        return None;
    }
    let matches = grep
        .file_matches
        .iter()
        .flat_map(|file| {
            file.matches.iter().map(|m| GrepMatchValue {
                path: file.path.clone(),
                line_number: m.line_number,
                line: m.content.clone(),
            })
        })
        .collect();
    let files = if grep.files.is_empty() {
        grep.file_matches
            .iter()
            .map(|file| GrepFileSummary {
                path: file.path.clone(),
                count: Some(file.matches.len()),
            })
            .collect()
    } else {
        grep.files.clone()
    };
    Some(GrepValue {
        matches,
        files,
        match_count: grep.match_count,
        truncated: grep.truncated,
    })
}

fn task_value(result: &xai_tool_types::TaskOutputResult) -> TaskValue {
    TaskValue {
        task_id: result.task_id.clone(),
        command: result.command.clone(),
        status: result.status.clone(),
        exit_code: result.exit_code,
        duration_secs: result.duration_secs,
        output: result.output.clone(),
        output_file: result.output_file.clone(),
        truncated: result.truncated,
    }
}

fn media_value(media: &MediaGenOutput) -> MediaValue {
    MediaValue {
        path: (!media.path.as_os_str().is_empty()).then(|| path_string(&media.path)),
        filename: media.filename.clone(),
        session_folder: media.session_folder.clone(),
        uploaded_url: media.uploaded_url.clone(),
    }
}

/// Schema for tools whose `Tool::Output` is a value type of this module.
macro_rules! script_schema {
    ($($output:ty => $value:ty),* $(,)?) => {
        $(impl ScriptSchema for $output {
            fn script_schema() -> Option<Value> {
                Some(schema_of::<$value>())
            }
        })*
    };
}
script_schema! {
    ReadFileOutput => ReadFileValue,
    ListDirOutput => ListDirValue,
    GrepSearchOutput => GrepValue,
    SearchReplaceOutput => EditValue,
    BashToolOutput => BashValue,
    TodoWriteOutput => TodoValue,
    WebSearchOutput => WebSearchValue,
    WebFetchOutput => WebFetchValue,
    TaskOutputOutput => TaskOutputValue,
    KillTaskOutput => KillTaskValue,
    SkillOutput => SkillValue,
    ApplyPatchOutput => ApplyPatchValue,
    CodexGrepFilesOutput => PathsValue,
    EnterPlanModeOutput => MessageValue,
    ExitPlanModeOutput => MessageValue,
    AskUserQuestionOutput => MessageValue,
    SendSubagentMessageOutput => SubagentMessageValue,
    MonitorOutput => MonitorOutput,
    SchedulerCreateOutput => SchedulerCreateOutput,
    SchedulerDeleteOutput => SchedulerDeleteOutput,
    SchedulerListOutput => SchedulerListOutput,
    UpdateGoalOutput => UpdateGoalOutput,
    WorkflowToolOutput => WorkflowToolOutput,
    GlobOutput => GlobValue,
    LspToolOutput => TextValue,
    SendFeedbackOutput => TextValue,
}

impl ScriptSchema for ToolOutput {
    fn script_schema() -> Option<Value> {
        None
    }
}

#[cfg(test)]
#[path = "script_value_tests.rs"]
mod tests;
