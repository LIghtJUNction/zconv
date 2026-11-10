use zconv::json::{self, Token};
use zconv::stream::*;
use zconv::{Error, Path, Protocol, Strategy};

const CHAT: &[u8] = concat!(
    "data: {\"id\":\"r1\",\"model\":\"m\",\"created\":7,\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"r1\",\"model\":\"m\",\"created\":7,\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi \\u4e2d\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"r1\",\"model\":\"m\",\"created\":7,\"choices\":[{\"index\":0,\"delta\":{\"content\":\" 🌍\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"r1\",\"model\":\"m\",\"created\":7,\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: {\"id\":\"r1\",\"model\":\"m\",\"created\":7,\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":3,\"total_tokens\":8}}\n\n",
    "data: [DONE]\n\n"
).as_bytes();

const TOOLS: &[u8] = concat!(
    "data: {\"id\":\"r1\",\"model\":\"m\",\"created\":7,\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"tool_calls\":[{\"index\":0,\"id\":\"a\",\"type\":\"function\",\"function\":{\"name\":\"f\",\"arguments\":\"{\\\"x\\\":\"}}]},\"finish_reason\":null}]}\n\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"b\",\"type\":\"function\",\"function\":{\"name\":\"g\",\"arguments\":\"{\\\"y\\\":\"}}]},\"finish_reason\":null}]}\n\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"1}\"}},{\"index\":1,\"function\":{\"arguments\":\"2}\"}}]},\"finish_reason\":null}]}\n\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":3,\"total_tokens\":8}}\n\n",
    "data: [DONE]\n\n"
).as_bytes();

fn config(source: Protocol, target: Protocol, strategy: Strategy) -> StreamConfig<'static> {
    let mut config = StreamConfig::new(source, target);
    config.strategy = strategy;
    config.context.created = Some(7);
    config.context.response_id = Some("r1");
    config.context.model = Some("m");
    config.context.item_id_prefix = Some("local_");
    config.context.initial_usage.input = Some(5);
    config
}

fn run(
    input: &[u8],
    config: StreamConfig<'_>,
    split: usize,
    output_size: usize,
) -> Result<(Vec<u8>, Path, Report, Usage, Memory), (Error, Vec<u8>)> {
    let mut framing = vec![0; 65536];
    let mut data = vec![0; 65536];
    let mut pending = vec![0; 65536];
    let mut tokens = vec![Token::EMPTY; 4096];
    let mut items = vec![ActiveItem::EMPTY; 64];
    let mut checkpoints = vec![ActiveItem::EMPTY; 64];
    let mut events = vec![Event::EMPTY; 256];
    let mut aggregate = vec![0; 65536];
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
    )
    .map_err(|error| (error, Vec::new()))?;
    let mut result = Vec::new();
    let mut position = 0;
    let mut output = vec![0; output_size];
    let mut turns = 0;
    loop {
        turns += 1;
        assert!(turns < input.len() * 500 + 10000, "no-progress loop");
        let end = (position + split).min(input.len());
        let progress = converter.step(&input[position..end], &mut output, end == input.len());
        position += progress.consumed;
        result.extend_from_slice(&output[..progress.written]);
        match progress.status {
            Status::Done if position == input.len() => {
                return Ok((
                    result,
                    converter.path(),
                    converter.report(),
                    converter.usage(),
                    converter.memory(),
                ))
            }
            Status::Failed(error) => return Err((error, result)),
            Status::NeedBuffer { buffer, needed, .. } => {
                panic!("insufficient {buffer:?}: {needed}")
            }
            _ => {}
        }
    }
}

fn json_events(wire: &[u8]) -> Vec<Vec<u8>> {
    let mut framing = vec![0; wire.len() + 64];
    let mut decoder = zconv::sse::Decoder::new(&mut framing);
    let mut output = vec![0; wire.len() + 64];
    let mut position = 0;
    let mut events = Vec::new();
    while position < wire.len() {
        let progress = decoder.push(&wire[position..]);
        position += progress.consumed;
        if progress.status == zconv::sse::Status::Event {
            if let Some(length) = decoder.event().unwrap().data_into(&mut output).unwrap() {
                if &output[..length] != b"[DONE]" {
                    let mut tokens = vec![Token::EMPTY; 8192];
                    json::parse(&output[..length], &mut tokens, json::Limits::default())
                        .expect("every output event must be valid JSON");
                    events.push(output[..length].to_vec());
                }
            }
            decoder.consume_event();
        }
    }
    events
}

fn chat_text(wire: &[u8]) -> String {
    let mut result = String::new();
    for event in json_events(wire) {
        let mut tokens = vec![Token::EMPTY; 4096];
        let doc = json::parse(&event, &mut tokens, json::Limits::default()).unwrap();
        if let Some(choices) = doc.root().get("choices") {
            for choice in choices.array() {
                if let Some(text) = choice
                    .get("delta")
                    .and_then(|delta| delta.get("content"))
                    .and_then(|value| value.as_string())
                {
                    result.push_str(&String::from_utf8(text.bytes().collect()).unwrap());
                }
            }
        }
    }
    result
}

#[test]
fn six_chat_centric_direct_directions_and_ir_are_equivalent() {
    for target in [
        Protocol::OpenAiResponses,
        Protocol::AnthropicMessages,
        Protocol::GeminiGenerateContent,
    ] {
        let direct = run(
            CHAT,
            config(Protocol::OpenAiChat, target, Strategy::Direct),
            CHAT.len(),
            65536,
        )
        .unwrap();
        let ir = run(
            CHAT,
            config(Protocol::OpenAiChat, target, Strategy::Ir),
            7,
            17,
        )
        .unwrap();
        assert_eq!(direct.0, ir.0, "forward {target:?}");
        assert_eq!(direct.1, Path::Direct);
        assert_eq!(direct.2.ir_events, 0);
        assert!(ir.2.ir_events > 0);
        json_events(&direct.0);
        let reverse_direct = run(
            &direct.0,
            config(target, Protocol::OpenAiChat, Strategy::Direct),
            1,
            1,
        )
        .unwrap();
        let reverse_ir = run(
            &direct.0,
            config(target, Protocol::OpenAiChat, Strategy::Ir),
            113,
            31,
        )
        .unwrap();
        assert_eq!(reverse_direct.0, reverse_ir.0, "reverse {target:?}");
        assert_eq!(chat_text(&reverse_direct.0), "Hi 中 🌍");
        assert_eq!(reverse_direct.3.output, Some(3));
    }
}

#[test]
fn every_fragment_size_and_one_byte_output_is_lossless() {
    let baseline = run(
        CHAT,
        config(
            Protocol::OpenAiChat,
            Protocol::OpenAiResponses,
            Strategy::Auto,
        ),
        CHAT.len(),
        65536,
    )
    .unwrap()
    .0;
    for split in 1..=97 {
        let output = run(
            CHAT,
            config(
                Protocol::OpenAiChat,
                Protocol::OpenAiResponses,
                Strategy::Auto,
            ),
            split,
            1,
        )
        .unwrap()
        .0;
        assert_eq!(output, baseline, "split {split}");
    }
}

#[test]
fn legacy_completions_and_independent_interactions_streams() {
    let legacy = b"data: {\"id\":\"r1\",\"created\":7,\"model\":\"m\",\"choices\":[{\"index\":0,\"text\":\"hello\",\"finish_reason\":\"stop\",\"logprobs\":null}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1,\"total_tokens\":6}}\n\ndata: [DONE]\n\n";
    let output = run(
        legacy,
        config(
            Protocol::OpenAiCompletions,
            Protocol::GeminiInteractions,
            Strategy::Auto,
        ),
        1,
        2,
    )
    .unwrap();
    assert_eq!(output.1, Path::Ir);
    assert!(String::from_utf8_lossy(&output.0).contains("step.start"));
    let back = run(
        &output.0,
        config(
            Protocol::GeminiInteractions,
            Protocol::OpenAiChat,
            Strategy::Auto,
        ),
        3,
        5,
    )
    .unwrap();
    assert_eq!(chat_text(&back.0), "hello");
}

#[test]
fn responses_final_snapshots_are_not_appended_again() {
    let responses = run(
        CHAT,
        config(
            Protocol::OpenAiChat,
            Protocol::OpenAiResponses,
            Strategy::Direct,
        ),
        1,
        43,
    )
    .unwrap()
    .0;
    let result = run(
        &responses,
        config(
            Protocol::OpenAiResponses,
            Protocol::OpenAiChat,
            Strategy::Direct,
        ),
        1,
        1,
    )
    .unwrap();
    assert_eq!(chat_text(&result.0), "Hi 中 🌍");
    assert!(String::from_utf8_lossy(&responses).contains("\"text\":\"Hi 中 🌍\""));
}

#[test]
fn anthropic_usage_is_cumulative_not_summed() {
    let wire = concat!(
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"r1\",\"model\":\"m\",\"role\":\"assistant\",\"content\":[],\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"x\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":null},\"usage\":{\"output_tokens\":2}}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n").as_bytes();
    let result = run(
        wire,
        config(
            Protocol::AnthropicMessages,
            Protocol::OpenAiChat,
            Strategy::Direct,
        ),
        1,
        1,
    )
    .unwrap();
    assert_eq!(result.3.output, Some(7));
    assert_eq!(result.3.total, Some(12));
}

#[test]
fn truncated_duplicate_terminal_and_late_unsupported_fail_without_replay() {
    let truncated = &CHAT[..CHAT.len() - 14];
    assert_eq!(
        run(
            truncated,
            config(
                Protocol::OpenAiChat,
                Protocol::OpenAiResponses,
                Strategy::Auto
            ),
            7,
            17
        )
        .unwrap_err()
        .0,
        Error::TruncatedStream
    );
    let mut duplicate = CHAT.to_vec();
    duplicate.extend_from_slice(b"data: [DONE]\n\n");
    assert_eq!(
        run(
            &duplicate,
            config(
                Protocol::OpenAiChat,
                Protocol::OpenAiResponses,
                Strategy::Auto
            ),
            duplicate.len(),
            8192
        )
        .unwrap_err()
        .0,
        Error::AlreadyFinished
    );
    let mut bad = CHAT[..CHAT.windows(2).position(|value| value == b"\n\n").unwrap() + 2].to_vec();
    bad.extend_from_slice(
        b"data: {\"choices\":[{\"index\":0,\"delta\":{\"audio\":{}},\"finish_reason\":null}]}\n\n",
    );
    let result = run(
        &bad,
        config(
            Protocol::OpenAiChat,
            Protocol::OpenAiResponses,
            Strategy::Auto,
        ),
        7,
        11,
    )
    .unwrap_err();
    assert!(matches!(result.0, Error::Unsupported { .. }));
    assert_eq!(
        String::from_utf8_lossy(&result.1)
            .matches("response.created")
            .count(),
        2
    ); // SSE name + JSON type, once.
}

#[test]
fn initial_context_and_direct_capability_are_explicit() {
    let error = run(
        CHAT,
        StreamConfig::new(Protocol::OpenAiChat, Protocol::AnthropicMessages),
        1,
        1,
    )
    .unwrap_err()
    .0;
    assert_eq!(
        error,
        Error::ContextRequired {
            field: "initial_usage.input"
        }
    );
    let error = run(
        CHAT,
        config(
            Protocol::OpenAiChat,
            Protocol::GeminiInteractions,
            Strategy::Direct,
        ),
        1,
        1,
    )
    .unwrap_err()
    .0;
    assert!(matches!(error, Error::DirectUnavailable { .. }));
}

#[test]
fn interleaved_tool_arguments_keep_ids_and_do_not_duplicate_snapshots() {
    for target in [
        Protocol::OpenAiResponses,
        Protocol::AnthropicMessages,
        Protocol::GeminiGenerateContent,
        Protocol::GeminiInteractions,
    ] {
        let forward = run(
            TOOLS,
            config(Protocol::OpenAiChat, target, Strategy::Auto),
            1,
            1,
        )
        .unwrap();
        let ir = run(
            TOOLS,
            config(Protocol::OpenAiChat, target, Strategy::Ir),
            19,
            53,
        )
        .unwrap();
        assert_eq!(forward.0, ir.0);
        let reverse = run(
            &forward.0,
            config(target, Protocol::OpenAiChat, Strategy::Auto),
            1,
            7,
        )
        .unwrap();
        let mut arguments = [String::new(), String::new()];
        let mut ids = [String::new(), String::new()];
        for payload in json_events(&reverse.0) {
            let mut tape = vec![Token::EMPTY; 2048];
            let document = json::parse(&payload, &mut tape, json::Limits::default()).unwrap();
            for choice in document.root().get("choices").unwrap().array() {
                if let Some(calls) = choice
                    .get("delta")
                    .and_then(|delta| delta.get("tool_calls"))
                {
                    for call in calls.array() {
                        let index = call.get("index").unwrap().as_u64().unwrap() as usize;
                        if let Some(id) = call.get("id").and_then(|value| value.as_string()) {
                            ids[index] = String::from_utf8(id.bytes().collect()).unwrap();
                        }
                        if let Some(text) = call
                            .get("function")
                            .and_then(|value| value.get("arguments"))
                            .and_then(|value| value.as_string())
                        {
                            arguments[index]
                                .push_str(&String::from_utf8(text.bytes().collect()).unwrap());
                        }
                    }
                }
            }
        }
        assert_eq!(ids, ["a", "b"], "{target:?}");
        assert_eq!(arguments, ["{\"x\":1}", "{\"y\":2}"], "{target:?}");
    }
}

#[test]
fn all_workspaces_grow_without_losing_consumed_input() {
    let mut framing = [];
    let mut data = [];
    let mut pending = [];
    let mut tokens = [];
    let mut items = [];
    let mut checkpoints = [];
    let mut events = [];
    let mut aggregate = [];
    let mut large_framing = vec![0; 8192];
    let mut large_data = vec![0; 8192];
    let mut large_pending = vec![0; 8192];
    let mut large_tokens = vec![Token::EMPTY; 2048];
    let mut large_items = vec![ActiveItem::EMPTY; 16];
    let mut large_checkpoints = vec![ActiveItem::EMPTY; 16];
    let mut large_events = vec![Event::EMPTY; 64];
    let mut large_aggregate = vec![0; 8192];
    let mut fr = Some(large_framing.as_mut_slice());
    let mut da = Some(large_data.as_mut_slice());
    let mut pe = Some(large_pending.as_mut_slice());
    let mut to = Some(large_tokens.as_mut_slice());
    let mut it = Some(large_items.as_mut_slice());
    let mut ch = Some(large_checkpoints.as_mut_slice());
    let mut ev = Some(large_events.as_mut_slice());
    let mut ag = Some(large_aggregate.as_mut_slice());
    let mut converter = StreamConverter::new(
        config(
            Protocol::OpenAiChat,
            Protocol::OpenAiResponses,
            Strategy::Ir,
        ),
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
    let mut result = Vec::new();
    let mut position = 0;
    let mut output = [0; 3];
    let mut seen = Vec::new();
    for _ in 0..100000 {
        let step = converter.step(&TOOLS[position..], &mut output, true);
        position += step.consumed;
        result.extend_from_slice(&output[..step.written]);
        match step.status {
            Status::NeedBuffer { buffer, .. } => {
                seen.push(buffer);
                match buffer {
                    BufferKind::Framing => {
                        converter
                            .replace_buffer(buffer, fr.take().unwrap())
                            .unwrap();
                    }
                    BufferKind::Data => {
                        converter
                            .replace_buffer(buffer, da.take().unwrap())
                            .unwrap();
                    }
                    BufferKind::Pending => {
                        converter
                            .replace_buffer(buffer, pe.take().unwrap())
                            .unwrap();
                    }
                    BufferKind::Aggregate => {
                        converter
                            .replace_buffer(buffer, ag.take().unwrap())
                            .unwrap();
                    }
                    BufferKind::Tokens => {
                        converter.replace_tokens(to.take().unwrap());
                    }
                    BufferKind::Items => converter
                        .replace_items(it.take().unwrap(), ch.take().unwrap())
                        .unwrap(),
                    BufferKind::Events => {
                        converter.replace_events(ev.take().unwrap());
                    }
                }
            }
            Status::Done => break,
            Status::Failed(error) => panic!("{error:?}"),
            _ => {}
        }
    }
    assert_eq!(position, TOOLS.len());
    assert_eq!(seen.len(), 7);
    assert_eq!(
        result,
        run(
            TOOLS,
            config(
                Protocol::OpenAiChat,
                Protocol::OpenAiResponses,
                Strategy::Ir
            ),
            TOOLS.len(),
            65536
        )
        .unwrap()
        .0
    );
}

#[test]
fn snapshot_mismatch_and_duplicate_lifecycle_are_rejected() {
    let response = run(
        CHAT,
        config(
            Protocol::OpenAiChat,
            Protocol::OpenAiResponses,
            Strategy::Direct,
        ),
        1,
        53,
    )
    .unwrap()
    .0;
    let modified = String::from_utf8(response)
        .unwrap()
        .replace("\"text\":\"Hi 中 🌍\"", "\"text\":\"Hi X 🌍\"");
    let mut strict = config(
        Protocol::OpenAiResponses,
        Protocol::OpenAiChat,
        Strategy::Direct,
    );
    strict.validate_snapshots = true;
    let result = run(modified.as_bytes(), strict, 3, 7).unwrap_err();
    assert!(matches!(
        result.0,
        Error::InvalidInput {
            field: "final snapshot",
            ..
        }
    ));
    let anthropic = run(
        CHAT,
        config(
            Protocol::OpenAiChat,
            Protocol::AnthropicMessages,
            Strategy::Direct,
        ),
        1,
        53,
    )
    .unwrap()
    .0;
    let end = anthropic
        .windows(2)
        .position(|bytes| bytes == b"\n\n")
        .unwrap()
        + 2;
    let mut duplicate = anthropic[..end].to_vec();
    duplicate.extend_from_slice(&anthropic);
    assert!(matches!(
        run(
            &duplicate,
            config(
                Protocol::AnthropicMessages,
                Protocol::OpenAiChat,
                Strategy::Direct
            ),
            3,
            7
        )
        .unwrap_err()
        .0,
        Error::InvalidInput {
            field: "stream start",
            ..
        }
    ));
}

#[test]
fn multi_choice_indices_survive_and_single_choice_targets_reject() {
    let input = concat!(
        "data: {\"id\":\"r1\",\"model\":\"m\",\"created\":7,\"choices\":[{\"index\":2,\"delta\":{\"content\":\"a\"},\"finish_reason\":null},{\"index\":4,\"delta\":{\"content\":\"b\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":4,\"delta\":{},\"finish_reason\":\"stop\"},{\"index\":2,\"delta\":{},\"finish_reason\":\"length\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":3,\"total_tokens\":8}}\n\ndata: [DONE]\n\n"
    ).as_bytes();
    let google = run(
        input,
        config(
            Protocol::OpenAiChat,
            Protocol::GeminiGenerateContent,
            Strategy::Direct,
        ),
        1,
        1,
    )
    .unwrap();
    let back = run(
        &google.0,
        config(
            Protocol::GeminiGenerateContent,
            Protocol::OpenAiChat,
            Strategy::Direct,
        ),
        1,
        1,
    )
    .unwrap();
    let text = String::from_utf8(back.0).unwrap();
    assert!(text.contains("\"index\":2"));
    assert!(text.contains("\"index\":4"));
    assert!(!text.contains("\"index\":0"));
    assert!(matches!(
        run(
            input,
            config(
                Protocol::OpenAiChat,
                Protocol::AnthropicMessages,
                Strategy::Direct
            ),
            1,
            1
        )
        .unwrap_err()
        .0,
        Error::Unsupported {
            feature: "multiple stream choices"
        }
    ));
}

#[test]
fn errors_are_streamed_and_malformed_arguments_are_never_committed_as_objects() {
    let source = b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"busy\"}}\n\n";
    for target in [
        Protocol::OpenAiChat,
        Protocol::OpenAiResponses,
        Protocol::GeminiInteractions,
        Protocol::GeminiGenerateContent,
    ] {
        let converted = run(
            source,
            config(Protocol::AnthropicMessages, target, Strategy::Auto),
            1,
            1,
        )
        .unwrap();
        assert!(String::from_utf8_lossy(&converted.0).contains("busy"));
        json_events(&converted.0);
    }
    let malformed = String::from_utf8(TOOLS.to_vec())
        .unwrap()
        .replace("1}", "x}");
    let failed = run(
        malformed.as_bytes(),
        config(
            Protocol::OpenAiChat,
            Protocol::GeminiGenerateContent,
            Strategy::Direct,
        ),
        1,
        1,
    )
    .unwrap_err();
    assert!(matches!(failed.0, Error::InvalidJson { .. }));
    assert!(!String::from_utf8_lossy(&failed.1).contains("\"functionCall\""));
}

#[test]
fn event_ir_can_be_edited_and_out_of_bounds_spans_are_rejected() {
    let payload = b"{\"id\":\"r1\",\"model\":\"m\",\"created\":7,\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}";
    let mut tokens = [Token::EMPTY; 128];
    let mut events = [Event::EMPTY; 16];
    let count = decode_event(
        Protocol::OpenAiChat,
        payload,
        &mut tokens,
        &mut events,
        json::Limits::default(),
    )
    .unwrap();
    let delta = events[..count]
        .iter_mut()
        .find(|event| event.kind == EventKind::Delta)
        .unwrap();
    delta.a.quoted = false;
    delta.a.offset += 1;
    delta.a.length = 2; // Borrow "he" from the original text.
    let mut framing = [0; 512];
    let mut data = [0; 512];
    let mut pending = [0; 4096];
    let mut items = [ActiveItem::EMPTY; 8];
    let mut checkpoints = [ActiveItem::EMPTY; 8];
    let mut tape = [Event::EMPTY; 16];
    let mut aggregate = [0; 4096];
    let mut converter = StreamConverter::new(
        config(
            Protocol::OpenAiChat,
            Protocol::OpenAiResponses,
            Strategy::Ir,
        ),
        StreamBuffers {
            framing: &mut framing,
            data: &mut data,
            pending: &mut pending,
            tokens: &mut tokens,
            items: &mut items,
            checkpoints: &mut checkpoints,
            events: &mut tape,
            aggregate: &mut aggregate,
        },
    )
    .unwrap();
    converter.stage_events(payload, &events[..count]).unwrap();
    let mut output = [0; 4096];
    let step = converter.step(&[], &mut output, false);
    assert!(String::from_utf8_lossy(&output[..step.written]).contains("\"delta\":\"he\""));
    let mut bad = Event {
        kind: EventKind::Delta,
        ..Event::EMPTY
    };
    bad.a = EventText {
        offset: usize::MAX,
        length: 1,
        quoted: false,
        present: true,
    };
    assert!(matches!(
        converter.stage_events(payload, &[bad]),
        Err(Status::Failed(Error::InvalidInput {
            field: "event IR span",
            ..
        }))
    ));
}

#[test]
fn response_to_chat_text_stream_does_not_accumulate_the_answer() {
    let small_response = run(
        CHAT,
        config(
            Protocol::OpenAiChat,
            Protocol::OpenAiResponses,
            Strategy::Auto,
        ),
        13,
        65536,
    )
    .unwrap()
    .0;
    let large_chat = String::from_utf8(CHAT.to_vec())
        .unwrap()
        .replace("Hi \\u4e2d", &"x".repeat(4096));
    let large_response = run(
        large_chat.as_bytes(),
        config(
            Protocol::OpenAiChat,
            Protocol::OpenAiResponses,
            Strategy::Auto,
        ),
        37,
        65536,
    )
    .unwrap()
    .0;
    let base = run(
        &small_response,
        config(
            Protocol::OpenAiResponses,
            Protocol::OpenAiChat,
            Strategy::Auto,
        ),
        1,
        17,
    )
    .unwrap()
    .4;
    let bounded = run(
        &large_response,
        config(
            Protocol::OpenAiResponses,
            Protocol::OpenAiChat,
            Strategy::Auto,
        ),
        1,
        17,
    )
    .unwrap()
    .4;
    assert_eq!(bounded.aggregate_used, base.aggregate_used);
    let mut exact = config(
        Protocol::OpenAiResponses,
        Protocol::OpenAiChat,
        Strategy::Auto,
    );
    exact.validate_snapshots = true;
    let checked = run(&large_response, exact, 1, 17).unwrap().4;
    assert!(checked.aggregate_used > bounded.aggregate_used + 4096);
}

#[test]
fn invalid_event_names_and_duplicate_keys_are_errors_in_auto() {
    let wrong = b"event: content_block_delta\ndata: {\"type\":\"message_stop\"}\n\n";
    assert!(matches!(
        run(
            wrong,
            config(
                Protocol::AnthropicMessages,
                Protocol::OpenAiChat,
                Strategy::Auto
            ),
            1,
            1
        )
        .unwrap_err()
        .0,
        Error::InvalidInput {
            field: "SSE event name",
            ..
        }
    ));
    let duplicate = b"data: {\"choices\":[],\"\\u0063hoices\":[]}\n\n";
    assert!(matches!(
        run(
            duplicate,
            config(
                Protocol::OpenAiChat,
                Protocol::OpenAiResponses,
                Strategy::Auto
            ),
            1,
            1
        )
        .unwrap_err()
        .0,
        Error::DuplicateKey { .. }
    ));
}

#[test]
fn anthropic_cached_token_translation_is_reversible_without_double_counting() {
    let cached = String::from_utf8(CHAT.to_vec()).unwrap().replace(
        "\"total_tokens\":8",
        "\"total_tokens\":8,\"prompt_tokens_details\":{\"cached_tokens\":2}",
    );
    let anthropic = run(
        cached.as_bytes(),
        config(
            Protocol::OpenAiChat,
            Protocol::AnthropicMessages,
            Strategy::Auto,
        ),
        1,
        1,
    )
    .unwrap();
    let back = run(
        &anthropic.0,
        config(
            Protocol::AnthropicMessages,
            Protocol::OpenAiChat,
            Strategy::Auto,
        ),
        1,
        1,
    )
    .unwrap();
    assert_eq!(
        back.3,
        Usage {
            input: Some(5),
            output: Some(3),
            total: Some(8),
            cached: Some(2),
            reasoning: None
        }
    );
}

#[test]
fn cancellations_are_explicit_in_event_ir() {
    let payload = br#"{"event_type":"interaction.completed","interaction":{"id":"r1","status":"cancelled","usage":{"total_input_tokens":5,"total_output_tokens":0,"total_tokens":5}}}"#;
    let mut tokens = [Token::EMPTY; 128];
    let mut events = [Event::EMPTY; 16];
    let count = decode_event(
        Protocol::GeminiInteractions,
        payload,
        &mut tokens,
        &mut events,
        json::Limits::default(),
    )
    .unwrap();
    assert_eq!(events[count - 1].kind, EventKind::Finish);
    assert_eq!(events[count - 1].completion, Completion::Cancelled);
}

#[test]
fn arbitrary_bytes_are_bounded_and_never_panic() {
    let mut state = 0x8f03_c001u32;
    for length in 0..256 {
        let mut bytes = Vec::from(b"data: ".as_slice());
        for _ in 0..length {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            bytes.push(state as u8);
        }
        bytes.extend_from_slice(b"\n\n");
        for source in [
            Protocol::OpenAiCompletions,
            Protocol::OpenAiChat,
            Protocol::OpenAiResponses,
            Protocol::AnthropicMessages,
            Protocol::GeminiGenerateContent,
            Protocol::GeminiInteractions,
        ] {
            let _ = run(
                &bytes,
                config(source, Protocol::OpenAiChat, Strategy::Auto),
                1,
                7,
            );
        }
    }
}
