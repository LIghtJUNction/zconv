# zconv

**Allocation-free AI protocol conversion in stable Rust.**

zconv converts model requests, complete responses and incremental SSE events.
The library is always `#![no_std]`, uses only `core`, forbids unsafe Rust, and has
zero normal, development, build, optional or target-specific dependencies.
Input is borrowed. Every output buffer, JSON index, IR arena and stream workspace
belongs to the caller. There is no allocator, runtime, I/O, global cache or
background work in the library.

**MSRV: Rust 1.74.0.** The same crate builds for Linux, macOS, Windows, aarch64,
WASM and bare-metal targets. Benchmarks, examples and tests may use `std`.

## Own stream state without borrowing buffers

`stream::EventEncoderState` keeps protocol state without storing references to the
caller's buffers. Pass `EventEncoderBuffers` and response identity context to
`encode` for each event batch. After a capacity error, grow the indicated buffer
while preserving its contents and retry the same batch; discard provisional
output on error. This supports application-owned growable buffers without unsafe
lifetime tricks or another dependency. Supply choice-end and finish events only
when the application is ready to publish a successful terminal response.

## Start here

Use the repository source until the first registry release is available:

```sh
cargo add zconv@0.3
```

This complete request conversion is also available as
[`examples/convert.rs`](examples/convert.rs):

```rust
use zconv::ir::Record;
use zconv::json::Token;
use zconv::{convert, Operation, Options, Protocol, Workspace};

fn main() -> Result<(), zconv::Error> {
    let input = br#"{"model":"example-model","messages":[{"role":"user","content":"Hello"}]}"#;
    let mut tokens = [Token::EMPTY; 64];
    let mut records = [Record::EMPTY; 16];
    let mut nested_tokens = [Token::EMPTY; 64];
    let mut scratch = [0; 512];
    let mut output = [0; 1024];

    let result = convert(
        Protocol::OpenAiChat.into(),
        Protocol::OpenAiResponses.into(),
        Operation::Request,
        input,
        &mut output,
        Options::default(),
        Workspace {
            tokens: &mut tokens,
            records: &mut records,
            scratch: &mut scratch,
            nested_tokens: &mut nested_tokens,
        },
    )?;

    assert_eq!(result.path, zconv::Path::Direct);
    assert_eq!(result.resources.ir_records_used, 0);
    assert_eq!(
        core::str::from_utf8(&output[..result.written()]).unwrap(),
        r#"{"model":"example-model","input":[{"role":"user","content":[{"type":"input_text","text":"Hello"}]}]}"#,
    );
    Ok(())
}
```

The example capacities are explicit choices for a small request. A successful
direct call can use empty IR storage. Auto callers that need IR fallback must
provide enough records for their supported inputs. A capacity error never
silently allocates a larger buffer.

## Choose a conversion path

| Strategy | Contract |
|---|---|
| `Auto` | Use an applicable direct implementation; use IR only on a capability miss. |
| `Direct` | Require direct support for this operation and these input features; otherwise return `DirectUnavailable`. |
| `Ir` | Always decode and encode the semantic representation, even for identical formats. |

`Conversion.path` reports `Identity`, `Direct` or `Ir`. Same protocol, version
and profile can preserve the original bytes after complete JSON validation and
the adapter's source-semantic checks. Invalid JSON, invalid known fields, missing
context, forbidden losses and capacity errors are never hidden by fallback.

Direct requests and complete responses read borrowed protocol views without
constructing normalized IR. Direct streams invoke an event sink without filling
the Event IR arena. The implementation shares JSON validation, semantic mapping
rules and writer primitives between routes.

For deliberate inspection and edits, use `protocol::decode_request` or
`decode_response`, modify the public IR, then call its encoder. The JSON token
tape can be reused immediately after decoding: IR fragments borrow input bytes,
not token storage. See [`examples/edit_ir.rs`](examples/edit_ir.rs).

## Six distinct protocols

| Adapter | Requests | Complete responses | SSE events |
|---|---|---|---|
| OpenAI Completions | Text prompt and legacy controls | Text choices and usage | Text chunks and `[DONE]` |
| OpenAI Chat Completions | Messages, old functions and new tools | Messages, tools, choices, refusal, usage | Text/tool/refusal deltas, choices and usage |
| OpenAI Responses | Input items, instructions, tools and context references | Output items, status and usage | Typed item/content/tool events and required final snapshots |
| Anthropic Messages | Content blocks, system, tools and media | Blocks, stop state and cache-aware usage | Message/block lifecycle, partial tool JSON and cumulative usage |
| Gemini GenerateContent | Contents/parts and function declarations | Candidates, functions and usage | `streamGenerateContent` with `alt=sse` |
| Gemini Interactions v1 | Independent interaction input and context | Independent steps and status | `interaction.*` and `step.*` events |

The six Chat ↔ Responses / Anthropic / GenerateContent directions implement
direct requests, complete responses and supported stream events. Remaining
directions use IR. A direct direction can still reject a feature and let Auto
select IR; for example, most nonstream media paths currently use IR.

The [support and fidelity matrix](docs/SUPPORT.md) distinguishes faithful
conversion, caller-supplied context, explicit downgrade and unrepresentable
features for each protocol and operation. Streamed media and managed server/agent
tool steps are outside the first release's supported stream subset. Unknown
versions and unsupported features return errors.

Gemini Interactions is not an alias for GenerateContent. Stored response,
interaction and provider file handles retain their source namespace. An OpenAI
compatible provider must select an explicit `Profile`; its extensions are not
automatically injected into another provider.

## Strict fidelity and explicit context

The default `Policy` rejects semantic loss. Narrow options can permit dropping
extensions, reasoning, provider context or nonportable metadata. The conversion
report counts these losses and identifies metadata fields. For per-field details,
retain the decoded IR and use `Request::losses` or `Response::losses`. Streaming
has its own exact `DroppedField` mask.

Missing, `null`, empty text, empty lists and numeric zero are distinct. Numbers
stay as original JSON spans where possible; integer usage never passes through
floating point. Anthropic cache counters and Gemini thought counters are
normalized with checked integer arithmetic, then separated again when required
by the target.

Supply facts absent from a source through `Context` / `StreamContext`: model,
response ID, timestamps, required output limits, tool ID/name bindings and
Responses output item IDs. No clock or random ID generator is hidden inside
the converter. GenerateContent model/stream selection is returned as transport
metadata because those settings do not belong in its JSON request body.

## Incremental streams and memory

[`examples/stream.rs`](examples/stream.rs) feeds seven-byte input fragments into a
Chat → Responses converter and drains thirteen-byte output fragments. The API
requires no async runtime:

```rust,ignore
let step = converter.step(input_fragment, output_fragment, eof);
// Advance input by step.consumed; commit output[..step.written].
// NeedInput / NeedOutput / NeedBuffer / Done / Failed are explicit.
```

SSE framing handles CR, LF, CRLF, BOM, comments, multiple data lines and arbitrary
network splits. Output backpressure and workspace growth preserve consumed
bytes and never replay committed events. Stream routing is fixed before the
first output.

Text paths without target snapshots do not retain the entire answer. Tool
arguments need bounded storage for validation at completion. Responses targets
must emit complete final snapshots, so their aggregate space grows with the
answer. This cost is supplied and measured separately; it is not omitted to
claim fixed memory. The [streaming contract](docs/STREAMING.md) explains lifecycle,
EOF, usage, snapshots, profiles and buffer replacement.

`Conversion.resources` and `StreamConverter::memory()` distinguish input/output,
reserved capacities, state size, IR records and observed high-water usage.
`OutputTooSmall` provides an exact output size; token/IR capacity errors provide
a lower bound. JSON input size, nesting, tokens and object members have explicit
limits. Duplicate keys, including escape-equivalent names, are rejected.

## Build, verify and measure

```sh
cargo test --all-targets --locked
cargo +1.74.0 test --all-targets --locked
cargo clippy --all-targets -- -D warnings
cargo run --example convert
cargo run --example edit_ir
cargo run --example stream
cargo run --example memory
cargo bench --bench convert -- --full
cargo bench --bench json -- --full
cargo bench --bench stream -- --full
python3 scripts/check_package.py
```

The main crate's tests and benchmarks have no third-party dependencies. Optional
`comparison/` and `fuzz/` workspaces contain the external serde_json/libFuzzer
oracles and are excluded from both the crate dependency graph and release
package. CI also runs Miri, MSRV/stable on three operating systems, cross-target
checks and a package audit.

- [Measured performance and memory](docs/PERFORMANCE.md)
- [Validation evidence and reproduction](docs/VALIDATION.md)
- [Architecture, borrowing and recovery](docs/ARCHITECTURE.md)
- [Version bump and registry release workflow](docs/RELEASING.md)
- [Changes](CHANGELOG.md)

## Sources and scope

Official protocol documentation was checked on **2026-11-10**. Adapter versions,
source URLs and interpretation rules are recorded in [SOURCES.md](docs/SOURCES.md).
The historical design study uses
[cortexfs-protocol at commit 84f349c97bc2f6a611109686a02e21de3800042a](https://github.com/LIghtJUNction/cortexfs/tree/84f349c97bc2f6a611109686a02e21de3800042a/crates/cortexfs-protocol).
This implementation is independent and does not depend on that crate.

The crates.io API confirmed that `zconv` did not exist before initialization.
The release gate rechecks name ownership and version availability immediately
before publication; that earlier result does not reserve the name.

The library's boundary is wire data and event semantics. Networking,
authentication, connection pooling, retries, model execution and server resource
management belong to the caller.

## License

MIT OR Apache-2.0.
