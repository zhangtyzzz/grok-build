//! Messages API wire format.

use super::*;

/// Whether a tool-result payload carries no cacheable content (so a cache
/// breakpoint would sit on an empty block).
fn tool_result_content_is_empty(content: &crate::messages::ToolResultContent) -> bool {
    use crate::messages::ToolResultContent;
    match content {
        ToolResultContent::Text(text) => text.is_empty(),
        ToolResultContent::Blocks(blocks) => blocks.is_empty(),
    }
}

/// Build an `ephemeral` cache breakpoint for the given TTL. Anthropic defaults
/// an omitted TTL to five minutes, so the 5m case leaves `ttl` absent to
/// preserve the legacy wire shape; one hour must be explicit. Shared by the
/// system-prefix and conversation-prefix breakpoints so their wire encoding
/// cannot drift.
pub(super) fn ephemeral_cache_control(ttl: crate::PromptCacheTtl) -> crate::messages::CacheControl {
    crate::messages::CacheControl {
        r#type: "ephemeral".to_string(),
        ttl: match ttl {
            crate::PromptCacheTtl::FiveMinutes => None,
            crate::PromptCacheTtl::OneHour => Some(crate::PromptCacheTtl::OneHour),
        },
    }
}

/// Marks the last non-empty block that can carry a cache breakpoint, scanning
/// back past `Thinking`, which the API rejects a breakpoint on.
fn mark_message_cache_breakpoint(
    msg: &mut crate::messages::Message,
    cache_control: crate::messages::CacheControl,
) -> bool {
    use crate::messages::{ContentBlock, MessageContent};

    match &mut msg.content {
        // Plain text cannot carry a breakpoint, so promote it to block form.
        MessageContent::Text(text) => {
            // Anthropic rejects empty cached text blocks ("text content blocks
            // must be non-empty"). This arm is unreachable from the current
            // builder (it always emits `Blocks`), but guard the empty case so a
            // future bare-text path can never mint an invalid breakpoint.
            if text.is_empty() {
                return false;
            }
            let text = std::mem::take(text);
            msg.content = MessageContent::Blocks(vec![ContentBlock::Text {
                text,
                cache_control: Some(cache_control),
            }]);
            true
        }
        MessageContent::Blocks(blocks) => {
            // Land on the last *non-empty* cacheable block, skipping trailing
            // thinking blocks and any empty Text / ToolResult. Image and ToolUse
            // blocks can carry a breakpoint on the current Messages wire shape.
            for block in blocks.iter_mut().rev() {
                let target = match block {
                    ContentBlock::Text {
                        text,
                        cache_control: cc,
                    } if !text.is_empty() => Some(cc),
                    ContentBlock::ToolResult {
                        content,
                        cache_control: cc,
                        ..
                    } if !tool_result_content_is_empty(content) => Some(cc),
                    ContentBlock::Image {
                        cache_control: cc, ..
                    }
                    | ContentBlock::ToolUse {
                        cache_control: cc, ..
                    } => Some(cc),
                    ContentBlock::Text { .. }
                    | ContentBlock::ToolResult { .. }
                    | ContentBlock::Thinking { .. }
                    | ContentBlock::RedactedThinking { .. } => None,
                };
                if let Some(target) = target {
                    *target = Some(cache_control);
                    return true;
                }
            }
            false
        }
    }
}

/// An entry is written only at a breakpoint, so marking the system prompt alone
/// leaves the transcript uncached. The third covers a turn that appends more
/// than the API's 20 block lookback. A tools-prefix breakpoint, when present,
/// uses the fourth and final explicit slot.
fn apply_cache_breakpoints(
    system_blocks: &mut [crate::messages::TextBlock],
    messages: &mut [crate::messages::Message],
    cache_control: crate::messages::CacheControl,
) {
    use crate::messages::MessageRole;

    if let Some(last) = system_blocks.last_mut() {
        last.cache_control = Some(cache_control.clone());
    }

    let tip = (0..messages.len())
        .rev()
        .find(|&i| mark_message_cache_breakpoint(&mut messages[i], cache_control.clone()));

    // Where the previous request ended. A turn can append several user messages
    // in a row, so skip the whole trailing run rather than a neighbour of the tip.
    if let Some(tip) = tip
        && let Some(prev) = messages[..tip]
            .iter()
            .rposition(|m| matches!(m.role, MessageRole::Assistant))
            .and_then(|assistant| {
                messages[..assistant]
                    .iter()
                    .rposition(|m| matches!(m.role, MessageRole::User))
            })
    {
        mark_message_cache_breakpoint(&mut messages[prev], cache_control);
    }
}

#[cfg(test)]
pub(super) fn apply_conversation_cache_breakpoint(
    messages: &mut [crate::messages::Message],
    cache_control: crate::messages::CacheControl,
) {
    if let Some(last) = messages.last_mut() {
        mark_message_cache_breakpoint(last, cache_control);
    }
}

/// Convert a `ConversationRequest` into a Messages API request using the
/// backwards-compatible five-minute stable-prefix cache policy.
pub fn build_messages_request(req: &ConversationRequest) -> crate::messages::MessagesRequest {
    build_messages_request_with_cache_policy(req, crate::PromptCachePolicy::default())
}

/// Convert a `ConversationRequest` into a Messages API request with an explicit
/// prompt-cache policy.
pub fn build_messages_request_with_cache_policy(
    req: &ConversationRequest,
    prompt_cache: crate::PromptCachePolicy,
) -> crate::messages::MessagesRequest {
    use crate::PromptCacheMode;
    use crate::messages::{
        ContentBlock, ImageSource, Message, MessageContent, MessageRole, MessagesRequest,
        OutputConfig, SystemParam, TextBlock, ToolChoiceParam, ToolParam, ToolResultContent,
    };

    let mut system_blocks: Vec<TextBlock> = Vec::new();
    let mut messages: Vec<Message> = Vec::new();
    let mut pending_assistant: Vec<ContentBlock> = Vec::new();
    let mut pending_tool_results: Vec<ContentBlock> = Vec::new();

    let sanitize_tool_call_id = |id: &str| -> String {
        id.chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };

    let content_parts_to_anthropic_blocks = |parts: &[ContentPart]| -> Vec<ContentBlock> {
        parts
            .iter()
            .map(|part| match part {
                ContentPart::Text { text } => ContentBlock::Text {
                    text: text.as_ref().to_owned(),
                    cache_control: None,
                },
                ContentPart::Image { url } => {
                    if url.starts_with("data:") {
                        if let Some((header, data)) = url.split_once(',') {
                            let media_type = header
                                .strip_prefix("data:")
                                .and_then(|h| h.strip_suffix(";base64"))
                                .unwrap_or("image/png")
                                .to_string();
                            ContentBlock::Image {
                                source: ImageSource::Base64 {
                                    media_type,
                                    data: data.to_string(),
                                },
                                cache_control: None,
                            }
                        } else {
                            // Malformed data URI, treat as text
                            ContentBlock::Text {
                                text: format!("[invalid image: {}]", url),
                                cache_control: None,
                            }
                        }
                    } else if url.starts_with("http://") || url.starts_with("https://") {
                        ContentBlock::Image {
                            source: ImageSource::Url {
                                url: url.as_ref().to_owned(),
                            },
                            cache_control: None,
                        }
                    } else {
                        // Unknown format, treat as text
                        ContentBlock::Text {
                            text: format!("[image: {}]", url),
                            cache_control: None,
                        }
                    }
                }
            })
            .collect()
    };

    let flush_assistant = |pending: &mut Vec<ContentBlock>, msgs: &mut Vec<Message>| {
        if !pending.is_empty() {
            msgs.push(Message {
                role: MessageRole::Assistant,
                content: MessageContent::Blocks(pending.clone()),
            });
            pending.clear();
        }
    };

    let flush_tool_results = |pending: &mut Vec<ContentBlock>, msgs: &mut Vec<Message>| {
        if !pending.is_empty() {
            msgs.push(Message {
                role: MessageRole::User,
                content: MessageContent::Blocks(pending.clone()),
            });
            pending.clear();
        }
    };

    for item in &req.items {
        match item {
            ConversationItem::System(s) => {
                flush_assistant(&mut pending_assistant, &mut messages);
                flush_tool_results(&mut pending_tool_results, &mut messages);
                system_blocks.push(TextBlock {
                    r#type: "text".to_string(),
                    text: s.content.as_ref().to_owned(),
                    cache_control: None,
                });
            }
            ConversationItem::User(u) => {
                flush_assistant(&mut pending_assistant, &mut messages);
                flush_tool_results(&mut pending_tool_results, &mut messages);
                let blocks = content_parts_to_anthropic_blocks(&u.content);
                messages.push(Message {
                    role: MessageRole::User,
                    content: MessageContent::Blocks(blocks),
                });
            }
            ConversationItem::Assistant(a) => {
                flush_tool_results(&mut pending_tool_results, &mut messages);

                if !a.content.is_empty() {
                    pending_assistant.push(ContentBlock::Text {
                        text: a.content.as_ref().to_owned(),
                        cache_control: None,
                    });
                }

                for tc in &a.tool_calls {
                    let input =
                        serde_json::from_str(&tc.arguments).unwrap_or(serde_json::json!({}));
                    pending_assistant.push(ContentBlock::ToolUse {
                        id: sanitize_tool_call_id(&tc.id),
                        name: tc.name.clone(),
                        input,
                        cache_control: None,
                    });
                }
            }
            ConversationItem::ToolResult(t) => {
                flush_assistant(&mut pending_assistant, &mut messages);
                let content = if t.images.is_empty() {
                    ToolResultContent::Text(t.content.as_ref().to_owned())
                } else {
                    let mut blocks = vec![ContentBlock::Text {
                        text: t.content.as_ref().to_owned(),
                        cache_control: None,
                    }];
                    for img in &t.images {
                        if let ContentPart::Image { url } = img {
                            let source = if let Some(rest) = url.strip_prefix("data:") {
                                if let Some((media_type, data)) = rest.split_once(";base64,") {
                                    ImageSource::Base64 {
                                        media_type: media_type.to_string(),
                                        data: data.to_string(),
                                    }
                                } else {
                                    ImageSource::Url {
                                        url: url.as_ref().to_owned(),
                                    }
                                }
                            } else {
                                ImageSource::Url {
                                    url: url.as_ref().to_owned(),
                                }
                            };
                            blocks.push(ContentBlock::Image {
                                source,
                                cache_control: None,
                            });
                        }
                    }
                    ToolResultContent::Blocks(blocks)
                };
                pending_tool_results.push(ContentBlock::ToolResult {
                    tool_use_id: sanitize_tool_call_id(&t.tool_call_id),
                    content,
                    cache_control: None,
                });
            }
            // No native equivalent, so emit synthetic text to retain context.
            ConversationItem::BackendToolCall(b) => {
                flush_tool_results(&mut pending_tool_results, &mut messages);
                pending_assistant.push(ContentBlock::Text {
                    text: b.text_summary(),
                    cache_control: None,
                });
            }
            // `tco_*` blobs carry only `signature`; real reasoning sets
            // `thinking`.
            ConversationItem::Reasoning(r) => {
                flush_tool_results(&mut pending_tool_results, &mut messages);
                let thinking = reasoning_item_text(r);
                let signature = r
                    .encrypted_content
                    .as_deref()
                    .map(str::to_owned)
                    .unwrap_or_default();
                if !thinking.is_empty() || !signature.is_empty() {
                    pending_assistant.push(ContentBlock::Thinking {
                        thinking,
                        signature,
                    });
                }
            }
        }
    }

    flush_assistant(&mut pending_assistant, &mut messages);
    flush_tool_results(&mut pending_tool_results, &mut messages);

    // Cache the system prefix plus the current and previous conversation tips.
    if prompt_cache.mode == PromptCacheMode::StablePrefix {
        apply_cache_breakpoints(
            &mut system_blocks,
            &mut messages,
            ephemeral_cache_control(prompt_cache.ttl),
        );
    }

    let system: Option<SystemParam> = if system_blocks.is_empty() {
        None
    } else if system_blocks.len() == 1 && system_blocks[0].cache_control.is_none() {
        // Single block without cache_control - can use text form
        Some(SystemParam::Text(system_blocks[0].text.clone()))
    } else {
        Some(SystemParam::Blocks(system_blocks))
    };

    // Build tools. A breakpoint on the final definition caches the complete
    // tools prefix independently, matching the Anthropic SDK / pi convention.
    let tools: Option<Vec<ToolParam>> = if req.tools.is_empty() {
        None
    } else {
        let len = req.tools.len();
        let cc = if prompt_cache.mode == PromptCacheMode::StablePrefix {
            Some(ephemeral_cache_control(prompt_cache.ttl))
        } else {
            None
        };
        Some(
            req.tools
                .iter()
                .enumerate()
                .map(|(i, t)| ToolParam {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    input_schema: t.parameters.clone(),
                    cache_control: if i == len - 1 { cc.clone() } else { None },
                })
                .collect(),
        )
    };

    let tool_choice: Option<ToolChoiceParam> = req.tool_choice.as_ref().map(|tc| match tc {
        ConversationToolChoice::Auto => ToolChoiceParam::Auto,
        ConversationToolChoice::Required => ToolChoiceParam::Any,
        ConversationToolChoice::Function(name) => ToolChoiceParam::Tool { name: name.clone() },
        ConversationToolChoice::None => ToolChoiceParam::Auto, // default
    });

    let effort = req
        .reasoning_effort
        .and_then(|e| e.to_messages_api())
        .map(|s| s.to_string());

    // A wire schema here suppresses tool calls, so the agent routes
    // structured output through the StructuredOutput tool instead.
    let format = req
        .json_schema
        .as_ref()
        .map(|schema| crate::messages::OutputFormat::JsonSchema {
            schema: schema.clone(),
        });

    // thinking is driven by reasoning_effort only, not by json_schema.
    let thinking = effort
        .as_ref()
        .map(|_| crate::messages::ThinkingConfig::Adaptive {
            display: Some(crate::messages::ThinkingDisplay::Summarized),
        });

    let output_config = if effort.is_some() || format.is_some() {
        Some(OutputConfig { effort, format })
    } else {
        None
    };

    MessagesRequest {
        model: req.model.clone().unwrap_or_default(),
        messages,
        max_tokens: req.max_output_tokens.unwrap_or(0),
        system,
        tools,
        tool_choice,
        temperature: req.temperature,
        top_p: req.top_p,
        top_k: None,
        stream: None, // Set by caller
        stop_sequences: None,
        thinking,
        output_config,
        metadata: None,
    }
}

/// `Thinking` is dropped because this `From` returns a single item; the
/// streaming consumer emits the sibling `Reasoning` item instead.
impl From<crate::messages::MessagesResponse> for ConversationItem {
    fn from(resp: crate::messages::MessagesResponse) -> Self {
        use crate::messages::ContentBlock;

        let mut content = String::new();
        let mut tool_calls = Vec::new();

        for block in resp.content {
            match block {
                ContentBlock::Text { text, .. } => {
                    if !content.is_empty() {
                        content.push('\n');
                    }
                    content.push_str(&text);
                }
                ContentBlock::ToolUse {
                    id, name, input, ..
                } => {
                    tool_calls.push(ToolCall {
                        id: Arc::<str>::from(id),
                        name,
                        arguments: Arc::<str>::from(
                            serde_json::to_string(&input).unwrap_or_default(),
                        ),
                    });
                }
                // Thinking dropped — see doc comment above.
                ContentBlock::Thinking { .. } => {}
                _ => {} // Image, ToolResult not expected in assistant responses
            }
        }

        ConversationItem::Assistant(AssistantItem {
            content: Arc::<str>::from(content),
            tool_calls,
            model_id: Some(resp.model),
            model_fingerprint: None,
            reasoning_effort: None,
        })
    }
}
