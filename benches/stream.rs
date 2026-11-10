//! Dependency-free, stable-Rust streaming benchmark.
//!
//! Default: a short smoke benchmark, including `cargo test --all-targets`.
//! Full: `cargo bench --bench stream -- --full` (about 10 seconds of target
//! batch time across 24 cases, plus calibration and individual latency samples).
//!
//! Every input, workspace, output slice and statistics array is allocated before
//! timing. Each timed stream constructs a fresh converter over reused buffers.
//! Throughput batches do not query the clock per stream. Individual latency
//! samples separately measure construction-to-completion and construction-to-
//! first `step` return with `written > 0`, the first caller-visible output.

use std::fmt::Write as _;
use std::hint::black_box;
use std::mem::size_of;
use std::time::Instant;
use zconv::json::{Limits, Token};
use zconv::stream::{
    decode_event, ActiveItem, Event, Memory, Report, Status, StreamBuffers, StreamConfig,
    StreamConverter,
};
use zconv::{Path, Protocol, Strategy};

const CHUNKS: usize = 16;
const TEXT_JSON: &str = "hello 中 🌍; line\\nquote=\\\"x\\\"; data=0123456789abcdef";
const TEXT_DECODED: &str = "hello 中 🌍; line\nquote=\"x\"; data=0123456789abcdef";
const RESPONSE_ID: &str = "bench-response";
const MODEL: &str = "bench-model";
const MAX_BATCHES: usize = 21;

struct Fixture {
    wire: Vec<u8>,
    decoded_text_bytes: usize,
    max_frame: usize,
    max_data: usize,
    max_ir_events: usize,
}

impl Fixture {
    fn new() -> Self {
        let mut fixture = Self {
            wire: Vec::new(),
            decoded_text_bytes: CHUNKS * TEXT_DECODED.len(),
            max_frame: 0,
            max_data: 0,
            max_ir_events: 0,
        };
        let mut payload = String::new();
        write!(payload, "{{\"id\":\"{RESPONSE_ID}\",\"model\":\"{MODEL}\",\"created\":7,\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\"content\":\"\"}},\"finish_reason\":null}}]}}").unwrap();
        fixture.add(&payload);
        for _ in 0..CHUNKS {
            payload.clear();
            write!(payload, "{{\"id\":\"{RESPONSE_ID}\",\"model\":\"{MODEL}\",\"created\":7,\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{TEXT_JSON}\"}},\"finish_reason\":null}}]}}").unwrap();
            fixture.add(&payload);
        }
        fixture.add("{\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}");
        fixture.add("{\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":64,\"total_tokens\":69}}");
        fixture.add("[DONE]");
        fixture
    }

    fn add(&mut self, payload: &str) {
        self.wire.extend_from_slice(b"data: ");
        self.wire.extend_from_slice(payload.as_bytes());
        self.wire.extend_from_slice(b"\n\n");
        self.max_frame = self.max_frame.max(payload.len() + 8);
        self.max_data = self.max_data.max(payload.len());
        if payload != "[DONE]" {
            let mut tokens = [Token::EMPTY; 64];
            let mut events = [Event::EMPTY; 4];
            let used = decode_event(
                Protocol::OpenAiChat,
                payload.as_bytes(),
                &mut tokens,
                &mut events,
                Limits::default(),
            )
            .expect("benchmark fixture must be a supported source event");
            self.max_ir_events = self.max_ir_events.max(used);
        }
    }
}

struct Buffers {
    framing: Vec<u8>,
    data: Vec<u8>,
    pending: Vec<u8>,
    tokens: Vec<Token>,
    items: Vec<ActiveItem>,
    checkpoints: Vec<ActiveItem>,
    events: Vec<Event>,
    aggregate: Vec<u8>,
    output: Vec<u8>,
}

impl Buffers {
    fn new(fixture: &Fixture, target: Protocol, strategy: Strategy, small_output: bool) -> Self {
        let snapshots = target == Protocol::OpenAiResponses;
        Self {
            framing: vec![0; fixture.max_frame],
            data: vec![0; fixture.max_data],
            pending: vec![
                0;
                if snapshots {
                    fixture.decoded_text_bytes * 3 + 2048
                } else {
                    512
                }
            ],
            tokens: vec![Token::EMPTY; 64],
            items: vec![ActiveItem::EMPTY; 1],
            checkpoints: vec![ActiveItem::EMPTY; 1],
            events: vec![
                Event::EMPTY;
                if strategy == Strategy::Ir {
                    fixture.max_ir_events
                } else {
                    0
                }
            ],
            aggregate: vec![
                0;
                if snapshots {
                    fixture.decoded_text_bytes
                        + CHUNKS * 2 * size_of::<usize>()
                        + RESPONSE_ID.len()
                        + MODEL.len()
                        + 32
                } else {
                    64
                }
            ],
            output: vec![0; if small_output { 17 } else { 16384 }],
        }
    }
}

#[derive(Clone, Copy)]
struct Observation {
    output_bytes: usize,
    calls: usize,
    path: Path,
    report: Report,
    memory: Memory,
    first_write_ns: Option<f64>,
}

/// There is no allocation, vector growth, formatting or printing in this routine.
fn run_stream(
    fixture: &Fixture,
    config: StreamConfig<'_>,
    input_chunk: usize,
    buffers: &mut Buffers,
    latency_start: Option<Instant>,
) -> Observation {
    let mut converter = StreamConverter::new(
        config,
        StreamBuffers {
            framing: &mut buffers.framing,
            data: &mut buffers.data,
            pending: &mut buffers.pending,
            tokens: &mut buffers.tokens,
            items: &mut buffers.items,
            checkpoints: &mut buffers.checkpoints,
            events: &mut buffers.events,
            aggregate: &mut buffers.aggregate,
        },
    )
    .expect("benchmark configuration and capacities are valid");
    let mut position = 0;
    let mut output_bytes = 0;
    let mut calls = 0;
    let mut first_write_ns = None;
    loop {
        let end = (position + input_chunk).min(fixture.wire.len());
        let progress = converter.step(
            black_box(&fixture.wire[position..end]),
            &mut buffers.output,
            end == fixture.wire.len(),
        );
        calls += 1;
        position += progress.consumed;
        output_bytes += progress.written;
        if progress.written != 0 {
            if first_write_ns.is_none() {
                if let Some(start) = latency_start {
                    first_write_ns = Some(start.elapsed().as_secs_f64() * 1e9);
                }
            }
            black_box(&buffers.output[..progress.written]);
        }
        match progress.status {
            Status::Done => {
                assert_eq!(position, fixture.wire.len());
                return Observation {
                    output_bytes,
                    calls,
                    path: converter.path(),
                    report: converter.report(),
                    memory: converter.memory(),
                    first_write_ns,
                };
            }
            Status::Failed(error) => panic!("benchmark conversion failed: {error}"),
            Status::NeedBuffer { buffer, needed, .. } => {
                panic!("benchmark {buffer:?} needs {needed} entries/bytes")
            }
            Status::NeedInput | Status::NeedOutput => {}
        }
    }
}

#[derive(Clone, Copy)]
struct Stats {
    mean: f64,
    median: f64,
    p95: f64,
}

fn stats(samples: &mut [f64]) -> Stats {
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    samples.sort_by(f64::total_cmp);
    let rank95 = (samples.len() * 95).div_ceil(100);
    Stats {
        mean,
        median: samples[samples.len() / 2],
        p95: samples[rank95.saturating_sub(1)],
    }
}

#[derive(Clone, Copy)]
struct ResultRow {
    target: Protocol,
    strategy: Strategy,
    input_chunk: usize,
    output_capacity: usize,
    output_backing_capacity: usize,
    iterations: usize,
    batches: usize,
    batch: Stats,
    single: Stats,
    first_write: Stats,
    observation: Observation,
}

fn bench_case(
    fixture: &Fixture,
    target: Protocol,
    strategy: Strategy,
    byte_input: bool,
    small_output: bool,
    full: bool,
) -> ResultRow {
    let input_chunk = if byte_input { 1 } else { fixture.wire.len() };
    let mut config = StreamConfig::new(Protocol::OpenAiChat, target);
    config.strategy = strategy;
    config.context.initial_usage.input = Some(5);
    config.context.item_id_prefix = Some("bench-item-");
    let mut buffers = Buffers::new(fixture, target, strategy, small_output);
    let warm = run_stream(fixture, config, input_chunk, &mut buffers, None);
    if !small_output {
        // A full-output case has enough caller space for the complete stream.
        // Resize once, before calibration and measurement.
        buffers.output.resize(warm.output_bytes, 0);
        buffers.output.shrink_to_fit();
    }
    let calibration_runs = if full { 4 } else { 1 };
    let calibration_start = Instant::now();
    for _ in 0..calibration_runs {
        black_box(run_stream(fixture, config, input_chunk, &mut buffers, None));
    }
    let stream_ns = calibration_start.elapsed().as_secs_f64() * 1e9 / calibration_runs as f64;
    let iterations = if full {
        // Target 20 ms per batch, with a bounded count on very fast machines.
        (20_000_000.0 / stream_ns.max(1.0)).clamp(1.0, 10_000.0) as usize
    } else {
        4
    };
    let batches = if full { MAX_BATCHES } else { 3 };
    let mut batch_samples = [0.0; MAX_BATCHES];
    let mut single_samples = [0.0; MAX_BATCHES];
    let mut first_write_samples = [0.0; MAX_BATCHES];
    let mut observation = warm;
    for batch in 0..batches {
        let start = Instant::now();
        for _ in 0..iterations {
            observation = black_box(run_stream(fixture, config, input_chunk, &mut buffers, None));
        }
        batch_samples[batch] = start.elapsed().as_secs_f64() * 1e9 / iterations as f64;

        // Independent, individually timed stream. These percentiles are not
        // percentiles of the throughput batch means.
        let start = Instant::now();
        let individual = run_stream(fixture, config, input_chunk, &mut buffers, Some(start));
        single_samples[batch] = start.elapsed().as_secs_f64() * 1e9;
        first_write_samples[batch] = individual.first_write_ns.expect("stream emits output");
        black_box(individual);
    }
    assert_eq!(observation.output_bytes, warm.output_bytes);
    assert_eq!(observation.report.dropped_extensions, 0);
    assert_eq!(observation.report.dropped_reasoning, 0);
    if strategy == Strategy::Ir {
        assert_eq!(observation.path, Path::Ir);
        assert!(observation.report.ir_events > 0);
    } else {
        assert_eq!(observation.path, Path::Direct);
        assert_eq!(observation.report.ir_events, 0);
        assert_eq!(observation.memory.event_ir_capacity, 0);
    }
    ResultRow {
        target,
        strategy,
        input_chunk,
        output_capacity: buffers.output.len(),
        output_backing_capacity: buffers.output.capacity(),
        iterations,
        batches,
        batch: stats(&mut batch_samples[..batches]),
        single: stats(&mut single_samples[..batches]),
        first_write: stats(&mut first_write_samples[..batches]),
        observation,
    }
}

fn main() {
    let full = std::env::args().any(|arg| arg == "--full");
    let fixture = Fixture::new();
    // Fixed capacity; rows are written only after each timed case completes.
    let mut results = Vec::with_capacity(24);
    println!(
        "# mode={} text_chunks={} input_bytes={} decoded_text_bytes={}",
        if full { "full" } else { "quick" },
        CHUNKS,
        fixture.wire.len(),
        fixture.decoded_text_bytes
    );
    println!(
        "# type_bytes StreamConverter={} ActiveItem={} Event={} Token={} Memory={} Report={}",
        size_of::<StreamConverter<'_, '_>>(),
        size_of::<ActiveItem>(),
        size_of::<Event>(),
        size_of::<Token>(),
        size_of::<Memory>(),
        size_of::<Report>()
    );
    println!("# batch_*_ns are statistics of per-stream batch means; single_*_ns and first_write_*_ns are separate individual samples.");
    println!("# first_write is the first step return with written>0, including converter construction; all-input/full-output may return only after the whole stream is converted.");
    println!("# All allocation, fixture building, calibration formatting and result printing are outside measured loops. No competitor speed claim is made.");
    for target in [Protocol::AnthropicMessages, Protocol::OpenAiResponses] {
        for strategy in [Strategy::Auto, Strategy::Direct, Strategy::Ir] {
            for byte_input in [false, true] {
                for small_output in [false, true] {
                    results.push(bench_case(
                        &fixture,
                        target,
                        strategy,
                        byte_input,
                        small_output,
                        full,
                    ));
                }
            }
        }
    }
    println!("target,strategy,path,input_chunk_bytes,output_slice_bytes,input_bytes,output_bytes,step_calls,streams_per_batch,batches,batch_mean_ns,batch_median_ns,batch_mean_p95_ns,single_mean_ns,single_median_ns,single_p95_ns,first_write_mean_ns,first_write_median_ns,first_write_p95_ns,input_mib_per_second,output_mib_per_second");
    for row in &results {
        let seconds = row.batch.mean * 1e-9;
        println!("{:?},{:?},{:?},{},{},{},{},{},{},{},{:.1},{:.1},{:.1},{:.1},{:.1},{:.1},{:.1},{:.1},{:.1},{:.3},{:.3}",
            row.target, row.strategy, row.observation.path, row.input_chunk, row.output_capacity,
            fixture.wire.len(), row.observation.output_bytes, row.observation.calls, row.iterations, row.batches,
            row.batch.mean, row.batch.median, row.batch.p95,
            row.single.mean, row.single.median, row.single.p95,
            row.first_write.mean, row.first_write.median, row.first_write.p95,
            fixture.wire.len() as f64 / 1048576.0 / seconds,
            row.observation.output_bytes as f64 / 1048576.0 / seconds);
    }
    println!("# memory: byte capacities/peaks except explicitly named *_records; input and caller output are separate from converter workspaces.");
    println!("# workspace_high_water_sum adds independent region high-water marks; it is not a measured concurrent peak, stack peak or RSS. Event IR high water is measured from fixture preflight decoding.");
    println!("target,strategy,input_chunk_bytes,output_slice_bytes,output_backing_capacity,state_bytes,framing_capacity,framing_peak,data_capacity,data_peak,pending_capacity,pending_peak,token_capacity,token_peak_records,token_peak_bytes,item_capacity,item_peak_records,item_peak_bytes,checkpoint_capacity,event_ir_capacity,event_ir_peak_records,event_ir_peak_bytes,ir_events_total,aggregate_capacity,aggregate_used,aggregate_peak,state_and_buffer_capacity_total,workspace_high_water_sum");
    for row in &results {
        let m = row.observation.memory;
        let event_peak = if row.observation.path == Path::Ir {
            fixture.max_ir_events
        } else {
            0
        };
        let token_peak_bytes = m.tokens_peak * size_of::<Token>();
        let item_peak_bytes = m.items_used * size_of::<ActiveItem>();
        let event_peak_bytes = event_peak * size_of::<Event>();
        let capacity = m.state
            + m.framing_capacity
            + m.data_capacity
            + m.pending_capacity
            + m.token_capacity
            + m.item_capacity
            + m.checkpoint_capacity
            + m.event_ir_capacity
            + m.aggregate_capacity;
        let peak = m.state
            + m.framing_peak
            + m.data_peak
            + m.pending_peak
            + token_peak_bytes
            + item_peak_bytes * 2
            + event_peak_bytes
            + m.aggregate_peak;
        println!(
            "{:?},{:?},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            row.target,
            row.strategy,
            row.input_chunk,
            row.output_capacity,
            row.output_backing_capacity,
            m.state,
            m.framing_capacity,
            m.framing_peak,
            m.data_capacity,
            m.data_peak,
            m.pending_capacity,
            m.pending_peak,
            m.token_capacity,
            m.tokens_peak,
            token_peak_bytes,
            m.item_capacity,
            m.items_used,
            item_peak_bytes,
            m.checkpoint_capacity,
            m.event_ir_capacity,
            event_peak,
            event_peak_bytes,
            row.observation.report.ir_events,
            m.aggregate_capacity,
            m.aggregate_used,
            m.aggregate_peak,
            capacity,
            peak
        );
    }
}
