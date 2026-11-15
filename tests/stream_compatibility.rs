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
