use zconv::ir::{Content, Presence, Record, Text};
use zconv::ir::{Context, Policy};
use zconv::{direct, json, Error, Format, Operation, Protocol};

fn run(
    source: Protocol,
    target: Protocol,
    operation: Operation,
    input: &[u8],
    context: Context<'_>,
    policy: Policy,
) -> Result<(Vec<u8>, usize), Error> {
    let mut tokens = vec![json::Token::EMPTY; 512];
    let document = json::parse(input, &mut tokens, json::Limits::default())?;
    let mut output = vec![0; 8192];
    let mut scratch = [0; 4096];
    let mut nested = [json::Token::EMPTY; 128];
    let report = direct::convert(
        Format::new(source),
        Format::new(target),
        operation,
        document.root(),
        &context,
        policy,
        &mut output,
        &mut scratch,
        &mut nested,
    )?;
    let written = report.written;
    let dropped = report.dropped_metadata;
    output.truncate(written);
    let mut output_tokens = vec![json::Token::EMPTY; 512];
    json::parse(&output, &mut output_tokens, json::Limits::default())?;
    Ok((output, dropped))
}

fn context() -> Context<'static> {
    Context {
        model: Some("test-model"),
        created: Some(123),
        max_tokens: Some(100),
        response_id: Some("response_1"),
        item_ids: &["msg_0", "msg_1"],
        tool_calls: &[zconv::ir::ToolBinding {
            id: "legacy_call",
            name: "legacy",
        }],
        ..Context::default()
    }
}

#[test]
fn six_request_directions_have_real_direct_implementations() {
    use Protocol::*;
    let chat = br#"{"model":"test-model","messages":[{"role":"system","content":"be brief"},{"role":"user","content":"hello \u4e2d\ud83d\ude00"}],"max_completion_tokens":17,"temperature":0,"stream":false}"#;
    for target in [OpenAiResponses, AnthropicMessages, GeminiGenerateContent] {
        let (out, _) = run(
            OpenAiChat,
            target,
            Operation::Request,
            chat,
            context(),
            Policy::default(),
        )
        .unwrap();
        let mut tokens = [json::Token::EMPTY; 128];
        let document = json::parse(&out, &mut tokens, json::Limits::default()).unwrap();
        let root = document.root();
        let list = root
            .get(if target == OpenAiResponses {
                "input"
            } else if target == GeminiGenerateContent {
                "contents"
            } else {
                "messages"
            })
            .unwrap();
        assert_eq!(
            list.array().count(),
            if target == OpenAiResponses { 2 } else { 1 }
        );
        if target == GeminiGenerateContent {
            assert!(root.get("model").is_none());
            assert!(root.get("stream").is_none());
            assert_eq!(
                root.get("generationConfig")
                    .unwrap()
                    .get("maxOutputTokens")
                    .unwrap()
                    .as_u64(),
                Some(17)
            );
        }
    }
    for (source, input) in [
        (OpenAiResponses, br#"{"model":"test-model","instructions":"be brief","input":[{"role":"user","content":[{"type":"input_text","text":"hello"}]}],"max_output_tokens":17}"#.as_slice()),
        (AnthropicMessages, br#"{"model":"test-model","system":"be brief","messages":[{"role":"user","content":[{"type":"text","text":"hello"}]}],"max_tokens":17}"#),
        (GeminiGenerateContent, br#"{"systemInstruction":{"parts":[{"text":"be brief"}]},"contents":[{"role":"user","parts":[{"text":"hello"}]}],"generationConfig":{"maxOutputTokens":17}}"#),
    ] {
        let (out, _) = run(source, OpenAiChat, Operation::Request, input, context(), Policy::default()).unwrap();
        let mut tokens = [json::Token::EMPTY; 128];
        let document = json::parse(&out, &mut tokens, json::Limits::default()).unwrap();
        assert_eq!(document.root().get("messages").unwrap().array().count(), 2);
        assert_eq!(document.root().get("max_completion_tokens").unwrap().as_u64(), Some(17));
    }
}

#[test]
fn six_response_directions_preserve_text_finish_and_usage() {
    use Protocol::*;
    let chat = br#"{"id":"r1","object":"chat.completion","created":123,"model":"test-model","choices":[{"index":0,"message":{"role":"assistant","content":"hello"},"finish_reason":"length"}],"usage":{"prompt_tokens":9007199254740993,"completion_tokens":2,"total_tokens":9007199254740995}}"#;
    for target in [OpenAiResponses, AnthropicMessages, GeminiGenerateContent] {
        let (out, dropped) = run(
            OpenAiChat,
            target,
            Operation::Response,
            chat,
            context(),
            Policy {
                drop_metadata: true,
                ..Policy::default()
            },
        )
        .unwrap();
        assert_eq!(dropped, usize::from(target != OpenAiResponses));
        let (returned, _) = run(
            target,
            OpenAiChat,
            Operation::Response,
            &out,
            context(),
            Policy {
                drop_metadata: true,
                ..Policy::default()
            },
        )
        .unwrap();
        let mut tokens = [json::Token::EMPTY; 256];
        let document = json::parse(&returned, &mut tokens, json::Limits::default()).unwrap();
        let root = document.root();
        let choice = root.get("choices").unwrap().array().next().unwrap();
        assert!(choice
            .get("finish_reason")
            .unwrap()
            .as_string()
            .unwrap()
            .eq_str("length"));
        assert!(choice
            .get("message")
            .unwrap()
            .get("content")
            .unwrap()
            .as_string()
            .unwrap()
            .eq_str("hello"));
        assert_eq!(
            root.get("usage")
                .unwrap()
                .get("prompt_tokens")
                .unwrap()
                .raw(),
            b"9007199254740993"
        );
    }
}

#[test]
fn applicability_checks_precede_output_and_are_narrow() {
    use Protocol::*;
    let input =
        br#"{"model":"m","messages":[{"role":"user","content":"x","provider_extra":false}]}"#;
    assert!(matches!(
        run(
            OpenAiChat,
            OpenAiResponses,
            Operation::Request,
            input,
            context(),
            Policy::default()
        ),
        Err(Error::DirectUnavailable { .. })
    ));
    let input = br#"{"model":"m","messages":[{"role":"user","content":"x"}],"tools":[{"type":"web_search"}]}"#;
    assert!(matches!(
        run(
            OpenAiChat,
            OpenAiResponses,
            Operation::Request,
            input,
            context(),
            Policy::default()
        ),
        Err(Error::DirectUnavailable { .. })
    ));
    let input = br#"{"model":"m","messages":[{"role":"user","content":"x"},{"role":"system","content":"late"}]}"#;
    assert!(matches!(
        run(
            OpenAiChat,
            AnthropicMessages,
            Operation::Request,
            input,
            context(),
            Policy::default()
        ),
        Err(Error::DirectUnavailable { .. })
    ));
    let invalid = br#"{"model":"m","messages":false}"#;
    assert!(matches!(
        run(
            OpenAiChat,
            OpenAiResponses,
            Operation::Request,
            invalid,
            context(),
            Policy::default()
        ),
        Err(Error::InvalidInput { .. })
    ));
}

#[test]
fn supported_wire_validation_rejects_malformed_values_without_ir() {
    for input in [
        br#"{"model":"m","messages":[{"role":"user","content":false}]}"#.as_slice(),
        br#"{"model":"m","messages":[{"role":"alien","content":"x"}]}"#,
        br#"{"model":"m","messages":{},"stream":true}"#,
        br#"{"model":"m","messages":[],"max_completion_tokens":-1}"#,
        br#"{"model":"m","messages":[],"max_completion_tokens":1.5}"#,
        br#"{"model":"m","messages":[],"temperature":"0"}"#,
        br#"{"model":"m","messages":[],"stream":"true"}"#,
    ] {
        assert!(
            matches!(
                run(
                    Protocol::OpenAiChat,
                    Protocol::OpenAiResponses,
                    Operation::Request,
                    input,
                    context(),
                    Policy::default()
                ),
                Err(Error::InvalidInput { .. })
            ),
            "{input:?}"
        );
    }
    let input = br#"{"id":"r","model":"m","created":1,"choices":[{"index":0,"message":{"role":"assistant","content":"x"},"finish_reason":"stop"}],"usage":{"prompt_tokens":-1}}"#;
    assert!(matches!(
        run(
            Protocol::OpenAiChat,
            Protocol::OpenAiResponses,
            Operation::Response,
            input,
            context(),
            Policy::default()
        ),
        Err(Error::InvalidInput { .. })
    ));
}

#[test]
fn unsupported_features_do_not_write_output() {
    let input = br#"{"model":"m","messages":[{"role":"user","content":"x"}],"tools":[{"type":"web_search"}]}"#;
    let mut tokens = [json::Token::EMPTY; 64];
    let document = json::parse(input, &mut tokens, json::Limits::default()).unwrap();
    let mut output = [0xaa; 512];
    let result = direct::convert(
        Protocol::OpenAiChat.into(),
        Protocol::OpenAiResponses.into(),
        Operation::Request,
        document.root(),
        &context(),
        Policy::default(),
        &mut output,
        &mut [],
        &mut [],
    );
    assert!(matches!(result, Err(Error::DirectUnavailable { .. })));
    assert!(output.iter().all(|byte| *byte == 0xaa));
}

#[test]
fn metadata_loss_and_missing_context_are_explicit() {
    use Protocol::*;
    let chat = br#"{"id":"r1","object":"chat.completion","created":123,"model":"m","choices":[{"index":0,"message":{"role":"assistant","content":"x"},"finish_reason":"stop"}]}"#;
    assert!(matches!(
        run(
            OpenAiChat,
            AnthropicMessages,
            Operation::Response,
            chat,
            context(),
            Policy::default()
        ),
        Err(Error::PolicyViolation { .. })
    ));
    let anthropic = br#"{"id":"r1","type":"message","role":"assistant","model":"m","content":[{"type":"text","text":"x"}],"stop_reason":"end_turn","stop_sequence":null}"#;
    assert_eq!(
        run(
            AnthropicMessages,
            OpenAiChat,
            Operation::Response,
            anthropic,
            Context::default(),
            Policy::default()
        )
        .unwrap_err(),
        Error::ContextRequired { field: "created" }
    );
}

#[test]
fn gemini_transport_facts_survive_separately_from_body() {
    let input = br#"{"model":"m","stream":true,"messages":[{"role":"user","content":"x"}]}"#;
    let mut tokens = [json::Token::EMPTY; 64];
    let document = json::parse(input, &mut tokens, json::Limits::default()).unwrap();
    let mut output = [0; 512];
    let report = direct::convert(
        Protocol::OpenAiChat.into(),
        Protocol::GeminiGenerateContent.into(),
        Operation::Request,
        document.root(),
        &Context::default(),
        Policy::default(),
        &mut output,
        &mut [],
        &mut [],
    )
    .unwrap();
    assert!(report.transport_model.value().unwrap().eq_str("m"));
    assert!(report.transport_stream.value().unwrap());
}

#[test]
fn insufficient_output_reports_exact_length_and_retries() {
    let input = br#"{"model":"m","input":"hello"}"#;
    let mut tokens = [json::Token::EMPTY; 64];
    let document = json::parse(input, &mut tokens, json::Limits::default()).unwrap();
    let mut empty = [];
    let error = direct::convert(
        Protocol::OpenAiResponses.into(),
        Protocol::OpenAiChat.into(),
        Operation::Request,
        document.root(),
        &Context::default(),
        Policy::default(),
        &mut empty,
        &mut [],
        &mut [],
    )
    .unwrap_err();
    let Error::OutputTooSmall { needed } = error else {
        panic!("{error:?}");
    };
    let mut output = vec![0; needed];
    let report = direct::convert(
        Protocol::OpenAiResponses.into(),
        Protocol::OpenAiChat.into(),
        Operation::Request,
        document.root(),
        &Context::default(),
        Policy::default(),
        &mut output,
        &mut [],
        &mut [],
    )
    .unwrap();
    assert_eq!(report.written, needed);
}

fn text_signature(text: Text<'_>) -> String {
    match text {
        Text::Utf8(value) => value.to_owned(),
        Text::Json(value) => String::from_utf8(value.bytes().collect()).unwrap(),
    }
}

fn presence_text(value: Presence<Text<'_>>) -> String {
    match value {
        Presence::Missing => "<missing>".into(),
        Presence::Null => "<null>".into(),
        Presence::Value(value) => format!("value:{}", text_signature(value)),
    }
}

fn semantic(protocol: Protocol, operation: Operation, bytes: &[u8]) -> Vec<String> {
    let mut tokens = vec![json::Token::EMPTY; 512];
    let document = json::parse(bytes, &mut tokens, json::Limits::default()).unwrap();
    let mut records = vec![Record::EMPTY; 128];
    let mut result = Vec::new();
    let used = match operation {
        Operation::Request => {
            let request =
                zconv::protocol::decode_request(protocol.into(), document.root(), &mut records)
                    .unwrap();
            result.push(presence_text(request.model));
            result.push(format!("{:?}", request.parameters));
            result.push(format!(
                "tools:{:?}|functions:{:?}|choice:{:?}",
                request.tools, request.functions, request.tool_choice
            ));
            request.records.len()
        }
        Operation::Response => {
            let response =
                zconv::protocol::decode_response(protocol.into(), document.root(), &mut records)
                    .unwrap();
            result.push(presence_text(response.id));
            result.push(presence_text(response.model));
            result.push(format!(
                "{:?}|{:?}|{:?}",
                response.created, response.status, response.usage
            ));
            response.records.len()
        }
    };
    for record in &records[..used] {
        match record {
            Record::Message(message) => result.push(format!(
                "message:{:?}|{}|{}|{}",
                message.role,
                presence_text(message.name),
                presence_text(message.item_id),
                presence_text(message.item_status)
            )),
            Record::Choice(choice) => {
                result.push(format!("choice:{}|{:?}", choice.index, choice.finish))
            }
            Record::Block(block) => match block.content {
                Content::Text(value) => result.push(format!("text:{}", text_signature(value))),
                _ => result.push(format!("block:{:?}", block.content)),
            },
            Record::Extension(extension) => result.push(format!(
                "extension:{}:{:?}",
                text_signature(Text::Json(extension.key)),
                extension.value
            )),
            _ => result.push(format!("{record:?}")),
        }
    }
    result
}

fn routed(
    source: Protocol,
    target: Protocol,
    operation: Operation,
    input: &[u8],
    strategy: zconv::Strategy,
) -> (Vec<u8>, usize, u32) {
    let mut tokens = vec![json::Token::EMPTY; 512];
    let mut records = vec![
        Record::EMPTY;
        if strategy == zconv::Strategy::Direct {
            0
        } else {
            256
        }
    ];
    let mut scratch = [0; 4096];
    let mut nested = [json::Token::EMPTY; 256];
    let mut output = vec![0; 8192];
    let result = zconv::convert(
        source.into(),
        target.into(),
        operation,
        input,
        &mut output,
        zconv::Options {
            strategy,
            context: context(),
            policy: Policy {
                drop_metadata: true,
                ..Policy::default()
            },
            ..zconv::Options::default()
        },
        zconv::Workspace {
            tokens: &mut tokens,
            records: &mut records,
            scratch: &mut scratch,
            nested_tokens: &mut nested,
        },
    )
    .unwrap_or_else(|error| panic!("{strategy:?} {source:?}->{target:?}: {error:?}"));
    if strategy == zconv::Strategy::Direct {
        assert_eq!(result.resources.ir_records_used, 0);
    }
    let used = result.written();
    let dropped = result.report.dropped_metadata;
    let fields = result.report.metadata_loss_fields;
    output.truncate(used);
    (output, dropped, fields)
}

#[test]
fn direct_and_ir_preserve_the_same_request_semantics() {
    use Protocol::*;
    let chat = br#"{"model":"m","messages":[{"role":"user","content":"hello \u4e2d"}],"temperature":0,"max_completion_tokens":17,"stream":false}"#;
    let cases = [
        (OpenAiChat, OpenAiResponses, chat.as_slice()),
        (OpenAiChat, AnthropicMessages, chat.as_slice()),
        (OpenAiChat, GeminiGenerateContent, chat.as_slice()),
        (OpenAiResponses, OpenAiChat, br#"{"model":"m","input":"hello","max_output_tokens":17,"stream":false}"#),
        (AnthropicMessages, OpenAiChat, br#"{"model":"m","messages":[{"role":"user","content":"hello"}],"max_tokens":17,"stream":false}"#),
        (GeminiGenerateContent, OpenAiChat, br#"{"contents":[{"role":"user","parts":[{"text":"hello"}]}],"generationConfig":{"maxOutputTokens":17}}"#),
        (OpenAiChat, OpenAiResponses, br#"{"model":"m","messages":[{"role":"system","content":"first"},{"role":"user","content":"hello"},{"role":"system","content":"later"}]}"#),
        (OpenAiChat, OpenAiResponses, br#"{"model":"m","messages":[{"role":"user","content":[]},{"role":"assistant","content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}]}"#),
    ];
    for (source, target, input) in cases {
        let direct = routed(
            source,
            target,
            Operation::Request,
            input,
            zconv::Strategy::Direct,
        );
        let ir = routed(
            source,
            target,
            Operation::Request,
            input,
            zconv::Strategy::Ir,
        );
        assert_eq!(
            semantic(target, Operation::Request, &direct.0),
            semantic(target, Operation::Request, &ir.0),
            "{source:?}->{target:?}\nDirect={}\nIR={}",
            String::from_utf8_lossy(&direct.0),
            String::from_utf8_lossy(&ir.0)
        );
        assert_eq!((direct.1, direct.2), (ir.1, ir.2));
    }
}

#[test]
fn function_definitions_and_choices_have_direct_ir_parity() {
    use Protocol::*;
    let fixtures = [
        (OpenAiChat, OpenAiResponses, br#"{"model":"m","messages":[{"role":"user","content":"weather"}],"tools":[{"type":"function","function":{"name":"weather","description":"Weather \u4e2d","parameters":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]},"strict":true}}],"tool_choice":{"type":"function","function":{"name":"weather"}},"parallel_tool_calls":false}"#.as_slice()),
        (OpenAiChat, OpenAiResponses, br#"{"model":"m","messages":[],"functions":[{"name":"legacy","description":null}],"function_call":{"name":"legacy"}}"#),
        (OpenAiChat, OpenAiResponses, br#"{"model":"m","messages":[],"tools":[{"type":"function","function":{"name":"f","parameters":null,"description":null,"strict":null}}],"tool_choice":null}"#),
        (OpenAiChat, OpenAiResponses, br#"{"model":"m","messages":[],"tools":[{"type":"function","function":{"name":"f","strict":false}}],"tool_choice":"none"}"#),
        (OpenAiChat, OpenAiResponses, br#"{"model":"m","messages":[],"tools":[]}"#),
        (OpenAiChat, OpenAiResponses, br#"{"model":"m","messages":[],"tools":null}"#),
        (OpenAiChat, OpenAiResponses, br#"{"model":"m","messages":[],"functions":[]}"#),
        (OpenAiResponses, OpenAiChat, br#"{"model":"m","input":"weather","tools":[{"type":"function","name":"weather","parameters":{"type":"object"},"strict":true}],"tool_choice":{"type":"function","name":"weather"},"parallel_tool_calls":true}"#),
        (OpenAiResponses, OpenAiChat, br#"{"model":"m","input":[],"tools":[{"type":"function","name":"f","description":null,"parameters":null,"strict":false}],"tool_choice":"required"}"#),
        (OpenAiResponses, OpenAiChat, br#"{"model":"m","input":[],"tools":[],"tool_choice":"auto"}"#),
        (OpenAiResponses, OpenAiChat, br#"{"model":"m","input":[],"tools":null,"tool_choice":null}"#),
    ];
    for (source, target, input) in fixtures {
        let direct = routed(
            source,
            target,
            Operation::Request,
            input,
            zconv::Strategy::Direct,
        );
        let ir = routed(
            source,
            target,
            Operation::Request,
            input,
            zconv::Strategy::Ir,
        );
        assert_eq!(
            semantic(target, Operation::Request, &direct.0),
            semantic(target, Operation::Request, &ir.0),
            "{source:?}->{target:?}\nDirect={}\nIR={}",
            String::from_utf8_lossy(&direct.0),
            String::from_utf8_lossy(&ir.0)
        );
    }
}

#[test]
fn function_call_and_result_history_have_direct_ir_parity() {
    use Protocol::*;
    let fixtures = [
        (OpenAiChat, OpenAiResponses, br#"{"model":"m","messages":[{"role":"user","content":"weather"},{"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"weather","arguments":"{\"city\":\"\\u4e2d\\ud83d\\ude00\"}"}}]},{"role":"tool","tool_call_id":"call_1","content":"{\"ok\":true}"}]}"#.as_slice()),
        (OpenAiChat, OpenAiResponses, br#"{"model":"m","messages":[{"role":"assistant","content":"check both","tool_calls":[{"id":"call_1","type":"function","function":{"name":"f","arguments":"{}"}},{"id":"call_2","type":"function","function":{"name":"g","arguments":"{\"x\":1}"}}]},{"role":"tool","tool_call_id":"call_2","content":"2"},{"role":"tool","tool_call_id":"call_1","content":"1"}]}"#),
        (OpenAiChat, OpenAiResponses, br#"{"model":"m","messages":[{"role":"assistant","function_call":{"name":"legacy","arguments":"{\"quoted\":\"a\\\"b\"}"}},{"role":"function","name":"legacy","content":"done"}]}"#),
        (OpenAiChat, OpenAiResponses, br#"{"model":"m","messages":[{"role":"assistant","tool_calls":[{"id":"call_1","type":"function","function":{"name":"f","arguments":"model produced invalid JSON {"}}]}]}"#),
        (OpenAiResponses, OpenAiChat, br#"{"model":"m","input":[{"role":"user","content":"weather"},{"type":"function_call","call_id":"call_1","name":"weather","arguments":"{\"city\":\"\\u4e2d\"}"},{"type":"function_call_output","call_id":"call_1","output":"sunny"}]}"#),
        (OpenAiResponses, OpenAiChat, br#"{"model":"m","input":[{"type":"function_call","id":"fc_1","status":"completed","call_id":"call_1","name":"f","arguments":"{}"},{"type":"function_call_output","id":"out_1","status":"completed","call_id":"call_1","output":"done"}]}"#),
    ];
    for (source, target, input) in fixtures {
        let direct = routed(
            source,
            target,
            Operation::Request,
            input,
            zconv::Strategy::Direct,
        );
        let ir = routed(
            source,
            target,
            Operation::Request,
            input,
            zconv::Strategy::Ir,
        );
        assert_eq!(
            semantic(target, Operation::Request, &direct.0),
            semantic(target, Operation::Request, &ir.0),
            "{source:?}->{target:?}\nDirect={}\nIR={}",
            String::from_utf8_lossy(&direct.0),
            String::from_utf8_lossy(&ir.0)
        );
        assert_eq!((direct.1, direct.2), (ir.1, ir.2));
    }
}

#[test]
fn tool_history_requires_real_ids_and_preserves_argument_string_bytes() {
    use Protocol::*;
    let legacy = br#"{"model":"m","messages":[{"role":"assistant","function_call":{"name":"missing","arguments":"{}"}}]}"#;
    assert_eq!(
        run(
            OpenAiChat,
            OpenAiResponses,
            Operation::Request,
            legacy,
            Context::default(),
            Policy::default()
        )
        .unwrap_err(),
        Error::ContextRequired {
            field: "tool call ID"
        }
    );
    let context = Context {
        tool_calls: &[
            zconv::ir::ToolBinding {
                id: "a",
                name: "missing",
            },
            zconv::ir::ToolBinding {
                id: "b",
                name: "missing",
            },
        ],
        ..Context::default()
    };
    assert_eq!(
        run(
            OpenAiChat,
            OpenAiResponses,
            Operation::Request,
            legacy,
            context,
            Policy::default()
        )
        .unwrap_err(),
        Error::ContextRequired {
            field: "unambiguous tool call ID"
        }
    );
    let wrong_role = br#"{"model":"m","messages":[{"role":"user","content":"x","tool_calls":[{"id":"a","type":"function","function":{"name":"f","arguments":"{}"}}]}]}"#;
    assert!(matches!(
        run(
            OpenAiChat,
            OpenAiResponses,
            Operation::Request,
            wrong_role,
            Context::default(),
            Policy::default()
        ),
        Err(Error::InvalidInput {
            field: "tool_calls",
            ..
        })
    ));
    let source = br#"{"model":"m","messages":[{"role":"assistant","tool_calls":[{"id":"call_exact","type":"function","function":{"name":"f","arguments":"{\"text\":\"\\u4e2d\\n\"}"}}]}]}"#;
    let (out, _) = run(
        OpenAiChat,
        OpenAiResponses,
        Operation::Request,
        source,
        Context::default(),
        Policy::default(),
    )
    .unwrap();
    let mut before_tokens = [json::Token::EMPTY; 64];
    let mut after_tokens = [json::Token::EMPTY; 64];
    let before = json::parse(source, &mut before_tokens, json::Limits::default()).unwrap();
    let after = json::parse(&out, &mut after_tokens, json::Limits::default()).unwrap();
    let call = before
        .root()
        .get("messages")
        .unwrap()
        .array()
        .next()
        .unwrap()
        .get("tool_calls")
        .unwrap()
        .array()
        .next()
        .unwrap();
    let item = after.root().get("input").unwrap().array().next().unwrap();
    assert_eq!(
        item.get("call_id").unwrap().raw(),
        call.get("id").unwrap().raw()
    );
    assert_eq!(
        item.get("arguments").unwrap().raw(),
        call.get("function")
            .unwrap()
            .get("arguments")
            .unwrap()
            .raw()
    );
}

#[test]
fn tool_strict_defaults_are_resolved_without_changing_provider_semantics() {
    use Protocol::*;
    for (strict, expected) in [
        ("", false),
        (",\"strict\":null", false),
        (",\"strict\":false", false),
        (",\"strict\":true", true),
    ] {
        let input = format!(
            r#"{{"model":"m","messages":[],"tools":[{{"type":"function","function":{{"name":"f","parameters":{{"type":"object","properties":{{}},"additionalProperties":false,"required":[]}}{strict}}}}}]}}"#
        );
        let direct = routed(
            OpenAiChat,
            OpenAiResponses,
            Operation::Request,
            input.as_bytes(),
            zconv::Strategy::Direct,
        );
        let ir = routed(
            OpenAiChat,
            OpenAiResponses,
            Operation::Request,
            input.as_bytes(),
            zconv::Strategy::Ir,
        );
        assert_eq!(
            semantic(OpenAiResponses, Operation::Request, &direct.0),
            semantic(OpenAiResponses, Operation::Request, &ir.0)
        );
        let mut tokens = [json::Token::EMPTY; 128];
        let document = json::parse(&direct.0, &mut tokens, json::Limits::default()).unwrap();
        assert_eq!(
            document
                .root()
                .get("tools")
                .unwrap()
                .array()
                .next()
                .unwrap()
                .get("strict")
                .unwrap()
                .as_bool(),
            Some(expected)
        );
    }
    for strict in ["", ",\"strict\":null"] {
        let input = format!(
            r#"{{"model":"m","input":[],"tools":[{{"type":"function","name":"f","parameters":{{}}{strict}}}]}}"#
        );
        for strategy in [zconv::Strategy::Direct, zconv::Strategy::Ir] {
            let mut tokens = [json::Token::EMPTY; 128];
            let mut records = [Record::EMPTY; 32];
            let mut output = [0; 1024];
            let result = zconv::convert(
                OpenAiResponses.into(),
                OpenAiChat.into(),
                Operation::Request,
                input.as_bytes(),
                &mut output,
                zconv::Options {
                    strategy,
                    ..zconv::Options::default()
                },
                zconv::Workspace {
                    tokens: &mut tokens,
                    records: &mut records,
                    scratch: &mut [],
                    nested_tokens: &mut [],
                },
            );
            assert!(
                matches!(
                    result,
                    Err(Error::ContextRequired {
                        field: "resolved tool strict mode"
                    })
                ),
                "{strategy:?}: {result:?}"
            );
        }
    }
}

#[test]
fn direct_and_ir_preserve_the_same_response_semantics() {
    use Protocol::*;
    let chat = br#"{"id":"r1","object":"chat.completion","created":123,"model":"m","choices":[{"index":0,"message":{"role":"assistant","content":"hello"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#;
    let cases = [
        (OpenAiChat, OpenAiResponses, chat.as_slice()),
        (OpenAiChat, AnthropicMessages, chat.as_slice()),
        (OpenAiChat, GeminiGenerateContent, chat.as_slice()),
        (OpenAiResponses, OpenAiChat, br#"{"id":"r1","object":"response","created_at":123,"model":"m","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello","annotations":[]}]}],"usage":{"input_tokens":3,"output_tokens":2,"total_tokens":5}}"#),
        (AnthropicMessages, OpenAiChat, br#"{"id":"r1","type":"message","role":"assistant","model":"m","content":[{"type":"text","text":"hello"}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":2}}"#),
        (GeminiGenerateContent, OpenAiChat, br#"{"responseId":"r1","modelVersion":"m","candidates":[{"index":0,"content":{"role":"model","parts":[{"text":"hello"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":2,"totalTokenCount":5}}"#),
        (OpenAiChat, OpenAiResponses, br#"{"id":"r1","created":123,"model":"m","choices":[{"index":0,"message":{"role":"assistant","content":"cut"},"finish_reason":"length"}]}"#),
        (OpenAiChat, OpenAiResponses, br#"{"id":"r1","created":123,"model":"m","choices":[{"index":0,"message":{"role":"assistant","content":""},"finish_reason":"content_filter"}]}"#),
        (OpenAiResponses, OpenAiChat, br#"{"id":"r1","created_at":123,"model":"m","status":"incomplete","incomplete_details":{"reason":"content_filter"},"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":""}]}]}"#),
        (OpenAiChat, GeminiGenerateContent, br#"{"id":"r1","created":123,"model":"m","choices":[{"index":3,"message":{"role":"assistant","content":"a"},"finish_reason":"stop"},{"index":8,"message":{"role":"assistant","content":"b"},"finish_reason":"length"}]}"#),
    ];
    for (source, target, input) in cases {
        let direct = routed(
            source,
            target,
            Operation::Response,
            input,
            zconv::Strategy::Direct,
        );
        let ir = routed(
            source,
            target,
            Operation::Response,
            input,
            zconv::Strategy::Ir,
        );
        assert_eq!(
            semantic(target, Operation::Response, &direct.0),
            semantic(target, Operation::Response, &ir.0),
            "{source:?}->{target:?}\nDirect={}\nIR={}",
            String::from_utf8_lossy(&direct.0),
            String::from_utf8_lossy(&ir.0)
        );
        assert_eq!((direct.1, direct.2), (ir.1, ir.2));
    }
}
