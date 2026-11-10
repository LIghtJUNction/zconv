# zconv

Allocation-free AI protocol conversion in stable Rust.

The core uses only `core`, is always `no_std`, and has no third-party dependencies. Callers own the input, token tape, semantic IR storage, output buffers, and streaming aggregation space.

## Conversion contract

- `Auto` selects an applicable direct adapter, otherwise the semantic IR path.
- `Direct` requires an applicable direct adapter.
- `Ir` always decodes and re-encodes the semantic representation.
- Invalid input and capacity errors never trigger fallback.
- Semantic loss requires explicit policy and a conversion report.

## Protocol scope

OpenAI Completions, Chat Completions and Responses; Anthropic Messages; Gemini GenerateContent and the separate Gemini Interactions resource.

## Design reference

The design study uses [cortexfs-protocol at commit 84f349c97bc2f6a611109686a02e21de3800042a](https://github.com/LIghtJUNction/cortexfs/tree/84f349c97bc2f6a611109686a02e21de3800042a/crates/cortexfs-protocol). The implementation is independent and does not depend on that crate.

Protocol documentation retrieval date: 2026-11-10. The crates.io API returned `crate zconv does not exist` before initialization. Publication rechecks ownership and version availability.

## License

MIT OR Apache-2.0.
