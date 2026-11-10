#![no_main]
use libfuzzer_sys::fuzz_target;
use zconv::json::Token;
use zconv::stream::*;
use zconv::{Protocol, Strategy};

const PROTOCOLS: [Protocol; 6] = [
    Protocol::OpenAiCompletions,
    Protocol::OpenAiChat,
    Protocol::OpenAiResponses,
    Protocol::AnthropicMessages,
    Protocol::GeminiGenerateContent,
    Protocol::GeminiInteractions,
];

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 || data.len() > 16_384 {
        return;
    }
    let mut framing = [0; 1024];
    let mut decoded = [0; 1024];
    let mut pending = [0; 4096];
    let mut tokens = [Token::EMPTY; 128];
    let mut items = [ActiveItem::EMPTY; 8];
    let mut checkpoints = [ActiveItem::EMPTY; 8];
    let mut events = [Event::EMPTY; 16];
    let mut aggregate = [0; 4096];
    let mut output = [0; 31];
    let mut config = StreamConfig::new(
        PROTOCOLS[data[0] as usize % 6],
        PROTOCOLS[data[1] as usize % 6],
    );
    config.strategy = match data[2] % 3 {
        0 => Strategy::Auto,
        1 => Strategy::Direct,
        _ => Strategy::Ir,
    };
    config.context.response_id = Some("r");
    config.context.model = Some("m");
    config.context.created = Some(1);
    config.context.item_id_prefix = Some("local_");
    config.context.initial_usage.input = Some(0);
    let Ok(mut stream) = StreamConverter::new(
        config,
        StreamBuffers {
            framing: &mut framing,
            data: &mut decoded,
            pending: &mut pending,
            tokens: &mut tokens,
            items: &mut items,
            checkpoints: &mut checkpoints,
            events: &mut events,
            aggregate: &mut aggregate,
        },
    ) else {
        return;
    };
    let bytes = &data[4..];
    let chunk = usize::from(data[3]).max(1);
    let mut position = 0usize;
    for _ in 0..bytes.len().saturating_mul(4).saturating_add(512) {
        let end = position.saturating_add(chunk).min(bytes.len());
        let step = stream.step(&bytes[position..end], &mut output, end == bytes.len());
        assert!(step.consumed <= end - position);
        assert!(step.written <= output.len());
        position += step.consumed;
        if matches!(
            step.status,
            Status::Done | Status::Failed(_) | Status::NeedBuffer { .. }
        ) {
            break;
        }
    }
});
