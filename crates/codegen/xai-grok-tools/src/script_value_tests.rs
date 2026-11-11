use super::*;
use crate::implementations::grok_build::todo::TodoItem;
use crate::types::output::{
    ApplyPatchFileResult, BackgroundTaskStarted, BashOutput, DynamicOutput, FileContent,
    GrepFileMatch, GrepLineMatch, ImageContent, ListDirContent, ListDirEntryKind, MCPOutput,
    NoMatchesFoundError, PdfPageImage, PdfPageImages, SearchReplaceEditContextInformation,
    SearchReplaceEditDetail, SearchReplaceEditsApplied, TextOutput, TodoWriteSuccess,
    WebFetchContent,
};
use xai_tool_types::{
    KillTaskResult, MultiTaskOutputResult, SubagentCompletedOutput, TaskOutputResult,
};

/// The projection resolves and matches `schema`.
#[track_caller]
fn conforms(output: ToolOutput, schema: &Value) -> Value {
    let value = output.script_value().expect("resolves");
    let validator = jsonschema::validator_for(schema).expect("valid schema");
    let errors: Vec<String> = validator
        .iter_errors(&value)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{value}\nagainst {schema}\n{errors:#?}");
    value
}

#[track_caller]
fn rejects(output: ToolOutput) -> String {
    let text = output.to_prompt_format();
    assert_eq!(output.script_value(), Err(text.clone()));
    text
}

fn schema<T: ScriptSchema>() -> Value {
    T::script_schema().expect("declared")
}

fn file_content(raw: &str, start_line: Option<usize>) -> FileContent {
    FileContent {
        content: format!("1→{raw}"),
        content_concise: None,
        absolute_path: "/w/a.rs".into(),
        offset: None,
        limit: None,
        raw_output: raw.to_owned(),
        total_lines: 10,
        start_line,
        extracted_images: Vec::new(),
    }
}

#[test]
fn read_values_carry_text_without_line_anchors() {
    let schema = schema::<ReadFileOutput>();
    let value = conforms(
        ToolOutput::ReadFile(ReadFileOutput::FileContent(file_content("a\nb\n", Some(4)))),
        &schema,
    );
    assert_eq!(
        value,
        json!({"kind":"text","path":"/w/a.rs","content":"a\nb\n","start_line":4,"line_count":2,"total_lines":10})
    );
    let image = conforms(
        ToolOutput::ReadFile(ReadFileOutput::ImageContent(ImageContent {
            data: "AAAA".into(),
            mime_type: "image/png".into(),
            annotations: None,
            uri: None,
            meta: None,
        })),
        &schema,
    );
    assert_eq!(
        image,
        json!({"kind":"image","mime_type":"image/png"}),
        "no base64 in the value"
    );
    conforms(
        ToolOutput::ReadFile(ReadFileOutput::PdfPageImages(PdfPageImages {
            pages: vec![PdfPageImage {
                data: "AAAA".into(),
                mime_type: "image/jpeg".into(),
                page_number: 2,
            }],
            total_pages: 9,
            file_size: 100,
        })),
        &schema,
    );
    rejects(ToolOutput::ReadFile(ReadFileOutput::FileNotFound(
        "File not found: a".into(),
    )));
}

#[test]
fn list_and_search_values_are_structured() {
    let list = conforms(
        ToolOutput::ListDir(ListDirOutput::Content(ListDirContent {
            content: "- /w/\n  a.rs".into(),
            absolute_root_path: "/w".into(),
            entries: vec![ListDirEntry {
                path: "a.rs".into(),
                kind: ListDirEntryKind::File,
            }],
            truncated: false,
        })),
        &schema::<ListDirOutput>(),
    );
    assert_eq!(list["entries"][0], json!({"path":"a.rs","kind":"file"}));
    rejects(ToolOutput::ListDir(ListDirOutput::NotFound(
        "missing".into(),
    )));

    let grep_schema = schema::<GrepSearchOutput>();
    let content = conforms(
        ToolOutput::GrepSearch(GrepSearchOutput {
            exit_code: 0,
            match_count: 2,
            file_matches: vec![GrepFileMatch {
                path: "a.rs".into(),
                matches: vec![
                    GrepLineMatch {
                        line_number: 3,
                        content: "fn a()".into(),
                    },
                    GrepLineMatch {
                        line_number: 9,
                        content: "fn b()".into(),
                    },
                ],
            }],
            ..Default::default()
        }),
        &grep_schema,
    );
    assert_eq!(
        content["matches"][1],
        json!({"path":"a.rs","line_number":9,"line":"fn b()"})
    );
    assert_eq!(content["files"], json!([{"path":"a.rs","count":2}]));
    let files = conforms(
        ToolOutput::GrepSearch(GrepSearchOutput {
            match_count: 1,
            files: vec![GrepFileSummary {
                path: "b.rs".into(),
                count: None,
            }],
            truncated: true,
            ..Default::default()
        }),
        &grep_schema,
    );
    assert_eq!(files["files"], json!([{"path":"b.rs"}]));
    assert_eq!(files["truncated"], true);
    // No matches is a result, an rg error or an empty timeout is not.
    conforms(
        ToolOutput::GrepSearch(GrepSearchOutput {
            exit_code: 1,
            ..Default::default()
        }),
        &grep_schema,
    );
    for exit_code in [2, -1] {
        rejects(ToolOutput::GrepSearch(GrepSearchOutput {
            stdout: b"Error calling tool".to_vec(),
            exit_code,
            ..Default::default()
        }));
    }

    let paths = conforms(
        ToolOutput::CodexGrepFiles(CodexGrepFilesOutput::Matches {
            content: "/w/a.rs\n/w/b.rs\n".into(),
            file_count: 2,
        }),
        &schema::<CodexGrepFilesOutput>(),
    );
    assert_eq!(paths, json!({"paths":["/w/a.rs","/w/b.rs"]}));
    let glob: ToolOutput = crate::implementations::opencode::glob::GlobOutput {
        tool_output_for_prompt: "/w/a.rs".into(),
        count: 1,
        total_count: 3,
        truncated: true,
        entries: vec!["/w/a.rs".into()],
        cwd_for_display: "/w".into(),
    }
    .into();
    assert_eq!(glob.to_prompt_format(), "/w/a.rs", "model text unchanged");
    let glob = conforms(
        glob,
        &schema::<crate::implementations::opencode::glob::GlobOutput>(),
    );
    assert_eq!(
        glob,
        json!({"paths":["/w/a.rs"],"total_count":3,"truncated":true})
    );
}

#[test]
fn edits_and_patches_report_locations_and_failures_reject() {
    let edit = conforms(
        ToolOutput::SearchReplace(SearchReplaceOutput::EditsApplied(
            SearchReplaceEditsApplied {
                old_string: "a".into(),
                new_string: "b".into(),
                tool_output_for_prompt: "The file has been updated.".into(),
                tool_output_for_prompt_concise: None,
                absolute_path: "/w/a.rs".into(),
                edits: SearchReplaceEditContextInformation {
                    details: vec![SearchReplaceEditDetail {
                        old_string: "a".into(),
                        old_line: 4,
                        new_string: "b".into(),
                        new_line: 4,
                        context_before: String::new(),
                        context_after: String::new(),
                        line_prefix: String::new(),
                    }],
                },
                patch: None,
                unicode_normalized: false,
            },
        )),
        &schema::<SearchReplaceOutput>(),
    );
    assert_eq!(
        edit,
        json!({"path":"/w/a.rs","edits":[{"old_line":4,"new_line":4}]})
    );
    rejects(ToolOutput::SearchReplace(
        SearchReplaceOutput::NoMatchesFound(NoMatchesFoundError {
            message: "String not found".into(),
            file_path: "/w/a.rs".into(),
            file_snapshot_at_edit: None,
        }),
    ));
    conforms(
        ToolOutput::ApplyPatch(ApplyPatchOutput::Success {
            files: vec![ApplyPatchFileResult {
                path: "/w/a.rs".into(),
                action: "moved".into(),
                old_text: None,
                new_text: String::new(),
                move_to: Some("/w/b.rs".into()),
            }],
            tool_output_for_prompt: "Done".into(),
        }),
        &schema::<ApplyPatchOutput>(),
    );
    rejects(ToolOutput::ApplyPatch(ApplyPatchOutput::ParseError(
        "bad hunk".into(),
    )));
}

fn bash(exit_code: i32) -> BashOutput {
    BashOutput {
        output: b"\x1b[31mred\x1b[0m\n".to_vec(),
        output_for_prompt: "red\n".into(),
        exit_code,
        command: "false".into(),
        truncated: false,
        signal: None,
        timed_out: false,
        description: None,
        current_dir: "/w".into(),
        output_file: "/tmp/out".into(),
        total_bytes: 4,
        output_delta: None,
        was_bare_echo: false,
    }
}

#[test]
fn shell_results_resolve_even_with_a_failing_exit_code() {
    let schema = schema::<BashToolOutput>();
    let value = conforms(ToolOutput::Bash(bash(1)), &schema);
    assert_eq!(value["kind"], "completed");
    assert_eq!(value["exit_code"], 1);
    assert_eq!(value["output"], "red\n", "ANSI escapes removed");
    let background = conforms(
        ToolOutput::BackgroundTaskStarted(BackgroundTaskStarted {
            task_id: "t1".into(),
            task_type: "bash".into(),
            output_file: "/tmp/t1".into(),
            status: "running".into(),
            command: "sleep 9".into(),
            summary: String::new(),
            retrieval_hint: String::new(),
            pre_formatted: None,
            pid: Some(7),
        }),
        &schema,
    );
    assert_eq!(background["kind"], "background");
    assert_eq!(background["task_id"], "t1");
}

#[test]
fn task_values_and_handles() {
    let task = TaskOutputResult {
        task_id: "t1".into(),
        command: "make".into(),
        status: "completed".into(),
        exit_code: Some(0),
        duration_secs: 1.5,
        output: "ok".into(),
        ..Default::default()
    };
    let schema = schema::<TaskOutputOutput>();
    let single = conforms(
        ToolOutput::TaskOutput(TaskOutputOutput::Result(task.clone())),
        &schema,
    );
    assert_eq!(single["kind"], "task");
    assert_eq!(single["exit_code"], 0);
    let multi = conforms(
        ToolOutput::TaskOutput(TaskOutputOutput::MultiResult(MultiTaskOutputResult {
            mode: "all".into(),
            results: vec![task],
            summary: "1 done".into(),
        })),
        &schema,
    );
    assert_eq!(multi["tasks"][0]["task_id"], "t1");
    rejects(ToolOutput::TaskOutput(TaskOutputOutput::TaskNotFound(
        "no task".into(),
    )));
    conforms(
        ToolOutput::KillTask(KillTaskOutput::Result(KillTaskResult {
            task_id: "t1".into(),
            outcome: "killed".into(),
            message: "stopped".into(),
        })),
        &schema::<KillTaskOutput>(),
    );

    let subagent_schema = schema_of::<SubagentValue>();
    let completed = conforms(
        ToolOutput::SubagentCompleted(SubagentCompletedOutput {
            output: "answer".into(),
            subagent_id: "s1".into(),
            subagent_type: "general-purpose".into(),
            tool_calls: 3,
            turns: 2,
            duration_ms: 10,
            worktree_path: None,
            persona: None,
            resume_from_hint: "s1".into(),
            persona_hint: None,
        }),
        &subagent_schema,
    );
    assert_eq!(completed["output"], "answer");
    let background = ToolOutput::Text(TextOutput::with_data(
        "Started subagent s2 in the background.",
        serde_json::to_value(SubagentValue::Background {
            subagent_id: "s2".into(),
            auto_backgrounded: false,
        })
        .unwrap(),
    ));
    assert_eq!(
        background.to_prompt_format(),
        "Started subagent s2 in the background."
    );
    assert_eq!(conforms(background, &subagent_schema)["subagent_id"], "s2");
}

#[test]
fn mcp_values_decode_structured_content_and_json_text() {
    let output_schema =
        json!({"type":"object","properties":{"id":{"type":"integer"}},"required":["id"]});
    let schema = mcp_value_schema(Some(&output_schema));
    assert_eq!(mcp_output_schema(&schema), Some(output_schema));
    let mcp = |text: &str, structured: Option<Value>| {
        ToolOutput::MCP(
            MCPOutput::okay_output("s__t".into(), "s".into(), format!("{text}\n<rendered>"))
                .with_protocol_parts(text.to_owned(), structured),
        )
    };
    let structured = conforms(mcp("Created 7", Some(json!({"id": 7}))), &schema);
    assert_eq!(structured, json!({"text":"Created 7","data":{"id":7}}));
    let generic = mcp_value_schema(None);
    assert_eq!(
        conforms(mcp(r#"{"id": 1}"#, None), &generic)["data"],
        json!({"id":1})
    );
    assert_eq!(
        conforms(mcp("{\"id\":1}\n{\"id\":2}", None), &generic)["data"],
        json!([{"id":1},{"id":2}])
    );
    assert_eq!(
        conforms(mcp("plain words", None), &generic)["data"],
        Value::Null
    );
    // A producer that kept only the joined rendering still yields its text.
    let joined = ToolOutput::MCP(MCPOutput::okay_output(
        "s__t".into(),
        "s".into(),
        "[1,2]".into(),
    ));
    assert_eq!(
        conforms(joined, &generic),
        json!({"text":"[1,2]","data":[1,2]})
    );
    let failed = ToolOutput::MCP(MCPOutput::errored("s__t".into(), "s".into(), "boom".into()));
    assert!(rejects(failed).contains("boom"));
}

#[test]
fn web_todo_media_and_misc_values() {
    conforms(
        ToolOutput::WebSearch(WebSearchOutput {
            query: "rust".into(),
            content: "Rust is".into(),
            citations: vec!["https://rust-lang.org".into()],
            allowed_domains: None,
            pre_formatted: None,
        }),
        &schema::<WebSearchOutput>(),
    );
    conforms(
        ToolOutput::WebFetch(WebFetchOutput::Content(WebFetchContent {
            url: "https://x.test".into(),
            content: "# Page".into(),
            content_type: "markdown".into(),
            status_code: 200,
            bytes: 6,
            source_artifact: None,
            inline_fallback: None,
            output_location: None,
        })),
        &schema::<WebFetchOutput>(),
    );
    rejects(ToolOutput::WebFetch(WebFetchOutput::DomainNotAllowed(
        "x.test".into(),
    )));
    let todo: TodoItem =
        serde_json::from_value(json!({"content":"ship","status":"in_progress"})).unwrap();
    let todos = conforms(
        ToolOutput::Todo(TodoWriteOutput::TodosUpdated(TodoWriteSuccess {
            summary_for_prompt: "1 todo".into(),
            todos: vec![todo],
            state: Default::default(),
        })),
        &schema::<TodoWriteOutput>(),
    );
    assert_eq!(
        todos["todos"][0],
        json!({"content":"ship","status":"in_progress","priority":"medium"})
    );
    let media = conforms(
        ToolOutput::ImageGen(MediaGenOutput::new("/s/images/1.jpg".into())),
        &schema_of::<MediaValue>(),
    );
    assert_eq!(media["path"], "/s/images/1.jpg");
    assert_eq!(media["filename"], "1.jpg");
    let uploaded = conforms(
        ToolOutput::ImageToVideo(MediaGenOutput::uploaded("https://u.test/v".into())),
        &schema_of::<MediaValue>(),
    );
    assert_eq!(uploaded["path"], Value::Null);
    conforms(
        ToolOutput::Skill(SkillOutput {
            success: true,
            tool_result: "loaded".into(),
            skill_name: "pdf".into(),
            skill_message: Some("Use pdftotext".into()),
            error: None,
        }),
        &schema::<SkillOutput>(),
    );
    conforms(
        ToolOutput::Text("hover: fn a()".into()),
        &schema::<LspToolOutput>(),
    );
    assert_eq!(
        ToolOutput::Dynamic(DynamicOutput::from(json!([1]))).script_value(),
        Ok(json!([1]))
    );
    let upsell = ToolOutput::Text(TextOutput::failure("Upgrade to use this"));
    assert!(!upsell.is_error(), "telemetry outcome unchanged");
    assert_eq!(rejects(upsell), "Upgrade to use this");
}

/// Each declared schema is a valid JSON Schema object.
#[test]
fn declared_schemas_compile() {
    for schema in [
        schema::<ReadFileOutput>(),
        schema::<ListDirOutput>(),
        schema::<GrepSearchOutput>(),
        schema::<SearchReplaceOutput>(),
        schema::<BashToolOutput>(),
        schema::<TodoWriteOutput>(),
        schema::<TaskOutputOutput>(),
        schema::<SendSubagentMessageOutput>(),
        schema::<MonitorOutput>(),
        schema::<SchedulerListOutput>(),
        schema::<WorkflowToolOutput>(),
        schema_of::<SubagentValue>(),
        schema_of::<MemorySearchValue>(),
        schema_of::<MemoryGetValue>(),
        mcp_value_schema(None),
    ] {
        assert!(schema.is_object(), "{schema}");
        jsonschema::validator_for(&schema).expect("compiles");
    }
    assert!(ToolOutput::script_schema().is_none());
}
