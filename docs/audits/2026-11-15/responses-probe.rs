use zconv::{convert, Format, Operation, Options, Protocol, Strategy, Workspace};
use zconv::ir::Record;
use zconv::json::Token;

fn check(label: &str, input: &str, target: Protocol, strategy: Strategy) {
    let mut tokens = [Token::EMPTY; 1024];
    let mut records = [Record::EMPTY; 256];
    let mut nested_tokens = [Token::EMPTY; 1024];
    let mut scratch = [0u8; 16384];
    let mut output = [0u8; 16384];
    let result = convert(
        Format::new(Protocol::OpenAiResponses), Format::new(target), Operation::Request,
        input.as_bytes(), &mut output,
        Options { strategy, ..Options::default() },
        Workspace { tokens: &mut tokens, records: &mut records,
            scratch: &mut scratch, nested_tokens: &mut nested_tokens },
    );
    match result {
        Ok(r) => println!("{label}\tOK {:?}\t{}", r.path, std::str::from_utf8(&output[..r.written()]).unwrap()),
        Err(e) => println!("{label}\tERR\t{e:?}"),
    }
}

fn main() {
    use Protocol::{OpenAiChat as Chat, OpenAiResponses as Responses};
    let cases = [
        ("assistant_no_id_status_annotations", r#"{"model":"m","input":[{"role":"assistant","content":[{"type":"output_text","text":"ok"}]}]}"#),
        ("assistant_content_missing", r#"{"model":"m","input":[{"type":"message","role":"assistant"}]}"#),
        ("assistant_content_null", r#"{"model":"m","input":[{"type":"message","role":"assistant","content":null}]}"#),
        ("output_text_annotations_null", r#"{"model":"m","input":[{"role":"assistant","content":[{"type":"output_text","text":"ok","annotations":null}]}]}"#),
        ("output_text_annotations_empty", r#"{"model":"m","input":[{"role":"assistant","content":[{"type":"output_text","text":"ok","annotations":[]}]}]}"#),
        ("reasoning_id_summary_missing", r#"{"model":"m","input":[{"type":"reasoning","encrypted_content":"opaque"}]}"#),
        ("reasoning_summary_null", r#"{"model":"m","input":[{"type":"reasoning","summary":null,"encrypted_content":"opaque"}]}"#),
        ("image_detail_missing", r#"{"model":"m","input":[{"role":"user","content":[{"type":"input_image","image_url":"https://example.org/a.png"}]}]}"#),
        ("image_detail_null", r#"{"model":"m","input":[{"role":"user","content":[{"type":"input_image","image_url":"https://example.org/a.png","detail":null}]}]}"#),
        ("function_tool_roundtrip", r#"{"model":"m","input":[{"type":"function_call","call_id":"c1","name":"lookup","arguments":"{}"},{"role":"assistant","content":[{"type":"output_text","text":"checking"}]},{"type":"function_call_output","call_id":"c1","output":"ok"}]}"#),
        ("function_result_image", r#"{"model":"m","input":[{"type":"function_call_output","call_id":"c1","output":[{"type":"input_image","image_url":"https://example.org/a.png"}]}]}"#),
        ("text_verbosity", r#"{"model":"m","input":"hello","text":{"verbosity":"low"}}"#),
        ("previous_response_id", r#"{"model":"m","input":"hello","previous_response_id":"resp_previous"}"#),
    ];
    for (label, input) in cases {
        check(&format!("{label}:identity"), input, Responses, Strategy::Auto);
        check(&format!("{label}:ir_same"), input, Responses, Strategy::Ir);
        check(&format!("{label}:chat_auto"), input, Chat, Strategy::Auto);
    }
}
