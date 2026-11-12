use zconv::{
    Error, Format, Protocol,
    ir::*,
    json::{self, Token},
    protocol,
};

fn convert(input: &[u8], source: Protocol, target: Format) -> Result<Vec<u8>, Error> {
    let mut tokens = [Token::EMPTY; 2048];
    let mut records = [Record::EMPTY; 512];
    let doc = json::parse(input, &mut tokens, json::Limits::default())?;
    let request = protocol::decode_request(source.into(), doc.root(), &mut records)?;
    encode(&request, target, &mut tokens)
}
fn encode(
    request: &Request<'_, '_>,
    target: Format,
    tokens: &mut [Token],
) -> Result<Vec<u8>, Error> {
    let mut output = vec![0; 16384];
    let mut scratch = [0; 8192];
    let report = protocol::encode_request(
        request,
        target,
        &Context {
            model: Some("m"),
            max_tokens: Some(32),
            ..Default::default()
        },
        Policy::default(),
        &mut output,
        &mut scratch,
        tokens,
    )?;
    output.truncate(report.written);
    Ok(output)
}
fn has(output: &[u8], fragment: &str) {
    assert!(
        std::str::from_utf8(output).unwrap().contains(fragment),
        "{} missing {fragment}",
        String::from_utf8_lossy(output)
    );
}

#[test]
fn adjacent_roles_and_ordered_instructions_are_merged_without_losing_blocks() {
    let input = br#"{"model":"m","messages":[{"role":"system","content":"first"},{"role":"developer","content":"second"},{"role":"assistant","content":null},{"role":"assistant","content":[{"type":"text","text":"answer"}]},{"role":"assistant","content":[]},{"role":"user","content":"next"}]}"#;
    let out = convert(
        input,
        Protocol::OpenAiChat,
        Protocol::AnthropicMessages.into(),
    )
    .unwrap();
    has(
        &out,
        r#""system":[{"type":"text","text":"first"},{"type":"text","text":"second"}]"#,
    );
    has(
        &out,
        r#""messages":[{"role":"assistant","content":[{"type":"text","text":"answer"}]},{"role":"user","content":[{"type":"text","text":"next"}]}]"#,
    );
}

#[test]
fn typed_provider_signatures_override_envelope_origin_only_for_their_block() {
    let input = br#"{"model":"m","messages":[{"role":"assistant","content":"answer"}]}"#;
    let mut tokens = [Token::EMPTY; 512];
    let mut records = [Record::EMPTY; 32];
    let doc = json::parse(input, &mut tokens, json::Limits::default()).unwrap();
    let request =
        protocol::decode_request(Protocol::OpenAiChat.into(), doc.root(), &mut records).unwrap();
    for record in request.records.iter_mut() {
        if let Record::Block(block) = record {
            block.content = Content::Reasoning {
                kind: ReasoningKind::Thought,
                text: Presence::Value(Text::Utf8("thought")),
                signature: Presence::Value(Text::Utf8("sig")),
            };
            block.signature = Presence::Value(ProviderSignature {
                source: Protocol::AnthropicMessages.into(),
                value: Text::Utf8("sig"),
            });
        }
    }
    let out = encode(&request, Protocol::AnthropicMessages.into(), &mut tokens).unwrap();
    has(&out, r#""thinking":"thought","signature":"sig""#);
    assert!(matches!(
        encode(
            &request,
            Protocol::GeminiGenerateContent.into(),
            &mut tokens
        ),
        Err(Error::PolicyViolation {
            feature: "signature belongs to provider"
        })
    ));
}

#[test]
fn gemini_tool_text_and_media_signatures_survive_and_cannot_cross_providers() {
    let input=br#"{"contents":[{"role":"model","parts":[{"functionCall":{"id":"c","name":"f","args":{"x":1}},"thoughtSignature":"call-sig"},{"text":"answer","thoughtSignature":"text-sig"},{"inlineData":{"mimeType":"image/png","data":"AQID"},"thoughtSignature":"media-sig"}]}]}"#;
    let out = convert(
        input,
        Protocol::GeminiGenerateContent,
        Protocol::GeminiGenerateContent.into(),
    )
    .unwrap();
    for signature in ["call-sig", "text-sig", "media-sig"] {
        has(&out, signature);
    }
    assert!(matches!(
        convert(
            input,
            Protocol::GeminiGenerateContent,
            Protocol::OpenAiChat.into()
        ),
        Err(Error::PolicyViolation { .. })
    ));
}

#[test]
fn media_extensions_are_retained_in_the_outer_block() {
    let input=br#"{"model":"m","max_tokens":32,"messages":[{"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"AQID"},"cache_control":{"type":"ephemeral"}}]}]}"#;
    let out = convert(
        input,
        Protocol::AnthropicMessages,
        Protocol::AnthropicMessages.into(),
    )
    .unwrap();
    has(
        &out,
        r#""data":"AQID"},"cache_control":{"type":"ephemeral"}"#,
    );
    assert!(
        convert(
            input,
            Protocol::AnthropicMessages,
            Protocol::OpenAiChat.into()
        )
        .is_err()
    );
}

#[test]
fn json_schema_dialect_and_original_key_order_survive_gemini_roundtrip() {
    let input=br#"{"contents":[],"tools":[{"functionDeclarations":[{"name":"f","parametersJsonSchema":{"type":"object","properties":{"z":{"const":1},"a":{"type":["string","null"]}},"additionalProperties":false,"default":{"type":"UPPER_IS_BUSINESS_DATA"}}}]}]}"#;
    let out = convert(
        input,
        Protocol::GeminiGenerateContent,
        Protocol::GeminiGenerateContent.into(),
    )
    .unwrap();
    has(
        &out,
        r#""parametersJsonSchema":{"type":"object","properties":{"z":{"const":1},"a":{"type":["string","null"]}},"additionalProperties":false,"default":{"type":"UPPER_IS_BUSINESS_DATA"}}"#,
    );
    let chat = convert(
        input,
        Protocol::GeminiGenerateContent,
        Protocol::OpenAiChat.into(),
    )
    .unwrap();
    has(&chat, "UPPER_IS_BUSINESS_DATA");
    let mut v1 = Format::new(Protocol::GeminiGenerateContent);
    v1.version = "v1";
    assert!(matches!(
        convert(input, Protocol::GeminiGenerateContent, v1),
        Err(Error::Unsupported { .. })
    ));
}

#[test]
fn typed_schema_normalization_visits_only_schema_positions() {
    let input=br#"{"contents":[],"tools":[{"functionDeclarations":[{"name":"f","parameters":{"type":"OBJECT","properties":{"type":{"type":"STRING","nullable":true,"default":{"type":"OBJECT"}},"x":{"type":"INTEGER"}},"required":["x"]}}]}]}"#;
    let out = convert(
        input,
        Protocol::GeminiGenerateContent,
        Protocol::OpenAiChat.into(),
    )
    .unwrap();
    has(
        &out,
        r#""parameters":{"type":"object","properties":{"type":{"type":["string","null"],"default":{"type":"OBJECT"}},"x":{"type":"integer"}},"required":["x"]}"#,
    );
}

#[test]
fn concurrency_and_reasoning_effort_have_explicit_target_representations() {
    let input=br#"{"model":"m","messages":[],"tools":[{"type":"function","function":{"name":"f","parameters":{"type":"object"}}}],"tool_choice":"auto","parallel_tool_calls":false}"#;
    let out = convert(
        input,
        Protocol::OpenAiChat,
        Protocol::AnthropicMessages.into(),
    )
    .unwrap();
    has(
        &out,
        r#""tool_choice":{"type":"auto","disable_parallel_tool_use":true}"#,
    );
    let back = convert(
        &out,
        Protocol::AnthropicMessages,
        Protocol::OpenAiChat.into(),
    )
    .unwrap();
    has(&back, r#""parallel_tool_calls":false"#);
    assert!(
        convert(
            input,
            Protocol::OpenAiChat,
            Protocol::GeminiGenerateContent.into()
        )
        .is_err()
    );
    let input = br#"{"model":"m","messages":[],"reasoning_effort":"xhigh"}"#;
    let out = convert(
        input,
        Protocol::OpenAiChat,
        Protocol::OpenAiResponses.into(),
    )
    .unwrap();
    has(&out, r#""reasoning":{"effort":"xhigh"}"#);
    assert!(
        convert(
            input,
            Protocol::OpenAiChat,
            Protocol::AnthropicMessages.into()
        )
        .is_err()
    );
}

#[test]
fn native_response_schema_preserves_its_original_dialect_and_order() {
    for field in ["responseSchema", "responseJsonSchema"] {
        let schema =
            r#"{"type":"OBJECT","properties":{"z":{"type":"STRING"},"a":{"type":"INTEGER"}}}"#;
        let input = format!(
            r#"{{"contents":[],"generationConfig":{{"responseMimeType":"application/json","{field}":{schema}}}}}"#
        );
        let out = convert(
            input.as_bytes(),
            Protocol::GeminiGenerateContent,
            Protocol::GeminiGenerateContent.into(),
        )
        .unwrap();
        has(&out, &format!(r#""{field}":{schema}"#));
    }
    let conflict =
        br#"{"contents":[],"generationConfig":{"responseSchema":{},"responseJsonSchema":{}}}"#;
    assert!(matches!(
        convert(
            conflict,
            Protocol::GeminiGenerateContent,
            Protocol::GeminiGenerateContent.into()
        ),
        Err(Error::InvalidInput { .. })
    ));
}

#[test]
fn explicit_text_object_policy_preserves_objects_and_wraps_other_results() {
    for (result, expected) in [
        ("plain text", r#"{"output":"plain text"}"#),
        ("{\"temp\":20}", r#"{"temp":20}"#),
        ("[1,2]", r#"{"output":"[1,2]"}"#),
    ] {
        let input=br#"{"model":"m","messages":[{"role":"assistant","content":null,"tool_calls":[{"id":"c","type":"function","function":{"name":"f","arguments":"{}"}}]},{"role":"tool","tool_call_id":"c","content":"placeholder"}]}"#;
        let mut tokens = [Token::EMPTY; 512];
        let mut records = [Record::EMPTY; 64];
        let doc = json::parse(input, &mut tokens, json::Limits::default()).unwrap();
        let request =
            protocol::decode_request(Protocol::OpenAiChat.into(), doc.root(), &mut records)
                .unwrap();
        for record in request.records.iter_mut() {
            if let Record::Block(Block {
                content: Content::ToolResult { result: data, .. },
                ..
            }) = record
            {
                *data = Data::ObjectOrText {
                    text: Text::Utf8(result),
                    key: Text::Utf8("output"),
                };
            }
        }
        let out = encode(
            &request,
            Protocol::GeminiGenerateContent.into(),
            &mut tokens,
        )
        .unwrap();
        has(
            &out,
            &format!(r#""functionResponse":{{"id":"c","name":"f","response":{expected}}}"#),
        );
    }
}

#[test]
fn native_media_allows_auto_detail_but_rejects_unrepresentable_detail() {
    for detail in ["auto", "high"] {
        let input = format!(
            r#"{{"model":"m","messages":[{{"role":"user","content":[{{"type":"image_url","image_url":{{"url":"data:image/png;base64,AQID","detail":"{detail}"}}}}]}}]}}"#
        );
        for target in [Protocol::AnthropicMessages, Protocol::GeminiGenerateContent] {
            let result = convert(input.as_bytes(), Protocol::OpenAiChat, target.into());
            assert_eq!(
                result.is_ok(),
                detail == "auto",
                "{target:?} {detail}: {result:?}"
            );
        }
    }
}

#[test]
fn long_tool_history_resolves_names_beyond_the_first_256_records() {
    let mut input = String::from(r#"{"model":"m","messages":["#);
    for _ in 0..130 {
        input.push_str(r#"{"role":"user","content":"history"},"#);
    }
    input.push_str(r#"{"role":"assistant","content":null,"tool_calls":[{"id":"late","type":"function","function":{"name":"f","arguments":"{}"}}]},{"role":"tool","tool_call_id":"late","content":"{}"}]}"#);
    let out = convert(
        input.as_bytes(),
        Protocol::OpenAiChat,
        Protocol::GeminiGenerateContent.into(),
    )
    .unwrap();
    has(
        &out,
        r#""functionResponse":{"id":"late","name":"f","response":{}}"#,
    );
}

#[test]
fn signed_reasoning_can_be_dropped_only_by_the_explicit_reasoning_policy() {
    let input=br#"{"contents":[{"role":"model","parts":[{"text":"thought","thought":true,"thoughtSignature":"sig"},{"text":"answer"}]}]}"#;
    let mut tokens = [Token::EMPTY; 512];
    let mut records = [Record::EMPTY; 64];
    let doc = json::parse(input, &mut tokens, json::Limits::default()).unwrap();
    let request = protocol::decode_request(
        Protocol::GeminiGenerateContent.into(),
        doc.root(),
        &mut records,
    )
    .unwrap();
    let mut out = [0; 4096];
    let mut scratch = [0; 4096];
    let report = protocol::encode_request(
        &request,
        Protocol::OpenAiChat.into(),
        &Context {
            model: Some("m"),
            ..Default::default()
        },
        Policy {
            drop_reasoning: true,
            ..Default::default()
        },
        &mut out,
        &mut scratch,
        &mut tokens,
    )
    .unwrap();
    assert_eq!(report.dropped_reasoning, 1);
    has(&out[..report.written], "answer");
    assert!(
        !std::str::from_utf8(&out[..report.written])
            .unwrap()
            .contains("sig")
    );
}

#[test]
fn duplicate_call_ids_cannot_select_an_arbitrary_result_function_name() {
    let input=br#"{"model":"m","messages":[{"role":"assistant","content":null,"tool_calls":[{"id":"duplicate","type":"function","function":{"name":"first","arguments":"{}"}},{"id":"duplicate","type":"function","function":{"name":"second","arguments":"{}"}}]},{"role":"tool","tool_call_id":"duplicate","content":"{}"}]}"#;
    assert!(matches!(
        convert(
            input,
            Protocol::OpenAiChat,
            Protocol::GeminiGenerateContent.into()
        ),
        Err(Error::ContextRequired {
            field: "unambiguous tool function name"
        })
    ));
}
