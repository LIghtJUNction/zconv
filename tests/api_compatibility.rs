use zconv::{
    Error, Format, Operation, Options, Path, Profile, Protocol, Strategy, Workspace,
    ir::{Context, Data, Policy, Record, Span, ToolBinding},
    json::{self, Token},
};

fn compatible(protocol: Protocol) -> Format {
    Format {
        profile: Profile::OpenAiCompatible("test-provider"),
        ..protocol.into()
    }
}

fn convert(
    input: &[u8],
    source: Format,
    target: Format,
    operation: Operation,
    strategy: Strategy,
) -> Result<(String, Path), Error> {
    let mut tokens = [Token::EMPTY; 4096];
    let mut records = [Record::EMPTY; 1024];
    let mut nested = [Token::EMPTY; 2048];
    let mut scratch = [0; 32768];
    let mut output = [0; 65536];
    let result = zconv::convert(
        source,
        target,
        operation,
        input,
        &mut output,
        Options {
            strategy,
            context: Context {
                model: Some("m"),
                response_id: Some("r"),
                created: Some(1),
                max_tokens: Some(32),
                tool_calls: &[ToolBinding { id: "c", name: "f" }],
                item_ids: &["i0", "i1", "i2", "i3", "i4", "i5"],
                ..Default::default()
            },
            ..Default::default()
        },
        Workspace {
            tokens: &mut tokens,
            records: &mut records,
            scratch: &mut scratch,
            nested_tokens: &mut nested,
        },
    )?;
    Ok((
        String::from_utf8(output[..result.written()].to_vec()).unwrap(),
        result.path,
    ))
}

fn request(
    input: &[u8],
    source: Format,
    target: Format,
    strategy: Strategy,
) -> Result<String, Error> {
    convert(input, source, target, Operation::Request, strategy).map(|v| v.0)
}

#[test]
fn responses_assistant_content_can_be_omitted_or_null_in_all_routes() {
    for suffix in ["", ",\"content\":null", ",\"content\":[]"] {
        let input = format!(r#"{{"model":"m","input":[{{"role":"assistant"{suffix}}}]}}"#);
        let mut outputs = Vec::new();
        for strategy in [Strategy::Auto, Strategy::Direct, Strategy::Ir] {
            outputs.push(
                request(
                    input.as_bytes(),
                    Protocol::OpenAiResponses.into(),
                    Protocol::OpenAiChat.into(),
                    strategy,
                )
                .unwrap(),
            );
        }
        assert_eq!(outputs[0], outputs[1]);
        assert_eq!(outputs[1], outputs[2]);
        assert!(outputs[0].contains(r#""content":[]"#), "{}", outputs[0]);
        let encoded = request(
            input.as_bytes(),
            Protocol::OpenAiResponses.into(),
            Protocol::OpenAiResponses.into(),
            Strategy::Ir,
        )
        .unwrap();
        assert!(encoded.contains(r#""content":[]"#));
    }
    let missing_user = br#"{"model":"m","input":[{"role":"user"}]}"#;
    assert!(matches!(
        request(
            missing_user,
            Protocol::OpenAiResponses.into(),
            Protocol::OpenAiChat.into(),
            Strategy::Auto
        ),
        Err(Error::InvalidInput {
            field: "content",
            ..
        })
    ));
}

#[test]
fn empty_annotations_accept_missing_null_and_empty_array() {
    for suffix in ["", ",\"annotations\":null", ",\"annotations\":[]"] {
        let input = format!(
            r#"{{"model":"m","input":[{{"role":"assistant","content":[{{"type":"output_text","text":"ok"{suffix}}}]}}]}}"#
        );
        let mut outputs = Vec::new();
        for strategy in [Strategy::Auto, Strategy::Direct, Strategy::Ir] {
            outputs.push(
                request(
                    input.as_bytes(),
                    Protocol::OpenAiResponses.into(),
                    Protocol::OpenAiChat.into(),
                    strategy,
                )
                .unwrap(),
            );
        }
        assert_eq!(outputs[0], outputs[1]);
        assert_eq!(outputs[1], outputs[2]);
    }
    let invalid = br#"{"model":"m","input":[{"role":"assistant","content":[{"type":"output_text","text":"ok","annotations":7}]}]}"#;
    assert!(matches!(
        request(
            invalid,
            Protocol::OpenAiResponses.into(),
            Protocol::OpenAiResponses.into(),
            Strategy::Auto
        ),
        Err(Error::InvalidInput {
            field: "annotations",
            ..
        })
    ));
}

#[test]
fn responses_reasoning_missing_and_null_summaries_normalize_to_empty() {
    for summary in ["", ",\"summary\":null", ",\"summary\":[]"] {
        let input = format!(r#"{{"model":"m","input":[{{"type":"reasoning"{summary}}}]}}"#);
        let output = request(
            input.as_bytes(),
            Protocol::OpenAiResponses.into(),
            Protocol::OpenAiResponses.into(),
            Strategy::Ir,
        )
        .unwrap();
        assert!(output.contains(r#""summary":[]"#), "{output}");
        request(
            input.as_bytes(),
            Protocol::OpenAiResponses.into(),
            Protocol::OpenAiResponses.into(),
            Strategy::Auto,
        )
        .unwrap();
    }
}

#[test]
fn native_reasoning_content_stays_separate_from_summary() {
    let input = br#"{"model":"m","input":[{"type":"reasoning","id":"ri","summary":[{"type":"summary_text","text":"summary"}],"content":[{"type":"reasoning_text","text":"first"},{"type":"reasoning_text","text":"second"}],"encrypted_content":"sig"}]}"#;
    let output = request(
        input,
        Protocol::OpenAiResponses.into(),
        Protocol::OpenAiResponses.into(),
        Strategy::Ir,
    )
    .unwrap();
    let mut tokens = [Token::EMPTY; 512];
    let doc = json::parse(output.as_bytes(), &mut tokens, Default::default()).unwrap();
    let item = doc.root().get("input").unwrap().array().next().unwrap();
    assert_eq!(
        item.get("id").unwrap().as_string().unwrap().as_str(),
        Some("ri")
    );
    assert_eq!(item.get("summary").unwrap().array().count(), 1);
    assert_eq!(item.get("content").unwrap().array().count(), 2);
    assert!(output.contains(r#""encrypted_content":"sig"#));
}

#[test]
fn chat_reasoning_alias_arrays_keep_order_and_map_to_native_thoughts() {
    let input = br#"{"model":"m","messages":[{"role":"assistant","reasoning":["first ","second"],"content":"answer"}]}"#;
    let source = compatible(Protocol::OpenAiChat);
    let canonical = request(input, source, source, Strategy::Ir).unwrap();
    assert!(
        canonical.contains(r#""reasoning_content":"first second"#),
        "{canonical}"
    );
    let native = request(
        input,
        source,
        Protocol::OpenAiResponses.into(),
        Strategy::Auto,
    )
    .unwrap();
    assert!(
        native.contains(r#""type":"reasoning_text","text":"first "#),
        "{native}"
    );
    assert!(!native.contains(r#""type":"summary_text"#));
    let replay = request(
        native.as_bytes(),
        Protocol::OpenAiResponses.into(),
        source,
        Strategy::Auto,
    )
    .unwrap();
    assert!(replay.find("first ").unwrap() < replay.find("second").unwrap());
    assert!(replay.contains("answer"));
    let anthropic = request(
        input,
        source,
        Protocol::AnthropicMessages.into(),
        Strategy::Ir,
    )
    .unwrap();
    assert!(anthropic.find("first ").unwrap() < anthropic.find("second").unwrap());
}

#[test]
fn reasoning_alias_conflicts_and_wrong_shapes_fail() {
    let source = compatible(Protocol::OpenAiChat);
    for input in [
        br#"{"model":"m","messages":[{"role":"assistant","reasoning":"a","reasoning_content":"b"}]}"#.as_slice(),
        br#"{"model":"m","messages":[{"role":"assistant","reasoning":["a",3]}]}"#.as_slice(),
        br#"{"model":"m","messages":[{"role":"user","reasoning":"a"}]}"#.as_slice(),
    ] {
        assert!(matches!(request(input, source, source, Strategy::Auto), Err(Error::InvalidInput { .. })));
    }
}

#[test]
fn summaries_and_provider_signatures_are_not_relabelled_as_raw_thoughts() {
    let summary = br#"{"model":"m","input":[{"type":"reasoning","summary":[{"type":"summary_text","text":"brief"}]}]}"#;
    assert!(matches!(
        request(
            summary,
            Protocol::OpenAiResponses.into(),
            compatible(Protocol::OpenAiChat),
            Strategy::Ir
        ),
        Err(Error::Unsupported {
            feature: "reasoning summary and provider thought differ"
        })
    ));
    let signature = br#"{"model":"m","input":[{"type":"reasoning","summary":[],"content":[{"type":"reasoning_text","text":"thought"}],"encrypted_content":"private-replay"}]}"#;
    assert!(matches!(
        request(
            signature,
            Protocol::OpenAiResponses.into(),
            compatible(Protocol::OpenAiChat),
            Strategy::Ir
        ),
        Err(Error::PolicyViolation { .. })
    ));
    let unsigned = br#"{"model":"m","input":[{"type":"reasoning","summary":null,"content":[{"type":"reasoning_text","text":"thought"}],"encrypted_content":null}]}"#;
    let output = request(
        unsigned,
        Protocol::OpenAiResponses.into(),
        compatible(Protocol::OpenAiChat),
        Strategy::Ir,
    )
    .unwrap();
    assert!(output.contains(r#""reasoning_content":"thought"#));
    for target in [
        Protocol::GeminiGenerateContent,
        Protocol::GeminiInteractions,
    ] {
        assert!(matches!(
            request(
                unsigned,
                Protocol::OpenAiResponses.into(),
                target.into(),
                Strategy::Ir
            ),
            Err(Error::Unsupported {
                feature: "reasoning summary and provider thought differ"
            })
        ));
    }
}

#[test]
fn object_function_arguments_have_direct_ir_parity() {
    for input in [
        br#"{"model":"m","messages":[{"role":"assistant","tool_calls":[{"id":"c","type":"function","function":{"name":"f","arguments":{"x":1,"escaped":"a\"b"}}}]}]}"#.as_slice(),
        br#"{"model":"m","messages":[{"role":"assistant","function_call":{"name":"f","arguments":{"x":1}}}]}"#.as_slice(),
    ] {
        let mut outputs = Vec::new();
        for strategy in [Strategy::Auto, Strategy::Direct, Strategy::Ir] {
            outputs.push(request(input, Protocol::OpenAiChat.into(), Protocol::OpenAiResponses.into(), strategy).unwrap());
        }
        assert_eq!(outputs[0], outputs[1]);
        assert_eq!(outputs[1], outputs[2]);
        let mut tokens = [Token::EMPTY; 512];
        let doc = json::parse(outputs[0].as_bytes(), &mut tokens, Default::default()).unwrap();
        let call = doc.root().get("input").unwrap().array().next().unwrap();
        assert!(call.get("arguments").unwrap().as_string().is_some());
    }
}

#[test]
fn text_verbosity_preserves_independent_format_constraints() {
    for config in [
        r#"{"verbosity":"low"}"#,
        r#"{"verbosity":"high","format":{"type":"json_schema","name":"r","strict":true,"schema":{"type":"object"}}}"#,
    ] {
        let input = format!(r#"{{"model":"m","input":"hello","text":{config}}}"#);
        let same = request(
            input.as_bytes(),
            Protocol::OpenAiResponses.into(),
            Protocol::OpenAiResponses.into(),
            Strategy::Ir,
        )
        .unwrap();
        assert!(same.contains("verbosity"));
        let chat = request(
            input.as_bytes(),
            Protocol::OpenAiResponses.into(),
            Protocol::OpenAiChat.into(),
            Strategy::Auto,
        )
        .unwrap();
        assert!(chat.contains("verbosity"));
        let back = request(
            chat.as_bytes(),
            Protocol::OpenAiChat.into(),
            Protocol::OpenAiResponses.into(),
            Strategy::Auto,
        )
        .unwrap();
        assert!(back.contains("verbosity"));
        if config.contains("json_schema") {
            assert!(
                same.contains("json_schema")
                    && chat.contains("json_schema")
                    && back.contains("json_schema")
            );
            assert!(back.contains(r#""strict":true"#));
        }
    }
}

#[test]
fn responses_image_detail_defaults_to_auto() {
    for detail in ["", ",\"detail\":null", ",\"detail\":\"auto\""] {
        let input = format!(
            r#"{{"model":"m","input":[{{"role":"user","content":[{{"type":"input_image","image_url":"data:image/png;base64,AQID"{detail}}}]}}]}}"#
        );
        let chat = request(
            input.as_bytes(),
            Protocol::OpenAiResponses.into(),
            Protocol::OpenAiChat.into(),
            Strategy::Auto,
        )
        .unwrap();
        assert!(chat.contains(r#""detail":"auto"#), "{chat}");
        let native = request(
            input.as_bytes(),
            Protocol::OpenAiResponses.into(),
            Protocol::AnthropicMessages.into(),
            Strategy::Ir,
        )
        .unwrap();
        assert!(native.contains(r#""media_type":"image/png"#));
    }
}

#[test]
fn video_and_audio_urls_are_typed_and_convert_without_fetching() {
    let input = br#"{"model":"m","messages":[{"role":"user","content":[{"type":"video_url","video_url":{"url":"data:video/mp4;base64,AQID"}},{"type":"audio_url","audio_url":"data:audio/wav;base64,BAUG"}]}]}"#;
    let native = request(
        input,
        Protocol::OpenAiChat.into(),
        Protocol::GeminiGenerateContent.into(),
        Strategy::Auto,
    )
    .unwrap();
    assert!(native.contains(r#""mimeType":"video/mp4"#), "{native}");
    assert!(native.contains(r#""mimeType":"audio/wav"#));
    let back = request(
        native.as_bytes(),
        Protocol::GeminiGenerateContent.into(),
        Protocol::OpenAiChat.into(),
        Strategy::Ir,
    )
    .unwrap();
    assert!(back.contains("video_url"));
    assert!(back.contains("input_audio"));
    assert!(back.find("AQID").unwrap() < back.find("BAUG").unwrap());
}

#[test]
fn tool_result_text_and_images_cross_chat_responses_and_anthropic() {
    let input = br#"{"model":"m","max_tokens":32,"messages":[{"role":"user","content":[{"type":"tool_result","tool_use_id":"c","is_error":false,"content":[{"type":"text","text":"before"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"AQID"}},{"type":"text","text":"after"}]}]}]}"#;
    for target in [Protocol::OpenAiChat, Protocol::OpenAiResponses] {
        let encoded = request(
            input,
            Protocol::AnthropicMessages.into(),
            target.into(),
            Strategy::Auto,
        )
        .unwrap();
        assert!(encoded.find("before").unwrap() < encoded.find("AQID").unwrap());
        assert!(encoded.find("AQID").unwrap() < encoded.find("after").unwrap());
        assert_eq!(
            encoded.matches("AQID").count(),
            1,
            "nested blocks must not be emitted twice: {encoded}"
        );
        let back = request(
            encoded.as_bytes(),
            target.into(),
            Protocol::AnthropicMessages.into(),
            Strategy::Auto,
        )
        .unwrap();
        assert!(back.contains(r#""type":"tool_result"#), "{back}");
        assert!(back.contains(r#""media_type":"image/png"#));
        assert_eq!(back.matches("AQID").count(), 1);
    }
}

#[test]
fn multiple_multimodal_tool_results_become_separate_chat_messages() {
    let input = br#"{"model":"m","max_tokens":32,"messages":[{"role":"user","content":[{"type":"tool_result","tool_use_id":"a","content":[{"type":"text","text":"one"}]},{"type":"tool_result","tool_use_id":"b","content":[{"type":"text","text":"two"}]}]}]}"#;
    let output = request(
        input,
        Protocol::AnthropicMessages.into(),
        Protocol::OpenAiChat.into(),
        Strategy::Ir,
    )
    .unwrap();
    assert_eq!(output.matches(r#""role":"tool"#).count(), 2, "{output}");
    assert_eq!(output.matches("one").count(), 1);
    assert_eq!(output.matches("two").count(), 1);
}

#[test]
fn gemini_native_tool_parts_round_trip_through_responses() {
    let input = br#"{"contents":[{"role":"user","parts":[{"functionResponse":{"id":"c","name":"f","response":{"ok":true},"parts":[{"inlineData":{"mimeType":"image/png","data":"AQID"}}]}}]}]}"#;
    let encoded = request(
        input,
        Protocol::GeminiGenerateContent.into(),
        Protocol::OpenAiResponses.into(),
        Strategy::Ir,
    )
    .unwrap();
    assert_eq!(encoded.matches("AQID").count(), 1, "{encoded}");
    let back = request(
        encoded.as_bytes(),
        Protocol::OpenAiResponses.into(),
        Protocol::GeminiGenerateContent.into(),
        Strategy::Ir,
    )
    .unwrap();
    assert!(
        back.contains(r#""response":{"ok":true},"parts":[{"inlineData"#),
        "{back}"
    );
    assert_eq!(back.matches("AQID").count(), 1);
}

#[test]
fn edited_tool_content_cannot_reference_itself_or_a_sibling() {
    let input = br#"{"model":"m","messages":[{"role":"tool","tool_call_id":"c","content":[{"type":"text","text":"ok"}]}]}"#;
    let mut tokens = [Token::EMPTY; 128];
    let mut records = [Record::EMPTY; 32];
    let doc = json::parse(input, &mut tokens, Default::default()).unwrap();
    let request =
        zconv::protocol::decode_request(Protocol::OpenAiChat.into(), doc.root(), &mut records)
            .unwrap();
    for record in request.records.iter_mut() {
        if let Record::Block(block) = record
            && let zconv::ir::Content::ToolResult { result, .. } = &mut block.content
        {
            *result = Data::Content(Span { start: 0, end: 3 });
        }
    }
    let mut output = [0; 512];
    let mut scratch = [0; 256];
    assert!(matches!(
        zconv::protocol::encode_request(
            &request,
            Protocol::OpenAiResponses.into(),
            &Context::default(),
            Policy::default(),
            &mut output,
            &mut scratch,
            &mut tokens
        ),
        Err(Error::InvalidInput {
            field: "IR tool result content",
            ..
        })
    ));
}

#[test]
fn nested_tool_operations_in_result_content_are_rejected() {
    let input = br#"{"model":"m","messages":[{"role":"tool","tool_call_id":"c","content":[{"type":"tool_use","id":"nested","name":"f","input":{}}]}]}"#;
    assert!(matches!(
        request(
            input,
            Protocol::OpenAiChat.into(),
            Protocol::OpenAiChat.into(),
            Strategy::Auto
        ),
        Err(Error::InvalidInput {
            field: "tool result content",
            ..
        })
    ));
}

#[test]
fn nonstream_response_reasoning_alias_keeps_visible_text_and_usage() {
    let input = br#"{"id":"r","model":"m","created":1,"choices":[{"index":0,"message":{"role":"assistant","reasoning":["a","b"],"content":"answer"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}}"#;
    let (output, _) = convert(
        input,
        compatible(Protocol::OpenAiChat),
        compatible(Protocol::OpenAiChat),
        Operation::Response,
        Strategy::Ir,
    )
    .unwrap();
    assert!(output.contains(r#""reasoning_content":"ab"#));
    assert!(output.contains(r#""content":"answer"#));
    assert!(output.contains(r#""total_tokens":5"#));
}

#[test]
fn stream_options_are_typed_preserved_and_returned_for_transport() {
    use zconv::ir::Presence;
    for (source, target, strategy) in [
        (Protocol::OpenAiChat, Protocol::OpenAiChat, Strategy::Auto),
        (Protocol::OpenAiChat, Protocol::OpenAiChat, Strategy::Ir),
        (
            Protocol::OpenAiChat,
            Protocol::OpenAiResponses,
            Strategy::Auto,
        ),
        (
            Protocol::OpenAiChat,
            Protocol::OpenAiCompletions,
            Strategy::Ir,
        ),
    ] {
        let input = br#"{"model":"m","messages":[{"role":"user","content":"x"}],"stream":false,"stream_options":{"include_usage":null,"continuous_usage_stats":true}}"#;
        let mut tokens = [Token::EMPTY; 256];
        let mut records = [Record::EMPTY; 64];
        let mut nested = [Token::EMPTY; 64];
        let mut scratch = [0; 1024];
        let mut output = [0; 2048];
        let result = zconv::convert(
            source.into(),
            target.into(),
            Operation::Request,
            input,
            &mut output,
            Options {
                strategy,
                ..Default::default()
            },
            Workspace {
                tokens: &mut tokens,
                records: &mut records,
                scratch: &mut scratch,
                nested_tokens: &mut nested,
            },
        )
        .unwrap();
        let Presence::Value(options) = result.report.transport_stream_options else {
            panic!("missing usage preferences")
        };
        assert!(options.include_usage.is_null());
        assert!(matches!(
            options.continuous_usage_stats,
            Presence::Value(true)
        ));
        let output = std::str::from_utf8(&output[..result.written()]).unwrap();
        assert_eq!(
            output.contains("stream_options"),
            target != Protocol::OpenAiResponses,
            "{output}"
        );
        assert!(output.contains(r#""stream":false"#));
    }
    let completion = br#"{"model":"m","prompt":"x","stream_options":{"include_usage":true,"continuous_usage_stats":false}}"#;
    let chat = request(
        completion,
        Protocol::OpenAiCompletions.into(),
        Protocol::OpenAiChat.into(),
        Strategy::Ir,
    )
    .unwrap();
    assert!(chat.contains(r#""include_usage":true,"continuous_usage_stats":false"#));
}

#[test]
fn stream_option_booleans_and_presence_are_validated_without_cross_field_policy() {
    for options in [
        "null",
        "{}",
        r#"{"include_usage":null}"#,
        r#"{"include_usage":true,"continuous_usage_stats":null}"#,
    ] {
        let input = format!(r#"{{"model":"m","messages":[],"stream_options":{options}}}"#);
        let output = request(
            input.as_bytes(),
            Protocol::OpenAiChat.into(),
            Protocol::OpenAiChat.into(),
            Strategy::Ir,
        )
        .unwrap();
        assert!(output.contains("stream_options"));
    }
    let invalid = br#"{"model":"m","messages":[],"stream_options":{"continuous_usage_stats":1}}"#;
    for strategy in [Strategy::Auto, Strategy::Ir] {
        assert!(matches!(
            request(
                invalid,
                Protocol::OpenAiChat.into(),
                Protocol::OpenAiChat.into(),
                strategy
            ),
            Err(Error::InvalidInput {
                field: "continuous_usage_stats",
                ..
            })
        ));
    }
}
