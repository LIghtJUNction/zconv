use std::{hint::black_box, time::Instant};
use zconv::json::{self, Token};

fn main() {
    let full = std::env::args().any(|arg| arg == "--full");
    println!("case,bytes,median_ns,mib_per_second");
    for (name, text) in [
        ("ascii", "aZ09+/_-"),
        ("unicode", "中文Δ😀"),
        ("escaped", "a\\n\\u4e2d"),
    ] {
        for repeat in [1usize, 16, 512, 8192] {
            let input = format!("{{\"model\":\"m\",\"input\":\"{}\"}}", text.repeat(repeat));
            let iterations = if full {
                (20_000_000 / input.len()).max(100)
            } else {
                20
            };
            let mut samples = Vec::new();
            let mut tape = [Token::EMPTY; 16];
            for _ in 0..if full { 9 } else { 3 } {
                let start = Instant::now();
                for _ in 0..iterations {
                    let doc = json::parse(
                        black_box(input.as_bytes()),
                        black_box(&mut tape),
                        json::Limits::default(),
                    )
                    .unwrap();
                    black_box(doc.tokens_used());
                }
                samples.push(start.elapsed().as_secs_f64() * 1e9 / iterations as f64);
            }
            samples.sort_by(f64::total_cmp);
            let ns = samples[samples.len() / 2];
            println!(
                "{name}_{repeat},{},{ns:.1},{:.2}",
                input.len(),
                input.len() as f64 / 1048576.0 / (ns * 1e-9)
            );
        }
    }
}
