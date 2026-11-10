use super::*;
use crate::json::Kind;

fn invalid(field: &'static str) -> Error {
    Error::InvalidInput {
        field,
        reason: "missing or invalid stream field",
    }
}
fn string<'a>(value: Value<'a, '_>, name: &'static str) -> Result<Text<'a>, Error> {
    value
        .get(name)
        .and_then(Value::as_string)
        .map(Text::Json)
        .ok_or_else(|| invalid(name))
}
fn optional<'a>(value: Value<'a, '_>, name: &'static str) -> Result<Option<Text<'a>>, Error> {
    match value.get(name) {
        None => Ok(None),
        Some(value) if value.kind() == Kind::Null => Ok(None),
        Some(value) => value
            .as_string()
            .map(Text::Json)
            .map(Some)
            .ok_or_else(|| invalid(name)),
    }
}
fn object<'a, 't>(value: Value<'a, 't>, name: &'static str) -> Result<Value<'a, 't>, Error> {
    let value = value.get(name).ok_or_else(|| invalid(name))?;
    if value.kind() != Kind::Object {
        return Err(invalid(name));
    }
    Ok(value)
}
fn number(value: Value<'_, '_>, name: &'static str) -> Result<Option<u64>, Error> {
    match value.get(name) {
        None => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| invalid(name)),
    }
}
fn index(value: Value<'_, '_>, name: &'static str) -> Result<u32, Error> {
    number(value, name)?
        .and_then(|number| u32::try_from(number).ok())
        .ok_or_else(|| invalid(name))
}
fn eq(value: Value<'_, '_>, name: &str, expected: &str) -> bool {
    value
        .get(name)
        .and_then(Value::as_string)
        .is_some_and(|value| value.eq_str(expected))
}
fn is_null_or_empty(value: Value<'_, '_>) -> bool {
    value.kind() == Kind::Null
        || (value.kind() == Kind::Array && value.array().next().is_none())
        || (value.kind() == Kind::Object && value.object().next().is_none())
}
fn fields(value: Value<'_, '_>, known: &[&str]) -> Result<(), Error> {
    if value.kind() != Kind::Object {
        return Err(invalid("event"));
    }
    for (key, _) in value.object() {
        if !known.iter().any(|name| key.eq_str(name)) {
            return Err(Error::Unsupported {
                feature: "unknown stream field",
            });
        }
    }
    Ok(())
}
fn array(value: Value<'_, '_>, name: &'static str) -> Result<(), Error> {
    if value
        .get(name)
        .is_some_and(|value| value.kind() == Kind::Array)
    {
        Ok(())
    } else {
        Err(invalid(name))
    }
}
fn empty_only(value: Value<'_, '_>, name: &'static str) -> Result<(), Error> {
    if value
        .get(name)
        .is_some_and(|value| !is_null_or_empty(value))
    {
        return Err(Error::Unsupported { feature: name });
    }
    Ok(())
}

pub(super) fn event(
    format: Format,
    value: Value<'_, '_>,
    sink: &mut impl Sink,
) -> Result<(), Error> {
    if value.kind() != Kind::Object {
        return Err(invalid("event"));
    }
    if let Some(error) = value.get("error") {
        if error.kind() != Kind::Object {
            return Err(invalid("error"));
        }
        return sink.failure(
            optional(error, "code")?.or(optional(error, "type")?),
            string(error, "message")?,
        );
    }
    match format.protocol {
        Protocol::OpenAiChat => chat(value, sink, false, format.profile),
        Protocol::OpenAiCompletions => chat(value, sink, true, format.profile),
        Protocol::AnthropicMessages => anthropic(value, sink),
        Protocol::OpenAiResponses => responses(value, sink),
        Protocol::GeminiGenerateContent => google(value, sink),
        Protocol::GeminiInteractions => interactions(value, sink),
    }
}

fn chat(
    value: Value<'_, '_>,
    sink: &mut impl Sink,
    legacy: bool,
    profile: Profile,
) -> Result<(), Error> {
    fields(
        value,
        &[
            "id",
            "object",
            "created",
            "model",
            "choices",
            "usage",
            "system_fingerprint",
            "service_tier",
        ],
    )?;
    for name in ["system_fingerprint", "service_tier"] {
        if value
            .get(name)
            .is_some_and(|value| value.kind() != Kind::Null)
        {
            sink.extension(if name == "system_fingerprint" {
                DroppedField::SystemFingerprint
            } else {
                DroppedField::ServiceTier
            })?;
        }
    }
    if let Some(usage) = value.get("usage") {
        if usage.kind() != Kind::Null {
            sink.usage(usage_openai(usage, false)?)?;
        }
    }
    sink.begin(
        optional(value, "id")?,
        optional(value, "model")?,
        number(value, "created")?,
    )?;
    array(value, "choices")?;
    for choice in value
        .get("choices")
        .ok_or_else(|| invalid("choices"))?
        .array()
    {
        fields(
            choice,
            if legacy {
                &["index", "text", "logprobs", "finish_reason"]
            } else {
                &["index", "delta", "logprobs", "finish_reason"]
            },
        )?;
        let choice_index = index(choice, "index")?;
        empty_only(choice, "logprobs")?;
        if legacy {
            if let Some(text) = optional(choice, "text")? {
                sink.delta(
                    Key {
                        choice: choice_index,
                        index: u32::MAX,
                        part: 0,
                    },
                    BlockKind::Text,
                    text,
                )?;
            }
        } else {
            let delta = object(choice, "delta")?;
            fields(
                delta,
                &[
                    "role",
                    "content",
                    "tool_calls",
                    "function_call",
                    "refusal",
                    "reasoning_content",
                    "reasoning",
                ],
            )?;
            if delta.get("role").is_some() && !eq(delta, "role", "assistant") {
                return Err(invalid("delta.role"));
            }
            if let Some(text) = optional(delta, "content")? {
                sink.delta(
                    Key {
                        choice: choice_index,
                        index: u32::MAX,
                        part: 0,
                    },
                    BlockKind::Text,
                    text,
                )?;
            }
            if let Some(text) = optional(delta, "refusal")? {
                sink.delta(
                    Key {
                        choice: choice_index,
                        index: u32::MAX - 1,
                        part: 0,
                    },
                    BlockKind::Refusal,
                    text,
                )?;
            }
            for name in ["reasoning_content", "reasoning"] {
                if let Some(text) = optional(delta, name)? {
                    if !matches!(profile, Profile::OpenAiCompatible(_)) {
                        return Err(Error::Unsupported {
                            feature: "vendor reasoning requires OpenAiCompatible profile",
                        });
                    }
                    sink.delta(
                        Key {
                            choice: choice_index,
                            index: u32::MAX - 2,
                            part: 0,
                        },
                        BlockKind::Reasoning,
                        text,
                    )?;
                }
            }
            if let Some(calls) = delta.get("tool_calls") {
                if calls.kind() != Kind::Array {
                    return Err(invalid("tool_calls"));
                }
                for call in calls.array() {
                    fields(call, &["index", "id", "type", "function"])?;
                    if call.get("type").is_some() && !eq(call, "type", "function") {
                        return Err(Error::Unsupported {
                            feature: "non-function streamed tool",
                        });
                    }
                    let key = Key {
                        choice: choice_index,
                        index: index(call, "index")?,
                        part: 0,
                    };
                    let function = object(call, "function")?;
                    fields(function, &["name", "arguments"])?;
                    let id = optional(call, "id")?;
                    let name = optional(function, "name")?;
                    if id.is_some() || name.is_some() {
                        sink.block(key, BlockKind::Tool, id, name, None)?;
                    }
                    if let Some(arguments) = optional(function, "arguments")? {
                        sink.delta(key, BlockKind::Tool, arguments)?;
                    }
                }
            }
            if let Some(function) = delta.get("function_call") {
                if delta.get("tool_calls").is_some() {
                    return Err(invalid("function_call and tool_calls"));
                }
                fields(function, &["name", "arguments"])?;
                let key = Key {
                    choice: choice_index,
                    index: 0,
                    part: 0,
                };
                if let Some(name) = optional(function, "name")? {
                    sink.block(key, BlockKind::Tool, None, Some(name), None)?;
                }
                if let Some(arguments) = optional(function, "arguments")? {
                    sink.delta(key, BlockKind::Tool, arguments)?;
                }
            }
        }
        if let Some(reason) = choice.get("finish_reason") {
            if reason.kind() != Kind::Null {
                sink.choice(choice_index, finish_reason(reason, false)?)?;
            }
        }
    }
    Ok(())
}

fn usage_openai(value: Value<'_, '_>, responses: bool) -> Result<Usage, Error> {
    if value.kind() != Kind::Object {
        return Err(invalid("usage"));
    }
    let (input_name, output_name, input_details, output_details) = if responses {
        (
            "input_tokens",
            "output_tokens",
            "input_tokens_details",
            "output_tokens_details",
        )
    } else {
        (
            "prompt_tokens",
            "completion_tokens",
            "prompt_tokens_details",
            "completion_tokens_details",
        )
    };
    fields(
        value,
        &[
            input_name,
            output_name,
            "total_tokens",
            input_details,
            output_details,
        ],
    )?;
    let mut usage = Usage {
        input: number(value, input_name)?,
        output: number(value, output_name)?,
        total: number(value, "total_tokens")?,
        ..Usage::default()
    };
    if let Some(details) = value.get(input_details) {
        fields(details, &["cached_tokens"])?;
        usage.cached = number(details, "cached_tokens")?;
    }
    if let Some(details) = value.get(output_details) {
        fields(details, &["reasoning_tokens"])?;
        usage.reasoning = number(details, "reasoning_tokens")?;
    }
    Ok(usage)
}
fn usage_anthropic(value: Value<'_, '_>) -> Result<Usage, Error> {
    fields(
        value,
        &[
            "input_tokens",
            "output_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
            "cache_creation",
            "server_tool_use",
            "service_tier",
            "inference_geo",
            "iterations",
        ],
    )?;
    for field in ["cache_creation", "server_tool_use", "iterations"] {
        if value
            .get(field)
            .is_some_and(|value| !is_null_or_empty(value) && value.as_u64() != Some(0))
        {
            return Err(Error::Unsupported {
                feature: "Anthropic detailed usage",
            });
        }
    }
    Ok(Usage {
        input: number(value, "input_tokens")?,
        output: number(value, "output_tokens")?,
        cached: number(value, "cache_read_input_tokens")?,
        cache_write: number(value, "cache_creation_input_tokens")?,
        input_basis: InputCount::Uncached,
        ..Usage::default()
    })
}
fn usage_google(value: Value<'_, '_>, interactions: bool) -> Result<Usage, Error> {
    let names = if interactions {
        [
            "total_input_tokens",
            "total_output_tokens",
            "total_tokens",
            "total_cached_tokens",
            "total_thought_tokens",
        ]
    } else {
        [
            "promptTokenCount",
            "candidatesTokenCount",
            "totalTokenCount",
            "cachedContentTokenCount",
            "thoughtsTokenCount",
        ]
    };
    fields(value, &names)?;
    Ok(Usage {
        input: number(value, names[0])?,
        output: number(value, names[1])?,
        total: number(value, names[2])?,
        cached: number(value, names[3])?,
        reasoning: number(value, names[4])?,
        cache_write: None,
        input_basis: InputCount::Total,
        output_basis: OutputCount::Visible,
    })
}

fn finish_reason(value: Value<'_, '_>, google: bool) -> Result<FinishReason, Error> {
    let value = value.as_string().ok_or_else(|| invalid("finish reason"))?;
    if ["stop", "end_turn", "stop_sequence", "STOP"]
        .iter()
        .any(|name| value.eq_str(name))
    {
        Ok(FinishReason::Stop)
    } else if ["length", "max_tokens", "MAX_TOKENS"]
        .iter()
        .any(|name| value.eq_str(name))
    {
        Ok(FinishReason::Length)
    } else if ["tool_calls", "function_call", "tool_use"]
        .iter()
        .any(|name| value.eq_str(name))
    {
        Ok(FinishReason::ToolCalls)
    } else if [
        "content_filter",
        "refusal",
        "SAFETY",
        "BLOCKLIST",
        "PROHIBITED_CONTENT",
        "SPII",
    ]
    .iter()
    .any(|name| value.eq_str(name))
    {
        Ok(FinishReason::Safety)
    } else if google && value.eq_str("FINISH_REASON_UNSPECIFIED") {
        Err(invalid("finishReason"))
    } else {
        Err(Error::Unsupported {
            feature: "finish reason",
        })
    }
}

fn anthropic(value: Value<'_, '_>, sink: &mut impl Sink) -> Result<(), Error> {
    let event_type = value
        .get("type")
        .and_then(Value::as_string)
        .ok_or_else(|| invalid("type"))?;
    if event_type.eq_str("ping") {
        fields(value, &["type"])?;
        return Ok(());
    }
    if event_type.eq_str("message_start") {
        fields(value, &["type", "message"])?;
        let message = object(value, "message")?;
        fields(
            message,
            &[
                "id",
                "type",
                "role",
                "content",
                "model",
                "stop_reason",
                "stop_sequence",
                "usage",
            ],
        )?;
        array(message, "content")?;
        if message.get("type").is_some() && !eq(message, "type", "message") {
            return Err(invalid("message.type"));
        }
        if !eq(message, "role", "assistant") {
            return Err(invalid("message.role"));
        }
        empty_only(message, "content")?;
        sink.usage(usage_anthropic(object(message, "usage")?)?)?;
        return sink.begin(
            Some(string(message, "id")?),
            Some(string(message, "model")?),
            None,
        );
    }
    if event_type.eq_str("content_block_start") {
        fields(value, &["type", "index", "content_block"])?;
        let key = Key {
            index: index(value, "index")?,
            ..Key::default()
        };
        let block = object(value, "content_block")?;
        if eq(block, "type", "tool_use") {
            fields(block, &["type", "id", "name", "input"])?;
            sink.block(
                key,
                BlockKind::Tool,
                Some(string(block, "id")?),
                Some(string(block, "name")?),
                None,
            )?;
            if let Some(input) = block.get("input") {
                if input.kind() != Kind::Object {
                    return Err(invalid("tool_use.input"));
                }
                if input.object().next().is_some() {
                    sink.delta(key, BlockKind::Tool, Text::Utf8(input.raw()))?;
                }
            }
        } else if eq(block, "type", "text") {
            fields(block, &["type", "text", "citations"])?;
            empty_only(block, "citations")?;
            sink.block(key, BlockKind::Text, None, None, None)?;
            if let Some(text) = optional(block, "text")? {
                if text.len() != 0 {
                    sink.delta(key, BlockKind::Text, text)?;
                }
            }
        } else if eq(block, "type", "thinking") {
            fields(block, &["type", "thinking", "signature"])?;
            sink.block(key, BlockKind::Reasoning, None, None, None)?;
            if let Some(text) = optional(block, "thinking")? {
                if text.len() != 0 {
                    sink.delta(key, BlockKind::Reasoning, text)?;
                }
            }
            if let Some(text) = optional(block, "signature")? {
                if text.len() != 0 {
                    sink.signature(key, text)?;
                }
            }
        } else {
            return Err(Error::Unsupported {
                feature: "Anthropic stream content block",
            });
        }
        return Ok(());
    }
    if event_type.eq_str("content_block_delta") {
        fields(value, &["type", "index", "delta"])?;
        let key = Key {
            index: index(value, "index")?,
            ..Key::default()
        };
        let delta = object(value, "delta")?;
        if eq(delta, "type", "text_delta") {
            fields(delta, &["type", "text"])?;
            sink.delta(key, BlockKind::Text, string(delta, "text")?)
        } else if eq(delta, "type", "input_json_delta") {
            fields(delta, &["type", "partial_json"])?;
            sink.delta(key, BlockKind::Tool, string(delta, "partial_json")?)
        } else if eq(delta, "type", "thinking_delta") {
            fields(delta, &["type", "thinking"])?;
            sink.delta(key, BlockKind::Reasoning, string(delta, "thinking")?)
        } else if eq(delta, "type", "signature_delta") {
            fields(delta, &["type", "signature"])?;
            sink.signature(key, string(delta, "signature")?)
        } else {
            Err(Error::Unsupported {
                feature: "Anthropic stream delta",
            })
        }
    } else if event_type.eq_str("content_block_stop") {
        fields(value, &["type", "index"])?;
        sink.close(Key {
            index: index(value, "index")?,
            ..Key::default()
        })
    } else if event_type.eq_str("message_delta") {
        fields(value, &["type", "delta", "usage"])?;
        if let Some(usage) = value.get("usage") {
            sink.usage(usage_anthropic(usage)?)?;
        }
        let delta = object(value, "delta")?;
        fields(delta, &["stop_reason", "stop_sequence"])?;
        if delta
            .get("stop_sequence")
            .is_some_and(|value| value.kind() != Kind::Null)
        {
            return Err(Error::Unsupported {
                feature: "stream stop_sequence",
            });
        }
        if let Some(reason) = delta.get("stop_reason") {
            if reason.kind() != Kind::Null {
                sink.choice(0, finish_reason(reason, false)?)?;
            }
        }
        Ok(())
    } else if event_type.eq_str("message_stop") {
        fields(value, &["type"])?;
        sink.finish(Completion::Completed)
    } else {
        Err(Error::Unsupported {
            feature: "Anthropic stream event",
        })
    }
}

fn responses(value: Value<'_, '_>, sink: &mut impl Sink) -> Result<(), Error> {
    let event_type = value
        .get("type")
        .and_then(Value::as_string)
        .ok_or_else(|| invalid("type"))?;
    if event_type.eq_str("error") {
        return sink.failure(optional(value, "code")?, string(value, "message")?);
    }
    if event_type.eq_str("response.created") || event_type.eq_str("response.in_progress") {
        let response = object(value, "response")?;
        if event_type.eq_str("response.in_progress") {
            return Ok(());
        }
        return sink.begin(
            optional(response, "id")?,
            optional(response, "model")?,
            number(response, "created_at")?,
        );
    }
    if event_type.eq_str("response.output_item.added") {
        let item = object(value, "item")?;
        let key = Key {
            index: index(value, "output_index")?,
            ..Key::default()
        };
        if eq(item, "type", "function_call") {
            sink.block(
                key,
                BlockKind::Tool,
                Some(string(item, "call_id")?),
                Some(string(item, "name")?),
                optional(item, "id")?,
            )?;
            if let Some(arguments) = optional(item, "arguments")? {
                if arguments.len() != 0 {
                    sink.delta(key, BlockKind::Tool, arguments)?;
                }
            }
        } else if eq(item, "type", "message") {
            if !eq(item, "role", "assistant") {
                return Err(invalid("item.role"));
            }
            empty_only(item, "content")?;
        } else if eq(item, "type", "reasoning") {
            sink.block(
                key,
                BlockKind::Reasoning,
                optional(item, "id")?,
                None,
                optional(item, "id")?,
            )?;
        } else {
            return Err(Error::Unsupported {
                feature: "Responses output item",
            });
        }
        return Ok(());
    }
    if event_type.eq_str("response.content_part.added") {
        let part = object(value, "part")?;
        let key = Key {
            index: index(value, "output_index")?,
            part: index(value, "content_index")?,
            choice: 0,
        };
        let kind = if eq(part, "type", "output_text") {
            BlockKind::Text
        } else if eq(part, "type", "refusal") {
            BlockKind::Refusal
        } else {
            return Err(Error::Unsupported {
                feature: "Responses content part",
            });
        };
        empty_only(part, "annotations")?;
        sink.block(
            key,
            kind,
            optional(value, "item_id")?,
            None,
            optional(value, "item_id")?,
        )?;
        let field = if kind == BlockKind::Text {
            "text"
        } else {
            "refusal"
        };
        if let Some(text) = optional(part, field)? {
            if text.len() != 0 {
                sink.delta(key, kind, text)?;
            }
        }
        return Ok(());
    }
    let kind = if event_type.eq_str("response.output_text.delta") {
        Some(BlockKind::Text)
    } else if event_type.eq_str("response.refusal.delta") {
        Some(BlockKind::Refusal)
    } else if event_type.eq_str("response.function_call_arguments.delta") {
        Some(BlockKind::Tool)
    } else if event_type.eq_str("response.reasoning_summary_text.delta") {
        Some(BlockKind::Reasoning)
    } else {
        None
    };
    if let Some(kind) = kind {
        let part = number(value, "content_index")?
            .or(number(value, "summary_index")?)
            .unwrap_or(0);
        let part = u32::try_from(part).map_err(|_| invalid("content_index"))?;
        return sink.delta(
            Key {
                index: index(value, "output_index")?,
                part,
                choice: 0,
            },
            kind,
            string(value, "delta")?,
        );
    }
    // Final snapshots are verified against accumulated deltas, never appended.
    if event_type.eq_str("response.output_text.done")
        || event_type.eq_str("response.refusal.done")
        || event_type.eq_str("response.function_call_arguments.done")
    {
        let kind = if event_type.eq_str("response.function_call_arguments.done") {
            BlockKind::Tool
        } else if event_type.eq_str("response.refusal.done") {
            BlockKind::Refusal
        } else {
            BlockKind::Text
        };
        let key = Key {
            index: index(value, "output_index")?,
            part: u32::try_from(number(value, "content_index")?.unwrap_or(0))
                .map_err(|_| invalid("content_index"))?,
            choice: 0,
        };
        return sink.snapshot(
            key,
            kind,
            string(
                value,
                if kind == BlockKind::Tool {
                    "arguments"
                } else if kind == BlockKind::Refusal {
                    "refusal"
                } else {
                    "text"
                },
            )?,
        );
    }
    if event_type.eq_str("response.content_part.done") {
        let part = object(value, "part")?;
        let kind = if eq(part, "type", "refusal") {
            BlockKind::Refusal
        } else if eq(part, "type", "output_text") {
            BlockKind::Text
        } else {
            return Err(Error::Unsupported {
                feature: "Responses final content part",
            });
        };
        return sink.snapshot(
            Key {
                index: index(value, "output_index")?,
                part: index(value, "content_index")?,
                choice: 0,
            },
            kind,
            string(
                part,
                if kind == BlockKind::Refusal {
                    "refusal"
                } else {
                    "text"
                },
            )?,
        );
    }
    if [
        "response.reasoning_summary_part.added",
        "response.reasoning_summary_part.done",
        "response.reasoning_summary_text.done",
    ]
    .iter()
    .any(|name| event_type.eq_str(name))
    {
        return Err(Error::Unsupported {
            feature: "Responses reasoning snapshot",
        });
    }
    if event_type.eq_str("response.output_item.done") {
        let key = Key {
            index: index(value, "output_index")?,
            ..Key::default()
        };
        let item = object(value, "item")?;
        response_snapshot(item, key.index, sink)?;
        if eq(item, "status", "incomplete") {
            sink.incomplete(key)?;
        }
        return sink.close(key);
    }
    if [
        "response.completed",
        "response.incomplete",
        "response.failed",
    ]
    .iter()
    .any(|name| event_type.eq_str(name))
    {
        let response = object(value, "response")?;
        if let Some(usage) = response.get("usage") {
            if usage.kind() != Kind::Null {
                sink.usage(usage_openai(usage, true)?)?;
            }
        }
        if event_type.eq_str("response.failed") {
            let error = object(response, "error")?;
            return sink.failure(optional(error, "code")?, string(error, "message")?);
        }
        let completion = if event_type.eq_str("response.incomplete") {
            Completion::Incomplete
        } else {
            Completion::Completed
        };
        let mut tools = false;
        if let Some(output) = response.get("output") {
            if output.kind() != Kind::Array {
                return Err(invalid("output"));
            }
            for (output_index, item) in output.array().enumerate() {
                tools |= eq(item, "type", "function_call");
                response_snapshot(
                    item,
                    u32::try_from(output_index).map_err(|_| invalid("output_index"))?,
                    sink,
                )?;
            }
        }
        sink.choice(
            0,
            if completion == Completion::Incomplete {
                let details = object(response, "incomplete_details")?;
                if eq(details, "reason", "max_output_tokens") {
                    FinishReason::Length
                } else if eq(details, "reason", "content_filter") {
                    FinishReason::Safety
                } else {
                    return Err(Error::Unsupported {
                        feature: "Responses incomplete reason",
                    });
                }
            } else if tools {
                FinishReason::ToolCalls
            } else {
                FinishReason::Stop
            },
        )?;
        return sink.finish(completion);
    }
    Err(Error::Unsupported {
        feature: "Responses stream event",
    })
}

fn response_snapshot(
    item: Value<'_, '_>,
    output_index: u32,
    sink: &mut impl Sink,
) -> Result<(), Error> {
    let key = Key {
        index: output_index,
        ..Key::default()
    };
    if eq(item, "type", "function_call") {
        return sink.snapshot(key, BlockKind::Tool, string(item, "arguments")?);
    }
    if eq(item, "type", "message") {
        array(item, "content")?;
        for (index, part) in item
            .get("content")
            .ok_or_else(|| invalid("content"))?
            .array()
            .enumerate()
        {
            let kind = if eq(part, "type", "refusal") {
                BlockKind::Refusal
            } else if eq(part, "type", "output_text") {
                BlockKind::Text
            } else {
                return Err(Error::Unsupported {
                    feature: "Responses final content",
                });
            };
            sink.snapshot(
                Key {
                    part: u32::try_from(index).map_err(|_| invalid("content_index"))?,
                    ..key
                },
                kind,
                string(
                    part,
                    if kind == BlockKind::Refusal {
                        "refusal"
                    } else {
                        "text"
                    },
                )?,
            )?;
        }
        return Ok(());
    }
    Err(Error::Unsupported {
        feature: "Responses final item",
    })
}

fn google(value: Value<'_, '_>, sink: &mut impl Sink) -> Result<(), Error> {
    fields(
        value,
        &[
            "candidates",
            "usageMetadata",
            "modelVersion",
            "responseId",
            "promptFeedback",
            "createTime",
        ],
    )?;
    if let Some(feedback) = value.get("promptFeedback") {
        if feedback.get("blockReason").is_some() {
            return Err(Error::Unsupported {
                feature: "Gemini prompt safety block",
            });
        }
    }
    if let Some(usage) = value.get("usageMetadata") {
        sink.usage(usage_google(usage, false)?)?;
    }
    sink.begin(
        optional(value, "responseId")?,
        optional(value, "modelVersion")?,
        None,
    )?;
    let Some(candidates) = value.get("candidates") else {
        return Ok(());
    };
    if candidates.kind() != Kind::Array {
        return Err(invalid("candidates"));
    }
    for candidate in candidates.array() {
        fields(
            candidate,
            &[
                "index",
                "content",
                "finishReason",
                "safetyRatings",
                "citationMetadata",
                "tokenCount",
                "groundingMetadata",
                "avgLogprobs",
                "logprobsResult",
                "finishMessage",
            ],
        )?;
        for name in ["citationMetadata", "groundingMetadata", "logprobsResult"] {
            empty_only(candidate, name)?;
        }
        for (name, field) in [
            ("safetyRatings", DroppedField::SafetyRatings),
            ("finishMessage", DroppedField::FinishMessage),
            ("avgLogprobs", DroppedField::AverageLogprobs),
        ] {
            if candidate
                .get(name)
                .is_some_and(|value| !is_null_or_empty(value))
            {
                sink.extension(field)?;
            }
        }
        let choice = number(candidate, "index")?.unwrap_or(0);
        let choice = u32::try_from(choice).map_err(|_| invalid("index"))?;
        if let Some(content) = candidate.get("content") {
            fields(content, &["role", "parts"])?;
            if content.get("role").is_some() && !eq(content, "role", "model") {
                return Err(invalid("content.role"));
            }
            array(content, "parts")?;
            for (part_index, part) in content
                .get("parts")
                .ok_or_else(|| invalid("parts"))?
                .array()
                .enumerate()
            {
                fields(
                    part,
                    &[
                        "text",
                        "thought",
                        "thoughtSignature",
                        "functionCall",
                        "inlineData",
                        "fileData",
                        "functionResponse",
                        "executableCode",
                        "codeExecutionResult",
                    ],
                )?;
                if let Some(text) = optional(part, "text")? {
                    let kind = if part.get("thought").and_then(Value::as_bool) == Some(true) {
                        BlockKind::Reasoning
                    } else {
                        BlockKind::Text
                    };
                    let key = Key {
                        choice,
                        index: if kind == BlockKind::Text {
                            u32::MAX
                        } else {
                            u32::MAX - 2
                        },
                        part: 0,
                    };
                    sink.delta(key, kind, text)?;
                    if let Some(signature) = optional(part, "thoughtSignature")? {
                        sink.signature(key, signature)?;
                    }
                } else if let Some(function) = part.get("functionCall") {
                    if part.get("thoughtSignature").is_some() {
                        return Err(Error::Unsupported {
                            feature: "Gemini function-bound signature",
                        });
                    }
                    fields(function, &["id", "name", "args"])?;
                    let key = Key {
                        choice,
                        index: u32::try_from(part_index).map_err(|_| invalid("parts"))?,
                        part: 0,
                    };
                    let arguments = object(function, "args")?;
                    sink.complete_tool(
                        key,
                        optional(function, "id")?,
                        string(function, "name")?,
                        Text::Utf8(arguments.raw()),
                    )?;
                } else if let Some(signature) = optional(part, "thoughtSignature")? {
                    sink.signature(
                        Key {
                            choice,
                            index: u32::MAX - 2,
                            part: 0,
                        },
                        signature,
                    )?;
                } else {
                    return Err(Error::Unsupported {
                        feature: "Gemini streamed media or server tool",
                    });
                }
            }
        }
        if let Some(reason) = candidate.get("finishReason") {
            sink.choice(choice, finish_reason(reason, true)?)?;
        }
    }
    Ok(())
}

fn interactions(value: Value<'_, '_>, sink: &mut impl Sink) -> Result<(), Error> {
    let event_type = value
        .get("event_type")
        .and_then(Value::as_string)
        .ok_or_else(|| invalid("event_type"))?;
    if event_type.eq_str("interaction.created") {
        let interaction = object(value, "interaction")?;
        return sink.begin(
            optional(interaction, "id")?,
            optional(interaction, "model")?,
            None,
        );
    }
    if event_type.eq_str("interaction.status_update") {
        if eq(value, "status", "in_progress") || eq(value, "status", "requires_action") {
            return Ok(());
        }
        if eq(value, "status", "cancelled") {
            return sink.finish(Completion::Cancelled);
        }
        return Err(Error::Unsupported {
            feature: "Interactions status transition",
        });
    }
    if event_type.eq_str("step.start") {
        let key = Key {
            index: index(value, "index")?,
            ..Key::default()
        };
        let step = object(value, "step")?;
        if eq(step, "type", "model_output") {
            empty_only(step, "content")?;
            sink.block(key, BlockKind::Text, None, None, None)
        } else if eq(step, "type", "thought") {
            sink.block(key, BlockKind::Reasoning, None, None, None)
        } else if eq(step, "type", "function_call") {
            sink.block(
                key,
                BlockKind::Tool,
                Some(string(step, "id")?),
                Some(string(step, "name")?),
                None,
            )?;
            if let Some(arguments) = step.get("arguments") {
                if arguments.kind() != Kind::Object {
                    return Err(invalid("arguments"));
                }
                if arguments.object().next().is_some() {
                    sink.delta(key, BlockKind::Tool, Text::Utf8(arguments.raw()))?;
                }
            }
            Ok(())
        } else {
            Err(Error::Unsupported {
                feature: "Interactions server-tool/agent step",
            })
        }
    } else if event_type.eq_str("step.delta") {
        let key = Key {
            index: index(value, "index")?,
            ..Key::default()
        };
        let delta = object(value, "delta")?;
        if eq(delta, "type", "text") {
            sink.delta(key, BlockKind::Text, string(delta, "text")?)
        } else if eq(delta, "type", "arguments_delta") {
            sink.delta(key, BlockKind::Tool, string(delta, "arguments")?)
        } else if eq(delta, "type", "thought_summary") {
            sink.delta(
                key,
                BlockKind::Reasoning,
                string(object(delta, "content")?, "text")?,
            )
        } else if eq(delta, "type", "thought_signature") {
            sink.signature(key, string(delta, "signature")?)
        } else {
            Err(Error::Unsupported {
                feature: "Interactions media delta",
            })
        }
    } else if event_type.eq_str("step.stop") {
        if let Some(usage) = value.get("usage") {
            sink.usage(usage_google(usage, true)?)?;
        }
        sink.close(Key {
            index: index(value, "index")?,
            ..Key::default()
        })
    } else if event_type.eq_str("interaction.completed") {
        let interaction = object(value, "interaction")?;
        if let Some(usage) = interaction.get("usage") {
            sink.usage(usage_google(usage, true)?)?;
        }
        let completion = if eq(interaction, "status", "requires_action") {
            Completion::RequiresAction
        } else if eq(interaction, "status", "incomplete")
            || eq(interaction, "status", "budget_exceeded")
        {
            Completion::Incomplete
        } else if eq(interaction, "status", "cancelled") {
            Completion::Cancelled
        } else if eq(interaction, "status", "failed") {
            Completion::Failed
        } else if eq(interaction, "status", "completed") {
            Completion::Completed
        } else {
            return Err(invalid("interaction.status"));
        };
        sink.choice(
            0,
            match completion {
                Completion::RequiresAction => FinishReason::ToolCalls,
                Completion::Incomplete => FinishReason::Length,
                _ => FinishReason::Stop,
            },
        )?;
        sink.finish(completion)
    } else {
        Err(Error::Unsupported {
            feature: "Interactions stream event",
        })
    }
}
