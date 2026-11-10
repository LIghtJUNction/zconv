//! Stable-Rust benchmark; allocations happen outside the timed conversion loop.
use std::{hint::black_box, time::Instant};
use zconv::ir::{Context, Policy, Record};
use zconv::json::Token;
use zconv::{convert, Operation, Options, Protocol, Strategy, Workspace};

fn main() {
    let quick = !std::env::args().any(|arg| arg == "--full");
    let iterations = if quick { 50 } else { 10_000 };
    let rounds = if quick { 3 } else { 15 };
    println!("case,strategy,input_bytes,output_bytes,median_ns,p95_ns,mib_per_second,token_bytes_reserved,ir_bytes_reserved,ir_records_used,scratch_bytes_reserved,nested_token_bytes_reserved");
    let small = br#"{"model":"test-model","messages":[{"role":"user","content":"hello"}],"max_completion_tokens":17,"temperature":0,"stream":false}"#.to_vec();
    let unicode = "{\"model\":\"test-model\",\"messages\":[{\"role\":\"user\",\"content\":\"你好，世界。Δοκιμή 😀 \\\"quoted\\\" \\u4e2d\"}],\"max_completion_tokens\":17}".as_bytes().to_vec();
    let long = format!("{{\"model\":\"test-model\",\"messages\":[{{\"role\":\"user\",\"content\":\"{}\"}}],\"max_completion_tokens\":17}}", "abcdef 中文 ".repeat(4096)).into_bytes();
    let tools = br#"{"model":"test-model","messages":[{"role":"user","content":"weather"}],"tools":[{"type":"function","function":{"name":"weather","description":"Weather lookup","parameters":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}}}],"max_completion_tokens":17}"#.to_vec();
    let mut tokens = vec![Token::EMPTY; 512];
    let mut nested = vec![Token::EMPTY; 256];
    let mut records = vec![Record::EMPTY; 128];
    let mut scratch = vec![0; 65536];
    let mut output = vec![0; 131072];
    let context = Context {
        max_tokens: Some(32),
        model: Some("test-model"),
        ..Context::default()
    };
    for (name, input) in [
        ("small", &small),
        ("unicode", &unicode),
        ("long_text", &long),
        ("tools", &tools),
    ] {
        let iterations = if name == "long_text" {
            (iterations / 32).max(1)
        } else {
            iterations
        };
        for target in [
            Protocol::OpenAiResponses,
            Protocol::AnthropicMessages,
            Protocol::GeminiGenerateContent,
        ] {
            for strategy in [Strategy::Auto, Strategy::Direct, Strategy::Ir] {
                if name == "tools"
                    && strategy == Strategy::Direct
                    && target != Protocol::OpenAiResponses
                {
                    continue;
                }
                let options = Options {
                    strategy,
                    context,
                    policy: Policy::default(),
                    ..Options::default()
                };
                let mut times = Vec::with_capacity(rounds);
                let mut last = None;
                for _ in 0..rounds {
                    let start = Instant::now();
                    for _ in 0..iterations {
                        let result = convert(
                            Protocol::OpenAiChat.into(),
                            target.into(),
                            Operation::Request,
                            black_box(input),
                            black_box(&mut output),
                            options,
                            Workspace {
                                tokens: &mut tokens,
                                records: &mut records,
                                scratch: &mut scratch,
                                nested_tokens: &mut nested,
                            },
                        )
                        .unwrap();
                        black_box(&output[..result.written()]);
                        last = Some(result);
                    }
                    times.push(start.elapsed().as_secs_f64() * 1e9 / f64::from(iterations));
                }
                times.sort_by(f64::total_cmp);
                let median = times[times.len() / 2];
                let p95 = times[(times.len() * 95 / 100).min(times.len() - 1)];
                let result = last.unwrap();
                let r = result.resources;
                println!("{name}_{target:?},{strategy:?},{},{},{median:.1},{p95:.1},{:.2},{},{},{},{},{}",
                    input.len(), result.written(), input.len() as f64 / 1048576.0 / (median * 1e-9),
                    r.token_storage_bytes, r.ir_storage_bytes, r.ir_records_used, r.scratch_capacity, r.nested_token_storage_bytes);
            }
        }
    }
    eprintln!(
        "sizes: Token={} Record={} StreamConverter={}",
        std::mem::size_of::<Token>(),
        std::mem::size_of::<Record<'_>>(),
        std::mem::size_of::<zconv::stream::StreamConverter<'_, '_>>()
    );
}
