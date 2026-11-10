use zconv::json::{self, JsonString, Limits, Token};
use zconv::stream::{
    decode_frame_format, validate_stream_format, BlockKind, Completion, Event, EventKind,
    InputCount, OutputCount, Usage, UsageAccumulator,
};
use zconv::{Error, Format, Profile, Protocol};

#[test]
fn framed_done_is_protocol_checked_and_recovers_event_capacity() {
    let mut events = [Event::EMPTY; 1];
    for protocol in [Protocol::OpenAiChat, Protocol::OpenAiCompletions] {
        assert_eq!(
            decode_frame_format(
                protocol.into(),
                b"message",
                b"[DONE]",
                &mut [],
                &mut [],
                Limits::default(),
            ),
            Err(Error::IrTooSmall { needed: 1 }),
        );
        assert_eq!(
            decode_frame_format(
                protocol.into(),
                b"message",
                b"[DONE]",
                &mut [],
                &mut events,
                Limits::default(),
            ),
            Ok(1),
        );
        assert_eq!(events[0].kind, EventKind::Finish);
        assert_eq!(events[0].completion, Completion::Completed);
        assert!(decode_frame_format(
            protocol.into(),
            b"response.completed",
            b"[DONE]",
            &mut [],
            &mut events,
            Limits::default(),
        )
        .is_err());
    }
    for protocol in [
        Protocol::OpenAiResponses,
        Protocol::AnthropicMessages,
        Protocol::GeminiGenerateContent,
        Protocol::GeminiInteractions,
    ] {
        assert!(matches!(
            decode_frame_format(
                protocol.into(),
                b"message",
                b"[DONE]",
                &mut [],
                &mut events,
                Limits::default(),
            ),
            Err(Error::InvalidInput {
                field: "SSE data",
                ..
            }),
        ));
    }
}

#[test]
fn frame_names_match_decoded_discriminators_and_preserve_distinct_ids() {
    let payload = br#"{"type":"response.output_item.ad\u0064ed","output_index":3,"item":{"type":"function_call","id":"item-7","call_id":"call-9","name":"lookup","arguments":""}}"#;
    let mut tokens = [Token::EMPTY; 64];
    let mut events = [Event::EMPTY; 8];
    let count = decode_frame_format(
        Protocol::OpenAiResponses.into(),
        b"response.output_item.added",
        payload,
        &mut tokens,
        &mut events,
        Limits::default(),
    )
    .unwrap();
    assert_eq!(count, 1);
    let event = events[0];
    assert_eq!(event.kind, EventKind::BlockStart);
    assert_eq!(event.block, BlockKind::Tool);
    assert_eq!(event.key.index, 3);
    for (span, expected) in [(event.a, "call-9"), (event.item_id, "item-7")] {
        assert!(span.present && span.quoted);
        assert!(
            JsonString::parse(&payload[span.offset..span.offset + span.length])
                .unwrap()
                .eq_str(expected)
        );
    }
    assert!(matches!(
        decode_frame_format(
            Protocol::OpenAiResponses.into(),
            b"response.completed",
            payload,
            &mut tokens,
            &mut events,
            Limits::default(),
        ),
        Err(Error::InvalidInput {
            field: "SSE event name",
            ..
        }),
    ));
    assert_eq!(
        decode_frame_format(
            Protocol::OpenAiResponses.into(),
            b"message",
            payload,
            &mut tokens,
            &mut events,
            Limits::default(),
        ),
        Ok(1)
    );
}

#[test]
fn stream_format_can_be_checked_without_input_or_storage() {
    assert!(validate_stream_format(Protocol::AnthropicMessages.into()).is_ok());
    assert!(validate_stream_format(Format {
        protocol: Protocol::AnthropicMessages,
        version: "2026-10-21",
        profile: Profile::Standard,
    })
    .is_err());
    assert!(validate_stream_format(Format {
        protocol: Protocol::OpenAiChat,
        version: "v1",
        profile: Profile::OpenAiCompatible("test-provider"),
    })
    .is_ok());
    assert!(validate_stream_format(Format {
        protocol: Protocol::OpenAiChat,
        version: "v1",
        profile: Profile::OpenAiCompatible(""),
    })
    .is_err());
}

#[test]
fn usage_replaces_cumulative_counts_and_recomputes_partial_cache_updates() {
    let mut accumulator = UsageAccumulator::default();
    assert_eq!(accumulator.usage(), Usage::default());
    let initial = Usage {
        input: Some(10),
        output: Some(2),
        cached: Some(3),
        cache_write: Some(4),
        input_basis: InputCount::Uncached,
        ..Usage::default()
    };
    let normalized = accumulator.update(initial).unwrap();
    assert_eq!(normalized.input, Some(17));
    assert_eq!(normalized.total, Some(19));
    assert_eq!(accumulator.update(initial).unwrap(), normalized);
    let next = accumulator
        .update(Usage {
            output: Some(7),
            input_basis: InputCount::Uncached,
            ..Usage::default()
        })
        .unwrap();
    assert_eq!(next.input, Some(17));
    assert_eq!(next.output, Some(7));
    assert_eq!(next.total, Some(24));
    let next = accumulator
        .update(Usage {
            cached: Some(5),
            ..Usage::default()
        })
        .unwrap();
    assert_eq!(next.input, Some(19));
    assert_eq!(next.output, Some(7));
    assert_eq!(next.total, Some(26));
    assert_eq!(next.input_basis, InputCount::Total);
    assert_eq!(next.output_basis, OutputCount::Total);
}

#[test]
fn usage_thought_updates_do_not_guess_missing_output_or_add_twice() {
    let mut accumulator = UsageAccumulator::default();
    assert_eq!(
        accumulator
            .update(Usage {
                reasoning: Some(2),
                output_basis: OutputCount::Visible,
                ..Usage::default()
            })
            .unwrap()
            .output,
        None
    );
    let update = Usage {
        input: Some(5),
        output: Some(7),
        total: Some(14),
        output_basis: OutputCount::Visible,
        ..Usage::default()
    };
    assert_eq!(accumulator.update(update).unwrap().output, Some(9));
    assert_eq!(accumulator.update(update).unwrap().output, Some(9));
    let next = accumulator
        .update(Usage {
            reasoning: Some(4),
            ..Usage::default()
        })
        .unwrap();
    assert_eq!(next.output, Some(11));
    assert_eq!(next.total, Some(14)); // A reported total is not guessed from fields.
}

#[test]
fn usage_overflow_is_atomic_and_initial_context_has_canonical_basis() {
    let mut accumulator = UsageAccumulator::new(Usage {
        input: Some(1),
        output: Some(2),
        ..Usage::default()
    })
    .unwrap();
    let saved = accumulator;
    assert!(accumulator
        .update(Usage {
            input: Some(u64::MAX),
            cached: Some(1),
            input_basis: InputCount::Uncached,
            ..Usage::default()
        })
        .is_err());
    assert_eq!(accumulator, saved);
    assert!(accumulator
        .update(Usage {
            output: Some(u64::MAX),
            reasoning: Some(1),
            output_basis: OutputCount::Visible,
            ..Usage::default()
        })
        .is_err());
    assert_eq!(accumulator, saved);
    assert!(UsageAccumulator::new(Usage {
        input_basis: InputCount::Uncached,
        ..Usage::default()
    })
    .is_err());
    assert!(UsageAccumulator::new(Usage {
        output_basis: OutputCount::Visible,
        ..Usage::default()
    })
    .is_err());
}

#[test]
fn scratch_capacity_is_distinct_from_token_capacity_and_retryable() {
    use zconv::ir::{Content, Context, Data, Policy, Record, Text};
    use zconv::protocol::{decode_request, encode_request};
    let input = br#"{"model":"m","messages":[{"role":"assistant","tool_calls":[{"id":"c1","type":"function","function":{"name":"f","arguments":"{\"x\":1}"}}]}]}"#;
    let mut tokens = [Token::EMPTY; 64];
    let mut records = [Record::EMPTY; 16];
    let doc = json::parse(input, &mut tokens, Limits::default()).unwrap();
    let request = decode_request(Protocol::OpenAiChat.into(), doc.root(), &mut records).unwrap();
    let context = Context {
        max_tokens: Some(10),
        ..Context::default()
    };
    let mut output = [0; 1024];
    let target = Protocol::AnthropicMessages.into();
    assert_eq!(
        encode_request(
            &request,
            target,
            &context,
            Policy::default(),
            &mut output,
            &mut [],
            &mut tokens,
        )
        .unwrap_err(),
        Error::ScratchTooSmall { needed: 7 }
    );
    let mut scratch = [0; 7];
    assert!(matches!(
        encode_request(
            &request,
            target,
            &context,
            Policy::default(),
            &mut output,
            &mut scratch,
            &mut [],
        ),
        Err(Error::WorkspaceTooSmall { .. })
    ));
    assert!(encode_request(
        &request,
        target,
        &context,
        Policy::default(),
        &mut output,
        &mut scratch,
        &mut tokens,
    )
    .is_ok());
    for record in request.records.iter_mut() {
        if let Record::Block(block) = record {
            if let Content::ToolCall { arguments, .. } = &mut block.content {
                *arguments = Data::JsonText(Text::Utf8("{\"x\":1}"));
            }
        }
    }
    assert_eq!(
        encode_request(
            &request,
            target,
            &context,
            Policy::default(),
            &mut output,
            &mut [],
            &mut tokens,
        )
        .unwrap_err(),
        Error::ScratchTooSmall { needed: 7 }
    );
}
