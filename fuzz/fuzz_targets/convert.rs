#![no_main]
use libfuzzer_sys::fuzz_target;
use zconv::{
    Format, Operation, Options, Protocol, Strategy, Workspace, convert, ir::Record, json::Token,
};

const PROTOCOLS: [Protocol; 6] = [
    Protocol::OpenAiCompletions,
    Protocol::OpenAiChat,
    Protocol::OpenAiResponses,
    Protocol::AnthropicMessages,
    Protocol::GeminiGenerateContent,
    Protocol::GeminiInteractions,
];

fuzz_target!(|data: &[u8]| {
    if data.len() < 3 {
        return;
    }
    let mut tokens = [Token::EMPTY; 256];
    let mut nested = [Token::EMPTY; 64];
    let mut records = [Record::EMPTY; 64];
    let mut output = [0; 4096];
    let mut scratch = [0; 1024];
    let source = Format::new(PROTOCOLS[data[0] as usize % 6]);
    let target = Format::new(PROTOCOLS[data[1] as usize % 6]);
    let options = Options {
        strategy: match data[2] % 3 {
            0 => Strategy::Auto,
            1 => Strategy::Direct,
            _ => Strategy::Ir,
        },
        ..Options::default()
    };
    let operation = if data[2] & 4 == 0 {
        Operation::Request
    } else {
        Operation::Response
    };
    let _ = convert(
        source,
        target,
        operation,
        &data[3..],
        &mut output,
        options,
        Workspace {
            tokens: &mut tokens,
            records: &mut records,
            scratch: &mut scratch,
            nested_tokens: &mut nested,
        },
    );
});
