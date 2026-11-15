# Resource API contracts

The `api` module validates and writes OpenAI resource messages without importing
an SDK. A conversation converter changes the representation of model messages;
a resource contract describes the bytes sent to a file, batch, embedding or image
endpoint. Those are separate jobs, so the six `Protocol` variants are unchanged.

Every input is borrowed. Token arrays, multipart boundaries, output buffers and
HTTP transport belong to the caller. The crate performs no network operation,
allocation, tokenization, image decoding, base64 decoding or provider-side task.
The source specifications are linked in [SOURCES.md](SOURCES.md).

## Contract inventory

| API | Request support | Response support | Caller responsibility |
|---|---|---|---|
| Embeddings | Single text, text arrays, token IDs and token batches; model, dimensions, float/base64 and user | Ordered embedding items, indexes, float/base64 vectors, model and integral usage | Tokenize text when token IDs are desired; choose model-supported dimensions; decode base64 if needed |
| Batch | Create; retrieve/list/cancel method/path metadata; nonstream input JSONL | Batch state, counts, file IDs, errors, usage, lists and result/error JSONL | Upload source file, ensure unique custom IDs across the file, validate endpoint-specific bodies, poll and download output |
| Files | Binary multipart upload, purpose and optional expiry; retrieve/list/delete/content method/path metadata | File metadata, nullable byte count, expiry, pagination and delete confirmation | HTTP transfer; treat content-download bytes according to their actual MIME type |
| Images | Generation JSON; image edit JSON references or binary multipart; legacy variation multipart | URL/base64 output, revised prompts, metadata and usage; partial/completed generation/edit SSE payloads | Model capabilities, media limits, actual image processing and network streaming |

`Endpoint::request_schema()` returns a JSON contract where one exists. Image edits
also accept multipart; Files upload and legacy variations are multipart-only.
`FileContent` has no response JSON schema. It is a raw byte response. The legacy
variation endpoint is retained for wire interoperability, not an availability
claim about a retired provider operation.

## Parse, inspect and preserve

```rust
use zconv::api::{self, Schema};
use zconv::json::{Limits, Token};

let input = br#"{"object":"file","id":"file_1","filename":"input.jsonl","purpose":"batch","bytes":null,"created_at":1,"future_field":{"x":1}}"#;
let mut tokens = [Token::EMPTY; 64];
let doc = api::parse(Schema::File, input, &mut tokens, Limits::default())?;
assert!(doc.id().unwrap().eq_str("file_1"));
assert!(doc.field("bytes").unwrap().is_null());
let mut output = [0; 512];
let n = doc.encode(&mut output)?;
assert_eq!(&output[..n], input);
# Ok::<(), zconv::Error>(())
```

`Document` borrows the input and token tape. Do not reuse that tape while using
the view. `Document::raw()` detaches a validated JSON fragment which borrows only
the input bytes. Fields that are unknown to this release remain available through
`field`/`value` and survive raw encoding; that does not assert their semantics are
understood. Known fields and required resource discriminators are checked. JSON
duplicate keys, including escape-equivalent names, remain invalid.

`encode_object` constructs arbitrary supported resource objects from `FieldValue`
entries, then validates the result against a chosen schema. `FieldValue::Json`
requires an already parsed `RawValue`; arbitrary bytes cannot masquerade as JSON.
On any error, discard provisional output. `OutputTooSmall` reports the exact size
needed; token limits and semantic errors do not cause hidden buffer growth.

## Typed embedding builders

`EmbeddingsRequest::new` accepts an `EmbeddingInput`: `Text`, `Texts`, `Tokens` or
`TokenBatches`. It writes JSON directly into caller storage. Empty inputs, empty
token batches and zero dimensions are rejected. Optional `Encoding::Float` and
`Encoding::Base64` select the requested vector representation.

`EmbeddingsResponse` writes borrowed `Embedding` items. `EmbeddingVector::Floats`
contains `f32` values; non-finite values fail instead of producing invalid JSON.
`Base64` preserves the supplied string. A parsed response exposes each vector
through `Document::embedding`, and the raw JSON number spellings are unchanged.
These APIs do not normalize vector lengths or claim to validate a model's shape.

## Batch JSONL

`encode_batch_input(custom_id, url, body, output)` accepts a parsed JSON object,
requires a nonempty custom ID and API path, and writes a POST envelope. It rejects
`stream:true`. Whitespace outside JSON strings is compacted so a pretty-printed
input still produces one physical JSONL line. String escapes and content remain
unchanged. Append a newline when assembling the file.

`JsonLines` yields borrowed lines, accepts LF/CRLF and a terminal newline, and
rejects blank records. Parse each line as `Schema::BatchInput` or
`Schema::BatchOutput` using a reusable token tape. Envelope validation does not
validate the entire model request inside `body`; use the relevant conversation
or resource adapter. File-wide ID uniqueness and request-count limits require
caller state and are not invented by a stateless line iterator.

## Multipart and binary content

`FileUpload` provides a typed upload with filename, MIME type, bytes, purpose and
optional expiry seconds. `encode_multipart` also supports image edits and the
legacy variation format using borrowed `Part` values. A filename-bearing part is
binary and is never decoded as UTF-8. Ordinary fields must contain UTF-8.

Choose a boundary of 1–70 allowed RFC 2046 characters and send a matching header,
for example `Content-Type: multipart/form-data; boundary="chosen-boundary"`.
Boundaries that occur at a potential delimiter in a part are rejected before any
output is written. Header control characters, quotes and backslashes are rejected;
choose an acceptable transport filename rather than inserting raw quoted header
text. Repeated `image[]` parts are allowed; duplicate scalar fields are rejected.

Use `Endpoint::write_path` to build paths. IDs are percent-encoded as one segment,
so an embedded slash does not alter routing. `ListQuery::encode` writes escaped
pagination fields starting with `?`; a zero limit is invalid. The caller combines
these bytes with a provider base URL and chooses appropriate endpoint-specific
pagination limits.

## Image streaming

Parse `image_generation.partial_image`, `image_generation.completed`,
`image_edit.partial_image` and `image_edit.completed` as
`Schema::ImageStreamEvent`. Partial events require an index. `Document::encode_sse`
frames the validated payload with an event discriminator and `data:` lines,
including when the source JSON contains formatting newlines. The existing `sse`
framer can decode fragmented transport input; these resource events are separate
from conversation `StreamConverter` events. Unknown image event types fail
explicitly so they cannot be mistaken for successful completion.

## Verification

`tests/resource_api.rs` exercises realistic response shapes, JSONL, unknown-field
preservation, malformed known fields, path escaping, binary multipart content,
image SSE and exact output capacities. `tests/allocation.rs` measures the builders,
parse/encode paths and multipart upload with a positive-controlled allocator.
`examples/resources.rs` combines an embedding request, Batch JSONL and Files
upload. `benches/resources.rs` measures resource parse/validation/encoding; its
results describe local CPU work, not model latency or remote API availability.
