# zconv API coverage audit — 2026-11-15

## Scope and result

Audited repository: [LIghtJUNction/zconv](https://github.com/LIghtJUNction/zconv).
Audited revision: `5c1e0eb8a3b3119ce5dd1af7a377605372241d50`; Cargo package version: `0.3.0`.

The requested feature list matches [dynamo-protocols 10.0.2](https://docs.rs/crate/dynamo-protocols/10.0.2). zconv implements a zero-dependency, no_std wire converter with caller-owned buffers. Dynamo defines or re-exports request/response types. “Owned” describes where types are defined; “input chain” describes nested input types, not server-side conversation storage.

zconv supports the core Chat, Responses, legacy Completions, and Anthropic Messages conversion paths. It does not cover the full requested list.

| Requested family | Verified zconv coverage |
|---|---|
| Chat Completions | Text, supported media, tools, profile-gated reasoning, and cumulative incoming usage. Compatibility limits are described below. |
| Batch API | No dedicated adapter or API types. |
| Files API | No dedicated adapter or API types. Message-level file references are supported separately. |
| Responses API | Own request/response/event conversion; partial relaxed-input compatibility. |
| Legacy Completions | Own request/response/event conversion; single text prompt conversion, caller expansion for batches/token prompts. |
| Anthropic Messages | Own core request/response/event conversion; provider extensions and server tools have explicit limits. |
| Embeddings API | No dedicated adapter or API types. |
| Images API | No dedicated adapter or API types. Message-level image support is separate. |
| Upstream re-exports | None: Cargo dependency sections are empty. |

## Confirmed implementation boundaries

- `Protocol` lists six adapters: Completions, Chat, Responses, Anthropic Messages, Gemini GenerateContent, and Gemini Interactions.
- Images and files inside a conversation are content representations. They do not supply image generation/editing or file resource management.
- Native provider file IDs and stored conversation IDs retain their source namespace.
- Chat tool calls accept string arguments; Dynamo's object-valued arguments extension is rejected.
- Chat `stream_options` remains an opaque source extension. It survives same-format conversion; strict cross-format conversion rejects it.
- Incoming cumulative usage is recorded correctly, but Chat output currently emits usage only at the end.
- Standard and compatible profiles, Identity and IR paths, and complete responses versus streams have distinct compatibility behavior.

## Baseline verification

Executed with Rust 1.100.0 on Linux:

```sh
cargo test --locked --offline --lib --tests
```

Result: 133 tests passed, 0 failed. This covers the existing test suite; the API coverage findings are based on source review and additional targeted conversion probes.

## Primary implementation evidence

All zconv references are pinned to the audited revision:

- [Manifest](https://github.com/LIghtJUNction/zconv/blob/5c1e0eb8a3b3119ce5dd1af7a377605372241d50/Cargo.toml)
- [Protocol enum](https://github.com/LIghtJUNction/zconv/blob/5c1e0eb8a3b3119ce5dd1af7a377605372241d50/src/types.rs)
- [Request/response decoder](https://github.com/LIghtJUNction/zconv/blob/5c1e0eb8a3b3119ce5dd1af7a377605372241d50/src/protocol/decode.rs)
- [Request/response encoder](https://github.com/LIghtJUNction/zconv/blob/5c1e0eb8a3b3119ce5dd1af7a377605372241d50/src/protocol/encode.rs)
- [Stream decoder](https://github.com/LIghtJUNction/zconv/blob/5c1e0eb8a3b3119ce5dd1af7a377605372241d50/src/stream/decode.rs)
- [Stream encoder](https://github.com/LIghtJUNction/zconv/blob/5c1e0eb8a3b3119ce5dd1af7a377605372241d50/src/stream/encode.rs)
