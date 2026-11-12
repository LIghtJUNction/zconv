use zconv::json::Token;
use zconv::stream::*;
use zconv::{Error, Protocol};

#[derive(Default)]
struct Storage {
    tokens: Vec<Token>,
    items: Vec<ActiveItem>,
    checkpoints: Vec<ActiveItem>,
    aggregate: Vec<u8>,
    output: Vec<u8>,
}
impl Storage {
    fn encode(
        &mut self,
        state: &mut EventEncoderState,
        context: StreamContext<'_>,
        payload: &[u8],
        events: &[Event],
    ) -> Vec<u8> {
        self.run(state, context, payload, events, false)
    }
    fn run(
        &mut self,
        state: &mut EventEncoderState,
        context: StreamContext<'_>,
        payload: &[u8],
        events: &[Event],
        preview: bool,
    ) -> Vec<u8> {
        let operation = if preview {
            EventEncoderState::preview
        } else {
            EventEncoderState::encode
        };
        let previous = state.report().ir_events;
        for _ in 0..32 {
            match operation(
                state,
                context,
                payload,
                events,
                EventEncoderBuffers {
                    tokens: &mut self.tokens,
                    items: &mut self.items,
                    checkpoints: &mut self.checkpoints,
                    aggregate: &mut self.aggregate,
                },
                &mut self.output,
            ) {
                Ok(n) => return self.output[..n].to_vec(),
                Err(Status::NeedBuffer { buffer, needed, .. }) => {
                    assert_eq!(state.report().ir_events, previous);
                    match buffer {
                        BufferKind::Tokens => self
                            .tokens
                            .resize(needed.max(self.tokens.len() * 2), Token::EMPTY),
                        BufferKind::Items => {
                            self.items
                                .resize(needed.max(self.items.len() * 2), ActiveItem::EMPTY);
                            self.checkpoints.resize(self.items.len(), ActiveItem::EMPTY);
                        }
                        BufferKind::Aggregate => self
                            .aggregate
                            .resize(needed.max(self.aggregate.len() * 2), 0),
                        BufferKind::Pending => self.output.resize(needed, 0),
                        other => panic!("unexpected encoding buffer: {other:?}"),
                    }
                }
                Err(error) => panic!("encoding failed: {error:?}"),
            }
        }
        panic!("capacity retries made no progress")
    }
}
fn span(offset: usize, length: usize) -> EventText {
    EventText {
        offset,
        length,
        quoted: false,
        present: true,
    }
}
fn context() -> StreamContext<'static> {
    StreamContext {
        response_id: Some("response"),
        model: Some("model"),
        created: Some(7),
        item_id_prefix: Some("local_"),
        ..StreamContext::default()
    }
}
#[test]
fn movable_buffers_resume_tool_arguments_without_replaying_output() {
    let mut config = StreamConfig::new(Protocol::OpenAiChat, Protocol::OpenAiResponses);
    config.context = context();
    let mut state = EventEncoderState::new(config).unwrap();
    let mut storage = Storage::default();
    let first = [
        Event {
            kind: EventKind::Begin,
            ..Event::EMPTY
        },
        Event {
            kind: EventKind::BlockStart,
            block: BlockKind::Tool,
            a: span(0, 4),
            b: span(4, 1),
            ..Event::EMPTY
        },
        Event {
            kind: EventKind::Delta,
            block: BlockKind::Tool,
            a: span(5, 5),
            ..Event::EMPTY
        },
    ];
    let mut wire = storage.encode(&mut state, context(), b"callf{\"x\":", &first);
    assert!(
        !wire
            .windows(b"response.completed".len())
            .any(|w| w == b"response.completed")
    );
    // Relocate every persistent allocation. The encoder has no borrowed buffers.
    storage.items = storage.items.clone();
    storage.aggregate = storage.aggregate.clone();
    let last = [
        Event {
            kind: EventKind::Delta,
            block: BlockKind::Tool,
            a: span(0, 2),
            ..Event::EMPTY
        },
        Event {
            kind: EventKind::BlockEnd,
            ..Event::EMPTY
        },
        Event {
            kind: EventKind::Usage,
            usage: Usage {
                input: Some(5),
                output: Some(3),
                total: Some(8),
                ..Usage::default()
            },
            ..Event::EMPTY
        },
        Event {
            kind: EventKind::ChoiceEnd,
            reason: FinishReason::ToolCalls,
            ..Event::EMPTY
        },
        Event {
            kind: EventKind::Finish,
            ..Event::EMPTY
        },
    ];
    wire.extend(storage.encode(&mut state, context(), b"1}", &last));
    let text = String::from_utf8(wire).unwrap();
    assert!(text.contains("response.completed"), "{text}");
    // Read the actual terminal JSON rather than asserting JSON escape spelling.
    let complete = text
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .find(|s| s.contains("\"type\":\"response.completed\""))
        .unwrap();
    assert!(
        complete.contains(r#""arguments":"{\"x\":1}""#),
        "{complete}"
    );
    assert_eq!(state.usage().input, Some(5));
    assert_eq!(state.report().ir_events, first.len() + last.len());
    assert!(state.is_finished());
}
#[test]
fn terminal_is_encoded_only_when_explicitly_supplied() {
    let mut config = StreamConfig::new(Protocol::OpenAiChat, Protocol::OpenAiChat);
    config.context = context();
    let mut state = EventEncoderState::new(config).unwrap();
    let mut storage = Storage::default();
    let first = [
        Event {
            kind: EventKind::Begin,
            ..Event::EMPTY
        },
        Event {
            kind: EventKind::BlockStart,
            ..Event::EMPTY
        },
        Event {
            kind: EventKind::Delta,
            a: span(0, 3),
            ..Event::EMPTY
        },
    ];
    let text = String::from_utf8(storage.encode(&mut state, context(), b"hey", &first)).unwrap();
    assert!(text.contains("hey"));
    assert!(!text.contains("[DONE]"));
    assert!(!state.is_finished());
    let last = [
        Event {
            kind: EventKind::BlockEnd,
            ..Event::EMPTY
        },
        Event {
            kind: EventKind::ChoiceEnd,
            ..Event::EMPTY
        },
        Event {
            kind: EventKind::Finish,
            ..Event::EMPTY
        },
    ];
    let text = String::from_utf8(storage.encode(&mut state, context(), b"", &last)).unwrap();
    assert!(text.ends_with("data: [DONE]\n\n"));
    assert!(matches!(
        state.encode(
            context(),
            b"",
            &[],
            EventEncoderBuffers {
                tokens: &mut storage.tokens,
                items: &mut storage.items,
                checkpoints: &mut storage.checkpoints,
                aggregate: &mut storage.aggregate,
            },
            &mut storage.output
        ),
        Err(Status::Failed(Error::AlreadyFinished))
    ));
}

#[test]
fn terminal_preview_can_commit_or_be_replaced_by_failure() {
    for commit in [false, true] {
        let mut config = StreamConfig::new(Protocol::OpenAiChat, Protocol::OpenAiResponses);
        config.context = context();
        let mut state = EventEncoderState::new(config).unwrap();
        let mut storage = Storage::default();
        storage.encode(
            &mut state,
            context(),
            b"ok",
            &[
                Event {
                    kind: EventKind::Begin,
                    ..Event::EMPTY
                },
                Event {
                    kind: EventKind::BlockStart,
                    block: BlockKind::Text,
                    ..Event::EMPTY
                },
                Event {
                    kind: EventKind::Delta,
                    block: BlockKind::Text,
                    a: span(0, 2),
                    ..Event::EMPTY
                },
            ],
        );
        let count = state.report().ir_events;
        let terminal = [
            Event {
                kind: EventKind::ChoiceEnd,
                reason: FinishReason::Stop,
                ..Event::EMPTY
            },
            Event {
                kind: EventKind::Finish,
                ..Event::EMPTY
            },
        ];
        let preview = storage.run(&mut state, context(), b"", &terminal, true);
        assert!(
            std::str::from_utf8(&preview)
                .unwrap()
                .contains("response.completed")
        );
        assert!(!state.is_finished());
        assert_eq!(state.report().ir_events, count);
        let failure = [Event {
            kind: EventKind::Failure,
            a: span(0, 6),
            b: span(6, 4),
            ..Event::EMPTY
        }];
        if commit {
            assert_eq!(
                storage.encode(&mut state, context(), b"", &terminal),
                preview
            );
            assert!(matches!(
                state.encode(
                    context(),
                    b"settlefail",
                    &failure,
                    EventEncoderBuffers {
                        tokens: &mut storage.tokens,
                        items: &mut storage.items,
                        checkpoints: &mut storage.checkpoints,
                        aggregate: &mut storage.aggregate
                    },
                    &mut storage.output
                ),
                Err(Status::Failed(Error::AlreadyFinished))
            ));
        } else {
            let wire = storage.encode(&mut state, context(), b"settlefail", &failure);
            let wire = std::str::from_utf8(&wire).unwrap();
            assert!(wire.contains("\"type\":\"error\""));
            assert!(wire.contains("\"sequence_number\":5"));
            assert!(!wire.contains("response.completed"));
        }
        assert!(state.is_finished());
    }
}
