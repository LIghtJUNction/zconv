use zconv::{
    ir::*,
    json::{self, Token},
    protocol, Error, Format, Protocol,
};

const PROTOCOLS: [Protocol; 6] = [
    Protocol::OpenAiCompletions,
    Protocol::OpenAiChat,
    Protocol::OpenAiResponses,
    Protocol::AnthropicMessages,
    Protocol::GeminiGenerateContent,
    Protocol::GeminiInteractions,
];
const REQUESTS: [&[u8]; 6] = [
    br#"{"model":"m","prompt":"hello"}"#,
    br#"{"model":"m","messages":[{"role":"user","content":"hello"}]}"#,
    br#"{"model":"m","input":"hello"}"#,
    br#"{"model":"m","max_tokens":32,"messages":[{"role":"user","content":"hello"}]}"#,
    br#"{"contents":[{"role":"user","parts":[{"text":"hello"}]}]}"#,
    br#"{"model":"m","input":"hello"}"#,
];
const RESPONSES: [&[u8]; 6] = [
    br#"{"id":"r","model":"m","created":1,"choices":[{"index":0,"text":"hello","finish_reason":"stop"}]}"#,
    br#"{"id":"r","model":"m","created":1,"choices":[{"index":0,"message":{"role":"assistant","content":"hello"},"finish_reason":"stop"}]}"#,
    br#"{"id":"r","model":"m","created_at":1,"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]}]}"#,
    br#"{"id":"r","model":"m","type":"message","role":"assistant","content":[{"type":"text","text":"hello"}],"stop_reason":"end_turn"}"#,
    br#"{"responseId":"r","modelVersion":"m","candidates":[{"index":0,"content":{"role":"model","parts":[{"text":"hello"}]},"finishReason":"STOP"}]}"#,
    br#"{"id":"r","model":"m","created":"2026-11-10T00:00:00Z","updated":"2026-11-10T00:00:01Z","status":"completed","steps":[{"type":"model_output","content":[{"type":"text","text":"hello"}]}]}"#,
];

fn context() -> Context<'static> {
    Context {
        model: Some("m"),
        response_id: Some("r"),
        created: Some(1),
        created_iso: Some("2026-11-10T00:00:00Z"),
        updated_iso: Some("2026-11-10T00:00:01Z"),
        max_tokens: Some(32),
        item_ids: &["msg_0", "msg_1", "msg_2"],
        ..Context::default()
    }
}

#[test]
fn all_six_requests_decode_and_encode_to_all_six() {
    for (source, input) in PROTOCOLS.iter().zip(REQUESTS) {
        for target in PROTOCOLS {
            let mut tokens = [Token::EMPTY; 512];
            let mut records = [Record::EMPTY; 128];
            let document = json::parse(input, &mut tokens, json::Limits::default()).unwrap();
            let request =
                protocol::decode_request(Format::new(*source), document.root(), &mut records)
                    .unwrap();
            // Token tape is independent of decoded IR and immediately reusable.
            let mut out = [0u8; 4096];
            let mut scratch = [0u8; 4096];
            let report = protocol::encode_request(
                &request,
                Format::new(target),
                &context(),
                Policy::default(),
                &mut out,
                &mut scratch,
                &mut tokens,
            )
            .unwrap_or_else(|e| panic!("{source:?} -> {target:?}: {e:?}"));
            let result =
                json::parse(&out[..report.written], &mut tokens, json::Limits::default()).unwrap();
            protocol::validate(
                Format::new(target),
                zconv::Operation::Request,
                result.root(),
            )
            .unwrap_or_else(|e| {
                panic!(
                    "{source:?} -> {target:?}: {e:?}; {}",
                    String::from_utf8_lossy(&out[..report.written])
                )
            });
        }
    }
}

#[test]
fn all_six_responses_decode_and_encode_to_all_six() {
    for (source, input) in PROTOCOLS.iter().zip(RESPONSES) {
        for target in PROTOCOLS {
            let mut tokens = [Token::EMPTY; 512];
            let mut records = [Record::EMPTY; 128];
            let document = json::parse(input, &mut tokens, json::Limits::default()).unwrap();
            let response =
                protocol::decode_response(Format::new(*source), document.root(), &mut records)
                    .unwrap();
            let mut out = [0u8; 4096];
            let mut scratch = [0u8; 4096];
            let report = protocol::encode_response(
                &response,
                Format::new(target),
                &context(),
                Policy {
                    drop_metadata: true,
                    ..Policy::default()
                },
                &mut out,
                &mut scratch,
                &mut tokens,
            )
            .unwrap_or_else(|e| panic!("{source:?} -> {target:?}: {e:?}"));
            let result =
                json::parse(&out[..report.written], &mut tokens, json::Limits::default()).unwrap();
            protocol::validate(
                Format::new(target),
                zconv::Operation::Response,
                result.root(),
            )
            .unwrap_or_else(|e| {
                panic!(
                    "{source:?} -> {target:?}: {e:?}; {}",
                    String::from_utf8_lossy(&out[..report.written])
                )
            });
        }
    }
}

#[test]
fn caller_can_edit_text_then_reuse_token_tape() {
    let mut tokens = [Token::EMPTY; 128];
    let mut records = [Record::EMPTY; 16];
    let doc = json::parse(REQUESTS[1], &mut tokens, json::Limits::default()).unwrap();
    let request =
        protocol::decode_request(Format::new(Protocol::OpenAiChat), doc.root(), &mut records)
            .unwrap();
    for record in request.records.iter_mut() {
        if let Record::Block(b) = record {
            if matches!(b.content, Content::Text(_)) {
                b.content = Content::Text(Text::Utf8("edited 😀\n\""));
            }
        }
    }
    let mut out = [0u8; 1024];
    let mut scratch = [0u8; 128];
    let report = protocol::encode_request(
        &request,
        Format::new(Protocol::OpenAiResponses),
        &context(),
        Policy::default(),
        &mut out,
        &mut scratch,
        &mut tokens,
    )
    .unwrap();
    let result = json::parse(&out[..report.written], &mut tokens, json::Limits::default()).unwrap();
    let text = result
        .root()
        .get("input")
        .unwrap()
        .array()
        .next()
        .unwrap()
        .get("content")
        .unwrap()
        .array()
        .next()
        .unwrap()
        .get("text")
        .unwrap()
        .as_string()
        .unwrap();
    assert!(text.eq_str("edited 😀\n\""));
}

#[test]
fn extensions_are_source_owned_and_losses_identify_them() {
    let input = br#"{"model":"m","messages":[{"role":"user","content":"hi"}],"vendor_secret":0}"#;
    let mut tokens = [Token::EMPTY; 128];
    let mut records = [Record::EMPTY; 32];
    let doc = json::parse(input, &mut tokens, json::Limits::default()).unwrap();
    let request =
        protocol::decode_request(Format::new(Protocol::OpenAiChat), doc.root(), &mut records)
            .unwrap();
    let mut out = [0; 1024];
    let mut scratch = [0; 128];
    let target = Format::new(Protocol::OpenAiResponses);
    assert!(matches!(
        protocol::encode_request(
            &request,
            target,
            &context(),
            Policy::default(),
            &mut out,
            &mut scratch,
            &mut tokens
        ),
        Err(Error::PolicyViolation { .. })
    ));
    let policy = Policy {
        drop_extensions: true,
        ..Policy::default()
    };
    let report = protocol::encode_request(
        &request,
        target,
        &context(),
        policy,
        &mut out,
        &mut scratch,
        &mut tokens,
    )
    .unwrap();
    assert_eq!(report.dropped_extensions, 1);
    let losses: Vec<_> = request.losses(target, policy).collect();
    assert_eq!(losses.len(), 1);
    assert!(matches!(losses[0],Loss::Extension(e) if e.key.eq_str("vendor_secret")));
}

#[test]
fn exact_usage_large_integer_survives() {
    let input=br#"{"id":"r","model":"m","created":1,"choices":[{"index":0,"message":{"role":"assistant","content":"x"},"finish_reason":"stop"}],"usage":{"prompt_tokens":9007199254740993,"completion_tokens":0,"total_tokens":9007199254740993}}"#;
    let mut tokens = [Token::EMPTY; 256];
    let mut records = [Record::EMPTY; 32];
    let doc = json::parse(input, &mut tokens, json::Limits::default()).unwrap();
    let response =
        protocol::decode_response(Format::new(Protocol::OpenAiChat), doc.root(), &mut records)
            .unwrap();
    let mut out = [0; 2048];
    let mut scratch = [0; 128];
    let report = protocol::encode_response(
        &response,
        Format::new(Protocol::OpenAiResponses),
        &context(),
        Policy::default(),
        &mut out,
        &mut scratch,
        &mut tokens,
    )
    .unwrap();
    let doc = json::parse(&out[..report.written], &mut tokens, json::Limits::default()).unwrap();
    assert_eq!(
        doc.root()
            .get("usage")
            .unwrap()
            .get("input_tokens")
            .unwrap()
            .as_u64(),
        Some(9007199254740993)
    );
}

#[test]
fn context_namespace_does_not_cross_vendors() {
    let input = br#"{"model":"m","input":"hi","previous_interaction_id":"server-state"}"#;
    let mut tokens = [Token::EMPTY; 128];
    let mut records = [Record::EMPTY; 32];
    let doc = json::parse(input, &mut tokens, json::Limits::default()).unwrap();
    let request = protocol::decode_request(
        Format::new(Protocol::GeminiInteractions),
        doc.root(),
        &mut records,
    )
    .unwrap();
    let mut out = [0; 2048];
    let mut scratch = [0; 128];
    assert!(matches!(
        protocol::encode_request(
            &request,
            Format::new(Protocol::OpenAiResponses),
            &context(),
            Policy::default(),
            &mut out,
            &mut scratch,
            &mut tokens
        ),
        Err(Error::ContextRequired { .. })
    ));
}

#[test]
fn unsupported_anthropic_version_controls_rejected() {
    let input = br#"{"model":"m","max_tokens":1,"temperature":1,"messages":[]}"#;
    let mut tokens = [Token::EMPTY; 128];
    let doc = json::parse(input, &mut tokens, json::Limits::default()).unwrap();
    let mut format = Format::new(Protocol::AnthropicMessages);
    format.version = "2026-10-21";
    assert!(protocol::validate(format, zconv::Operation::Request, doc.root()).is_err());
}

#[test]
fn workspace_and_output_failures_report_recovery_sizes() {
    let mut tokens = [Token::EMPTY; 128];
    let doc = json::parse(REQUESTS[1], &mut tokens, json::Limits::default()).unwrap();
    assert!(matches!(
        protocol::decode_request(Format::new(Protocol::OpenAiChat), doc.root(), &mut []),
        Err(Error::IrTooSmall { needed: 1 })
    ));
    let mut records = [Record::EMPTY; 32];
    let request =
        protocol::decode_request(Format::new(Protocol::OpenAiChat), doc.root(), &mut records)
            .unwrap();
    let mut scratch = [0; 128];
    let err = protocol::encode_request(
        &request,
        Format::new(Protocol::OpenAiResponses),
        &context(),
        Policy::default(),
        &mut [],
        &mut scratch,
        &mut tokens,
    )
    .unwrap_err();
    let needed = if let Error::OutputTooSmall { needed } = err {
        needed
    } else {
        panic!("{err:?}")
    };
    let mut out = vec![0; needed];
    assert_eq!(
        protocol::encode_request(
            &request,
            Format::new(Protocol::OpenAiResponses),
            &context(),
            Policy::default(),
            &mut out,
            &mut scratch,
            &mut tokens
        )
        .unwrap()
        .written,
        needed
    );
}

fn request_conversion(
    input: &[u8],
    source: Protocol,
    target: Protocol,
    policy: Policy,
) -> Result<Vec<u8>, Error> {
    let mut tokens = [Token::EMPTY; 1024];
    let mut records = [Record::EMPTY; 128];
    let doc = json::parse(input, &mut tokens, json::Limits::default())?;
    let request = protocol::decode_request(source.into(), doc.root(), &mut records)?;
    let mut output = vec![0; 16384];
    let mut scratch = [0; 4096];
    let report = protocol::encode_request(
        &request,
        target.into(),
        &context(),
        policy,
        &mut output,
        &mut scratch,
        &mut tokens,
    )?;
    let written = report.written;
    output.truncate(written);
    Ok(output)
}

#[test]
fn tools_definitions_calls_and_results_cross_all_modern_protocols() {
    let cases=[
        (Protocol::OpenAiChat,br#"{"model":"m","messages":[{"role":"assistant","content":null,"tool_calls":[{"id":"c","type":"function","function":{"name":"f","arguments":"{\"x\":1}"}}]},{"role":"tool","tool_call_id":"c","content":"{\"ok\":true}"}],"tools":[{"type":"function","function":{"name":"f","parameters":{"type":"object","properties":{"x":{"type":"integer"}}}}}]}"#.as_slice()),
        (Protocol::OpenAiResponses,br#"{"model":"m","input":[{"type":"function_call","call_id":"c","name":"f","arguments":"{\"x\":1}"},{"type":"function_call_output","call_id":"c","output":"{\"ok\":true}"}],"tools":[{"type":"function","name":"f","strict":false,"parameters":{"type":"object","properties":{"x":{"type":"integer"}}}}]}"#.as_slice()),
        (Protocol::AnthropicMessages,br#"{"model":"m","max_tokens":32,"messages":[{"role":"assistant","content":[{"type":"tool_use","id":"c","name":"f","input":{"x":1}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"c","content":"{\"ok\":true}"}]}],"tools":[{"name":"f","input_schema":{"type":"object","properties":{"x":{"type":"integer"}}}}]}"#.as_slice()),
        (Protocol::GeminiGenerateContent,br#"{"contents":[{"role":"model","parts":[{"functionCall":{"id":"c","name":"f","args":{"x":1}}}]},{"role":"user","parts":[{"functionResponse":{"id":"c","name":"f","response":{"ok":true}}}]}],"tools":[{"functionDeclarations":[{"name":"f","parameters":{"type":"object","properties":{"x":{"type":"integer"}}}}]}]}"#.as_slice()),
        (Protocol::GeminiInteractions,br#"{"model":"m","input":[{"type":"function_call","id":"c","name":"f","arguments":{"x":1}},{"type":"function_result","call_id":"c","name":"f","result":{"ok":true}}],"tools":[{"type":"function","name":"f","parameters":{"type":"object","properties":{"x":{"type":"integer"}}}}]}"#.as_slice()),
    ];
    for (source, input) in cases {
        for target in PROTOCOLS.into_iter().skip(1) {
            let out = request_conversion(input, source, target, Policy::default())
                .unwrap_or_else(|e| panic!("tools {source:?}->{target:?}: {e:?}"));
            let mut tape = [Token::EMPTY; 1024];
            let mut storage = [Record::EMPTY; 128];
            let doc = json::parse(&out, &mut tape, json::Limits::default()).unwrap();
            let decoded = protocol::decode_request(target.into(), doc.root(), &mut storage)
                .unwrap_or_else(|e| panic!("{target:?}: {e:?}: {}", String::from_utf8_lossy(&out)));
            assert_eq!(
                decoded
                    .records
                    .iter()
                    .filter(|r| matches!(r, Record::Tool(_)))
                    .count(),
                1
            );
            assert_eq!(
                decoded
                    .records
                    .iter()
                    .filter(|r| matches!(
                        r,
                        Record::Block(Block {
                            content: Content::ToolCall { .. },
                            ..
                        })
                    ))
                    .count(),
                1
            );
            assert_eq!(
                decoded
                    .records
                    .iter()
                    .filter(|r| matches!(
                        r,
                        Record::Block(Block {
                            content: Content::ToolResult { .. },
                            ..
                        })
                    ))
                    .count(),
                1,
                "{source:?}->{target:?}: {}",
                String::from_utf8_lossy(&out)
            );
        }
    }
}

#[test]
fn responses_implicit_tool_strictness_requires_caller_resolution() {
    for strict_field in ["", ",\"strict\":null"] {
        let input = format!(
            "{{\"model\":\"m\",\"input\":\"hello\",\"tools\":[{{\"type\":\"function\",\"name\":\"f\",\"parameters\":{{\"type\":\"object\"}}{strict_field}}}]}}"
        );
        for target in PROTOCOLS.into_iter().skip(1) {
            if target == Protocol::OpenAiResponses {
                let output = request_conversion(
                    input.as_bytes(),
                    Protocol::OpenAiResponses,
                    target,
                    Policy::default(),
                )
                .unwrap();
                let mut tape = [Token::EMPTY; 128];
                let doc = json::parse(&output, &mut tape, json::Limits::default()).unwrap();
                let strict = doc
                    .root()
                    .get("tools")
                    .unwrap()
                    .array()
                    .next()
                    .unwrap()
                    .get("strict");
                assert_eq!(
                    strict.map(|v| v.is_null()),
                    if strict_field.is_empty() {
                        None
                    } else {
                        Some(true)
                    }
                );
            } else {
                assert_eq!(
                    request_conversion(
                        input.as_bytes(),
                        Protocol::OpenAiResponses,
                        target,
                        Policy::default()
                    ),
                    Err(Error::ContextRequired {
                        field: "resolved tool strict mode"
                    }),
                );
            }
        }
        let mut tape = [Token::EMPTY; 128];
        let mut records = [Record::EMPTY; 16];
        let doc = json::parse(input.as_bytes(), &mut tape, json::Limits::default()).unwrap();
        let request =
            protocol::decode_request(Protocol::OpenAiResponses.into(), doc.root(), &mut records)
                .unwrap();
        for record in request.records.iter_mut() {
            if let Record::Tool(tool) = record {
                assert!(!matches!(tool.strict, Presence::Value(_)));
                tool.strict = Presence::Value(false);
            }
        }
        let mut output = [0; 1024];
        let mut scratch = [0; 128];
        protocol::encode_request(
            &request,
            Protocol::AnthropicMessages.into(),
            &context(),
            Policy::default(),
            &mut output,
            &mut scratch,
            &mut tape,
        )
        .unwrap();
    }
}

#[test]
fn strict_tool_guarantees_are_never_lost_in_best_effort_targets() {
    let input = br#"{"model":"m","input":"hello","tools":[{"type":"function","name":"f","strict":true,"parameters":{"type":"object"}}]}"#;
    for target in [
        Protocol::AnthropicMessages,
        Protocol::GeminiGenerateContent,
        Protocol::GeminiInteractions,
    ] {
        assert_eq!(
            request_conversion(input, Protocol::OpenAiResponses, target, Policy::default()),
            Err(Error::Unsupported {
                feature: "strict tool schema guarantee in target"
            })
        );
    }
}

#[test]
fn legacy_functions_keep_legacy_wire_and_need_ids_when_upgrading() {
    let input=br#"{"model":"m","messages":[{"role":"assistant","content":null,"function_call":{"name":"f","arguments":"{}"}},{"role":"function","name":"f","content":"done"}],"functions":[{"name":"f","parameters":{"type":"object"}}],"function_call":"auto"}"#;
    let output = request_conversion(
        input,
        Protocol::OpenAiChat,
        Protocol::OpenAiChat,
        Policy::default(),
    )
    .unwrap();
    let mut tokens = [Token::EMPTY; 512];
    let doc = json::parse(&output, &mut tokens, json::Limits::default()).unwrap();
    assert!(doc.root().get("functions").is_some());
    assert!(doc.root().get("tools").is_none());
    assert!(doc.root().get("function_call").is_some());
    assert!(doc
        .root()
        .get("messages")
        .unwrap()
        .array()
        .next()
        .unwrap()
        .get("function_call")
        .is_some());
    assert!(matches!(
        request_conversion(
            input,
            Protocol::OpenAiChat,
            Protocol::AnthropicMessages,
            Policy::default()
        ),
        Err(Error::ContextRequired {
            field: "tool call ID"
        })
    ));
}

#[test]
fn data_urls_become_native_base64_without_aggregation() {
    let input=br#"{"model":"m","messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image\/png;base64,YQ=="}}]}]}"#;
    for target in [
        Protocol::AnthropicMessages,
        Protocol::GeminiGenerateContent,
        Protocol::GeminiInteractions,
    ] {
        let output =
            request_conversion(input, Protocol::OpenAiChat, target, Policy::default()).unwrap();
        let string = String::from_utf8(output).unwrap();
        assert!(string.contains("image/png"));
        assert!(string.contains("YQ=="));
        assert!(!string.contains("data:image"));
    }
}

#[test]
fn inline_audio_and_provider_file_references_are_typed() {
    let audio=br#"{"model":"m","messages":[{"role":"user","content":[{"type":"input_audio","input_audio":{"data":"YQ==","format":"wav"}}]}]}"#;
    let out = request_conversion(
        audio,
        Protocol::OpenAiChat,
        Protocol::GeminiGenerateContent,
        Policy::default(),
    )
    .unwrap();
    assert!(String::from_utf8(out).unwrap().contains("audio/wav"));
    let file=br#"{"model":"m","input":[{"role":"user","content":[{"type":"input_file","file_id":"file_abc"}]}]}"#;
    assert!(matches!(
        request_conversion(
            file,
            Protocol::OpenAiResponses,
            Protocol::AnthropicMessages,
            Policy::default()
        ),
        Err(Error::ContextRequired {
            field: "mapped provider file reference"
        })
    ));
}

#[test]
fn structured_schema_chat_responses_roundtrip_and_gemini_same_format() {
    let input=br#"{"model":"m","messages":[{"role":"user","content":"hi"}],"response_format":{"type":"json_schema","json_schema":{"name":"answer","strict":true,"schema":{"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false}}}}"#;
    let response = request_conversion(
        input,
        Protocol::OpenAiChat,
        Protocol::OpenAiResponses,
        Policy::default(),
    )
    .unwrap();
    let mut tokens = [Token::EMPTY; 512];
    let doc = json::parse(&response, &mut tokens, json::Limits::default()).unwrap();
    let format = doc.root().get("text").unwrap().get("format").unwrap();
    assert_eq!(format.get("strict").unwrap().as_bool(), Some(true));
    assert!(format.get("schema").is_some());
    let back = request_conversion(
        &response,
        Protocol::OpenAiResponses,
        Protocol::OpenAiChat,
        Policy::default(),
    )
    .unwrap();
    let doc = json::parse(&back, &mut tokens, json::Limits::default()).unwrap();
    assert!(doc
        .root()
        .get("response_format")
        .unwrap()
        .get("json_schema")
        .unwrap()
        .get("name")
        .unwrap()
        .as_string()
        .unwrap()
        .eq_str("answer"));
    let google=br#"{"contents":[{"parts":[{"text":"hi"}]}],"generationConfig":{"responseMimeType":"application/json","responseJsonSchema":{"type":"object"}}}"#;
    assert!(request_conversion(
        google,
        Protocol::GeminiGenerateContent,
        Protocol::GeminiGenerateContent,
        Policy::default()
    )
    .is_ok());
}

#[test]
fn public_ir_ranges_fail_safely() {
    for bad_end in [0, usize::MAX] {
        let mut tokens = [Token::EMPTY; 128];
        let mut records = [Record::EMPTY; 16];
        let doc = json::parse(REQUESTS[1], &mut tokens, json::Limits::default()).unwrap();
        let request =
            protocol::decode_request(Protocol::OpenAiChat.into(), doc.root(), &mut records)
                .unwrap();
        if let Record::Message(m) = &mut request.records[0] {
            m.end = bad_end;
        }
        let mut out = [0; 1024];
        let mut scratch = [];
        assert!(matches!(
            protocol::encode_request(
                &request,
                Protocol::OpenAiResponses.into(),
                &context(),
                Policy::default(),
                &mut out,
                &mut scratch,
                &mut tokens
            ),
            Err(Error::InvalidInput { .. })
        ));
    }
}

#[test]
fn anthropic_cache_usage_is_inclusive_in_ir_and_reversible() {
    let input=br#"{"id":"r","model":"m","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":20}}"#;
    let mut tokens = [Token::EMPTY; 512];
    let mut records = [Record::EMPTY; 32];
    let doc = json::parse(input, &mut tokens, json::Limits::default()).unwrap();
    let response =
        protocol::decode_response(Protocol::AnthropicMessages.into(), doc.root(), &mut records)
            .unwrap();
    assert_eq!(
        response.usage.input_tokens.value().unwrap().as_u64(),
        Some(30)
    );
    let mut out = [0; 2048];
    let mut scratch = [];
    let report = protocol::encode_response(
        &response,
        Protocol::OpenAiChat.into(),
        &context(),
        Policy::default(),
        &mut out,
        &mut scratch,
        &mut tokens,
    )
    .unwrap();
    let doc = json::parse(&out[..report.written], &mut tokens, json::Limits::default()).unwrap();
    let usage = doc.root().get("usage").unwrap();
    assert_eq!(usage.get("prompt_tokens").unwrap().as_u64(), Some(30));
    assert_eq!(usage.get("total_tokens").unwrap().as_u64(), Some(35));
    let mut other = [Record::EMPTY; 32];
    let back =
        protocol::decode_response(Protocol::OpenAiChat.into(), doc.root(), &mut other).unwrap();
    let mut final_out = [0; 2048];
    let report = protocol::encode_response(
        &back,
        Protocol::AnthropicMessages.into(),
        &context(),
        Policy {
            drop_metadata: true,
            ..Policy::default()
        },
        &mut final_out,
        &mut scratch,
        &mut tokens,
    )
    .unwrap();
    let doc = json::parse(
        &final_out[..report.written],
        &mut tokens,
        json::Limits::default(),
    )
    .unwrap();
    assert_eq!(
        doc.root()
            .get("usage")
            .unwrap()
            .get("input_tokens")
            .unwrap()
            .as_u64(),
        Some(10)
    );
}

#[test]
fn metadata_losses_enumerate_every_dropped_field() {
    let input=br#"{"id":"r","model":"m","created_at":1,"status":"completed","output":[{"id":"msg","status":"completed","type":"message","role":"assistant","content":[{"type":"output_text","text":"x"}]}]}"#;
    let mut tokens = [Token::EMPTY; 256];
    let mut records = [Record::EMPTY; 32];
    let doc = json::parse(input, &mut tokens, json::Limits::default()).unwrap();
    let response =
        protocol::decode_response(Protocol::OpenAiResponses.into(), doc.root(), &mut records)
            .unwrap();
    let mut out = [0; 2048];
    let mut scratch = [];
    let policy = Policy {
        drop_metadata: true,
        ..Policy::default()
    };
    let target = Protocol::AnthropicMessages.into();
    let report = protocol::encode_response(
        &response,
        target,
        &context(),
        policy,
        &mut out,
        &mut scratch,
        &mut tokens,
    )
    .unwrap();
    let losses: Vec<_> = response.losses(target, policy).collect();
    assert_eq!(losses.len(), report.dropped_metadata);
    assert_eq!(losses.len(), 3);
}

#[test]
fn complete_tool_responses_map_stop_state_and_arguments() {
    let cases=[
        (Protocol::OpenAiChat,br#"{"id":"r","model":"m","created":1,"choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":[{"id":"c","type":"function","function":{"name":"f","arguments":"{\"x\":1}"}}]},"finish_reason":"tool_calls"}]}"#.as_slice()),
        (Protocol::OpenAiResponses,br#"{"id":"r","model":"m","created_at":1,"status":"completed","output":[{"id":"item","type":"function_call","call_id":"c","name":"f","arguments":"{\"x\":1}"}]}"#.as_slice()),
        (Protocol::AnthropicMessages,br#"{"id":"r","model":"m","type":"message","role":"assistant","content":[{"type":"tool_use","id":"c","name":"f","input":{"x":1}}],"stop_reason":"tool_use"}"#.as_slice()),
        (Protocol::GeminiGenerateContent,br#"{"responseId":"r","modelVersion":"m","candidates":[{"index":0,"content":{"role":"model","parts":[{"functionCall":{"id":"c","name":"f","args":{"x":1}}}]},"finishReason":"STOP"}]}"#.as_slice()),
        (Protocol::GeminiInteractions,br#"{"id":"r","model":"m","created":"2026-11-10T00:00:00Z","updated":"2026-11-10T00:00:01Z","status":"requires_action","steps":[{"type":"function_call","id":"c","name":"f","arguments":{"x":1}}]}"#.as_slice()),
    ];
    for (source, input) in cases {
        for target in PROTOCOLS.into_iter().skip(1) {
            let mut tokens = [Token::EMPTY; 512];
            let mut records = [Record::EMPTY; 64];
            let doc = json::parse(input, &mut tokens, json::Limits::default()).unwrap();
            let response =
                protocol::decode_response(source.into(), doc.root(), &mut records).unwrap();
            let mut out = [0; 4096];
            let mut scratch = [0; 512];
            let report = protocol::encode_response(
                &response,
                target.into(),
                &context(),
                Policy {
                    drop_metadata: true,
                    ..Policy::default()
                },
                &mut out,
                &mut scratch,
                &mut tokens,
            )
            .unwrap_or_else(|e| panic!("{source:?}->{target:?}: {e:?}"));
            let doc =
                json::parse(&out[..report.written], &mut tokens, json::Limits::default()).unwrap();
            let mut other = [Record::EMPTY; 64];
            let decoded = protocol::decode_response(target.into(), doc.root(), &mut other)
                .unwrap_or_else(|e| {
                    panic!(
                        "{source:?}->{target:?}: {e:?}: {}",
                        String::from_utf8_lossy(&out[..report.written])
                    )
                });
            assert!(
                decoded.records.iter().any(|r| matches!(
                    r,
                    Record::Choice(Choice {
                        finish: Finish::ToolCalls,
                        ..
                    })
                )),
                "{source:?}->{target:?}"
            );
            assert_eq!(
                decoded
                    .records
                    .iter()
                    .filter(|r| matches!(
                        r,
                        Record::Block(Block {
                            content: Content::ToolCall { .. },
                            ..
                        })
                    ))
                    .count(),
                1
            );
        }
    }
}

#[test]
fn fields_unknown_to_this_protocol_remain_extensions() {
    let input=br#"{"model":"m","messages":[],"prompt":"do not lose me","top_k":3,"previous_interaction_id":"foreign"}"#;
    let output = request_conversion(
        input,
        Protocol::OpenAiChat,
        Protocol::OpenAiChat,
        Policy::default(),
    )
    .unwrap();
    let mut tokens = [Token::EMPTY; 256];
    let doc = json::parse(&output, &mut tokens, json::Limits::default()).unwrap();
    assert!(doc.root().get("prompt").is_some());
    assert!(doc.root().get("top_k").is_some());
    assert!(doc.root().get("previous_interaction_id").is_some());
    assert!(matches!(
        request_conversion(
            input,
            Protocol::OpenAiChat,
            Protocol::OpenAiResponses,
            Policy::default()
        ),
        Err(Error::PolicyViolation { .. })
    ));
}

#[test]
fn empty_tools_and_null_fields_remain_distinct() {
    let input=br#"{"model":"m","messages":[{"role":"assistant","content":null,"tool_calls":[],"refusal":null}],"tools":[],"temperature":0}"#;
    let output = request_conversion(
        input,
        Protocol::OpenAiChat,
        Protocol::OpenAiChat,
        Policy::default(),
    )
    .unwrap();
    let mut tokens = [Token::EMPTY; 256];
    let doc = json::parse(&output, &mut tokens, json::Limits::default()).unwrap();
    let message = doc.root().get("messages").unwrap().array().next().unwrap();
    assert!(message.get("content").unwrap().is_null());
    assert!(message.get("tool_calls").unwrap().array().next().is_none());
    assert!(message.get("refusal").unwrap().is_null());
    assert!(doc.root().get("tools").is_some());
    assert_eq!(doc.root().get("temperature").unwrap().as_u64(), Some(0));
}

#[test]
fn role_names_are_protocol_specific() {
    for (protocol, input) in [
        (
            Protocol::OpenAiChat,
            br#"{"model":"m","messages":[{"role":"model","content":"x"}]}"#.as_slice(),
        ),
        (
            Protocol::AnthropicMessages,
            br#"{"model":"m","max_tokens":1,"messages":[{"role":"model","content":"x"}]}"#
                .as_slice(),
        ),
        (
            Protocol::GeminiGenerateContent,
            br#"{"contents":[{"role":"assistant","parts":[{"text":"x"}]}]}"#.as_slice(),
        ),
    ] {
        let mut tape = [Token::EMPTY; 128];
        let doc = json::parse(input, &mut tape, json::Limits::default()).unwrap();
        assert!(matches!(
            protocol::validate(protocol.into(), zconv::Operation::Request, doc.root()),
            Err(Error::InvalidInput { field: "role", .. })
        ));
    }
}

#[test]
fn token_id_completion_prompt_requires_external_tokenizer_only_for_ir() {
    let input = br#"{"model":"m","prompt":[1,2,3]}"#;
    let mut tokens = [Token::EMPTY; 128];
    let doc = json::parse(input, &mut tokens, json::Limits::default()).unwrap();
    protocol::validate(
        Protocol::OpenAiCompletions.into(),
        zconv::Operation::Request,
        doc.root(),
    )
    .unwrap();
    let mut records = [Record::EMPTY; 16];
    assert!(matches!(
        protocol::decode_request(Protocol::OpenAiCompletions.into(), doc.root(), &mut records),
        Err(Error::ContextRequired { .. })
    ));
}

#[test]
fn reasoning_summary_keeps_item_identity_signature_and_order() {
    let input=br#"{"id":"r","model":"m","created_at":1,"status":"completed","output":[{"id":"reason_1","type":"reasoning","summary":[{"type":"summary_text","text":"first"},{"type":"summary_text","text":"second"}],"encrypted_content":"opaque-signature"}]}"#;
    let mut tokens = [Token::EMPTY; 512];
    let mut records = [Record::EMPTY; 32];
    let doc = json::parse(input, &mut tokens, json::Limits::default()).unwrap();
    let response =
        protocol::decode_response(Protocol::OpenAiResponses.into(), doc.root(), &mut records)
            .unwrap();
    let mut out = [0; 4096];
    let mut scratch = [];
    let report = protocol::encode_response(
        &response,
        Protocol::OpenAiResponses.into(),
        &context(),
        Policy::default(),
        &mut out,
        &mut scratch,
        &mut tokens,
    )
    .unwrap();
    let doc = json::parse(&out[..report.written], &mut tokens, json::Limits::default()).unwrap();
    let output = doc.root().get("output").unwrap();
    assert_eq!(output.array().count(), 1);
    let item = output.array().next().unwrap();
    assert!(item
        .get("id")
        .unwrap()
        .as_string()
        .unwrap()
        .eq_str("reason_1"));
    assert_eq!(item.get("summary").unwrap().array().count(), 2);
    assert!(item
        .get("encrypted_content")
        .unwrap()
        .as_string()
        .unwrap()
        .eq_str("opaque-signature"));
    assert!(matches!(
        protocol::encode_response(
            &response,
            Protocol::GeminiInteractions.into(),
            &context(),
            Policy {
                drop_metadata: true,
                ..Policy::default()
            },
            &mut out,
            &mut scratch,
            &mut tokens
        ),
        Err(Error::PolicyViolation {
            feature: "reasoning signature belongs to source"
        })
    ));
}

#[test]
fn refusal_and_safety_are_never_plain_success() {
    let input=br#"{"id":"r","model":"m","type":"message","role":"assistant","content":[{"type":"text","text":"declined"}],"stop_reason":"refusal"}"#;
    let mut tokens = [Token::EMPTY; 256];
    let mut records = [Record::EMPTY; 32];
    let doc = json::parse(input, &mut tokens, json::Limits::default()).unwrap();
    let response =
        protocol::decode_response(Protocol::AnthropicMessages.into(), doc.root(), &mut records)
            .unwrap();
    let mut out = [0; 2048];
    let mut scratch = [];
    let report = protocol::encode_response(
        &response,
        Protocol::OpenAiChat.into(),
        &context(),
        Policy::default(),
        &mut out,
        &mut scratch,
        &mut tokens,
    )
    .unwrap();
    let doc = json::parse(&out[..report.written], &mut tokens, json::Limits::default()).unwrap();
    assert!(doc
        .root()
        .get("choices")
        .unwrap()
        .array()
        .next()
        .unwrap()
        .get("message")
        .unwrap()
        .get("refusal")
        .unwrap()
        .as_string()
        .unwrap()
        .eq_str("declined"));
    let blocked = br#"{"promptFeedback":{"blockReason":"SAFETY"}}"#;
    let doc = json::parse(blocked, &mut tokens, json::Limits::default()).unwrap();
    let response = protocol::decode_response(
        Protocol::GeminiGenerateContent.into(),
        doc.root(),
        &mut records,
    )
    .unwrap();
    assert_eq!(response.status, Finish::Safety);
    let report = protocol::encode_response(
        &response,
        Protocol::OpenAiChat.into(),
        &context(),
        Policy {
            drop_extensions: true,
            ..Policy::default()
        },
        &mut out,
        &mut scratch,
        &mut tokens,
    )
    .unwrap();
    let doc = json::parse(&out[..report.written], &mut tokens, json::Limits::default()).unwrap();
    assert!(doc
        .root()
        .get("choices")
        .unwrap()
        .array()
        .next()
        .unwrap()
        .get("finish_reason")
        .unwrap()
        .as_string()
        .unwrap()
        .eq_str("content_filter"));
}

#[test]
fn failed_and_cancelled_resources_keep_status() {
    for state in ["failed", "cancelled"] {
        let input=format!("{{\"id\":\"r\",\"model\":\"m\",\"created_at\":1,\"status\":\"{state}\",\"output\":[],\"error\":null}}");
        let mut tokens = [Token::EMPTY; 256];
        let mut records = [Record::EMPTY; 32];
        let doc = json::parse(input.as_bytes(), &mut tokens, json::Limits::default()).unwrap();
        let response =
            protocol::decode_response(Protocol::OpenAiResponses.into(), doc.root(), &mut records)
                .unwrap();
        let mut out = [0; 2048];
        let mut scratch = [];
        let report = protocol::encode_response(
            &response,
            Protocol::GeminiInteractions.into(),
            &context(),
            Policy {
                drop_metadata: true,
                ..Policy::default()
            },
            &mut out,
            &mut scratch,
            &mut tokens,
        )
        .unwrap();
        let doc =
            json::parse(&out[..report.written], &mut tokens, json::Limits::default()).unwrap();
        assert!(doc
            .root()
            .get("status")
            .unwrap()
            .as_string()
            .unwrap()
            .eq_str(state));
        assert!(protocol::encode_response(
            &response,
            Protocol::OpenAiChat.into(),
            &context(),
            Policy::default(),
            &mut out,
            &mut scratch,
            &mut tokens
        )
        .is_err());
    }
}

#[test]
fn gemini_thought_counts_are_inclusive_only_in_ir_and_openai() {
    let input=br#"{"responseId":"r","modelVersion":"m","candidates":[{"index":0,"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":23,"thoughtsTokenCount":49,"totalTokenCount":79}}"#;
    let mut tokens = [Token::EMPTY; 512];
    let mut records = [Record::EMPTY; 32];
    let doc = json::parse(input, &mut tokens, json::Limits::default()).unwrap();
    let response = protocol::decode_response(
        Protocol::GeminiGenerateContent.into(),
        doc.root(),
        &mut records,
    )
    .unwrap();
    assert_eq!(
        response.usage.output_tokens.value().unwrap().as_u64(),
        Some(72)
    );
    let mut output = [0; 4096];
    let mut scratch = [];
    let report = protocol::encode_response(
        &response,
        Protocol::OpenAiChat.into(),
        &context(),
        Policy::default(),
        &mut output,
        &mut scratch,
        &mut tokens,
    )
    .unwrap();
    let doc = json::parse(
        &output[..report.written],
        &mut tokens,
        json::Limits::default(),
    )
    .unwrap();
    assert_eq!(
        doc.root()
            .get("usage")
            .unwrap()
            .get("completion_tokens")
            .unwrap()
            .as_u64(),
        Some(72)
    );
    let report = protocol::encode_response(
        &response,
        Protocol::GeminiInteractions.into(),
        &context(),
        Policy::default(),
        &mut output,
        &mut scratch,
        &mut tokens,
    )
    .unwrap();
    let doc = json::parse(
        &output[..report.written],
        &mut tokens,
        json::Limits::default(),
    )
    .unwrap();
    assert_eq!(
        doc.root()
            .get("usage")
            .unwrap()
            .get("total_output_tokens")
            .unwrap()
            .as_u64(),
        Some(23)
    );
    assert_eq!(
        doc.root()
            .get("usage")
            .unwrap()
            .get("total_thought_tokens")
            .unwrap()
            .as_u64(),
        Some(49)
    );
}
