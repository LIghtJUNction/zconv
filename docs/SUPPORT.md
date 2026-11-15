# Support and fidelity matrix

The release implements six distinct adapters. Support means parsing, conversion
and tests exist. It does not mean every vendor feature can be expressed in every
target. The default policy is strict.

**F** = faithful within the documented portable subset. **C** = caller context is
required. **D** = explicit permission and a concrete loss report are required.
**X** = the target has no supported equivalent; conversion fails. An implemented
adapter may return `Unsupported` for a feature outside its declared subset.

## Requests

| Source adapter | Implemented input | Faithful cross-protocol subset | Context / downgrade / rejection |
|---|---|---|---|
| OpenAI Completions | String prompt and generation controls; identity validates string/token batches | F: one user text prompt | C: token-ID prompts need tokenizer and batches need caller expansion before IR; X: flattening arbitrary chat history into a prompt |
| OpenAI Chat | Ordered messages, text blocks, legacy functions/function_call/function role, tools/tool_calls/tool results, media, generation controls | F: text and function semantics; schemas to Responses | C: target-required max_tokens or item IDs; D: provider-only fields; X: unsupported role/order/target features |
| OpenAI Responses | String or item input, omitted/null message content, message/function_call/function_call_output items, typed result content, tools, output constraints, text verbosity and server context | F: ordered items and function semantics; raw reasoning distinct from summaries | C: materialized provider history when changing namespace; D: explicit reference removal; X: server-managed built-in tools without a declared representation |
| Anthropic Messages | System content, ordered content blocks, tool_use/tool_result, text/thinking/media, generation controls | F: portable text/tools/media | C: missing tool names in target results; D/X: signatures and provider controls; version 2026-10-21 rejects removed sampling controls |
| Gemini GenerateContent | contents/parts, systemInstruction, functionDeclarations, functionCall/functionResponse, inlineData/fileData, generationConfig | F: portable text/tools/media | C: model from transport context; C: missing call IDs/names; model/stream returned separately for target transport |
| Gemini Interactions | Independent v1 input/steps model, user_input/model_output, function_call/function_result, tools, generation_config, previous_interaction_id | F: portable model-call semantics | C: materialized stored history; D: explicit context removal; X: agent/background/server-tool features outside model-call conversion |

### Content and structured output

| Semantic feature | Preservation rule |
|---|---|
| Text | Original JSON strings stay borrowed; modifications are escaped correctly |
| Missing / null / empty | Represented separately; same-format IR retains declared supported distinctions; cross-format shape differences follow target semantics |
| Images | URL/base64 mappings exist; OpenAI data URLs are split into native MIME/base64 fields without a session-sized temporary buffer |
| Audio | Chat wav/mp3 input and native MIME/base64/URI descriptions where supported; incompatible target forms fail |
| Audio/video URLs | Typed media references retain URLs without fetching; native targets require a representable MIME/source form |
| Files | URL/native file descriptions where supported; provider file IDs never cross namespaces implicitly |
| JSON Schema output | Chat ↔ Responses normalized JSON object/schema conversion; same-GenerateContent schema controls preserved; other dialect guarantees fail explicitly |
| Function strict defaults | Chat missing/null maps to explicit false in Responses; unresolved Responses missing/null needs a caller IR edit before converting to another protocol; explicit strict true cannot map to a target without the same guarantee |
| Reasoning | Compatible Chat accepts ordered strings/arrays under reasoning_content or reasoning; Responses content is raw thought and summary remains summary; raw thought cannot be relabelled as a Gemini summary; standard Chat rejects undocumented fields |
| Typed tool results | Nested text/media arrays map across Chat, Responses and Anthropic; Gemini uses native functionResponse parts where representable; missing IDs/names/MIME, unsupported media and impossible ordering fail explicitly |
| Agent input defaults | Missing/null Responses content is empty content; null/absent summary becomes an empty summary list; null/empty annotations are accepted; missing/null image detail defaults to auto |
| Text verbosity | Retained independently of structured-output format; targets without a supported verbosity representation require explicit loss policy |
| Stream options | Typed include_usage and continuous_usage_stats survive Chat/Completions encoding; targets without the field return transport_stream_options for the caller |
| Signatures | Opaque provider-owned data stays in its originating format; cross-provider use requires explicit reasoning loss or fails |
| Refusal / safety | Dedicated refusal blocks and finish states retained where the target can express them; no successful answer substituted |
| Extensions | Source protocol/version/profile retained; unknown fields never injected into another provider by default |

## Complete responses

| Source adapter | Implemented representation | Faithful subset | Caller context / explicit loss / rejection |
|---|---|---|---|
| Completions | choices[index].text, finish_reason, usage | F: ordered text alternatives and exact counters | C: required target IDs/timestamps; X: multi-choice to a single-result API |
| Chat | choices/message, modern and legacy tool calls, refusal, finish and usage | F: text/tools/alternatives | C: Responses output item IDs; D: timestamp metadata when target lacks a field |
| Responses | output items, status/error/incomplete_details, tool calls, usage | F: message/tool semantics and completion status where representable | D: item metadata when target lacks it; X: failed/cancelled status in a success-only envelope |
| Anthropic | content blocks, stop_reason/stop_sequence, usage and cache counters | F: text/tools and checked accounting conversion | C: OpenAI timestamp; D/X: source-only metadata and unsupported stop semantics |
| GenerateContent | candidates/index/content, finishReason, usageMetadata, prompt feedback | F: portable candidates and usage | C: required target timestamp/IDs; D: vendor diagnostics; X: multiple candidates in a single-result target |
| Interactions | independent steps, id/model, ISO created/updated, status/error, usage | F: portable steps and status | C: target timestamp representation; D: incompatible timestamp metadata; X: server execution steps without an equivalent |

Output IDs are caller facts. `Context.response_id` identifies the overall result;
`Context.item_ids` supplies distinct Responses output item IDs by ordinal. A tool
`call_id` is never substituted for an output item ID. `Context.tool_calls` supplies
missing name/ID bindings when the source omits a value required by the target.

Usage stays integral. Anthropic uncached input plus cache-read/write counters is
normalized before conversion. Gemini visible-output/thought accounting is
normalized before mapping to inclusive OpenAI counters. Nonportable accounting
details are retained as extensions or rejected/reported under the explicit policy.

## Streaming events

| Source adapter | Implemented stream | Termination | Important boundaries |
|---|---|---|---|
| Completions | SSE text choices and usage | [DONE] | Legacy prompt API has no tool-block semantics |
| Chat | SSE delta text, function/tool argument shards, choices, usage, errors | [DONE] | Reasoning extension requires explicit compatible profile |
| Responses | Typed response/item/content/text/function-argument events, separate reasoning text/summary events and final snapshots | completed / incomplete / failed | Final snapshots are not appended as deltas; required target snapshots use caller aggregation; cancellation has no documented SSE terminal variant |
| Anthropic | message/block lifecycle, thinking, redacted thinking, citations, server-tool blocks/results, detailed usage, ping/error | message_stop | Provider-only payloads preserve source Format and fail cross-provider conversion; server-owned tools never become local function execution |
| GenerateContent | streamGenerateContent with `alt=sse`, candidates/parts/tool calls/usage | finish reasons plus explicit EOF | The raw streamed JSON-array transport is outside the SSE adapter |
| Interactions | interaction.created, step.start/delta/stop, interaction lifecycle/error | interaction terminal event | Separate v1 step lifecycle, independent of GenerateContent |

Text, tool calls/arguments, reasoning in declared profiles, usage, choices,
errors and terminal state have event-level implementations and tests. Version
0.3 adds explicit event representations for Chat-compatible image URL arrays,
GenerateContent grounding metadata and continuation tokens, and Anthropic matched
stop sequences. These are source-owned extensions: a target without an equivalent
must reject them or use an explicit loss policy, never silently approximate them.
Version 0.4 adds same-origin Anthropic provider blocks, deltas and usage fields.
Other streamed media generation blocks and provider event families outside this
matrix remain unsupported. Images resource SSE is handled separately by `api`.

Usage includes observed cache lifetime buckets and compatible-provider receipt
fields without assigning prices, granting authority or deciding charges. Missing
counters remain unknown. Applications retain financial policy and any deliberately
chosen partial-usage presentation.

`UsageEmission::Continuous` attaches the latest observed usage to Chat/Completions
chunks and emits usage-only updates. The default `Final` mode retains terminal
usage behavior. Neither mode fills in unknown counts. Reasoning events distinguish
raw thought from summary, including two part indexes within one Responses item.

`EventEncoderState` offers the same event encoder with non-borrowing persistent
state. The caller retains and may grow item/aggregate storage between calls;
capacity retries discard provisional output and restore committed state. This
API performs no allocation and does not schedule tasks. Successful terminal
emission remains an explicit caller action, so an application can settle before
submitting choice-end and finish events.

## Resource APIs

`api` owns Batch, Files, Embeddings and Images wire contracts. These types are
implemented locally to preserve the zero-dependency contract; they are not SDK
re-exports. Parsing, validated encoding, endpoint paths, JSONL, multipart and image
SSE are supported as detailed in [RESOURCE_APIS.md](RESOURCE_APIS.md). Networking,
file storage, remote job execution and model-specific limits belong to the caller.

## Direct paths

All six Chat-centric directions implement direct requests, complete responses and
supported streams. Common nonstream text, generation and usage shapes use direct
views. Chat ↔ Responses **requests** also directly map supported function
definitions, selection and tool history. Complete tool responses still use IR.
Other nonstream tools/media/reasoning enter IR when the direct feature preflight
rejects them. All other protocol pairs use IR.

Direct and IR obey the same semantic and loss policy. A capability miss can
select IR only before output commitment. A later unsupported stream event fails
without replaying already emitted data.

## Evidence

`tests/protocol.rs` covers all 36 request directions and all 36 complete-response
directions, plus all 25 modern-protocol tool request and response directions.
`tests/direct.rs` compares direct and IR semantic results for the six direct
pairs. `tests/stream.rs` checks fragmentation, interleaving, lifecycle, backpressure,
workspace growth and direct/IR output equality. Unsupported semantics have
negative tests; ordinary success is never used as a substitute for a loss report.
`tests/api_compatibility.rs`, `tests/stream_compatibility.rs` and
`tests/resource_api.rs` cover the 0.4 additions and malformed/ambiguous inputs.
