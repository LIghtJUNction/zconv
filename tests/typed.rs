use zconv::{
    ir::*,
    json::{self, Token},
    protocol, Protocol,
};

#[test]
fn typed_finish_edits_are_encoded_in_the_original_format() {
    let cases = [
        (Protocol::OpenAiChat, br#"{"id":"r","model":"m","created":1,"choices":[{"index":0,"message":{"role":"assistant","content":"x"},"finish_reason":"stop"}]}"#.as_slice()),
        (Protocol::OpenAiCompletions, br#"{"id":"r","model":"m","created":1,"choices":[{"index":0,"text":"x","finish_reason":"stop"}]}"#.as_slice()),
        (Protocol::AnthropicMessages, br#"{"id":"r","model":"m","role":"assistant","content":[{"type":"text","text":"x"}],"stop_reason":"end_turn"}"#.as_slice()),
        (Protocol::GeminiGenerateContent, br#"{"responseId":"r","modelVersion":"m","candidates":[{"index":0,"content":{"role":"model","parts":[{"text":"x"}]},"finishReason":"STOP"}]}"#.as_slice()),
    ];
    for (format, input) in cases {
        let mut tokens = [Token::EMPTY; 128];
        let mut records = [Record::EMPTY; 16];
        let doc = json::parse(input, &mut tokens, json::Limits::default()).unwrap();
        let response = protocol::decode_response(format.into(), doc.root(), &mut records).unwrap();
        for record in response.records.iter_mut() {
            if let Record::Choice(choice) = record {
                choice.finish = Finish::Length;
                choice.raw_finish = Presence::Missing;
            }
        }
        let mut output = [0; 1024];
        let report = protocol::encode_response(
            &response,
            format.into(),
            &Context::default(),
            Policy::default(),
            &mut output,
            &mut [],
            &mut tokens,
        )
        .unwrap();
        let mut decoded_records = [Record::EMPTY; 16];
        let doc = json::parse(
            &output[..report.written],
            &mut tokens,
            json::Limits::default(),
        )
        .unwrap();
        let decoded =
            protocol::decode_response(format.into(), doc.root(), &mut decoded_records).unwrap();
        assert!(
            decoded.records.iter().any(
                |record| matches!(record, Record::Choice(choice) if choice.finish == Finish::Length)
            ),
            "{format:?}"
        );
    }
}

#[test]
fn unchanged_missing_null_and_unknown_finish_are_preserved() {
    for ending in [
        "",
        ",\"finish_reason\":null",
        ",\"finish_reason\":\"vendor-finish\"",
    ] {
        let input = format!("{{\"id\":\"r\",\"model\":\"m\",\"created\":1,\"choices\":[{{\"index\":0,\"message\":{{\"role\":\"assistant\",\"content\":\"x\"}}{ending}}}]}}");
        let mut tokens = [Token::EMPTY; 128];
        let mut records = [Record::EMPTY; 16];
        let doc = json::parse(input.as_bytes(), &mut tokens, json::Limits::default()).unwrap();
        let response =
            protocol::decode_response(Protocol::OpenAiChat.into(), doc.root(), &mut records)
                .unwrap();
        let mut output = [0; 1024];
        let report = protocol::encode_response(
            &response,
            Protocol::OpenAiChat.into(),
            &Context::default(),
            Policy::default(),
            &mut output,
            &mut [],
            &mut tokens,
        )
        .unwrap();
        let doc = json::parse(
            &output[..report.written],
            &mut tokens,
            json::Limits::default(),
        )
        .unwrap();
        let choice = doc.root().get("choices").unwrap().array().next().unwrap();
        match ending {
            "" => assert!(choice.get("finish_reason").is_none()),
            ",\"finish_reason\":null" => assert!(choice.get("finish_reason").unwrap().is_null()),
            _ => assert!(choice
                .get("finish_reason")
                .unwrap()
                .as_string()
                .unwrap()
                .eq_str("vendor-finish")),
        }
    }
}

#[test]
fn typed_generation_and_usage_edits_override_borrowed_values() {
    let mut tokens = [Token::EMPTY; 128];
    let n = json::parse(b"12", &mut tokens, json::Limits::default())
        .unwrap()
        .root()
        .raw_value();
    let input = br#"{"model":"m","messages":[{"role":"user","content":"x"}],"max_tokens":1}"#;
    let mut records = [Record::EMPTY; 16];
    let doc = json::parse(input, &mut tokens, json::Limits::default()).unwrap();
    let mut request =
        protocol::decode_request(Protocol::OpenAiChat.into(), doc.root(), &mut records).unwrap();
    request.parameters.max_output_tokens = Presence::Value(n);
    let mut output = [0; 1024];
    let report = protocol::encode_request(
        &request,
        Protocol::OpenAiChat.into(),
        &Context::default(),
        Policy::default(),
        &mut output,
        &mut [],
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
        doc.root().get("max_completion_tokens").unwrap().as_u64(),
        Some(12)
    );
    let input = br#"{"id":"r","model":"m","created":1,"choices":[],"usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}}"#;
    let mut records = [Record::EMPTY; 16];
    let doc = json::parse(input, &mut tokens, json::Limits::default()).unwrap();
    let mut response =
        protocol::decode_response(Protocol::OpenAiChat.into(), doc.root(), &mut records).unwrap();
    response.usage.input_tokens = Presence::Value(Number::Unsigned(9));
    response.usage.total_tokens = Presence::Value(Number::Unsigned(11));
    let report = protocol::encode_response(
        &response,
        Protocol::OpenAiChat.into(),
        &Context::default(),
        Policy::default(),
        &mut output,
        &mut [],
        &mut tokens,
    )
    .unwrap();
    let doc = json::parse(
        &output[..report.written],
        &mut tokens,
        json::Limits::default(),
    )
    .unwrap();
    let usage = doc.root().get("usage").unwrap();
    assert_eq!(usage.get("prompt_tokens").unwrap().as_u64(), Some(9));
    assert_eq!(usage.get("total_tokens").unwrap().as_u64(), Some(11));
}
