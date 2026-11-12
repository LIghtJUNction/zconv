# Changelog

## 0.3.0

- Add caller-owned `EventEncoderState` and per-call `EventEncoderBuffers`; buffer growth no longer requires a self-referential application object. Capacity retries roll back state without replaying committed output.
- Add rollback-only `preview` encoding for applications that must prepare terminal bytes before committing an external operation. Committed completion still rejects further events.
- Preserve block signature provenance, Responses reasoning lifecycle and output identity, and Gemini signatures and continuation in their original parts.
- Decode Chat reasoning before text and refusal in responses, accept omitted response assistant roles, tolerate null `tool_calls`/`function_call` deltas, and close Responses content parts whose `done` event carries no part payload. Out-of-range sparse tool indexes fail as resource bounds instead of driving allocation.
- Extend explicit compatible profiles with cache lifetime buckets, raw uncached input, observed cost numbers, Chat image URL arrays, grounding and continuation metadata. Missing usage counters remain unknown. See the support matrix and tests for exact representability.
- Keep the core allocation-free, dependency-free, no_std and safe Rust on Rust 1.99.1.

Compatibility: new public event and usage fields require consumers with exhaustive literals or matches to update. This release uses the next minor version; the stream encoder remains explicit about capacities, terminal events and policy.

## 0.2.0

- Add `decode_frame_format` and `validate_stream_format` for consumers of the public event IR. SSE discriminator and terminal-marker validation use the same implementation as `StreamConverter`.
- Add `UsageAccumulator`; both event consumers and the stream encoder share checked cumulative usage normalization.
- Preserve source output-item IDs separately from tool-call IDs in `Event.item_id`.
- Report exact embedded-JSON scratch capacity with `Error::ScratchTooSmall`.
- Keep Rust 1.74, `no_std`, no allocation, no dependencies and safe Rust.

Compatibility: adding the public `Event.item_id` field requires clients with exhaustive struct literals to add that field or use `..Event::EMPTY`. This release uses the next minor version for that source change. `Event` is 240 bytes on the tested 64-bit target, versus 208 bytes previously. `[DONE]` with a non-default SSE event name is rejected. Target Responses IDs continue to come from caller-selected output identity context.

## 0.1.0

- Six independent AI wire protocol adapters with borrowed request/response IR.
- Auto, Direct and Ir routing with validated identity and narrow fallback.
- Caller-owned token, output, semantic, streaming and aggregation buffers.
- Stable Rust 1.74 MSRV; always no_std; no third-party dependencies or unsafe core.
- Full JSON validation, decoded duplicate-key rejection and bounded resource use.
- Incremental SSE conversion with resumable backpressure and explicit EOF.
- Strict provenance, field-specific downgrade reports and integer usage mapping.
- Native tests, semantic and external JSON differential tests, allocation oracle,
  Miri corpus, fuzz workspaces, reproducible benchmarks and guarded releases.
