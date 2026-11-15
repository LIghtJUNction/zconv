use std::{hint::black_box, time::Instant};
use zconv::{
    api::{self, Schema},
    json::{Limits, Token},
};

fn main() {
    let full = std::env::args().any(|arg| arg == "--full");
    let vectors = (0..1536)
        .map(|i| format!("{}", (i as f32 - 768.0) / 1536.0))
        .collect::<Vec<_>>()
        .join(",");
    let embedding = format!(
        r#"{{"object":"list","model":"m","data":[{{"object":"embedding","index":0,"embedding":[{vectors}]}}],"usage":{{"prompt_tokens":8,"total_tokens":8}}}}"#
    );
    let cases = [
        ("embedding_1536", Schema::EmbeddingsResponse, embedding.as_bytes()),
        ("batch", Schema::Batch, br#"{"object":"batch","id":"b","completion_window":"24h","endpoint":"/v1/embeddings","input_file_id":"f","status":"completed","created_at":1,"request_counts":{"total":20,"completed":20,"failed":0},"output_file_id":"o"}"#.as_slice()),
        ("file", Schema::File, br#"{"object":"file","id":"f","filename":"input.jsonl","purpose":"batch","bytes":1024,"created_at":1}"#.as_slice()),
        ("image_event", Schema::ImageStreamEvent, br#"{"type":"image_generation.completed","b64_json":"aW1hZ2U=","created_at":1,"usage":{"input_tokens":8,"output_tokens":256,"total_tokens":264}}"#.as_slice()),
    ];
    println!("case,bytes,median_ns,mib_per_second");
    for (name, schema, input) in cases {
        let mut tokens = [Token::EMPTY; 4096];
        let mut output = vec![0; input.len()];
        let iterations = if full {
            (20_000_000 / input.len()).max(100)
        } else {
            20
        };
        let mut samples = Vec::new();
        for _ in 0..if full { 9 } else { 3 } {
            let start = Instant::now();
            for _ in 0..iterations {
                let doc = api::parse(
                    schema,
                    black_box(input),
                    black_box(&mut tokens),
                    Limits::default(),
                )
                .unwrap();
                black_box(doc.encode(black_box(&mut output)).unwrap());
            }
            samples.push(start.elapsed().as_secs_f64() * 1e9 / iterations as f64);
        }
        samples.sort_by(f64::total_cmp);
        let ns = samples[samples.len() / 2];
        println!(
            "{name},{},{ns:.1},{:.2}",
            input.len(),
            input.len() as f64 / 1048576.0 / (ns * 1e-9)
        );
    }
}
