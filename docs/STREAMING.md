# Streaming conversion

`StreamConverter` converts an SSE response as bytes arrive. It consumes arbitrary input fragments, stages one source event, and drains the resulting target events into arbitrary output fragments. It owns no heap allocations. Every growing region is supplied in `StreamBuffers`.

The runnable example is [`examples/stream.rs`](../examples/stream.rs). The conformance and recovery tests are [`tests/stream.rs`](../tests/stream.rs) and [`tests/sse.rs`](../tests/sse.rs).

## Calling contract

Construct `StreamConfig::new(source, target)`, supply any missing target context, and initialize the buffers. Call `step(input, output, eof)` repeatedly. Advance the input cursor by `Step.consumed` and use only `output[..Step.written]`.

| Status | Caller action |
|---|---|
| `NeedInput` | Supply the next input fragment. |
| `NeedOutput` | Supply more output space and resend only the unconsumed input suffix. |
| `NeedBuffer` | Enlarge the named workspace with the corresponding replacement method, then resume the same unconsumed suffix. |
| `Done` | The source terminated and all staged output has drained. |
| `Failed(error)` | Stop conversion. Previously returned bytes remain committed; this converter cannot restart or choose another path. |

`eof=true` means the supplied fragment is the last fragment. If output fills before all of it is consumed, keep `eof=true` while resubmitting its unconsumed suffix. A zero-length output slice is supported: it applies backpressure and can return `NeedOutput` without writing anything.

`NeedBuffer.needed` is an exact size for decoded event data and completed pending output. For a partially received event, token tape, item slots, IR events or an aggregate arena, it is a lower bound. The caller can choose a larger growth factor or impose an application limit. A configured JSON depth, input or token limit is a hard error; supplying a larger slice does not bypass that limit.

Each completed source event is a transaction. On a capacity failure, item records, aggregate links, counters and output sequence numbers are restored to their pre-event state. The framed event remains available. Growing a buffer does not require refeeding bytes already counted as consumed. The zero-capacity recovery test exercises all seven replaceable workspace classes.

## Routing and event IR

`Direct` invokes the target writer from validated source views without creating an `Event` record. `Ir` first fills the caller's `events` slice, then encodes those records. Tests compare the emitted bytes for both implementations, including interleaved tools and one-byte output buffers.

Direct streaming exists for Chat ↔ Responses, Chat ↔ Anthropic Messages, Chat ↔ Gemini GenerateContent, and same-protocol conversion. Other directions use IR in `Auto`; explicit `Direct` rejects them. The stream's route is fixed before any output is committed. A later unsupported event is an error and never causes replay through another route.

The supported features of these direct directions and their IR counterparts are the same because both use the same semantic sink contract. There are no additional IR-only streaming features hidden behind a late fallback. Protocol version, profile and policy are validated before conversion; source event semantics are validated as events arrive. Same-protocol streaming validates and re-encodes events; it does not claim byte-preserving `Identity`.

For manual IR inspection, `decode_event` decodes a single standard-profile JSON payload. `decode_event_format` also accepts an explicit vendor profile. Each `EventText` references a byte span in that payload, and declares whether it is a JSON string or decoded UTF-8. The caller can inspect or modify the event records and call `stage_events` on an IR converter. The payload is borrowed only for that call. Staged output is then drained with `step(&[], output, false)`. Edited spans are bounds-checked; edited strings are validated again.

Event IR represents starts, text/tool/reasoning/refusal deltas, signatures, block endings, cumulative usage, choice endings, final status, failures and final snapshots. `Usage.input_basis` distinguishes total prompt tokens from an uncached count with separate cache reads and writes. `output_basis` distinguishes inclusive output from visible output with a separate thought count. These distinctions are explicit in the IR rather than inferred from an undocumented vendor assumption.

## Context requirements

| Target or source feature | Required external information |
|---|---|
| Target requires an ID/model absent from the source | `StreamContext.response_id` / `model`. |
| Chat, Completions or Responses target, source has no integer creation time | `StreamContext.created`. No clock is read. |
| Responses target | `item_id_prefix`, used with the target item index to create local item IDs. |
| Anthropic target from another source | `initial_usage.input`, known before the first emitted event. Cache details can also be supplied. |
| Legacy Chat `function_call` or Gemini call without an ID | A matching `ToolId` entry. |
| An explicitly present partial usage set cannot populate a required target usage object | Supply the missing counters; they are not guessed. |

Context input-token counts use the canonical total, including separately reported cache reads and writes. The Anthropic writer subtracts these details when populating its uncached `input_tokens`. Its initial output-token count can be zero because no target output has yet been emitted. Final counters use actual supplied or parsed values.

Response IDs are correlation data. A copied response ID is not authorization to resume a conversation on another provider. Local Responses item IDs are scoped by the caller's prefix. Actual tool call IDs are preserved independently of these generated item IDs. Server-side conversation references belong to the request/context API, outside the SSE response converter.

## Implemented stream coverage

The transport framing for all adapters is SSE. Gemini GenerateContent uses the API's `alt=sse` response representation; a bare JSON array or newline-delimited JSON is not SSE input.

| Protocol | Input/output streaming support | Limits that produce explicit errors |
|---|---|---|
| OpenAI Completions | Text choices, integer indices, finish reasons, usage, `[DONE]`, error envelopes | Tool calls, media and non-null logprobs cannot be represented by this text protocol. |
| OpenAI Chat Completions | Text/refusal deltas, new `tool_calls`, legacy `function_call` with caller ID, multiple choices, usage and `[DONE]` | A tool header must supply a complete function name before its argument deltas. Unknown vendor fields require an implemented profile. |
| OpenAI Responses | Created lifecycle, text/refusal content parts, function-call items and arguments, final snapshots, usage and completion/incomplete/failure status | Reasoning summary snapshot variants, server-side tools and streamed media are rejected. Complete final snapshots are emitted when this is the target. |
| Anthropic Messages | Message/content-block lifecycle, text, tool input JSON fragments, thinking and same-origin signatures, cumulative usage, stops, ping and errors | Unsupported server-tool blocks, citation payloads and detailed usage breakdowns are rejected. Multiple choices cannot be encoded. |
| Gemini GenerateContent | Candidates and indices, text/thought parts, complete function-call objects, finish reasons, usage and stream EOF | Streamed media, server-tool parts, function-bound signatures and partial-argument extension schemas are rejected. |
| Gemini Interactions v1 | Independent `interaction.created`, `step.start/delta/stop`, `interaction.completed` model-output, thought and function-call steps; usage/errors/final statuses | Agent/server-tool steps and streamed media are rejected. The older content-event schema is not accepted as v1. |

These rows describe stream features, not a promise that every target can express every source feature. Multiple choices are representable in Chat, Completions and GenerateContent. Anthropic, Responses and Interactions reject nonzero choice indices. Refusal deltas are representable in Chat and Responses. Textual reasoning can be represented in Anthropic, GenerateContent, Interactions and explicitly profiled compatible Chat. Opaque signatures cannot cross provider/profile identities.

`Profile::OpenAiCompatible("provider-name")` is supported for Chat streaming. It enables the recognized `reasoning_content` / `reasoning` extensions. Standard Chat rejects those fields. The writer emits `reasoning_content` only for an explicitly compatible Chat target; it never injects that field into the standard profile. Arbitrary vendor field names remain unsupported.

## Usage and explicit losses

Usage events replace the counters they contain. They are not summed across events. In particular, an Anthropic output-token update from 2 to 7 produces 7, not 9. Arithmetic is integer-only and checked. Cache reads and writes are included when normalizing Anthropic prompt totals; encoding Anthropic reverses that normalization. Gemini visible output and thought counts are added for canonical/OpenAI output, then separated again for Gemini targets. Source bases are retained so a partial cache/thought update can recompute the total without double-counting.

The default policy rejects losses. `drop_reasoning` permits dropping unsupported textual reasoning, with the number of dropped deltas in `Report.dropped_reasoning`. `drop_extensions` permits only the field classes listed below. The report includes both a count and a `dropped_fields` bit mask using `DroppedField`.

| Bit | Dropped field |
|---|---|
| `SystemFingerprint` | `system_fingerprint` |
| `ServiceTier` | `service_tier` |
| `SafetyRatings` | Gemini `safetyRatings` |
| `FinishMessage` | Gemini `finishMessage` |
| `AverageLogprobs` | Gemini `avgLogprobs` |
| `CacheCreationTokens` | Separate `cache_creation_input_tokens` detail in a target without this counter; the canonical total still includes these tokens. |

The opt-in policy does not permit unknown events, malformed JSON, namespace-bound signatures, fabricated usage or silent tool/result removal. Errors retain their classification and never trigger IR fallback.

## Memory accounting

`memory()` separates the converter structure, every caller buffer's capacity, and observed utilization. Capacity fields are bytes except that `items_used` and `tokens_peak` are record counts. `aggregate_peak` includes transient contiguous tool-argument validation space; `aggregate_used` is the retained arena length.

| Region | Growth and purpose |
|---|---|
| `framing` | One source SSE event, normalized line delimiters. |
| `data` | That event's joined and UTF-8-decoded data payload. |
| `pending` | Target events generated by one source event. Required final snapshots may make this larger than one incremental event. |
| `tokens` | Compact JSON indices for the current event; reused to validate completed tool arguments. |
| `items` | Bounded item identities, indices, lifecycle and aggregation links for one response. |
| `checkpoints` | One copy of the item slots for transactional rollback. |
| `events` | IR records for one source event; Direct does not use this slice. |
| `aggregate` | IDs, names, tool JSON and target-required final snapshots; optional exact source snapshot verification. |

Text paths without required snapshots do not accumulate the answer. The regression test compares short and 4096-byte Responses→Chat answers and finds identical retained aggregate usage. Tool arguments are retained until completion for strict JSON validation; interleaved tools use append-only linked fragments. Final validation temporarily needs a contiguous copy when a tool has multiple fragments. This storage cost is explicit.

Responses targets must emit complete text and argument snapshots, including the final response object's output. Their aggregate storage therefore grows with those contents. `validate_snapshots=true` additionally retains source Responses text for byte-for-byte final-snapshot checks even if the target does not need it. With the default `false`, deltas are authoritative, final snapshots are not appended, and their lengths and lifecycle are checked. A target that already requires aggregation performs exact checks.

Framing does not rescan already consumed input. Fragment append is constant work plus the newly decoded bytes; the final snapshots traverse the retained fragments. Active-item lookup is linear in the caller-bounded item count. It is not a function of the number of network fragments. No giant internal stack array substitutes for caller-provided storage.

## EOF, failures and signatures

The standalone SSE decoder follows the WHATWG rule that EOF does not dispatch a partial event. It accepts CR, LF and CRLF, one leading BOM, comments and multiple data lines. Its payload and `*_into` metadata methods apply UTF-8 replacement decoding. Raw metadata views are also available. Last-Event-ID persistence and reconnect scheduling belong to the transport caller.

The protocol converter adds a stricter completion contract. Completions/Chat require `[DONE]`; Anthropic requires `message_stop`; Responses and Interactions require a terminal lifecycle event. GenerateContent completes at EOF after finish reasons for its choices. Premature EOF or a residual incomplete SSE event returns `TruncatedStream`. This includes an unterminated trailing comment; a completed comment event is harmless. Repeated terminal events or new data events after termination return `AlreadyFinished`.

An upstream error event is converted to the target's error envelope and ends the stream. A library parsing, semantic or capacity error is never formatted as an invented upstream model failure. Cancellation is an explicit Event IR final status; a target without a representable terminal status reports `Unsupported`.

In particular, `response.cancelled` is not a Responses SSE terminal event in the documented stream union. Cancellation cannot be encoded by borrowing the similarly named webhook event. Length and safety endings do map to `response.incomplete`, retaining `max_output_tokens` versus `content_filter` in `incomplete_details`.

## Protocol evidence

The implementation was checked against official documentation retrieved on **2026-11-10**:

- [WHATWG SSE parsing and interpretation](https://html.spec.whatwg.org/multipage/server-sent-events.html#parsing-an-event-stream), Living Standard last updated 2026-11-09: line handling, BOM, data joining, UTF-8 and EOF rules.
- [Anthropic streaming messages](https://platform.claude.com/docs/en/build-with-claude/streaming): content-block lifecycle, partial tool JSON, thinking signatures and cumulative usage.
- [Gemini Interactions streaming](https://ai.google.dev/gemini-api/docs/streaming): step-based events and partial argument strings.
- [Gemini Interactions v1 reference](https://ai.google.dev/api/interactions-api-v1): independent interaction resource, steps, usage and statuses.
- OpenAI and GenerateContent references, together with the exact historical design-reference commit, are recorded in [`SOURCES.md`](SOURCES.md).

## Evidence review

| Claim | Executable evidence |
|---|---|
| Arbitrary fragmentation and backpressure preserve bytes | All tested split sizes 1–97 and one-byte output match the unfragmented result. |
| Direct bypasses Event IR | Direct reports zero IR events; forced IR records events and produces identical bytes. |
| Capacity recovery is atomic | Every workspace starts empty, grows on its classified error, and matches the pre-sized conversion. |
| Tool IDs and argument order survive interleaving | Two interleaved calls round-trip through Responses, Anthropic, GenerateContent and Interactions. |
| Usage is not double-counted | Cumulative output updates and cache-read/write round trips have exact expected integers. |
| Final snapshots are not duplicated | Text appears once after round trips; exact mode rejects altered final content. |
| Vendor fields remain explicit | Standard reasoning extensions fail; compatible-profile output is tested separately. |
| Public edited IR is safe | Invalid span arithmetic and bounds are rejected before encoding. |

The design review checks API clarity, representation limits, memory costs, adversarial input and claim/evidence alignment. Unsupported cells remain explicit errors; the tests do not justify claiming support for streamed media or managed-agent events.
