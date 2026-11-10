# Performance and memory measurements

These are reproducible measurements of the library, not model/network latency
or a comparison with another product. The measured code is commit
[`e227184715ba53e827d5435974de88c1f7533c3f`](https://github.com/LIghtJUNction/zconv/commit/e227184715ba53e827d5435974de88c1f7533c3f).
Documentation and the corpus initialization script were added afterward without
changing the measured kernel.

## Environment and method

Measured on **2026-11-10**, Linux x86_64, Intel Xeon Platinum 8573C,
Rust **1.99.1** (`58cf8f9c3`, LLVM 23.1.1). Cargo's bench profile uses thin LTO,
one codegen unit and its default optimized level. This is a shared environment;
CPU affinity, frequency and other tenants were not controlled. Small differences
and tail outliers should not be treated as stable guarantees. Local compilation,
tests and fuzzing were stopped during the final conversion/stream measurements.

Fixture creation, allocation and output formatting are outside the timed loops.
The input bytes and result are passed through `black_box`. Conversion includes
complete JSON syntax/UTF-8/duplicate-key validation, source feature checks and
target writing. Output is actually generated; a parser-only result is not
labeled conversion throughput.

## Complete request conversion

Run `cargo bench --bench convert -- --full`. The harness takes 15 batches of
10,000 conversions (312 per batch for the long fixture). The CSV's `median_ns`
and `p95_ns` describe **batch-average time per conversion**, not individual-call
latency percentiles. Input throughput uses input bytes, not target byte count.

| Chat source fixture → target | Input | Direct median | IR median | IR / Direct |
|---|---:|---:|---:|---:|
| Small → Responses | 127 B | 3.702 µs | 4.122 µs | 1.11× |
| Small → Anthropic | 127 B | 2.908 µs | 4.973 µs | 1.71× |
| Small → GenerateContent | 127 B | 2.558 µs | 4.187 µs | 1.64× |
| Function definition → Responses | 280 B | 4.571 µs | 5.095 µs | 1.11× |
| Long mixed text → Responses | 57,435 B | 73.363 µs | 72.615 µs | 0.99× |
| Long mixed text → Anthropic | 57,435 B | 71.033 µs | 77.278 µs | 1.09× |
| Long mixed text → GenerateContent | 57,435 B | 71.544 µs | 75.720 µs | 1.06× |

For the long fixture, Direct input throughput is approximately **747–771 MiB/s**.
Direct removes semantic record construction, but long inputs are dominated by
validation and byte handling shared with IR. It is not faster on every measured
case. Default Auto prefers an applicable Direct path because that is the API
contract; it does not promise the lowest latency for every input.

Tool requests to Anthropic/GenerateContent currently require IR. Auto spends
about 0.8 µs more than forced IR on these fixtures while checking direct
applicability. Callers with a known IR-only workload can select `Strategy::Ir`.
Direct and IR may emit different legal JSON shapes, such as explicit default
item type fields; the differential tests compare semantics and loss reports.

The throughput harness reserves generous reusable buffers for all cases. Its
`*_reserved` columns are capacities, not minimum requirements or measured peaks.
Use the separate capacity example below for the small-footprint case.

Raw results: [convert.csv](benchmarks/convert.csv).

## Incremental streaming

Run `cargo bench --bench stream -- --full`. There are 24 cases: two targets,
three strategies, one-byte or whole-stream input, and 17-byte or whole-result
output slices. Each stream contains 16 text chunks, UTF-8 and escapes, a finish
event, usage and `[DONE]`: **3,348 input bytes and 848 decoded text bytes**.

The harness uses 21 calibrated batches, aiming at about 20 ms per batch. It also
samples individual streams separately. `batch_*` statistics and `single_*`
statistics are intentionally distinct. First-write latency includes converter
construction and ends when the first `step` call returns `written > 0`.

Selected Auto cases, all using Direct and a 17-byte output slice:

| Target | Input slice | Batch-average median / stream | Individual first-write median | Output |
|---|---:|---:|---:|---:|
| Anthropic | Whole 3,348 B | 67.844 µs | 3.455 µs | 3,490 B |
| Anthropic | 1 B | 119.102 µs | 5.360 µs | 3,490 B |
| Responses | Whole 3,348 B | 84.284 µs | 3.727 µs | 9,173 B |
| Responses | 1 B | 131.907 µs | 5.647 µs | 9,173 B |

These numbers measure conversion work after bytes are available. They do not
measure time to the first generated model token. With both entire-stream input
and sufficient output space, one `step` can finish the whole conversion before
returning, so first caller-visible output time can approach total stream time.

Direct and IR streaming use the same semantic sink. Their timing differences are
small and noisy on these fixtures; the clear Direct storage saving is the
absence of the Event IR arena. The full CSV includes all latency percentiles,
step counts, actual paths and memory regions. It has a timing table followed by
a separately headed memory table; lines beginning with `#` explain the schema.

Raw results: [stream.csv](benchmarks/stream.csv).

## Zero allocation and small storage are separate checks

`tests/allocation.rs` installs a counting allocator **only in the std test
binary**. A positive allocation control verifies the oracle. Thread-local
counters exclude unrelated test-runner activity. Parsing, Identity, Direct, IR,
and Direct/IR streaming all produce **zero allocation calls and zero allocated
bytes** in the measured section. The library itself forbids unsafe Rust; the
test's allocator wrapper delegates unchanged contracts to `System`.

Run `cargo run --example memory` for a fixture-specific storage measurement.
It determines token/record capacities and exact output length before the final
conversion. Those probe allocations are outside the conversion and are released
before its buffers are prepared.

| Small Chat → Responses resource | Direct | IR |
|---|---:|---:|
| Borrowed input | 72 B | 72 B |
| Output capacity | 100 B | 117 B |
| JSON token storage: 10 × 20 B | 200 B | 200 B |
| Semantic IR storage | 0 B | 432 B: 2 × 216 B |
| Scratch / nested token storage | 0 / 0 B | 0 / 0 B |
| **Provided buffers, excluding input** | **300 B** | **749 B** |
| **Input plus provided buffers** | **372 B** | **821 B** |

These are slice capacities for this exact fixture, not process RSS, stack usage,
allocator bookkeeping or a universal bound. Larger structures need more tokens
and IR records. The output shapes differ legally, accounting for the 17-byte
output difference.

## Streaming storage and aggregation

On the measured x86_64 build, `StreamConverter` is **832 B**, `ActiveItem` 88 B,
`Event` 216 B and `Token` 20 B. The table below describes the same 848-byte answer
fixture. Each number comes from `memory()` or the caller's declared storage.

| Region | Anthropic target | Responses target |
|---|---:|---:|
| Converter state | 832 B | 832 B |
| Framing capacity / high water | 189 / 188 B | 189 / 188 B |
| Data capacity / high water | 181 / 181 B | 181 / 181 B |
| Pending capacity / high water | 512 / 475 B | 4,592 / 3,336 B |
| JSON token capacity / high water | 1,280 / 400 B | 1,280 / 400 B |
| Item and rollback checkpoint capacity | 88 + 88 B | 88 + 88 B |
| Direct Event IR capacity | 0 B | 0 B |
| IR Event arena capacity / high water | 432 / 432 B | 432 / 432 B |
| Aggregate capacity / high water | 64 / 25 B | 1,161 / 1,129 B |
| **Direct state + workspace capacity** | **3,234 B** | **8,411 B** |
| **IR state + workspace capacity** | **3,666 B** | **8,843 B** |

Input and the caller's output slice are separate: a one-byte input fragment and
a 17-byte output slice can be reused. The benchmark happens to hold its whole
source fixture for repetition. No full-source buffer is owned by the converter.

Anthropic conversion retains 25 bytes of response ID/model metadata and does not
accumulate the answer. Responses conversion retains text, fragment links and
metadata to produce required final snapshots. This is a real cost that grows
with content. Tool arguments also require bounded aggregation and may temporarily
need a contiguous validation copy at completion.

`workspace_high_water_sum` adds separately observed region high waters; those
maxima need not occur at the same instant. It is not labeled a simultaneous
peak or RSS. Stack frames and the caller's storage placement are additional
costs. JSON recursion has a hard depth ceiling of 64 and can be lowered through
`json::Limits`; lower limits reduce both accepted nesting and recursive stack
requirements.

### Stack cost is additional

An independent diagnostic build records compiler-emitted static stack frames.
It uses Rust `1.102.0-nightly (ac8310ba6 2026-11-09)`, LLVM 23.1.5, x86_64,
`opt-level=3`, one codegen unit and `-Z emit-stack-sizes`, producing a relocatable
object without final linking/LTO. It does not change the crate's stable-Rust or
MSRV requirement.

| Function | Static frame in the diagnostic object |
|---|---:|
| `json::parse` | 168 B |
| JSON recursive `value` / `array` / `object` | 72 / 88 / 264 B |
| Top-level `convert` | 1,864 B |
| Direct request / response writer | 1,320 / 1,544 B |
| IR request decoder / encoder | 1,400 / 1,016 B |
| IR response decoder / encoder | 920 / 1,000 B |
| `StreamConverter::step` | 1,640 B |

There are 231 records; the largest individual record is the content-block
decoder at 3,128 B. Frames on an active call chain add together, and recursive
JSON levels repeat frames. Inlining, ABI and application LTO can change these
numbers. This is **not a measured whole-call stack peak or a proven total memory
bound**. The 300-byte Direct buffer example must therefore not be described as
300-byte total conversion memory. Embedded callers should select nesting limits
for their stack budget and measure their final linked application.

Full records: [stack-sizes.csv](benchmarks/stack-sizes.csv). Reproduce on a
little-endian ELF64 Linux target with Rust nightly and GNU `c++filt` available:

```sh
rustc +nightly --crate-name zconv --crate-type lib --edition 2021 \
  -C opt-level=3 -C codegen-units=1 -Z emit-stack-sizes \
  --emit=obj src/lib.rs -o /tmp/zconv.o
python3 scripts/measure_stack.py /tmp/zconv.o
```

## Measured JSON optimization

The retained optimization skips an initial ordinary string prefix using safe,
portable 64-bit byte tests. Quotes, backslashes and control bytes return to the
scalar validator; UTF-8 validation still covers the complete input. It uses no
unsafe loads or architecture-specific intrinsics. Boundary tests cover every
byte value at positions around word boundaries, including controls and escapes.

An isolated A/B harness builds the pre-optimization JSON kernel from
`96fa6fc606e265d58b60a20073f2354f7564169e` and the current kernel with the same
types, benchmark and compiler flags. Two passes reverse before/after order.
Each result is the median of nine batch averages. The second pass gives:

| JSON fixture | Bytes | Before | After | Ratio |
|---|---:|---:|---:|---:|
| Long ASCII | 65,560 | 78.558 µs | 13.834 µs | 5.68× |
| Long Unicode | 98,328 | 184.454 µs | 101.162 µs | 1.82× |
| Escape-dense | 73,752 | 84.226 µs | 72.596 µs | 1.16× |

Across both passes, long ASCII improved 4.89–5.68× and long Unicode 1.66–1.82×;
the escape-dense case did not regress in these measurements. These ratios apply
to this JSON kernel benchmark, not to end-to-end model calling or every protocol
conversion.

Raw results: [before 1](benchmarks/json-before-1.csv),
[after 1](benchmarks/json-after-1.csv), [before 2](benchmarks/json-before-2.csv),
[after 2](benchmarks/json-after-2.csv).

## Reproduction

```sh
cargo bench --bench convert -- --full > convert.csv
cargo bench --bench stream -- --full > stream.csv
cargo bench --bench json -- --full > json.csv
cargo run --example memory
python3 scripts/compare_json.py --output /tmp/zconv-json-comparison
```

Run benchmarks separately from compilation, tests and fuzzing. Retain compiler,
architecture, fixture and capacity details with any new results. CI runs quick
benchmark smoke tests for correctness; noisy timing thresholds are not release
gates.
