//! Tests for the Messages API conversion.

use super::messages::{apply_conversation_cache_breakpoint, ephemeral_cache_control};
use super::test_support::*;
use super::*;

fn messages_test_request(reasoning_effort: Option<crate::ReasoningEffort>) -> ConversationRequest {
    ConversationRequest {
        items: vec![ConversationItem::user("Hello")],
        model: Some("test-model".to_string()),
        reasoning_effort,
        ..Default::default()
    }
}

#[test]
fn json_schema_and_reasoning_effort_are_orthogonal_in_output_config() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": { "x": { "type": "string" } },
        "required": ["x"]
    });
    let mut req = ConversationRequest::from_items(vec![ConversationItem::user("go")])
        .with_json_schema(schema);
    req.reasoning_effort = Some(crate::ReasoningEffort::High);

    let msgs = build_messages_request(&req);
    let oc = msgs.output_config.expect("output_config present");
    assert_eq!(oc.effort.as_deref(), Some("high"));
    assert!(oc.format.is_some());
    assert!(
        msgs.thinking.is_some(),
        "thinking set when effort is present"
    );
}

#[test]
fn test_messages_request_wire_format_for_supported_variants() {
    for (variant, expected) in [
        (crate::ReasoningEffort::Low, "low"),
        (crate::ReasoningEffort::Medium, "medium"),
        (crate::ReasoningEffort::High, "high"),
        (crate::ReasoningEffort::Xhigh, "xhigh"),
        (crate::ReasoningEffort::Max, "max"),
    ] {
        let req = messages_test_request(Some(variant));
        let msgs = build_messages_request(&req);
        let json = serde_json::to_value(&msgs).unwrap();
        assert_eq!(
            json.pointer("/output_config/effort")
                .and_then(|v| v.as_str()),
            Some(expected),
            "{variant:?} should map to output_config.effort={expected:?}; got: {json:#}",
        );
        assert_eq!(
            json.pointer("/thinking/type").and_then(|v| v.as_str()),
            Some("adaptive"),
            "{variant:?} should auto-pair thinking.type=adaptive; got: {json:#}",
        );
    }
}

#[test]
fn test_messages_request_omits_output_config_when_no_supported_effort() {
    let none_or_unsupported = [
        None,
        Some(crate::ReasoningEffort::None),
        Some(crate::ReasoningEffort::Minimal),
    ];
    for input in none_or_unsupported {
        let req = messages_test_request(input);
        let msgs = build_messages_request(&req);
        assert!(
            msgs.output_config.is_none(),
            "input {input:?} must not produce output_config",
        );
        assert!(
            msgs.thinking.is_none(),
            "input {input:?} must not auto-pair thinking",
        );
    }
}

#[test]
fn test_messages_request_thinking_carries_summarized_display() {
    let req = ConversationRequest {
        reasoning_effort: Some(crate::ReasoningEffort::High),
        ..ConversationRequest::from_items(vec![ConversationItem::user("hi")])
            .with_model("messages-compatible-model")
    };
    let msg = build_messages_request(&req);
    let json = serde_json::to_value(&msg).unwrap();
    assert_eq!(
        json.pointer("/thinking/type").and_then(|v| v.as_str()),
        Some("adaptive"),
        "thinking.type should be 'adaptive'; got: {json:#}",
    );
    assert_eq!(
        json.pointer("/thinking/display").and_then(|v| v.as_str()),
        Some("summarized"),
        "thinking.display must be 'summarized' so 4.7+ surfaces thinking content; got: {json:#}",
    );
}

#[test]
fn test_messages_request_omits_thinking_when_effort_unset() {
    let req = ConversationRequest::from_items(vec![ConversationItem::user("hi")])
        .with_model("messages-compatible-model");
    let msg = build_messages_request(&req);
    let json = serde_json::to_value(&msg).unwrap();
    assert!(
        json.get("thinking").is_none()
            || json
                .pointer("/thinking")
                .map(|v| v.is_null())
                .unwrap_or(false),
        "thinking must be absent when reasoning_effort is unset; got: {json:#}",
    );
    assert!(
        json.get("output_config").is_none()
            || json
                .pointer("/output_config")
                .map(|v| v.is_null())
                .unwrap_or(false),
        "output_config must be absent when reasoning_effort is unset; got: {json:#}",
    );
}

#[test]
fn test_messages_request_previous_tip_skips_a_trailing_user_run() {
    let mut items = vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user("Fix the bug"),
    ];
    items.extend(agent_turn(0));
    items.extend(agent_turn(1));
    // The shape after a parallel batch: tool results, then followups.
    items.push(ConversationItem::user("[Image content]"));
    items.push(ConversationItem::user("<system-reminder>"));

    let json = serde_json::to_value(build_messages_request(
        &ConversationRequest::from_items(items).with_model("messages-compatible-model"),
    ))
    .unwrap();
    let messages = json["messages"].as_array().unwrap();

    let marked: Vec<usize> = (0..messages.len())
        .filter(|&i| marker_on_last_block(&messages[i]).is_some())
        .collect();
    let last_assistant = messages
        .iter()
        .rposition(|m| m["role"] == "assistant")
        .unwrap();
    assert_eq!(marked.len(), 2, "tip and previous tip only: {json:#}");
    assert_eq!(marked[1], messages.len() - 1, "tip: {json:#}");
    assert!(
        marked[0] < last_assistant,
        "the previous tip must sit before the last assistant turn, not inside \
             the trailing user run; got {marked:?} in {json:#}",
    );
}

#[test]
fn test_messages_request_cache_breakpoint_marks_an_image_tip() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::User(UserItem {
            content: vec![
                ContentPart::Text {
                    text: "what is in this screenshot".into(),
                },
                ContentPart::Image {
                    url: "data:image/png;base64,iVBOR".into(),
                },
            ],
            ..Default::default()
        }),
    ])
    .with_model("messages-compatible-model");

    let json = serde_json::to_value(build_messages_request(&req)).unwrap();
    let blocks = json["messages"][0]["content"].as_array().unwrap();

    assert_eq!(blocks.last().unwrap()["type"].as_str(), Some("image"));
    assert_eq!(
        marker_on_last_block(&json["messages"][0]),
        Some("ephemeral"),
        "{json:#}",
    );
    assert!(blocks[0].get("cache_control").is_none(), "{json:#}");
}

#[test]
fn test_messages_request_cache_breakpoint_skips_thinking() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("Fix the bug"),
        ConversationItem::Reasoning(synthesized_reasoning_item("weighing options")),
        ConversationItem::assistant("Fixed it."),
    ])
    .with_model("messages-compatible-model");

    let json = serde_json::to_value(build_messages_request(&req)).unwrap();
    let blocks = json["messages"][1]["content"].as_array().unwrap();

    let thinking = blocks
        .iter()
        .find(|b| b["type"] == "thinking")
        .expect("reasoning should emit a thinking block");
    assert!(thinking.get("cache_control").is_none(), "{json:#}");
    assert_eq!(
        marker_on_last_block(&json["messages"][1]),
        Some("ephemeral"),
        "{json:#}",
    );
}

#[test]
fn test_btw_stripped_reasoning_produces_no_thinking_blocks() {
    // Simulate a conversation where the model responded with thinking.
    let with_reasoning = ConversationItem::Assistant(AssistantItem {
        content: "Here is the answer.".into(),
        tool_calls: vec![],
        model_id: Some("messages-compatible-model".into()),
        model_fingerprint: None,
        reasoning_effort: None,
    });

    // Reasoning now lives as a sibling `ConversationItem::Reasoning`,
    // so "stripping reasoning" means filtering those siblings out — see
    // `strip_reasoning_blocks` in xai-chat-state. Here the assistant
    // never had a sibling Reasoning, so the strip is a no-op.
    let stripped = with_reasoning;

    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("hello"),
        stripped,
        ConversationItem::user("btw what is X?"),
    ]);

    let msg = build_messages_request(&req);
    let json = serde_json::to_value(&msg).unwrap();

    // No thinking blocks should appear in any message.
    let messages = json.get("messages").unwrap().as_array().unwrap();
    for (i, m) in messages.iter().enumerate() {
        if let Some(content) = m.get("content").and_then(|c| c.as_array()) {
            for block in content {
                assert_ne!(
                    block.get("type").and_then(|t| t.as_str()),
                    Some("thinking"),
                    "message[{i}] must not contain thinking blocks after stripping reasoning",
                );
            }
        }
    }

    // Top-level thinking must also be absent.
    assert!(
        json.get("thinking").is_none()
            || json
                .pointer("/thinking")
                .map(|v| v.is_null())
                .unwrap_or(false),
        "top-level thinking must be absent; got: {json:#}",
    );
}

#[test]
fn test_btw_mid_turn_truncation_removes_trailing_tool_use() {
    // Simulate a conversation that was snapshotted mid-turn: the last
    // assistant made a tool call that hasn't been answered yet.
    let mut items = vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user("Fix the bug"),
        ConversationItem::assistant("I'll look at the code."),
        // Completed tool call pair:
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_1".into(),
            name: "read_file".to_string(),
            arguments: r#"{"path": "src/main.rs"}"#.into(),
        }]),
        ConversationItem::tool_result("call_1", "fn main() {}"),
        ConversationItem::assistant("I see the issue. Let me fix it."),
        // Mid-turn: tool call with NO tool_result yet
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_2".into(),
            name: "search_replace".to_string(),
            arguments: "{}".into(),
        }]),
    ];

    // Apply the same truncation pattern as handle_side_question.
    while let Some(last) = items.last() {
        match last {
            ConversationItem::Assistant(a) if !a.tool_calls.is_empty() => {
                items.pop();
            }
            ConversationItem::ToolResult(_) => {
                items.pop();
            }
            _ => break,
        }
    }

    // Add the btw user question.
    items.push(ConversationItem::user("btw what is X?"));

    let msg = build_messages_request(&ConversationRequest::from_items(items.clone()));
    let json = serde_json::to_value(&msg).unwrap();
    let messages = json.get("messages").unwrap().as_array().unwrap();

    // The last message before the btw question should be a plain
    // assistant text (not a tool_use), so the request is valid.
    // Messages: user("Fix the bug"), asst("I'll look"), asst(tool_use call_1),
    //           user(tool_result call_1), asst("I see the issue"),
    //           user("btw what is X?")
    // The orphaned call_2 assistant must be gone.
    let last_assistant = messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("assistant"))
        .unwrap();
    if let Some(content) = last_assistant.get("content").and_then(|c| c.as_array()) {
        for block in content {
            assert_ne!(
                block.get("type").and_then(|t| t.as_str()),
                Some("tool_use"),
                "last assistant must not have unanswered tool_use blocks",
            );
        }
    }

    // Verify the original complete pair (call_1) survived.
    // system + user + asst_text + asst(call_1) + tool_result(call_1) + asst_text + user(btw) = 7
    assert_eq!(items.len(), 7);
}

#[test]
fn test_btw_cross_api_messages_no_regressions() {
    let items = btw_prepare_items(btw_mid_turn_conversation());
    let req = ConversationRequest::from_items(items);
    let msg = build_messages_request(&req);
    let json = serde_json::to_value(&msg).unwrap();

    let messages = json.get("messages").unwrap().as_array().unwrap();

    // No thinking blocks anywhere.
    for (i, m) in messages.iter().enumerate() {
        if let Some(content) = m.get("content").and_then(|c| c.as_array()) {
            for block in content {
                assert_ne!(
                    block.get("type").and_then(|t| t.as_str()),
                    Some("thinking"),
                    "messages[{i}] must not contain thinking blocks",
                );
            }
        }
    }

    // Last assistant message must not have unanswered tool_use.
    let last_assistant = messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("assistant"))
        .expect("should have an assistant message");
    if let Some(content) = last_assistant.get("content").and_then(|c| c.as_array()) {
        for block in content {
            assert_ne!(
                block.get("type").and_then(|t| t.as_str()),
                Some("tool_use"),
                "last assistant in btw request must not have unanswered tool_use",
            );
        }
    }

    // Top-level thinking must be absent (no reasoning_effort set).
    assert!(
        json.get("thinking").is_none() || json.pointer("/thinking").is_some_and(|v| v.is_null()),
        "top-level thinking must be absent; got: {json:#}",
    );

    // Temperature must be absent (not hardcoded).
    assert!(
        json.get("temperature").is_none()
            || json.pointer("/temperature").is_some_and(|v| v.is_null()),
        "temperature must be absent so proxy defaults can apply; got: {json:#}",
    );

    // The completed tool pair (call_1) must survive.
    let has_tool_use_call_1 = messages.iter().any(|m| {
        m.get("content")
            .and_then(|c| c.as_array())
            .is_some_and(|blocks| {
                blocks.iter().any(|b| {
                    b.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                        && b.get("id").and_then(|id| id.as_str()) == Some("call_1")
                })
            })
    });
    assert!(
        has_tool_use_call_1,
        "completed tool_use call_1 must survive"
    );

    let has_tool_result_call_1 = messages.iter().any(|m| {
        m.get("content")
            .and_then(|c| c.as_array())
            .is_some_and(|blocks| {
                blocks.iter().any(|b| {
                    b.get("type").and_then(|t| t.as_str()) == Some("tool_result")
                        && b.get("tool_use_id").and_then(|id| id.as_str()) == Some("call_1")
                })
            })
    });
    assert!(
        has_tool_result_call_1,
        "completed tool_result for call_1 must survive"
    );
}

#[test]
fn test_tool_result_with_images_to_anthropic() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("Read this"),
        ConversationItem::Assistant(AssistantItem {
            content: String::new().into(),
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                name: "read_file".to_string(),
                arguments: "{}".into(),
            }],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        ConversationItem::tool_result_with_images(
            "call_1",
            "Read image file: photo.png",
            vec![ContentPart::Image {
                url: "data:image/png;base64,iVBOR".into(),
            }],
        ),
    ]);

    let messages_req = build_messages_request(&req);

    // Find the user message that contains the tool result
    // (the Messages API wraps tool results in user messages)
    let tool_result_msg = messages_req
        .messages
        .iter()
        .find(|m| {
            if let crate::messages::MessageContent::Blocks(blocks) = &m.content {
                blocks
                    .iter()
                    .any(|b| matches!(b, crate::messages::ContentBlock::ToolResult { .. }))
            } else {
                false
            }
        })
        .expect("Expected a message with ToolResult block");

    let crate::messages::MessageContent::Blocks(blocks) = &tool_result_msg.content else {
        panic!("Expected Blocks");
    };
    let tool_result_block = blocks
        .iter()
        .find_map(|b| {
            if let crate::messages::ContentBlock::ToolResult { content, .. } = b {
                Some(content)
            } else {
                None
            }
        })
        .unwrap();

    // Should be Blocks variant with text + image, not Text
    let crate::messages::ToolResultContent::Blocks(inner) = tool_result_block else {
        panic!("Expected ToolResultContent::Blocks, got Text");
    };
    assert_eq!(inner.len(), 2);
    assert!(
        matches!(&inner[0], crate::messages::ContentBlock::Text { text, .. } if text == "Read image file: photo.png")
    );
    assert!(
        matches!(&inner[1], crate::messages::ContentBlock::Image { source: crate::messages::ImageSource::Base64 { media_type, data }, .. } if media_type == "image/png" && data == "iVBOR")
    );
}

#[test]
fn upgrade_legacy_reasoning_singular_anthropic_no_id() {
    // Messages streaming sets id = "" (see stream/messages.rs:340).
    // The upgrader must still emit a sibling carrying text + signature.
    let raw = serde_json::json!({
        "type": "assistant",
        "content": "answer",
        "reasoning": {
            "text": "Let me think about this...",
            "encrypted": "signature-bytes-here",
            "id": ""
        },
        "model_id": "messages-compatible-model"
    });
    let mut seen = std::collections::HashSet::new();
    let siblings = upgrade_legacy_reasoning(&raw, &mut seen);
    assert_eq!(siblings.len(), 1);
    let ConversationItem::Reasoning(r) = &siblings[0] else {
        panic!("expected Reasoning sibling");
    };
    assert_eq!(r.id, "");
    assert_eq!(r.encrypted_content.as_deref(), Some("signature-bytes-here"));
}

fn messages_cache_test_request() -> ConversationRequest {
    ConversationRequest {
        items: vec![
            ConversationItem::system("Stable system prompt"),
            ConversationItem::user("Hello"),
        ],
        model: Some("test-model".to_string()),
        ..Default::default()
    }
}

#[test]
fn messages_default_cache_policy_keeps_legacy_5m_wire_shape() {
    let json =
        serde_json::to_value(build_messages_request(&messages_cache_test_request())).unwrap();
    let cache_control = json
        .pointer("/system/0/cache_control")
        .expect("default policy should cache the stable system prefix");
    assert_eq!(
        cache_control
            .get("type")
            .and_then(serde_json::Value::as_str),
        Some("ephemeral")
    );
    assert!(
        cache_control.get("ttl").is_none(),
        "default 5m must stay implicit on the wire: {json:#}"
    );
}

#[test]
fn messages_one_hour_cache_policy_serializes_explicit_ttl() {
    let json = serde_json::to_value(build_messages_request_with_cache_policy(
        &messages_cache_test_request(),
        crate::PromptCachePolicy::STABLE_PREFIX_1H,
    ))
    .unwrap();
    assert_eq!(
        json.pointer("/system/0/cache_control/ttl")
            .and_then(serde_json::Value::as_str),
        Some("1h"),
        "one-hour cache must carry an explicit TTL: {json:#}"
    );
}

#[test]
fn messages_off_cache_policy_emits_no_breakpoint() {
    let json = serde_json::to_value(build_messages_request_with_cache_policy(
        &messages_cache_test_request(),
        crate::PromptCachePolicy::OFF,
    ))
    .unwrap();
    assert!(
        json.pointer("/system/0/cache_control").is_none()
            && !json.to_string().contains("cache_control"),
        "off policy must not emit a cache breakpoint: {json:#}"
    );
    assert_eq!(
        json.get("system").and_then(serde_json::Value::as_str),
        Some("Stable system prompt"),
        "a single uncached system block should retain the compact text form"
    );
}

#[test]
fn messages_stable_prefix_also_caches_conversation_tail() {
    // The final message (here the sole user turn) must carry a second
    // ephemeral breakpoint so the growing conversation prefix is cached and
    // read back on the next agentic step, not just the static system block.
    let json =
        serde_json::to_value(build_messages_request(&messages_cache_test_request())).unwrap();
    let tail = json
        .pointer("/messages/0/content/0/cache_control")
        .expect("stable-prefix must cache the last message's tail block");
    assert_eq!(
        tail.get("type").and_then(serde_json::Value::as_str),
        Some("ephemeral")
    );
    assert!(
        tail.get("ttl").is_none(),
        "default 5m must stay implicit on the conversation breakpoint too: {json:#}"
    );
    // System and conversation breakpoints must coexist on the same request
    // (exactly two), guarding against a refactor dropping either one.
    assert!(
        json.pointer("/system/0/cache_control").is_some(),
        "the system-prefix breakpoint must remain alongside the conversation one: {json:#}"
    );
    assert_eq!(
        json.to_string().matches("cache_control").count(),
        2,
        "exactly two breakpoints (system + conversation) expected: {json:#}"
    );
}

#[test]
fn messages_stable_prefix_caches_only_last_tool_definition() {
    let request = ConversationRequest {
        tools: vec![
            ToolSpec {
                name: "read_file".to_string(),
                description: Some("Read a file".to_string()),
                parameters: serde_json::json!({"type": "object"}),
            },
            ToolSpec {
                name: "run_command".to_string(),
                description: Some("Run a command".to_string()),
                parameters: serde_json::json!({"type": "object"}),
            },
        ],
        ..messages_cache_test_request()
    };

    let json = serde_json::to_value(build_messages_request(&request)).unwrap();
    assert!(
        json.pointer("/tools/0/cache_control").is_none(),
        "only the final tool definition should carry a breakpoint: {json:#}"
    );
    assert_eq!(
        json.pointer("/tools/1/cache_control/type")
            .and_then(serde_json::Value::as_str),
        Some("ephemeral")
    );
    assert_eq!(
        json.to_string().matches("cache_control").count(),
        3,
        "expected tools + system + conversation breakpoints: {json:#}"
    );

    let off_json = serde_json::to_value(build_messages_request_with_cache_policy(
        &request,
        crate::PromptCachePolicy::OFF,
    ))
    .unwrap();
    assert!(
        !off_json.to_string().contains("cache_control"),
        "off policy must omit the tool breakpoint too: {off_json:#}"
    );
}

#[test]
fn conversation_breakpoint_converts_plain_text_message() {
    // Defensive `MessageContent::Text` arm: the current builder always emits
    // `Blocks`, but if a bare-text message reaches the helper it must be
    // promoted to a single cached text block rather than silently skipped.
    use crate::messages::{ContentBlock, Message, MessageContent, MessageRole};
    let mut messages = vec![Message {
        role: MessageRole::User,
        content: MessageContent::Text("plain user turn".to_string()),
    }];
    apply_conversation_cache_breakpoint(
        &mut messages,
        ephemeral_cache_control(crate::PromptCacheTtl::OneHour),
    );
    let MessageContent::Blocks(blocks) = &messages[0].content else {
        panic!("a bare-text message must be promoted to Blocks");
    };
    assert!(
        matches!(
            &blocks[0],
            ContentBlock::Text { text, cache_control: Some(_) } if text == "plain user turn"
        ),
        "the promoted text block must preserve its text and carry the breakpoint"
    );
}

#[test]
fn conversation_breakpoint_skips_empty_text_message() {
    // An empty bare-text message must not be promoted to an (invalid) empty
    // cached text block; leave it untouched.
    use crate::messages::{Message, MessageContent, MessageRole};
    let mut messages = vec![Message {
        role: MessageRole::User,
        content: MessageContent::Text(String::new()),
    }];
    apply_conversation_cache_breakpoint(
        &mut messages,
        ephemeral_cache_control(crate::PromptCacheTtl::OneHour),
    );
    assert!(
        matches!(&messages[0].content, MessageContent::Text(t) if t.is_empty()),
        "empty text must stay an untouched no-op, not an empty cached block"
    );
}

#[test]
fn conversation_breakpoint_lands_on_last_cacheable_block() {
    // Multiple cacheable blocks: the breakpoint must land on the LAST one
    // (the ToolResult), not the earlier Text.
    use crate::messages::{ContentBlock, Message, MessageContent, MessageRole, ToolResultContent};
    let mut messages = vec![Message {
        role: MessageRole::User,
        content: MessageContent::Blocks(vec![
            ContentBlock::Text {
                text: "here are the results".to_string(),
                cache_control: None,
            },
            ContentBlock::ToolResult {
                tool_use_id: "tool-1".to_string(),
                content: ToolResultContent::Text("output".to_string()),
                cache_control: None,
            },
        ]),
    }];
    apply_conversation_cache_breakpoint(
        &mut messages,
        ephemeral_cache_control(crate::PromptCacheTtl::OneHour),
    );
    let MessageContent::Blocks(blocks) = &messages[0].content else {
        panic!("expected blocks");
    };
    assert!(
        matches!(
            &blocks[0],
            ContentBlock::Text {
                cache_control: None,
                ..
            }
        ),
        "the earlier Text block must not carry the breakpoint"
    );
    assert!(
        matches!(
            &blocks[1],
            ContentBlock::ToolResult {
                cache_control: Some(_),
                ..
            }
        ),
        "the breakpoint must land on the last cacheable block (ToolResult)"
    );
}

#[test]
fn conversation_breakpoint_skips_empty_trailing_tool_result() {
    // Reachable agentic case: a tool returns empty output as the final
    // block. The breakpoint must skip the empty ToolResult and land on the
    // preceding non-empty Text, never marking an empty block.
    use crate::messages::{ContentBlock, Message, MessageContent, MessageRole, ToolResultContent};
    let mut messages = vec![Message {
        role: MessageRole::User,
        content: MessageContent::Blocks(vec![
            ContentBlock::Text {
                text: "context".to_string(),
                cache_control: None,
            },
            ContentBlock::ToolResult {
                tool_use_id: "tool-1".to_string(),
                content: ToolResultContent::Text(String::new()),
                cache_control: None,
            },
        ]),
    }];
    apply_conversation_cache_breakpoint(
        &mut messages,
        ephemeral_cache_control(crate::PromptCacheTtl::OneHour),
    );
    let MessageContent::Blocks(blocks) = &messages[0].content else {
        panic!("expected blocks");
    };
    assert!(
        matches!(
            &blocks[0],
            ContentBlock::Text {
                cache_control: Some(_),
                ..
            }
        ),
        "breakpoint must fall back to the non-empty Text block"
    );
    assert!(
        matches!(
            &blocks[1],
            ContentBlock::ToolResult {
                cache_control: None,
                ..
            }
        ),
        "the empty ToolResult must not carry a breakpoint"
    );
}

#[test]
fn messages_one_hour_cache_extends_to_conversation_tail() {
    let json = serde_json::to_value(build_messages_request_with_cache_policy(
        &messages_cache_test_request(),
        crate::PromptCachePolicy::STABLE_PREFIX_1H,
    ))
    .unwrap();
    assert_eq!(
        json.pointer("/messages/0/content/0/cache_control/ttl")
            .and_then(serde_json::Value::as_str),
        Some("1h"),
        "one-hour policy must extend the explicit TTL to the conversation breakpoint: {json:#}"
    );
}

#[test]
fn conversation_breakpoint_lands_on_tool_result_tail() {
    // The real agentic case: the final message is a user turn whose last
    // block is a ToolResult (Blocks path, not the Text->Blocks conversion).
    use crate::messages::{ContentBlock, Message, MessageContent, MessageRole, ToolResultContent};
    let mut messages = vec![Message {
        role: MessageRole::User,
        content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
            tool_use_id: "tool-1".to_string(),
            content: ToolResultContent::Text("output".to_string()),
            cache_control: None,
        }]),
    }];
    apply_conversation_cache_breakpoint(
        &mut messages,
        ephemeral_cache_control(crate::PromptCacheTtl::OneHour),
    );
    let json = serde_json::to_value(&messages).unwrap();
    assert_eq!(
        json.pointer("/0/content/0/cache_control/ttl")
            .and_then(serde_json::Value::as_str),
        Some("1h"),
        "the agentic ToolResult tail must carry the conversation breakpoint: {json:#}"
    );
}

#[test]
fn conversation_breakpoint_marks_trailing_tool_use_block() {
    // A production-shaped assistant turn: [Text, ToolUse]. The current wire
    // shape lets ToolUse carry the breakpoint, so it should land at the tip.
    use crate::messages::{ContentBlock, Message, MessageContent, MessageRole};
    let mut messages = vec![Message {
        role: MessageRole::Assistant,
        content: MessageContent::Blocks(vec![
            ContentBlock::Text {
                text: "let me call a tool".to_string(),
                cache_control: None,
            },
            ContentBlock::ToolUse {
                id: "call-1".to_string(),
                name: "search".to_string(),
                input: serde_json::json!({"q": "x"}),
                cache_control: None,
            },
        ]),
    }];
    apply_conversation_cache_breakpoint(
        &mut messages,
        ephemeral_cache_control(crate::PromptCacheTtl::FiveMinutes),
    );
    let MessageContent::Blocks(blocks) = &messages[0].content else {
        panic!("expected blocks");
    };
    assert!(
        matches!(
            &blocks[0],
            ContentBlock::Text {
                cache_control: None,
                ..
            }
        ),
        "the earlier Text block must remain unmarked"
    );
    let json = serde_json::to_value(&messages).unwrap();
    assert!(
        json.pointer("/0/content/1/cache_control").is_some(),
        "the trailing tool_use block must carry cache_control: {json:#}"
    );
}

#[test]
fn conversation_breakpoint_noops_without_cacheable_block() {
    // No Text/ToolResult block anywhere in the final message: a whole-message
    // no-op (that turn's prefix simply isn't cached), never a panic.
    use crate::messages::{ContentBlock, Message, MessageContent, MessageRole};
    let mut messages = vec![Message {
        role: MessageRole::Assistant,
        content: MessageContent::Blocks(vec![ContentBlock::Thinking {
            thinking: "only thinking".to_string(),
            signature: String::new(),
        }]),
    }];
    apply_conversation_cache_breakpoint(
        &mut messages,
        ephemeral_cache_control(crate::PromptCacheTtl::OneHour),
    );
    let json = serde_json::to_value(&messages).unwrap();
    assert!(
        !json.to_string().contains("cache_control"),
        "a message with no cacheable block must stay a no-op: {json:#}"
    );
}
