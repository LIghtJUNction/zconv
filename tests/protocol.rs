use zconv::{ir::*, json::{self, Token}, protocol, Error, Format, Protocol};

const PROTOCOLS: [Protocol; 6] = [Protocol::OpenAiCompletions, Protocol::OpenAiChat, Protocol::OpenAiResponses, Protocol::AnthropicMessages, Protocol::GeminiGenerateContent, Protocol::GeminiInteractions];
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

fn context() -> Context<'static> { Context { model: Some("m"), response_id: Some("r"), created: Some(1), created_iso: Some("2026-11-10T00:00:00Z"), updated_iso: Some("2026-11-10T00:00:01Z"), max_tokens: Some(32), item_ids:&["msg_0","msg_1","msg_2"], ..Context::default() } }

#[test]
fn all_six_requests_decode_and_encode_to_all_six() {
    for (source, input) in PROTOCOLS.iter().zip(REQUESTS) {
        for target in PROTOCOLS {
            let mut tokens=[Token::EMPTY;512]; let mut records=[Record::EMPTY;128];
            let document=json::parse(input,&mut tokens,json::Limits::default()).unwrap();
            let request=protocol::decode_request(Format::new(*source),document.root(),&mut records).unwrap();
            // Token tape is independent of decoded IR and immediately reusable.
            let mut out=[0u8;4096]; let mut scratch=[0u8;4096];
            let report=protocol::encode_request(&request,Format::new(target),&context(),Policy::default(),&mut out,&mut scratch,&mut tokens).unwrap_or_else(|e|panic!("{source:?} -> {target:?}: {e:?}"));
            let result=json::parse(&out[..report.written],&mut tokens,json::Limits::default()).unwrap();
            protocol::validate(Format::new(target),zconv::Operation::Request,result.root()).unwrap_or_else(|e|panic!("{source:?} -> {target:?}: {e:?}; {}",String::from_utf8_lossy(&out[..report.written])));
        }
    }
}

#[test]
fn all_six_responses_decode_and_encode_to_all_six() {
    for (source,input) in PROTOCOLS.iter().zip(RESPONSES) {
        for target in PROTOCOLS {
            let mut tokens=[Token::EMPTY;512]; let mut records=[Record::EMPTY;128];
            let document=json::parse(input,&mut tokens,json::Limits::default()).unwrap();
            let response=protocol::decode_response(Format::new(*source),document.root(),&mut records).unwrap();
            let mut out=[0u8;4096]; let mut scratch=[0u8;4096];
            let report=protocol::encode_response(&response,Format::new(target),&context(),Policy{drop_metadata:true,..Policy::default()},&mut out,&mut scratch,&mut tokens).unwrap_or_else(|e|panic!("{source:?} -> {target:?}: {e:?}"));
            let result=json::parse(&out[..report.written],&mut tokens,json::Limits::default()).unwrap();
            protocol::validate(Format::new(target),zconv::Operation::Response,result.root()).unwrap_or_else(|e|panic!("{source:?} -> {target:?}: {e:?}; {}",String::from_utf8_lossy(&out[..report.written])));
        }
    }
}

#[test]
fn caller_can_edit_text_then_reuse_token_tape() {
    let mut tokens=[Token::EMPTY;128];let mut records=[Record::EMPTY;16];
    let doc=json::parse(REQUESTS[1],&mut tokens,json::Limits::default()).unwrap();
    let request=protocol::decode_request(Format::new(Protocol::OpenAiChat),doc.root(),&mut records).unwrap();
    for record in request.records.iter_mut() {if let Record::Block(b)=record{if matches!(b.content,Content::Text(_)){b.content=Content::Text(Text::Utf8("edited 😀\n\""));}}}
    let mut out=[0u8;1024];let mut scratch=[0u8;128];
    let report=protocol::encode_request(&request,Format::new(Protocol::OpenAiResponses),&context(),Policy::default(),&mut out,&mut scratch,&mut tokens).unwrap();
    let result=json::parse(&out[..report.written],&mut tokens,json::Limits::default()).unwrap();
    let text=result.root().get("input").unwrap().array().next().unwrap().get("content").unwrap().array().next().unwrap().get("text").unwrap().as_string().unwrap();
    assert!(text.eq_str("edited 😀\n\""));
}

#[test]
fn extensions_are_source_owned_and_losses_identify_them() {
    let input=br#"{"model":"m","messages":[{"role":"user","content":"hi"}],"vendor_secret":0}"#;
    let mut tokens=[Token::EMPTY;128];let mut records=[Record::EMPTY;32];
    let doc=json::parse(input,&mut tokens,json::Limits::default()).unwrap();
    let request=protocol::decode_request(Format::new(Protocol::OpenAiChat),doc.root(),&mut records).unwrap();
    let mut out=[0;1024];let mut scratch=[0;128];
    let target=Format::new(Protocol::OpenAiResponses);
    assert!(matches!(protocol::encode_request(&request,target,&context(),Policy::default(),&mut out,&mut scratch,&mut tokens),Err(Error::PolicyViolation{..})));
    let policy=Policy{drop_extensions:true,..Policy::default()};
    let report=protocol::encode_request(&request,target,&context(),policy,&mut out,&mut scratch,&mut tokens).unwrap();
    assert_eq!(report.dropped_extensions,1);
    let losses:Vec<_>=request.losses(target,policy).collect();assert_eq!(losses.len(),1);
    assert!(matches!(losses[0],Loss::Extension(e) if e.key.eq_str("vendor_secret")));
}

#[test]
fn exact_usage_large_integer_survives() {
    let input=br#"{"id":"r","model":"m","created":1,"choices":[{"index":0,"message":{"role":"assistant","content":"x"},"finish_reason":"stop"}],"usage":{"prompt_tokens":9007199254740993,"completion_tokens":0,"total_tokens":9007199254740993}}"#;
    let mut tokens=[Token::EMPTY;256];let mut records=[Record::EMPTY;32];
    let doc=json::parse(input,&mut tokens,json::Limits::default()).unwrap();
    let response=protocol::decode_response(Format::new(Protocol::OpenAiChat),doc.root(),&mut records).unwrap();
    let mut out=[0;2048];let mut scratch=[0;128];
    let report=protocol::encode_response(&response,Format::new(Protocol::OpenAiResponses),&context(),Policy::default(),&mut out,&mut scratch,&mut tokens).unwrap();
    let doc=json::parse(&out[..report.written],&mut tokens,json::Limits::default()).unwrap();
    assert_eq!(doc.root().get("usage").unwrap().get("input_tokens").unwrap().as_u64(),Some(9007199254740993));
}

#[test]
fn context_namespace_does_not_cross_vendors() {
    let input=br#"{"model":"m","input":"hi","previous_interaction_id":"server-state"}"#;
    let mut tokens=[Token::EMPTY;128];let mut records=[Record::EMPTY;32];
    let doc=json::parse(input,&mut tokens,json::Limits::default()).unwrap();
    let request=protocol::decode_request(Format::new(Protocol::GeminiInteractions),doc.root(),&mut records).unwrap();
    let mut out=[0;2048];let mut scratch=[0;128];
    assert!(matches!(protocol::encode_request(&request,Format::new(Protocol::OpenAiResponses),&context(),Policy::default(),&mut out,&mut scratch,&mut tokens),Err(Error::ContextRequired{..})));
}

#[test]
fn unsupported_anthropic_version_controls_rejected() {
    let input=br#"{"model":"m","max_tokens":1,"temperature":1,"messages":[]}"#;
    let mut tokens=[Token::EMPTY;128];let doc=json::parse(input,&mut tokens,json::Limits::default()).unwrap();
    let mut format=Format::new(Protocol::AnthropicMessages);format.version="2026-10-21";
    assert!(protocol::validate(format,zconv::Operation::Request,doc.root()).is_err());
}

#[test]
fn workspace_and_output_failures_report_recovery_sizes() {
    let mut tokens=[Token::EMPTY;128];let doc=json::parse(REQUESTS[1],&mut tokens,json::Limits::default()).unwrap();
    assert!(matches!(protocol::decode_request(Format::new(Protocol::OpenAiChat),doc.root(),&mut []),Err(Error::IrTooSmall{needed:1})));
    let mut records=[Record::EMPTY;32];let request=protocol::decode_request(Format::new(Protocol::OpenAiChat),doc.root(),&mut records).unwrap();
    let mut scratch=[0;128];let err=protocol::encode_request(&request,Format::new(Protocol::OpenAiResponses),&context(),Policy::default(),&mut [],&mut scratch,&mut tokens).unwrap_err();
    let needed=if let Error::OutputTooSmall{needed}=err{needed}else{panic!("{err:?}")};let mut out=vec![0;needed];
    assert_eq!(protocol::encode_request(&request,Format::new(Protocol::OpenAiResponses),&context(),Policy::default(),&mut out,&mut scratch,&mut tokens).unwrap().written,needed);
}
