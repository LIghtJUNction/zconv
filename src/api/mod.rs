//! Borrowed resource and decision API messages, endpoint metadata and multipart bodies.
//!
//! These APIs complement the model-conversation [`crate::Protocol`] adapters.
//! They validate and write wire data without networking, allocation or an SDK.
//! [`Document`] borrows both input and the caller's JSON token tape; [`Document::raw`]
//! detaches a validated fragment from the tape. Unknown object fields are preserved.

mod embeddings;
mod multipart;
mod validate;

pub use embeddings::{
    Embedding, EmbeddingInput, EmbeddingVector, EmbeddingsRequest, EmbeddingsResponse, Encoding,
};
pub use multipart::{FileUpload, Part, encode_multipart};

use crate::{
    Error,
    json::{self, JsonString, Kind, RawValue, Token, Value, Writer},
};

/// A resource or decision endpoint. Paths exclude a caller's base URL.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Endpoint {
    SystemOne,
    OpenAiDecisions,
    Embeddings,
    ImageGenerations,
    ImageEdits,
    /// Wire compatibility for the legacy image variation contract.
    ImageVariations,
    BatchCreate,
    BatchRetrieve,
    BatchList,
    BatchCancel,
    FileUpload,
    FileRetrieve,
    FileList,
    FileDelete,
    FileContent,
}

impl Endpoint {
    pub const fn method(self) -> &'static str {
        match self {
            Self::BatchRetrieve
            | Self::BatchList
            | Self::FileRetrieve
            | Self::FileList
            | Self::FileContent => "GET",
            Self::FileDelete => "DELETE",
            _ => "POST",
        }
    }

    /// The operation suffix, without the API version or any base path.
    pub const fn decision_operation(self) -> Option<&'static str> {
        match self {
            Self::SystemOne => Some("systemone"),
            Self::OpenAiDecisions => Some("decisions"),
            _ => None,
        }
    }

    /// Write a path, percent-encoding resource IDs as one path segment.
    pub fn write_path(self, id: Option<&str>, output: &mut [u8]) -> Result<usize, Error> {
        let (base, needs_id, suffix) = match self {
            Self::SystemOne => ("/v1/systemone", false, ""),
            Self::OpenAiDecisions => ("/v1/decisions", false, ""),
            Self::Embeddings => ("/v1/embeddings", false, ""),
            Self::ImageGenerations => ("/v1/images/generations", false, ""),
            Self::ImageEdits => ("/v1/images/edits", false, ""),
            Self::ImageVariations => ("/v1/images/variations", false, ""),
            Self::BatchCreate | Self::BatchList => ("/v1/batches", false, ""),
            Self::BatchRetrieve => ("/v1/batches/", true, ""),
            Self::BatchCancel => ("/v1/batches/", true, "/cancel"),
            Self::FileUpload | Self::FileList => ("/v1/files", false, ""),
            Self::FileRetrieve | Self::FileDelete => ("/v1/files/", true, ""),
            Self::FileContent => ("/v1/files/", true, "/content"),
        };
        if needs_id && id.is_none_or(str::is_empty) {
            return Err(invalid("id", "nonempty resource ID required"));
        }
        if needs_id && matches!(id, Some("." | "..")) {
            return Err(invalid("id", "URL dot segments are not resource IDs"));
        }
        if !needs_id && id.is_some() {
            return Err(invalid("id", "endpoint does not accept a resource ID"));
        }
        let mut w = Writer::new(output);
        w.raw(base.as_bytes());
        if needs_id {
            write_component(&mut w, id.unwrap_or_default().as_bytes());
        }
        w.raw(suffix.as_bytes());
        w.finish()
    }

    /// JSON request schema; multipart-only and bodyless operations return `None`.
    /// Image edits also support multipart through [`encode_multipart`].
    pub const fn request_schema(self) -> Option<Schema> {
        match self {
            Self::SystemOne => Some(Schema::SystemOneRequest),
            Self::OpenAiDecisions => Some(Schema::OpenAiDecisionsRequest),
            Self::Embeddings => Some(Schema::EmbeddingsRequest),
            Self::ImageGenerations => Some(Schema::ImageGenerationRequest),
            Self::ImageEdits => Some(Schema::ImageEditRequest),
            Self::BatchCreate => Some(Schema::BatchCreateRequest),
            _ => None,
        }
    }

    /// File content is arbitrary bytes and therefore has no JSON schema.
    pub const fn response_schema(self) -> Option<Schema> {
        match self {
            Self::SystemOne => Some(Schema::SystemOneResponse),
            Self::OpenAiDecisions => Some(Schema::OpenAiDecisionsResponse),
            Self::Embeddings => Some(Schema::EmbeddingsResponse),
            Self::ImageGenerations | Self::ImageEdits | Self::ImageVariations => {
                Some(Schema::ImagesResponse)
            }
            Self::BatchCreate | Self::BatchRetrieve | Self::BatchCancel => Some(Schema::Batch),
            Self::BatchList => Some(Schema::BatchList),
            Self::FileUpload | Self::FileRetrieve => Some(Schema::File),
            Self::FileList => Some(Schema::FileList),
            Self::FileDelete => Some(Schema::FileDeleted),
            Self::FileContent => None,
        }
    }
}

/// Validated JSON contracts for the resource APIs and batch JSONL records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Schema {
    SystemOneRequest,
    SystemOneResponse,
    OpenAiDecisionsRequest,
    OpenAiDecisionsResponse,
    EmbeddingsRequest,
    EmbeddingsResponse,
    ImageGenerationRequest,
    /// JSON image references, as accepted by image edits and Batch image edits.
    ImageEditRequest,
    ImagesResponse,
    ImageStreamEvent,
    BatchCreateRequest,
    Batch,
    BatchList,
    BatchInput,
    BatchOutput,
    File,
    FileList,
    FileDeleted,
    Error,
}

#[derive(Clone, Copy, Debug)]
pub struct Document<'a, 't> {
    schema: Schema,
    value: Value<'a, 't>,
}

/// Parse and validate, retaining unknown fields without interpreting their semantics.
pub fn parse<'a, 't>(
    schema: Schema,
    input: &'a [u8],
    tokens: &'t mut [Token],
    limits: json::Limits,
) -> Result<Document<'a, 't>, Error> {
    let document = json::parse(input, tokens, limits)?;
    Document::decode(schema, document.root())
}

impl<'a, 't> Document<'a, 't> {
    pub fn decode(schema: Schema, value: Value<'a, 't>) -> Result<Self, Error> {
        validate::validate(schema, value)?;
        Ok(Self { schema, value })
    }
    pub const fn schema(self) -> Schema {
        self.schema
    }
    pub const fn value(self) -> Value<'a, 't> {
        self.value
    }
    pub fn raw(self) -> RawValue<'a> {
        self.value.raw_value()
    }
    pub fn field(self, key: &str) -> Option<Value<'a, 't>> {
        self.value.get(key)
    }
    /// Copy the validated message with all unknown fields and number spellings intact.
    pub fn encode(self, output: &mut [u8]) -> Result<usize, Error> {
        let mut w = Writer::new(output);
        w.raw(self.value.raw());
        w.finish()
    }
    /// Write a decision request with a gateway-selected upstream model.
    /// Other fields retain their original JSON values, including extensions.
    pub fn encode_decision_model(self, model: &str, output: &mut [u8]) -> Result<usize, Error> {
        if !matches!(
            self.schema,
            Schema::SystemOneRequest | Schema::OpenAiDecisionsRequest
        ) {
            return Err(invalid("schema", "expected a decision request"));
        }
        if model.is_empty() || model.chars().any(char::is_control) {
            return Err(invalid(
                "model",
                "expected a nonempty model without control characters",
            ));
        }
        let mut writer = Writer::new(output);
        writer.raw(b"{\"model\":");
        writer.string(model.as_bytes());
        for (key, value) in self.value.object() {
            if !key.eq_str("model") {
                writer.byte(b',');
                writer.json_string(key);
                writer.byte(b':');
                writer.raw(value.raw());
            }
        }
        writer.byte(b'}');
        writer.finish()
    }

    /// Frame a validated image stream event as SSE. Pretty-printed JSON remains
    /// valid: each physical line receives a `data:` prefix, and CR/LF normalize
    /// to SSE line endings. The caller owns transport and incremental decoding.
    pub fn encode_sse(self, output: &mut [u8]) -> Result<usize, Error> {
        if self.schema != Schema::ImageStreamEvent {
            return Err(invalid("schema", "expected image stream event"));
        }
        let ty = required(self.value, "type")?
            .as_string()
            .ok_or(invalid("type", "expected string"))?;
        let mut w = Writer::new(output);
        w.raw(b"event: ");
        for byte in ty.bytes() {
            w.byte(byte);
        }
        w.byte(b'\n');
        let raw = self.value.raw();
        let mut start = 0;
        let mut cursor = 0;
        while cursor < raw.len() {
            if matches!(raw[cursor], b'\r' | b'\n') {
                w.raw(b"data: ");
                w.raw(&raw[start..cursor]);
                w.byte(b'\n');
                if raw[cursor] == b'\r' && raw.get(cursor + 1) == Some(&b'\n') {
                    cursor += 1;
                }
                start = cursor + 1;
            }
            cursor += 1;
        }
        w.raw(b"data: ");
        w.raw(&raw[start..]);
        w.byte(b'\n');
        w.byte(b'\n');
        w.finish()
    }
    pub fn id(self) -> Option<JsonString<'a>> {
        self.field("id").and_then(Value::as_string)
    }
    pub fn model(self) -> Option<JsonString<'a>> {
        self.field("model").and_then(Value::as_string)
    }
    pub fn data(self) -> Option<json::Array<'a, 't>> {
        self.field("data")
            .filter(|v| v.kind() == Kind::Array)
            .map(Value::array)
    }
    pub fn usage(self) -> Option<Value<'a, 't>> {
        self.field("usage").filter(|v| !v.is_null())
    }
    pub fn status(self) -> Option<JsonString<'a>> {
        self.field("status").and_then(Value::as_string)
    }
    pub fn has_more(self) -> Option<bool> {
        self.field("has_more").and_then(Value::as_bool)
    }
    pub fn batch_body(self) -> Result<RawValue<'a>, Error> {
        if self.schema != Schema::BatchInput {
            return Err(invalid("schema", "expected batch input"));
        }
        Ok(required(self.value, "body")?.raw_value())
    }
    pub fn embedding(self, index: usize) -> Result<EmbeddingView<'a, 't>, Error> {
        if self.schema != Schema::EmbeddingsResponse {
            return Err(invalid("schema", "expected embeddings response"));
        }
        let item = required(self.value, "data")?
            .array()
            .nth(index)
            .ok_or(invalid("index", "embedding index outside data"))?;
        Ok(EmbeddingView { value: item })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct EmbeddingView<'a, 't> {
    value: Value<'a, 't>,
}

impl<'a, 't> EmbeddingView<'a, 't> {
    pub fn index(self) -> u64 {
        self.value.get("index").and_then(Value::as_u64).unwrap_or(0)
    }
    pub fn vector(self) -> Value<'a, 't> {
        self.value.get("embedding").unwrap_or(self.value)
    }
    pub fn is_base64(self) -> bool {
        self.vector().kind() == Kind::String
    }
}

/// Values for constructing resource objects, including optional/future fields.
/// `Json` can only come from a successfully parsed JSON document.
#[derive(Clone, Copy, Debug)]
pub enum FieldValue<'a> {
    Text(&'a str),
    Unsigned(u64),
    Bool(bool),
    Null,
    Json(RawValue<'a>),
}

/// Encode and validate a newly constructed message. On error discard output bytes.
/// Exact required output size is returned before the validation pass when too small.
pub fn encode_object(
    schema: Schema,
    fields: &[(&str, FieldValue<'_>)],
    tokens: &mut [Token],
    output: &mut [u8],
) -> Result<usize, Error> {
    let mut w = Writer::new(output);
    w.byte(b'{');
    for (i, (name, value)) in fields.iter().enumerate() {
        if i != 0 {
            w.byte(b',');
        }
        w.string(name.as_bytes());
        w.byte(b':');
        match value {
            FieldValue::Text(s) => w.string(s.as_bytes()),
            FieldValue::Unsigned(n) => write_u64(&mut w, *n),
            FieldValue::Bool(b) => w.raw(if *b { b"true" } else { b"false" }),
            FieldValue::Null => w.raw(b"null"),
            FieldValue::Json(v) => w.raw(v.raw()),
        }
    }
    w.byte(b'}');
    let n = w.finish()?;
    parse(schema, &output[..n], tokens, json::Limits::default())?;
    Ok(n)
}

/// Borrow one complete JSONL record at a time; no whole-file token tape is needed.
/// Empty records are errors; one terminal newline is permitted. CRLF is accepted.
#[derive(Clone, Debug)]
pub struct JsonLines<'a> {
    remaining: &'a [u8],
    line: usize,
}

impl<'a> JsonLines<'a> {
    pub const fn new(input: &'a [u8]) -> Self {
        Self {
            remaining: input,
            line: 0,
        }
    }
    pub const fn line_number(&self) -> usize {
        self.line
    }
}

impl<'a> Iterator for JsonLines<'a> {
    type Item = Result<&'a [u8], Error>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining.is_empty() {
            return None;
        }
        self.line += 1;
        let end = self
            .remaining
            .iter()
            .position(|b| *b == b'\n')
            .unwrap_or(self.remaining.len());
        let line = self.remaining[..end]
            .strip_suffix(b"\r")
            .unwrap_or(&self.remaining[..end]);
        self.remaining = if end == self.remaining.len() {
            &[]
        } else {
            &self.remaining[end + 1..]
        };
        Some(if line.iter().all(u8::is_ascii_whitespace) {
            Err(invalid("JSONL", "empty record"))
        } else {
            Ok(line)
        })
    }
}

/// Build a batch request line around a parsed object, rejecting streaming bodies.
/// The endpoint-specific body schema and file-wide custom ID uniqueness belong
/// to the caller; this validates the Batch envelope, not a model's capabilities.
pub fn encode_batch_input(
    custom_id: &str,
    url: &str,
    body: Value<'_, '_>,
    output: &mut [u8],
) -> Result<usize, Error> {
    if custom_id.is_empty() {
        return Err(invalid("custom_id", "must not be empty"));
    }
    if !url.starts_with("/v1/") || url.bytes().any(|b| b.is_ascii_control()) {
        return Err(invalid("url", "expected an API path"));
    }
    validate::batch_body(body)?;
    let mut w = Writer::new(output);
    w.raw(b"{\"custom_id\":");
    w.string(custom_id.as_bytes());
    w.raw(b",\"method\":\"POST\",\"url\":");
    w.string(url.as_bytes());
    w.raw(b",\"body\":");
    // JSONL requires one physical line even when the parsed body was pretty
    // printed. Its validated strings cannot contain unescaped control bytes.
    let mut in_string = false;
    let mut escaped = false;
    for &byte in body.raw() {
        if in_string {
            w.byte(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else if byte == b'"' {
            in_string = true;
            w.byte(byte);
        } else if !byte.is_ascii_whitespace() {
            w.byte(byte);
        }
    }
    w.byte(b'}');
    w.finish()
}

/// Pagination/query fields shared by list endpoints; `purpose` applies to Files.
#[derive(Clone, Copy, Debug, Default)]
pub struct ListQuery<'a> {
    pub after: Option<&'a str>,
    pub limit: Option<u32>,
    pub purpose: Option<&'a str>,
    pub descending: Option<bool>,
}

impl ListQuery<'_> {
    pub fn encode(self, output: &mut [u8]) -> Result<usize, Error> {
        if self.limit == Some(0) {
            return Err(invalid("limit", "must be positive"));
        }
        let mut w = Writer::new(output);
        let mut sep = b'?';
        for (name, value) in [
            ("after", self.after),
            ("purpose", self.purpose),
            (
                "order",
                self.descending.map(|d| if d { "desc" } else { "asc" }),
            ),
        ] {
            if let Some(value) = value {
                w.byte(sep);
                sep = b'&';
                w.raw(name.as_bytes());
                w.byte(b'=');
                write_component(&mut w, value.as_bytes());
            }
        }
        if let Some(limit) = self.limit {
            w.byte(sep);
            w.raw(b"limit=");
            write_u64(&mut w, u64::from(limit));
        }
        w.finish()
    }
}

pub(crate) fn invalid(field: &'static str, reason: &'static str) -> Error {
    Error::InvalidInput { field, reason }
}
pub(crate) fn required<'a, 't>(
    v: Value<'a, 't>,
    field: &'static str,
) -> Result<Value<'a, 't>, Error> {
    v.get(field).ok_or(invalid(field, "required field missing"))
}
pub(crate) fn write_u64(w: &mut Writer<'_>, mut value: u64) {
    let mut buffer = [0u8; 20];
    let mut i = buffer.len();
    loop {
        i -= 1;
        buffer[i] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    w.raw(&buffer[i..]);
}
fn write_component(w: &mut Writer<'_>, input: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &b in input {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            w.byte(b);
        } else {
            w.raw(&[b'%', HEX[(b >> 4) as usize], HEX[(b & 15) as usize]]);
        }
    }
}
