//! A bounded Miri corpus that exercises borrowing and stream state transitions.
use zconv::ir::{Context, Record};
use zconv::json::{self, Token};
use zconv::stream::{ActiveItem, Event, Status, StreamBuffers, StreamConfig, StreamConverter};
use zconv::{convert, Operation, Options, Protocol, Strategy, Workspace};

#[test]
fn unicode_tape_ir_and_reused_storage() {
    let input = br#"{"model":"m","messages":[{"role":"user","content":"\u4e2d\ud83d\ude00"}]}"#;
    let mut tokens = [Token::EMPTY; 64];
    let mut nested = [Token::EMPTY; 32];
    let mut records = [Record::EMPTY; 8];
    let mut output = [0; 512];
    let mut scratch = [0; 128];
    for strategy in [Strategy::Direct, Strategy::Ir] {
        let result = convert(
            Protocol::OpenAiChat.into(),
            Protocol::OpenAiResponses.into(),
            Operation::Request,
            input,
            &mut output,
            Options {
                strategy,
                context: Context::default(),
                ..Options::default()
            },
            Workspace {
                tokens: &mut tokens,
                records: &mut records,
                scratch: &mut scratch,
                nested_tokens: &mut nested,
            },
        )
        .unwrap();
        json::parse(
            &output[..result.written()],
            &mut tokens,
            json::Limits::default(),
        )
        .unwrap();
    }
}

#[test]
fn stream_split_utf8_and_pending_output() {
    let wire = concat!(
        "data: {\"id\":\"r\",\"model\":\"m\",\"created\":1,\"choices\":[{\"index\":0,\"delta\":{\"content\":\"中\"},\"finish_reason\":null}]}\r\n\r\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1,\"total_tokens\":3}}\n\n",
        "data: [DONE]\n\n").as_bytes();
    let mut framing = [0; 256];
    let mut data = [0; 256];
    let mut pending = [0; 2048];
    let mut tokens = [Token::EMPTY; 64];
    let mut items = [ActiveItem::EMPTY; 4];
    let mut checkpoints = [ActiveItem::EMPTY; 4];
    let mut events = [Event::EMPTY; 8];
    let mut aggregate = [0; 256];
    let mut config = StreamConfig::new(Protocol::OpenAiChat, Protocol::OpenAiResponses);
    config.strategy = Strategy::Ir;
    config.context.item_id_prefix = Some("local_");
    let mut stream = StreamConverter::new(
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
    )
    .unwrap();
    let mut offset = 0;
    let mut output = [0; 13];
    let mut finished = false;
    for _ in 0..2000 {
        let end = (offset + 7).min(wire.len());
        let result = stream.step(&wire[offset..end], &mut output, end == wire.len());
        offset += result.consumed;
        match result.status {
            Status::Done => {
                finished = true;
                break;
            }
            Status::NeedInput | Status::NeedOutput => {}
            other => panic!("{other:?}"),
        }
    }
    assert!(finished);
    assert_eq!(offset, wire.len());
    assert_eq!(stream.usage().output, Some(1));
}
