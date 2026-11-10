# Protocol and design sources

Documentation retrieved on 2026-11-10. Adapter versions are explicit in `Format`.

| Adapter | Wire version | Primary specification |
|---|---|---|
| OpenAI Completions | v1, legacy prompt API | https://developers.openai.com/api/reference/resources/completions/methods/create |
| OpenAI Chat Completions | v1, legacy functions and current tools | https://developers.openai.com/api/reference/resources/chat |
| OpenAI Responses | v1 | https://developers.openai.com/api/reference/resources/responses |
| Responses events | v1 | https://developers.openai.com/api/reference/resources/responses/streaming-events |
| Anthropic Messages | 2023-06-01; explicit newer version checks | https://platform.claude.com/docs/en/api/messages |
| Anthropic version differences | 2026-10-21 | https://platform.claude.com/docs/en/api/migration-2026-10-21 |
| Anthropic streaming | versioned Messages events | https://platform.claude.com/docs/en/build-with-claude/streaming |
| Gemini GenerateContent | v1beta JSON REST mapping | https://ai.google.dev/api/generate-content |
| Gemini Interactions | v1, step-based resource | https://ai.google.dev/api/interactions-api-v1 |
| Gemini Interactions events | interaction / step events | https://ai.google.dev/gemini-api/docs/streaming |
| SSE framing | WHATWG Living Standard, 2026-11-09 revision | https://html.spec.whatwg.org/multipage/server-sent-events.html |
| JSON grammar | RFC 8259 | https://www.rfc-editor.org/rfc/rfc8259 |

## Pinned design study

Repository: https://github.com/LIghtJUNction/cortexfs

Commit: `84f349c97bc2f6a611109686a02e21de3800042a`

Directory: `crates/cortexfs-protocol`

Read: `borrowed.rs`, `bridge.rs`, `direct.rs`, `context.rs`, `event.rs`,
`request.rs`, `response.rs`, `semantic.rs`, `openaichat.rs`,
`openairesponses.rs`, `gemini.rs`, and the crate README.

Borrowed wire views, a separate semantic model, explicit context ownership and
provider namespaces are the design ideas reused. zconv is independently
implemented with caller-owned storage and has no dependency on cortexfs-protocol.
Identity conversions validate their input. Automatic fallback is restricted to
capability errors; malformed input and capacity errors remain errors.

## Registry check

Before initialization, `GET https://crates.io/api/v1/crates/zconv?nocache=20261110`
with an identifying User-Agent returned HTTP 404 and
`{"errors":[{"detail":"crate `zconv` does not exist"}]}`.
The sparse registry entry `https://index.crates.io/zc/on/zconv` was also absent.
The initial no-User-Agent request returned HTTP 403; that response was not treated
as evidence of name availability. The release gate repeats the API check before
publication; name availability is not a reservation.

## Interpretation rules

Gemini Interactions is a separate resource: its `previous_interaction_id` belongs
to its provider namespace. It cannot become an OpenAI `previous_response_id`.
Current Interactions uses `steps`, including `model_output`, `thought`,
`function_call`, and `function_result`, and step-based streaming events.

Anthropic 2026-10-21 removes sampling parameters. Adapters must reject requests
that would emit `temperature`, `top_p` or `top_k` for that version, even if a value
matches an earlier default. These are protocol version differences, not vendor
extensions.

Specifications define the supported wire contract. Additional model-specific
validation and network headers remain the caller's responsibility.
