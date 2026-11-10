# Independent fuzz workspace

This workspace is excluded from the published crate and its dependency graph.
The library itself retains zero third-party dependencies. libFuzzer is an
external development oracle; none of its C/C++ or runtime code is linked into
zconv.

Install `cargo-fuzz` and run:

```sh
cargo install cargo-fuzz --locked
python3 fuzz/seed_corpus.py
cargo +nightly fuzz run json -- -max_total_time=60 -max_len=65536
cargo +nightly fuzz run convert -- -max_total_time=60 -max_len=65536
cargo +nightly fuzz run stream -- -max_total_time=60 -max_len=65536
```

The `json` target validates arbitrary bytes and visits their borrowed spans. The
`convert` target chooses source, target, request/response and strategy from the
first three input bytes, then converts the remaining bytes using bounded caller
storage. Capacity and capability failures are expected; panics are defects.
The `stream` target exercises the incremental converter with bounded storage,
arbitrary input/output chunking, explicit EOF and a bounded step count.

AddressSanitizer and coverage instrumentation are enabled by `cargo-fuzz`.
If LeakSanitizer is unavailable in a ptrace-based container, use
`ASAN_OPTIONS=detect_leaks=0` for that environment; this does not disable
AddressSanitizer. The separate allocation-count test checks the library's zero
allocation contract.

Mutation fixtures and deterministic differential tests run independently in
`comparison/`. Miri uses the bounded corpus in `tests/miri.rs`.
