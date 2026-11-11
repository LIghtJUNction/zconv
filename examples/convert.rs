use zconv::ir::Record;
use zconv::json::Token;
use zconv::{Operation, Options, Protocol, Workspace, convert};

fn main() -> Result<(), zconv::Error> {
    let input = br#"{"model":"example-model","messages":[{"role":"user","content":"Hello"}]}"#;
    let mut tokens = [Token::EMPTY; 64];
    let mut records = [Record::EMPTY; 16];
    let mut nested_tokens = [Token::EMPTY; 64];
    let mut scratch = [0; 512];
    let mut output = [0; 1024];
    let converted = convert(
        Protocol::OpenAiChat.into(),
        Protocol::OpenAiResponses.into(),
        Operation::Request,
        input,
        &mut output,
        Options::default(),
        Workspace {
            tokens: &mut tokens,
            records: &mut records,
            scratch: &mut scratch,
            nested_tokens: &mut nested_tokens,
        },
    )?;
    println!(
        "path={:?}, IR records={}",
        converted.path, converted.resources.ir_records_used
    );
    println!(
        "{}",
        String::from_utf8_lossy(&output[..converted.written()])
    );
    Ok(())
}
