use zconv::json::{self, Token};
use zconv::stream::*;
use zconv::{Error, Path, Protocol, Strategy};

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

type RunResult = Result<(Vec<u8>, Path, Report, Usage, Memory), (Error, Vec<u8>)>;

fn run(input: &[u8], config: StreamConfig<'_>, split: usize, output_size: usize) -> RunResult {
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
                ));
            }
            Status::Failed(error) => return Err((error, result)),
            Status::NeedBuffer { buffer, needed, .. } => {
                panic!("insufficient {buffer:?}: {needed}")
            }
            _ => {}
        }
    }
}

const CONTINUOUS: &str = concat!(
    "data: {\"id\":\"r1\",\"model\":\"m\",\"created\":7,\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"x\"},\"finish_reason\":null}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1,\"total_tokens\":6}}\n\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"y\"},\"finish_reason\":null}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2,\"total_tokens\":7}}\n\n",
    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":7,\"total_tokens\":12}}\n\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: [DONE]\n\n"
);

fn usage_values(wire: &[u8]) -> Vec<Option<u64>> {
    let mut values = Vec::new();
    for line in wire.split(|b| *b == b'\n') {
        let Some(payload) = line.strip_prefix(b"data: ") else {
            continue;
        };
        if payload == b"[DONE]" {
            continue;
        }
        let mut tokens = [Token::EMPTY; 512];
        let document = json::parse(payload, &mut tokens, json::Limits::default()).unwrap();
        if let Some(usage) = document.root().get("usage") {
            values.push(usage.get("completion_tokens").and_then(|v| v.as_u64()));
        }
    }
    values
}

#[test]
fn continuous_usage_is_emitted_during_direct_and_ir_streams() {
    let mut baseline = None;
    for strategy in [Strategy::Direct, Strategy::Ir] {
        for split in [1, 7, CONTINUOUS.len()] {
            let mut config = config(Protocol::OpenAiChat, Protocol::OpenAiChat, strategy);
            config.usage_emission = UsageEmission::Continuous;
            let result = run(CONTINUOUS.as_bytes(), config, split, 1).unwrap();
            let values = usage_values(&result.0);
            assert_eq!(values, [Some(1), Some(1), Some(2), Some(7), Some(7)]);
            assert_eq!(result.3.output, Some(7));
            assert_eq!(result.3.total, Some(12));
            if let Some(ref baseline) = baseline {
                assert_eq!(&result.0, baseline);
            } else {
                baseline = Some(result.0);
            }
        }
    }
    let final_only = run(
        CONTINUOUS.as_bytes(),
        config(Protocol::OpenAiChat, Protocol::OpenAiChat, Strategy::Direct),
        7,
        31,
    )
    .unwrap();
    assert_eq!(usage_values(&final_only.0), [Some(7)]);
}

#[test]
fn continuous_usage_does_not_invent_missing_counters() {
    let unknown = CONTINUOUS
        .replace(
            "\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1,\"total_tokens\":6}",
            "\"usage\":null",
        )
        .replace(
            "\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2,\"total_tokens\":7}",
            "\"usage\":null",
        )
        .replace(
            "\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":7,\"total_tokens\":12}",
            "\"usage\":null",
        );
    let mut config = config(Protocol::OpenAiChat, Protocol::OpenAiChat, Strategy::Ir);
    config.context.initial_usage = Usage::default();
    config.usage_emission = UsageEmission::Continuous;
    let result = run(unknown.as_bytes(), config, 1, 1).unwrap();
    assert!(usage_values(&result.0).is_empty());
    assert_eq!(result.3, Usage::default());
    let partial = CONTINUOUS
        .replace("\"prompt_tokens\":5,", "")
        .replace(",\"total_tokens\":6", "")
        .replace(",\"total_tokens\":7", "")
        .replace(",\"total_tokens\":12", "");
    config.allow_partial_usage = true;
    let result = run(partial.as_bytes(), config, 3, 1).unwrap();
    assert_eq!(result.3.input, None);
    assert_eq!(result.3.output, Some(7));
    assert_eq!(result.3.total, None);
    let output = String::from_utf8(result.0).unwrap();
    assert!(!output.contains("prompt_tokens"));
    assert!(!output.contains("total_tokens"));
}

#[test]
fn native_targets_receive_continuous_usage_updates() {
    for target in [Protocol::AnthropicMessages, Protocol::GeminiGenerateContent] {
        let mut config = config(Protocol::OpenAiChat, target, Strategy::Ir);
        config.usage_emission = UsageEmission::Continuous;
        let result = run(CONTINUOUS.as_bytes(), config, 1, 1).unwrap();
        let wire = String::from_utf8(result.0).unwrap();
        let key = if target == Protocol::AnthropicMessages {
            "output_tokens"
        } else {
            "candidatesTokenCount"
        };
        for count in [1, 2, 7] {
            assert!(wire.contains(&format!("\"{key}\":{count}")));
        }
    }
    for target in [Protocol::OpenAiResponses, Protocol::GeminiInteractions] {
        let mut config = config(Protocol::OpenAiChat, target, Strategy::Ir);
        config.usage_emission = UsageEmission::Continuous;
        assert!(matches!(
            EventEncoderState::new(config),
            Err(Error::Unsupported {
                feature: "continuous usage in target protocol"
            })
        ));
    }
}

#[test]
fn object_tool_arguments_are_normalized_without_losing_json() {
    let input = concat!(
        "data: {\"id\":\"r1\",\"model\":\"m\",\"created\":7,\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c\",\"type\":\"function\",\"function\":{\"name\":\"f\",\"arguments\":{\"x\":1,\"text\":\"a\\\"b\"}}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let mut expected = None;
    for strategy in [Strategy::Direct, Strategy::Ir] {
        let result = run(
            input.as_bytes(),
            config(Protocol::OpenAiChat, Protocol::OpenAiChat, strategy),
            1,
            1,
        )
        .unwrap();
        let wire = String::from_utf8(result.0).unwrap();
        assert!(wire.contains("\\\"x\\\":1"));
        assert!(wire.contains("\\\"text\\\":\\\"a\\\\\\\"b\\\""));
        if let Some(ref expected) = expected {
            assert_eq!(&wire, expected);
        } else {
            expected = Some(wire);
        }
    }
}

#[test]
fn continuous_owned_encoder_rolls_back_output_capacity_failures() {
    let mut config = config(Protocol::OpenAiChat, Protocol::OpenAiChat, Strategy::Ir);
    config.usage_emission = UsageEmission::Continuous;
    config.context.initial_usage = Usage::default();
    let mut encoder = EventEncoderState::new(config).unwrap();
    let mut tokens = [Token::EMPTY; 256];
    let mut items = [ActiveItem::EMPTY; 8];
    let mut checkpoints = [ActiveItem::EMPTY; 8];
    let mut aggregate = [0; 1024];
    let mut output = [0; 2048];
    let events = [
        Event {
            kind: EventKind::Begin,
            ..Event::EMPTY
        },
        Event {
            kind: EventKind::Usage,
            usage: Usage {
                input: Some(5),
                output: Some(1),
                total: Some(6),
                ..Usage::default()
            },
            ..Event::EMPTY
        },
    ];
    let before = encoder.usage();
    let error = encoder
        .encode(
            config.context,
            b"",
            &events,
            EventEncoderBuffers {
                tokens: &mut tokens,
                items: &mut items,
                checkpoints: &mut checkpoints,
                aggregate: &mut aggregate,
            },
            &mut [],
        )
        .unwrap_err();
    assert!(matches!(error, Status::NeedBuffer { .. }));
    assert_eq!(encoder.usage(), before);
    let written = encoder
        .encode(
            config.context,
            b"",
            &events,
            EventEncoderBuffers {
                tokens: &mut tokens,
                items: &mut items,
                checkpoints: &mut checkpoints,
                aggregate: &mut aggregate,
            },
            &mut output,
        )
        .unwrap();
    assert_eq!(usage_values(&output[..written]), [Some(1)]);
    for count in [2, 7] {
        let update = [Event {
            kind: EventKind::Usage,
            usage: Usage {
                input: Some(5),
                output: Some(count),
                total: Some(5 + count),
                ..Usage::default()
            },
            ..Event::EMPTY
        }];
        let saved = encoder.usage();
        assert!(
            encoder
                .encode(
                    config.context,
                    b"",
                    &update,
                    EventEncoderBuffers {
                        tokens: &mut tokens,
                        items: &mut items,
                        checkpoints: &mut checkpoints,
                        aggregate: &mut aggregate
                    },
                    &mut []
                )
                .is_err()
        );
        assert_eq!(encoder.usage(), saved);
        let written = encoder
            .encode(
                config.context,
                b"",
                &update,
                EventEncoderBuffers {
                    tokens: &mut tokens,
                    items: &mut items,
                    checkpoints: &mut checkpoints,
                    aggregate: &mut aggregate,
                },
                &mut output,
            )
            .unwrap();
        assert_eq!(usage_values(&output[..written]), [Some(count)]);
    }
}

const ANTHROPIC_PROVIDER: &str = concat!(
    "data: {\"type\":\"message_start\",\"message\":{\"id\":\"r1\",\"model\":\"m\",\"role\":\"assistant\",\"type\":\"message\",\"content\":[],\"usage\":{\"input_tokens\":5,\"output_tokens\":0,\"service_tier\":\"standard\",\"inference_geo\":\"us\"}}}\n\n",
    "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"redacted_thinking\",\"data\":\"opaque-signed-by-provider\"}}\n\n",
    "data: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
    "data: {\"type\":\"content_block_start\",\"index\":7,\"content_block\":{\"type\":\"server_tool_use\",\"id\":\"srv_1\",\"name\":\"web_search\",\"input\":{}}}\n\n",
    "data: {\"type\":\"content_block_delta\",\"index\":7,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"query\\\":\\\"test\\\"}\"}}\n\n",
    "data: {\"type\":\"content_block_stop\",\"index\":7}\n\n",
    "data: {\"type\":\"content_block_start\",\"index\":12,\"content_block\":{\"type\":\"web_search_tool_result\",\"tool_use_id\":\"srv_1\",\"content\":[{\"type\":\"web_search_result\",\"url\":\"https://example.test\",\"title\":\"result\",\"encrypted_content\":\"opaque\"}]}}\n\n",
    "data: {\"type\":\"content_block_stop\",\"index\":12}\n\n",
    "data: {\"type\":\"content_block_start\",\"index\":42,\"content_block\":{\"type\":\"text\",\"text\":\"answer\",\"citations\":[{\"type\":\"char_location\",\"cited_text\":\"source\",\"document_index\":0,\"start_char_index\":0,\"end_char_index\":6}]}}\n\n",
    "data: {\"type\":\"content_block_delta\",\"index\":42,\"delta\":{\"type\":\"text_delta\",\"text\":\" continued\"}}\n\n",
    "data: {\"type\":\"content_block_delta\",\"index\":42,\"delta\":{\"type\":\"citations_delta\",\"citation\":{\"type\":\"char_location\",\"cited_text\":\"other\",\"document_index\":0,\"start_char_index\":7,\"end_char_index\":12}}}\n\n",
    "data: {\"type\":\"content_block_stop\",\"index\":42}\n\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":7,\"server_tool_use\":{\"web_search_requests\":1},\"iterations\":[{\"type\":\"message\",\"input_tokens\":5,\"output_tokens\":7}]}}\n\n",
    "data: {\"type\":\"message_stop\"}\n\n"
);

#[test]
fn anthropic_provider_blocks_citations_and_usage_round_trip() {
    let mut expected = None;
    for strategy in [Strategy::Direct, Strategy::Ir] {
        let result = run(
            ANTHROPIC_PROVIDER.as_bytes(),
            config(
                Protocol::AnthropicMessages,
                Protocol::AnthropicMessages,
                strategy,
            ),
            1,
            1,
        )
        .unwrap();
        let wire = String::from_utf8(result.0).unwrap();
        for field in [
            "opaque-signed-by-provider",
            "server_tool_use",
            "web_search_tool_result",
            "\"citations\":[",
            "citations_delta",
            "\"web_search_requests\":1",
            "\"service_tier\":\"standard\"",
            "\"inference_geo\":\"us\"",
            "\"iterations\":[",
        ] {
            assert!(wire.contains(field), "missing {field}");
        }
        for line in wire.lines().filter_map(|line| line.strip_prefix("data: ")) {
            let mut tokens = [Token::EMPTY; 512];
            json::parse(line.as_bytes(), &mut tokens, json::Limits::default()).unwrap();
        }
        assert_eq!(result.3.output, Some(7));
        assert_eq!(result.3.total, Some(12));
        if let Some(ref expected) = expected {
            assert_eq!(&wire, expected);
        } else {
            expected = Some(wire);
        }
    }
}

#[test]
fn provider_owned_stream_data_does_not_cross_namespaces() {
    let error = run(
        ANTHROPIC_PROVIDER.as_bytes(),
        config(
            Protocol::AnthropicMessages,
            Protocol::OpenAiChat,
            Strategy::Ir,
        ),
        1,
        1,
    )
    .unwrap_err()
    .0;
    assert!(matches!(
        error,
        Error::Unsupported {
            feature: "provider-owned Anthropic stream data in target"
        }
    ));
}

#[test]
fn provider_blocks_and_usage_enforce_shape_and_lifecycle() {
    let late_delta = ANTHROPIC_PROVIDER.replace(
        "\"type\":\"content_block_stop\",\"index\":2",
        "\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"text_delta\",\"text\":\"invalid\"}",
    );
    assert!(matches!(
        run(
            late_delta.as_bytes(),
            config(
                Protocol::AnthropicMessages,
                Protocol::AnthropicMessages,
                Strategy::Ir
            ),
            1,
            1
        ),
        Err((Error::InvalidInput { .. }, _))
    ));
    for invalid in [
        ANTHROPIC_PROVIDER.replace("\"web_search_requests\":1", "\"web_search_requests\":-1"),
        ANTHROPIC_PROVIDER.replace(
            "\"type\":\"web_search_tool_result\",\"tool_use_id\":\"srv_1\"",
            "\"type\":\"web_search_tool_result\",\"tool_use_id\":\"srv_1\",\"is_error\":\"true\"",
        ),
        ANTHROPIC_PROVIDER.replace(
            "\"type\":\"citations_delta\",\"citation\":{\"type\":\"char_location\"",
            "\"type\":\"citations_delta\",\"citation\":{\"type\":null",
        ),
    ] {
        assert!(matches!(
            run(
                invalid.as_bytes(),
                config(
                    Protocol::AnthropicMessages,
                    Protocol::AnthropicMessages,
                    Strategy::Direct
                ),
                7,
                13
            ),
            Err((Error::InvalidInput { .. }, _))
        ));
    }
}
