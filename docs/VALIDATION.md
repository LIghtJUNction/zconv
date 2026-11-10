# Validation evidence

The executable implementation was verified on **2026-11-10**. The measured code
commit is [`e227184715ba53e827d5435974de88c1f7533c3f`](https://github.com/LIghtJUNction/zconv/commit/e227184715ba53e827d5435974de88c1f7533c3f).
The [GitHub CI run](https://github.com/LIghtJUNction/zconv/actions/runs/42817235107)
completed successfully with **12 / 12 jobs**. Later documentation/corpus-script
changes do not change the kernel; their own CI is visible in the repository.

## Correctness and portability gates

| Gate | Observed result | What it establishes |
|---|---|---|
| Stable Rust 1.99.1, debug | 89 integration tests + 1 doc test pass | Current stable compilation and checked arithmetic behavior |
| Rust 1.74.0 MSRV | Same tests and all examples/bench targets pass | The stated minimum is actually exercised |
| Optimized release tests | 89 integration tests + 1 doc test pass | Optimized code follows the same contracts |
| GitHub Linux, macOS, Windows | Stable and 1.74.0 debug/release/doc pass | Three operating-system runners actually executed tests |
| WASM, aarch64 Linux, bare-metal thumb | `cargo check --lib --no-default-features` passes | Cross-target compilation and absence of std/allocator linkage requirements |
| Miri | 2 bounded core integration tests pass locally and in CI | Borrowing/state transitions on the selected corpus |
| Direct Miri during development | Original 10 cases, 2 new tool cases and 1 strict-default case pass | Additional direct-view and tool-mapping checks |
| Clippy and rustfmt | All targets pass with `-D warnings`; formatting check passes | No outstanding lints in the shipped Rust targets |
| Package audit | Pass; dependency graph has exactly one package | No dependencies of any kind, no build.rs, external oracle isolation |
| `cargo package` | Package builds from its extracted contents | The distributed file set is usable |
| `cargo publish --dry-run` | Package verification succeeds; upload deliberately aborted by Cargo | Publication preparation works without publishing a registry version |
| Registry lookup | Explicit zconv 404 before initialization and again before dry run | Name was available at those times; it is not reserved |
| Python release gates | 14 tests pass | Version validation, lockfile synchronization, registry ownership and Git-ref preflight |
| Independent serde_json oracle | 4 tests pass | Grammar/writer/deterministic mutation comparisons, with documented stricter duplicate-key rejection |

Cross-compilation is not hardware execution. In particular, the aarch64 Linux,
WASM and thumb rows do not claim a device/emulator runtime test. Fuzzing and Miri
exercise finite corpora; neither is a proof for all inputs.

## Main test inventory

| Test file | Tests | Principal coverage |
|---|---:|---|
| `allocation.rs` | 1 | Positive-controlled zero allocation for parse/Identity/Direct/IR and Direct/IR streaming |
| `direct.rs` | 14 | Six direct pairs, borrowed views, explicit losses/context, functions/tools/history, strict defaults, IR differential results |
| `json.rs` | 11 | UTF-8, escapes, surrogate pairs, all scalar kinds, duplicate decoded names, field order, limits, truncation, writer capacity, word boundaries |
| `miri.rs` | 2 | Input/token/IR lifetimes and fragmented stream/pending-output transitions |
| `protocol.rs` | 27 | Six adapters, editable compact IR, media/schema/reasoning, context ownership, usage, errors/statuses, strict modes |
| `routing.rs` | 6 | Identity validation, forced IR, input-specific Auto, fallback classification, no error masking |
| `sse.rs` | 6 | WHATWG line handling, BOM/comments, multiline data, UTF-8 replacement, EOF and recovery |
| `stream.rs` | 22 | Six protocol event lifecycles, fragmentation/backpressure, interleaved tools, usage, final snapshots, profiles and terminal states |
| Library documentation | 1 | A compiled, runnable public-API example |

Several tests contain direction/fragment matrices rather than a single fixture:

- 36 request directions and 36 complete-response directions for common text.
- 25 modern-protocol tool request directions and 25 tool-response directions.
- Direct/IR comparisons for the six Chat-centered direct pairs.
- 11 additional tool-definition/selector fixtures and 6 history fixtures.
- Stream fragment sizes 1–97, one-byte output and zero-capacity buffer growth.
- Every byte value around string word boundaries and deterministic arbitrary-byte
  and truncation checks.

These counts describe tested cases, not full vendor-schema coverage. The
[support matrix](SUPPORT.md) remains the authoritative scope statement.

## Fuzz and sanitizer checks

The independent `fuzz/` workspace uses cargo-fuzz 0.13.2 and libFuzzer. It is not
part of the published crate. Three targets each completed **30,000 runs** with
AddressSanitizer and coverage instrumentation on the final relevant kernel:

| Target | Runs | Input exercised |
|---|---:|---|
| JSON | 30,000 | Arbitrary bytes, token limits, borrowed spans and escaping |
| Complete conversion | 30,000 | Source/target/strategy/operation selectors plus arbitrary JSON bytes |
| Incremental stream | 30,000 | Source/target/strategy selectors, chunking, output bounds and EOF |

No library panic or AddressSanitizer defect was found in these runs. LeakSanitizer
cannot run in the managed ptrace-based environment, so `ASAN_OPTIONS=detect_leaks=0`
was used. AddressSanitizer remained enabled. The separate allocator oracle, not
this switch or libFuzzer's process RSS, supports the zero-allocation claim.

The committed corpus initializer recreates synthetic seeds from the protocol
fixtures. Existing local mutations are retained. Exact fuzz mutations depend on
the libFuzzer run; the counts above are finite smoke campaigns, not an exhaustive
security audit.

```sh
cargo install cargo-fuzz --locked
python3 fuzz/seed_corpus.py
ASAN_OPTIONS=detect_leaks=0 cargo +nightly fuzz run json -- -runs=30000 -max_len=65536
ASAN_OPTIONS=detect_leaks=0 cargo +nightly fuzz run convert -- -runs=30000 -max_len=65536
ASAN_OPTIONS=detect_leaks=0 cargo +nightly fuzz run stream -- -runs=30000 -max_len=65536
cargo +nightly miri test --test miri
```

Omit the LeakSanitizer environment override on a host where it is supported.
The fuzz targets stop on expected capacity/capability errors and bound their
storage and step loops. Real provider network calls are outside this library's
boundary and were not performed.

## Reproduce release preparation

```sh
cargo test --all-targets --locked
cargo +1.74.0 test --all-targets --locked
cargo test --release --locked
cargo test --doc --locked
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
python3 scripts/check_package.py
python3 -m unittest discover -s scripts -p 'test_*.py'
cargo test --manifest-path comparison/Cargo.toml --locked
cargo package --locked
python3 scripts/release.py registry
cargo publish --dry-run --locked
```

Actual registry publication has not been performed as part of these validation
results. The [release workflow](RELEASING.md) accepts version bumps and the
`CARGO_REGISTRY_TOKEN` secret, rechecks ownership and Git refs, then publishes and
records the tag/release when explicitly run in publish mode.
