use zconv::ir::{Content, Context, Policy, Record, Text};
use zconv::json::{self, Token};
use zconv::{Protocol, protocol};

fn main() -> Result<(), zconv::Error> {
    let input = br#"{"model":"example-model","messages":[{"role":"user","content":"before"}]}"#;
    let mut tokens = [Token::EMPTY; 64];
    let mut records = [Record::EMPTY; 16];
    let document = json::parse(input, &mut tokens, json::Limits::default())?;
    let request =
        protocol::decode_request(Protocol::OpenAiChat.into(), document.root(), &mut records)?;
    for record in request.records.iter_mut() {
        if let Record::Block(block) = record
            && matches!(block.content, Content::Text(_))
        {
            block.content = Content::Text(Text::Utf8("After an explicit IR edit: 你好 😀"));
        }
    }
    let mut output = [0; 1024];
    let mut scratch = [0; 256];
    // The decoded IR does not borrow the token tape: reuse the same storage.
    let report = protocol::encode_request(
        &request,
        Protocol::OpenAiResponses.into(),
        &Context::default(),
        Policy::default(),
        &mut output,
        &mut scratch,
        &mut tokens,
    )?;
    println!("{}", String::from_utf8_lossy(&output[..report.written]));
    Ok(())
}
