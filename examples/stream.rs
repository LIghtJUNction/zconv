use std::io::Write;
use zconv::json::Token;
use zconv::stream::{ActiveItem, Event, Status, StreamBuffers, StreamConfig, StreamConverter};
use zconv::{Error, Protocol};

fn main() -> Result<(), Error> {
    let input = concat!(
        "data: {\"id\":\"r\",\"model\":\"example-model\",\"created\":1,\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1,\"total_tokens\":3}}\n\n",
        "data: [DONE]\n\n").as_bytes();
    let mut framing = [0; 512];
    let mut data = [0; 512];
    let mut pending = [0; 2048];
    let mut tokens = [Token::EMPTY; 128];
    let mut items = [ActiveItem::EMPTY; 4];
    let mut checkpoints = [ActiveItem::EMPTY; 4];
    let mut events = [Event::EMPTY; 8];
    let mut aggregate = [0; 1024];
    let mut config = StreamConfig::new(Protocol::OpenAiChat, Protocol::OpenAiResponses);
    config.context.item_id_prefix = Some("local_message_");
    let mut converter = StreamConverter::new(
        config,
        StreamBuffers {
            framing: &mut framing,
            data: &mut data,
            pending: &mut pending,
            tokens: &mut tokens,
            items: &mut items,
            checkpoints: &mut checkpoints,
            events: &mut events,
            aggregate: &mut aggregate,
        },
    )?;
    let mut output = [0; 13];
    let mut position = 0;
    let mut stdout = std::io::stdout().lock();
    loop {
        let end = (position + 7).min(input.len());
        let step = converter.step(&input[position..end], &mut output, end == input.len());
        position += step.consumed;
        stdout
            .write_all(&output[..step.written])
            .expect("write stdout");
        match step.status {
            Status::Done => break,
            Status::Failed(error) => return Err(error),
            Status::NeedBuffer { needed, .. } => return Err(Error::WorkspaceTooSmall { needed }),
            Status::NeedInput | Status::NeedOutput => {}
        }
    }
    Ok(())
}
