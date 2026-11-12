use zconv::json::{self, Token};
use zconv::stream::*;
use zconv::{Error, Format, Profile, Protocol, Strategy};

fn format(protocol: Protocol) -> Format {
    Format {
        profile: if protocol == Protocol::OpenAiChat {
            Profile::OpenAiCompatible("lmm")
        } else {
            Profile::Standard
        },
        ..protocol.into()
    }
}
fn decode(protocol: Protocol, payload: &[u8]) -> Result<Vec<Event>, Error> {
    let mut tokens = vec![Token::EMPTY; 1024];
    let mut events = vec![Event::EMPTY; 128];
    let n = decode_frame_format(
        format(protocol),
        b"message",
        payload,
        &mut tokens,
        &mut events,
        json::Limits::default(),
    )?;
    events.truncate(n);
    Ok(events)
}
struct Encoder {
    state: EventEncoderState,
    tokens: Vec<Token>,
    items: Vec<ActiveItem>,
    checkpoints: Vec<ActiveItem>,
    aggregate: Vec<u8>,
}
impl Encoder {
    fn new(source: Protocol, target: Protocol) -> Self {
        let mut config = StreamConfig::new(source, target);
        config.source = format(source);
        config.target = format(target);
        config.strategy = Strategy::Ir;
        config.allow_partial_usage = true;
        Self {
            state: EventEncoderState::new(config).unwrap(),
            tokens: vec![Token::EMPTY; 1024],
            items: vec![ActiveItem::EMPTY; 64],
            checkpoints: vec![ActiveItem::EMPTY; 64],
            aggregate: vec![0; 32768],
        }
    }
    fn encode(&mut self, payload: &[u8], events: &[Event]) -> Result<String, Status> {
        let mut output = vec![0; 65536];
        let n = self.state.encode(
            StreamContext {
                response_id: Some("r"),
                model: Some("m"),
                created: Some(7),
                item_id_prefix: Some("test_"),
                ..Default::default()
            },
            payload,
            events,
            EventEncoderBuffers {
                tokens: &mut self.tokens,
                items: &mut self.items,
                checkpoints: &mut self.checkpoints,
                aggregate: &mut self.aggregate,
            },
            &mut output,
        )?;
        Ok(String::from_utf8(output[..n].to_vec()).unwrap())
    }
    fn frame(&mut self, protocol: Protocol, payload: &[u8]) -> Result<String, Status> {
        let events = decode(protocol, payload).map_err(Status::Failed)?;
        self.encode(payload, &events)
    }
}

#[test]
fn chat_images_keep_raw_objects_and_reject_unrepresentable_target() {
    let payload = br#"{"choices":[{"index":0,"delta":{"images":[{"type":"image_url","image_url":{"url":"https://example.com/a.png"},"index":2,"extra":"kept"}]}}]}"#;
    let events = decode(Protocol::OpenAiChat, payload).unwrap();
    assert!(events.iter().any(|e| e.kind == EventKind::Images));
    let output = Encoder::new(Protocol::OpenAiChat, Protocol::OpenAiChat)
        .encode(payload, &events)
        .unwrap();
    assert!(output.contains("\"extra\":\"kept\""));
    assert!(output.contains("\"index\":2"));
    assert!(matches!(
        Encoder::new(Protocol::OpenAiChat, Protocol::OpenAiResponses).encode(payload, &events),
        Err(Status::Failed(Error::Unsupported { .. }))
    ));
}

#[test]
fn usage_keeps_lifetimes_cost_and_missing_counters_without_synthetic_zero() {
    let payload = br#"{"choices":[],"usage":{"cache_creation_input_tokens":3,"prompt_tokens_details":{"cache_write_tokens":3,"cache_write_tokens_details":{"ttl_30m_tokens":1,"ttl_12h_tokens":2}},"cost":1.25e-7}}"#;
    let events = decode(Protocol::OpenAiChat, payload).unwrap();
    let usage = events
        .iter()
        .find(|e| e.kind == EventKind::Usage)
        .unwrap()
        .usage;
    assert_eq!(usage.input, None);
    assert_eq!(usage.output, None);
    assert_eq!(usage.cache_write_30m, Some(1));
    assert_eq!(usage.cache_write_12h, Some(2));
    assert_eq!(usage.cost.unwrap().raw(), b"1.25e-7");
    let mut encoder = Encoder::new(Protocol::OpenAiChat, Protocol::OpenAiChat);
    encoder
        .encode(
            b"",
            &[Event {
                kind: EventKind::Begin,
                ..Event::EMPTY
            }],
        )
        .unwrap();
    encoder.encode(payload, &events).unwrap();
    let output = encoder
        .encode(
            b"",
            &[
                Event {
                    kind: EventKind::ChoiceEnd,
                    ..Event::EMPTY
                },
                Event {
                    kind: EventKind::Finish,
                    ..Event::EMPTY
                },
            ],
        )
        .unwrap();
    assert!(output.contains("\"ttl_30m_tokens\":1"));
    assert!(output.contains("\"ttl_12h_tokens\":2"));
    assert!(output.contains("\"cost\":1.25e-7"));
    assert!(!output.contains("\"prompt_tokens\":0"));
    assert!(!output.contains("\"completion_tokens\":0"));
}

#[test]
fn cache_aliases_and_lifetime_conflicts_are_strict() {
    for payload in [
        br#"{"choices":[],"usage":{"prompt_tokens":10,"prompt_cache_hit_tokens":2,"prompt_cache_miss_tokens":7}}"#.as_slice(),
        br#"{"choices":[],"usage":{"cache_creation_input_tokens":3,"prompt_tokens_details":{"cache_write_tokens":2}}}"#,
        br#"{"choices":[],"usage":{"prompt_tokens":-1}}"#,
    ] { assert!(decode(Protocol::OpenAiChat, payload).is_err()); }
    assert!(
        decode(
            Protocol::OpenAiChat,
            br#"{"choices":[],"usage":{"prompt_tokens_details":null}}"#
        )
        .is_ok()
    );
}

#[test]
fn anthropic_partial_lifetimes_remain_unknown_and_stop_sequence_survives_batches() {
    let mut encoder = Encoder::new(Protocol::AnthropicMessages, Protocol::AnthropicMessages);
    encoder.frame(Protocol::AnthropicMessages, br#"{"type":"message_start","message":{"id":"r","model":"m","role":"assistant","content":[],"usage":{"input_tokens":2,"output_tokens":0,"cache_creation":{"ephemeral_5m_input_tokens":3}}}}"#).unwrap();
    assert_eq!(encoder.state.usage().input, None);
    let output = encoder.frame(Protocol::AnthropicMessages, br#"{"type":"message_delta","delta":{"stop_reason":"stop_sequence","stop_sequence":"END"},"usage":{"output_tokens":1}}"#).unwrap();
    assert!(!output.contains("stop_sequence"));
    let output = encoder
        .frame(Protocol::AnthropicMessages, br#"{"type":"message_stop"}"#)
        .unwrap();
    assert!(output.contains("\"stop_reason\":\"stop_sequence\""));
    assert!(output.contains("\"stop_sequence\":\"END\""));
    assert!(output.contains("\"ephemeral_5m_input_tokens\":3"));
    assert!(!output.contains("ephemeral_1h_input_tokens"));
}

#[test]
fn gemini_grounding_continuation_and_tool_prompt_usage_round_trip() {
    let payload = br#"{"usageMetadata":{"toolUsePromptTokenCount":4},"candidates":[{"index":0,"groundingMetadata":{"webSearchQueries":["q"],"groundingChunks":[{"web":{"uri":"https://example.com"}}]},"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"CONTINUATION","continuationToken":"next"}]}"#;
    let events = decode(Protocol::GeminiGenerateContent, payload).unwrap();
    assert!(events.iter().any(|e| e.kind == EventKind::Grounding));
    assert!(
        events
            .iter()
            .any(|e| e.reason == FinishReason::Continuation)
    );
    let mut encoder = Encoder::new(
        Protocol::GeminiGenerateContent,
        Protocol::GeminiGenerateContent,
    );
    let mut output = encoder.encode(payload, &events).unwrap();
    output += &encoder
        .encode(
            b"",
            &[Event {
                kind: EventKind::Finish,
                ..Event::EMPTY
            }],
        )
        .unwrap();
    assert!(output.contains("\"webSearchQueries\":[\"q\"]"));
    assert!(output.contains("\"continuationToken\":\"next\""));
    assert!(output.contains("\"finishReason\":\"CONTINUATION\""));
    assert!(output.contains("\"toolUsePromptTokenCount\":4"));
    assert!(matches!(
        Encoder::new(Protocol::GeminiGenerateContent, Protocol::OpenAiChat)
            .encode(payload, &events),
        Err(Status::Failed(Error::Unsupported { .. }))
    ));
}

#[test]
fn raw_event_edits_are_validated_before_encoding() {
    let mut encoder = Encoder::new(Protocol::OpenAiChat, Protocol::OpenAiChat);
    encoder
        .encode(
            b"",
            &[Event {
                kind: EventKind::Begin,
                ..Event::EMPTY
            }],
        )
        .unwrap();
    for raw in [b"{}".as_slice(), b"[", b"[{}]"] {
        let event = Event {
            kind: EventKind::Images,
            a: EventText {
                present: true,
                quoted: false,
                offset: 0,
                length: raw.len(),
            },
            ..Event::EMPTY
        };
        assert!(encoder.encode(raw, &[event]).is_err());
    }
    let output = encoder
        .frame(
            Protocol::OpenAiChat,
            br#"{"choices":[{"index":0,"delta":{"content":"still usable"}}]}"#,
        )
        .unwrap();
    assert!(output.contains("still usable"));
}

fn partial_tool(target: Protocol, reason: FinishReason, partial: &str) -> Result<String, Status> {
    let mut encoder = Encoder::new(Protocol::OpenAiChat, target);
    let payload = format!("callf{partial}");
    let span = |offset, length| EventText {
        offset,
        length,
        quoted: false,
        present: true,
    };
    encoder.encode(
        payload.as_bytes(),
        &[
            Event {
                kind: EventKind::Begin,
                ..Event::EMPTY
            },
            Event {
                kind: EventKind::BlockStart,
                block: BlockKind::Tool,
                a: span(0, 4),
                b: span(4, 1),
                ..Event::EMPTY
            },
            Event {
                kind: EventKind::Delta,
                block: BlockKind::Tool,
                a: span(5, partial.len()),
                ..Event::EMPTY
            },
        ],
    )?;
    encoder.encode(
        b"",
        &[
            Event {
                kind: EventKind::ChoiceEnd,
                reason,
                ..Event::EMPTY
            },
            Event {
                kind: EventKind::Finish,
                ..Event::EMPTY
            },
        ],
    )
}

#[test]
fn truncated_tools_only_survive_string_targets_with_truncating_finish() {
    for target in [Protocol::OpenAiChat, Protocol::OpenAiResponses] {
        for reason in [FinishReason::Length, FinishReason::Safety] {
            for partial in ["{\"x\":", ""] {
                assert!(
                    partial_tool(target, reason, partial).is_ok(),
                    "{target:?} {reason:?}"
                );
            }
        }
        for reason in [FinishReason::Stop, FinishReason::ToolCalls] {
            assert!(partial_tool(target, reason, "{\"x\":").is_err());
        }
    }
    for target in [Protocol::AnthropicMessages, Protocol::GeminiGenerateContent] {
        assert!(partial_tool(target, FinishReason::Length, "{\"x\":").is_err());
    }
}

#[test]
fn raw_lifetime_observations_are_preserved_and_standard_encoding_rejects_conflict() {
    let payload = br#"{"choices":[],"usage":{"cache_creation_input_tokens":2,"cache_creation_details":{"ephemeral_5m_input_tokens":3,"ephemeral_1h_input_tokens":4}}}"#;
    let events = decode(Protocol::OpenAiChat, payload).unwrap();
    let usage = events
        .iter()
        .find(|e| e.kind == EventKind::Usage)
        .unwrap()
        .usage;
    assert_eq!(usage.cache_write, Some(2));
    assert_eq!(usage.cache_write_5m, Some(3));
    assert_eq!(usage.cache_write_1h, Some(4));
    let mut encoder = Encoder::new(Protocol::OpenAiChat, Protocol::OpenAiChat);
    encoder
        .encode(
            b"",
            &[Event {
                kind: EventKind::Begin,
                ..Event::EMPTY
            }],
        )
        .unwrap();
    encoder.encode(payload, &events).unwrap();
    assert!(matches!(
        encoder.encode(
            b"",
            &[
                Event {
                    kind: EventKind::ChoiceEnd,
                    ..Event::EMPTY
                },
                Event {
                    kind: EventKind::Finish,
                    ..Event::EMPTY
                }
            ]
        ),
        Err(Status::Failed(Error::InvalidInput { .. }))
    ));
}

#[test]
fn responses_reasoning_parts_have_independent_ir_lifecycles() {
    let mut encoder = Encoder::new(Protocol::OpenAiResponses, Protocol::OpenAiChat);
    let mut output = String::new();
    for payload in [
        br#"{"type":"response.created","response":{"id":"r","model":"m"}}"#.as_slice(),
        br#"{"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"thought","summary":[]}}"#,
        br#"{"type":"response.reasoning_summary_part.added","output_index":0,"summary_index":0,"part":{"type":"summary_text","text":""}}"#,
        br#"{"type":"response.reasoning_summary_text.delta","output_index":0,"summary_index":0,"delta":"thought"}"#,
        br#"{"type":"response.reasoning_summary_text.done","output_index":0,"summary_index":0,"text":"thought"}"#,
        br#"{"type":"response.reasoning_summary_part.done","output_index":0,"summary_index":0,"part":{"type":"summary_text","text":"thought"}}"#,
        br#"{"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","summary":[{"type":"summary_text","text":"thought"}]}}"#,
        br#"{"type":"response.completed","response":{"id":"r","status":"completed","output":[{"type":"reasoning","summary":[{"type":"summary_text","text":"thought"}]}]}}"#,
    ] { output += &encoder.frame(Protocol::OpenAiResponses, payload).unwrap(); }
    assert!(output.contains("\"reasoning_content\":\"thought\""));
    assert!(output.contains("[DONE]"));
}

#[test]
fn responses_compatible_profile_encodes_native_cache_lifetimes_without_loss() {
    let mut config = StreamConfig::new(Protocol::OpenAiChat, Protocol::OpenAiResponses);
    config.source = format(Protocol::OpenAiChat);
    config.target.profile = Profile::OpenAiCompatible("lmm");
    config.strategy = Strategy::Ir;
    config.allow_partial_usage = true;
    let mut encoder = Encoder::new(Protocol::OpenAiChat, Protocol::OpenAiResponses);
    encoder.state = EventEncoderState::new(config).unwrap();
    let payload = br#"{"choices":[],"usage":{"cache_creation_input_tokens":3,"prompt_tokens_details":{"cache_write_tokens_details":{"ttl_30m_tokens":1,"ttl_12h_tokens":2}}}}"#;
    encoder.frame(Protocol::OpenAiChat, payload).unwrap();
    let output = encoder
        .encode(
            b"",
            &[
                Event {
                    kind: EventKind::ChoiceEnd,
                    ..Event::EMPTY
                },
                Event {
                    kind: EventKind::Finish,
                    ..Event::EMPTY
                },
            ],
        )
        .unwrap();
    assert!(output.contains("\"input_tokens_details\":{\"cache_write_tokens\":3"));
    assert!(output.contains("\"ttl_30m_tokens\":1"));
    assert!(output.contains("\"ttl_12h_tokens\":2"));
    assert_eq!(encoder.state.report().dropped_extensions, 0);
    assert!(decode_frame_format(config.target, b"message", br#"{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens_details":{"cache_write_tokens":3,"cache_write_tokens_details":{"ttl_30m_tokens":1,"ttl_12h_tokens":2}}}}}"#,
        &mut [Token::EMPTY;128], &mut [Event::EMPTY;16], Default::default()).is_ok());
    for invalid in [
        Format {
            profile: Profile::OpenAiCompatible(""),
            ..Protocol::OpenAiResponses.into()
        },
        Format {
            version: "future",
            profile: Profile::OpenAiCompatible("lmm"),
            ..Protocol::OpenAiResponses.into()
        },
        Format {
            profile: Profile::OpenAiCompatible("lmm"),
            ..Protocol::AnthropicMessages.into()
        },
    ] {
        assert!(validate_stream_format(invalid).is_err());
    }
}

#[test]
fn explicit_signature_origin_and_responses_reasoning_keep_original_lifecycle() {
    let mut config = StreamConfig::new(Protocol::OpenAiResponses, Protocol::OpenAiResponses);
    config.target.profile = Profile::OpenAiCompatible("lmm");
    config.allow_partial_usage = true;
    let mut encoder = Encoder::new(Protocol::OpenAiResponses, Protocol::OpenAiResponses);
    encoder.state = EventEncoderState::new(config).unwrap();
    let span = |offset, length| EventText {
        offset,
        length,
        quoted: false,
        present: true,
    };
    let wire = encoder
        .encode(
            b"textsiggeminiout",
            &[
                Event::EMPTY,
                Event {
                    kind: EventKind::BlockStart,
                    block: BlockKind::Reasoning,
                    item_id: span(13, 3),
                    ..Event::EMPTY
                },
                Event {
                    kind: EventKind::Delta,
                    block: BlockKind::Reasoning,
                    a: span(0, 4),
                    ..Event::EMPTY
                },
                Event {
                    kind: EventKind::Signature,
                    a: span(4, 3),
                    b: span(7, 6),
                    ..Event::EMPTY
                },
                Event {
                    kind: EventKind::ChoiceEnd,
                    ..Event::EMPTY
                },
                Event {
                    kind: EventKind::Finish,
                    ..Event::EMPTY
                },
            ],
        )
        .unwrap();
    let expected = [
        "response.created",
        "response.in_progress",
        "response.output_item.added",
        "response.reasoning_summary_part.added",
        "response.reasoning_summary_text.delta",
        "response.reasoning_summary_text.done",
        "response.reasoning_summary_part.done",
        "response.output_item.done",
        "response.completed",
    ];
    let names: Vec<_> = wire
        .lines()
        .filter_map(|s| s.strip_prefix("event: "))
        .collect();
    assert_eq!(names, expected);
    for (sequence, line) in wire
        .lines()
        .filter_map(|s| s.strip_prefix("data: "))
        .enumerate()
    {
        let mut tokens = [Token::EMPTY; 256];
        let doc = json::parse(line.as_bytes(), &mut tokens, Default::default()).unwrap();
        assert_eq!(
            doc.root().get("sequence_number").unwrap().as_u64(),
            Some(sequence as u64)
        );
    }
    assert!(wire.contains("\"item_id\":\"out\""));
    assert!(wire.contains("\"provider_metadata\":{\"protocol\":\"gemini\",\"signature\":\"sig\"}"));
    assert!(wire.contains("\"store\":false"));
}

#[test]
fn gemini_signature_and_continuation_are_in_their_owning_wire_objects() {
    let mut encoder = Encoder::new(
        Protocol::GeminiGenerateContent,
        Protocol::GeminiGenerateContent,
    );
    let span = |offset, length| EventText {
        offset,
        length,
        quoted: false,
        present: true,
    };
    encoder
        .encode(
            b"text",
            &[
                Event::EMPTY,
                Event {
                    kind: EventKind::BlockStart,
                    block: BlockKind::Reasoning,
                    ..Event::EMPTY
                },
                Event {
                    kind: EventKind::Delta,
                    block: BlockKind::Reasoning,
                    a: span(0, 4),
                    ..Event::EMPTY
                },
            ],
        )
        .unwrap();
    let mut wire = encoder
        .encode(
            b"sig",
            &[Event {
                kind: EventKind::Signature,
                a: span(0, 3),
                ..Event::EMPTY
            }],
        )
        .unwrap();
    wire += &encoder
        .encode(
            b"next",
            &[
                Event {
                    kind: EventKind::Continuation,
                    a: span(0, 4),
                    ..Event::EMPTY
                },
                Event {
                    kind: EventKind::ChoiceEnd,
                    reason: FinishReason::Continuation,
                    ..Event::EMPTY
                },
                Event {
                    kind: EventKind::Finish,
                    ..Event::EMPTY
                },
            ],
        )
        .unwrap();
    assert!(
        wire.lines()
            .any(|s| s.contains("\"text\":\"text\"") && s.contains("\"thoughtSignature\":\"sig\""))
    );
    assert!(
        wire.lines()
            .any(|s| s.contains("\"finishReason\":\"CONTINUATION\"")
                && s.contains("\"continuationToken\":\"next\""))
    );
}

#[test]
fn generic_receipt_preserves_raw_input_aggregate_and_lifetime_observations() {
    let mut encoder = Encoder::new(Protocol::AnthropicMessages, Protocol::AnthropicMessages);
    let usage = Usage {
        input: Some(37),
        uncached_input: Some(10),
        output: Some(1),
        total: Some(38),
        cached: Some(20),
        cache_write: Some(2),
        cache_write_5m: Some(3),
        cache_write_1h: Some(4),
        anthropic_cache_details: true,
        ..Default::default()
    };
    let wire = encoder
        .encode(
            b"",
            &[
                Event {
                    kind: EventKind::Usage,
                    usage,
                    ..Event::EMPTY
                },
                Event::EMPTY,
                Event {
                    kind: EventKind::ChoiceEnd,
                    ..Event::EMPTY
                },
                Event {
                    kind: EventKind::Finish,
                    ..Event::EMPTY
                },
            ],
        )
        .unwrap();
    assert_eq!(encoder.state.usage().input, Some(37));
    assert_eq!(encoder.state.usage().uncached_input, Some(10));
    assert_eq!(encoder.state.usage().cache_write, Some(2));
    assert!(wire.contains("\"input_tokens\":10"));
    assert!(wire.contains("\"cache_read_input_tokens\":20"));
    assert!(wire.contains("\"cache_creation_input_tokens\":2"));
    assert!(wire.contains("\"ephemeral_5m_input_tokens\":3"));
    assert!(wire.contains("\"ephemeral_1h_input_tokens\":4"));
}

#[test]
fn explicit_unknown_usage_is_null_and_unstaged_usage_stays_absent() {
    for seen in [false, true] {
        let mut encoder = Encoder::new(Protocol::OpenAiChat, Protocol::OpenAiChat);
        encoder.encode(b"", &[Event::EMPTY]).unwrap();
        if seen {
            encoder
                .encode(
                    b"",
                    &[Event {
                        kind: EventKind::Usage,
                        ..Event::EMPTY
                    }],
                )
                .unwrap();
        }
        let wire = encoder
            .encode(
                b"",
                &[
                    Event {
                        kind: EventKind::ChoiceEnd,
                        ..Event::EMPTY
                    },
                    Event {
                        kind: EventKind::Finish,
                        ..Event::EMPTY
                    },
                ],
            )
            .unwrap();
        assert_eq!(wire.contains("\"usage\":null"), seen);
        assert!(!wire.contains("\"usage\":{}"));
    }
}

#[test]
fn chat_tool_indexes_are_independent_of_visible_text_blocks() {
    let mut encoder = Encoder::new(Protocol::OpenAiChat, Protocol::OpenAiChat);
    let span = |offset, length| EventText {
        offset,
        length,
        quoted: false,
        present: true,
    };
    let key = Key {
        index: 1,
        ..Key::default()
    };
    let wire = encoder
        .encode(
            b"textcallf{}",
            &[
                Event::EMPTY,
                Event {
                    kind: EventKind::BlockStart,
                    ..Event::EMPTY
                },
                Event {
                    kind: EventKind::Delta,
                    a: span(0, 4),
                    ..Event::EMPTY
                },
                Event {
                    kind: EventKind::BlockStart,
                    key,
                    block: BlockKind::Tool,
                    a: span(4, 4),
                    b: span(8, 1),
                    ..Event::EMPTY
                },
                Event {
                    kind: EventKind::Delta,
                    key,
                    block: BlockKind::Tool,
                    a: span(9, 2),
                    ..Event::EMPTY
                },
            ],
        )
        .unwrap();
    assert!(wire.contains("\"tool_calls\":[{\"index\":0"));
    assert!(!wire.contains("\"tool_calls\":[{\"index\":1"));
}

#[test]
fn complete_chat_tool_call_extensions_stay_outside_function() {
    let input = br#"{"id":"r","model":"m","created":1,"choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":[{"id":"call","type":"function","function":{"name":"f","arguments":"{}","provider_metadata":{"protocol":"gemini","signature":"sig"}}}]}}]}"#;
    let mut tokens = [Token::EMPTY; 256];
    let document = json::parse(input, &mut tokens, Default::default()).unwrap();
    let mut records = [zconv::ir::Record::EMPTY; 128];
    let response = zconv::protocol::decode_response(
        format(Protocol::OpenAiChat),
        document.root(),
        &mut records,
    )
    .unwrap();
    for record in response.records.iter_mut() {
        if let zconv::ir::Record::Extension(extension) = record
            && extension.key.eq_str("provider_metadata")
        {
            extension.location = "tool_call";
        }
    }
    let mut out = [0; 8192];
    let mut scratch = [0; 1024];
    let report = zconv::protocol::encode_response(
        &response,
        format(Protocol::OpenAiChat),
        &Default::default(),
        Default::default(),
        &mut out,
        &mut scratch,
        &mut tokens,
    )
    .unwrap();
    let doc = json::parse(&out[..report.written], &mut tokens, Default::default()).unwrap();
    let call = doc
        .root()
        .get("choices")
        .unwrap()
        .array()
        .next()
        .unwrap()
        .get("message")
        .unwrap()
        .get("tool_calls")
        .unwrap()
        .array()
        .next()
        .unwrap();
    assert!(call.get("provider_metadata").is_some());
    assert!(
        call.get("function")
            .unwrap()
            .get("provider_metadata")
            .is_none()
    );
}
