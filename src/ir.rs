//! Borrowed, caller-owned semantic representations.
//!
//! Every list is stored in a caller-provided `Record` slice. A message or choice
//! owns the following records up to its `end` index. Raw JSON and string slices
//! borrow the validated input; replacing a `Text` with `Utf8` borrows the
//! replacement. Validated raw fragments borrow input only; the token tape can
//! be reused immediately after decoding.

use crate::{json::{JsonString, RawValue}, Format};

/// Missing, explicit null, and present values are distinct states.
#[derive(Clone, Copy, Debug, Default)]
pub enum Presence<T> {
    #[default]
    Missing,
    Null,
    Value(T),
}

impl<T> Presence<T> {
    pub const fn is_missing(&self) -> bool { matches!(self, Self::Missing) }
    pub const fn is_null(&self) -> bool { matches!(self, Self::Null) }
    pub fn value(self) -> Option<T> { if let Self::Value(v) = self { Some(v) } else { None } }
}

/// An unchanged wire string or caller-provided replacement text.
#[derive(Clone, Copy, Debug)]
pub enum Text<'a> {
    Json(JsonString<'a>),
    Utf8(&'a str),
}

impl<'a> Text<'a> {
    pub fn eq_str(self, s: &str) -> bool {
        match self { Self::Json(v) => v.eq_str(s), Self::Utf8(v) => v == s }
    }
    pub fn as_str(self) -> Option<&'a str> {
        match self { Self::Json(v) => v.as_str(), Self::Utf8(v) => Some(v) }
    }
    pub fn decoded_len(self) -> usize {
        match self { Self::Json(v) => v.decoded_len(), Self::Utf8(v) => v.len() }
    }
}

/// Exact JSON number or an explicitly edited unsigned integer; no float cast.
#[derive(Clone, Copy, Debug)]
pub enum Number<'a> {
    Json(RawValue<'a>),
    Unsigned(u64),
}

impl Number<'_> {
    pub fn as_u64(self) -> Option<u64> {
        match self { Self::Json(v) => v.as_u64(), Self::Unsigned(v) => Some(v) }
    }
}

/// JSON tool arguments are encoded as strings by OpenAI and objects elsewhere.
#[derive(Clone, Copy, Debug)]
pub enum Data<'a> {
    Json(RawValue<'a>),
    /// The decoded string must itself be a valid JSON value before object output.
    JsonText(Text<'a>),
    Text(Text<'a>),
    EmptyObject,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role { System, Developer, User, Assistant, Tool }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentShape { Missing, Null, String, Array }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaKind { Image, Audio, File, Video }

#[derive(Clone, Copy, Debug)]
pub enum MediaSource<'a> {
    Url(Text<'a>),
    DataUrl(Text<'a>),
    Base64 { data: Text<'a>, mime_type: Text<'a> },
    FileId { id: Text<'a>, namespace: Format },
}

#[derive(Clone, Copy, Debug)]
pub struct Media<'a> {
    pub kind: MediaKind,
    pub mime_type: Presence<Text<'a>>,
    pub source: MediaSource<'a>,
    pub detail: Presence<Text<'a>>,
    pub filename: Presence<Text<'a>>,
}

#[derive(Clone, Copy, Debug)]
pub enum Content<'a> {
    Text(Text<'a>),
    Reasoning { text: Presence<Text<'a>>, signature: Presence<Text<'a>> },
    Refusal(Text<'a>),
    Media(Media<'a>),
    ToolCall {
        id: Presence<Text<'a>>,
        name: Text<'a>,
        arguments: Data<'a>,
        legacy: bool,
    },
    ToolResult {
        id: Presence<Text<'a>>,
        name: Presence<Text<'a>>,
        result: Data<'a>,
        is_error: Presence<bool>,
    },
    /// A source-owned block that has no declared cross-protocol representation.
    Opaque { source: Format, value: RawValue<'a> },
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Span { pub start: usize, pub end: usize }

impl Span { pub const EMPTY: Self = Self { start: 0, end: 0 }; }

#[derive(Clone, Copy, Debug)]
pub struct Message<'a> {
    pub role: Role,
    pub legacy_function: bool,
    pub system_instruction: bool,
    pub tool_calls: Presence<()>,
    pub function_call: Presence<()>,
    pub refusal: Presence<()>,
    pub reasoning: Presence<()>,
    pub name: Presence<Text<'a>>,
    pub shape: ContentShape,
    pub end: usize,
    pub extensions: Span,
    /// Responses message/item ID has a different namespace from a tool call ID.
    pub item_id: Presence<Text<'a>>,
    pub item_status: Presence<Text<'a>>,
}

#[derive(Clone, Copy, Debug)]
pub struct Block<'a> {
    pub content: Content<'a>,
    pub extensions: Span,
    pub end: usize,
    pub item_id: Presence<Text<'a>>,
    pub item_status: Presence<Text<'a>>,
}

#[derive(Clone, Copy, Debug)]
pub struct Tool<'a> {
    pub name: Text<'a>,
    pub description: Presence<Text<'a>>,
    pub parameters: Presence<RawValue<'a>>,
    pub strict: Presence<bool>,
    pub extensions: Span,
    pub legacy: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum ToolChoice<'a> {
    Missing,
    Null,
    Auto,
    None,
    Required,
    Named(Text<'a>),
    Opaque { source: Format, value: RawValue<'a> },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Finish { Missing, Null, Stop, Length, ToolCalls, Safety, Refusal, Error, Cancelled, InProgress, Other }

#[derive(Clone, Copy, Debug)]
pub struct Choice<'a> {
    pub index: u64,
    pub finish: Finish,
    pub raw_finish: Presence<Text<'a>>,
    pub end: usize,
    pub extensions: Span,
}

/// Source identity is part of an extension's value, not an optional annotation.
#[derive(Clone, Copy, Debug)]
pub struct Extension<'a> {
    pub source: Format,
    pub key: JsonString<'a>,
    pub value: RawValue<'a>,
    /// Human-readable location plus parent arena index identifies an exact loss.
    pub location: &'static str,
    pub owner: usize,
}

impl Extension<'_> {
    /// Empty standard annotation lists carry no annotation to translate. Their
    /// presence is retained in the IR and restored for the original format.
    pub fn is_empty_annotation(&self) -> bool {
        self.location == "content" && self.key.eq_str("annotations")
            && self.value.kind() == crate::json::Kind::Array
            && self.value.raw().iter().filter(|b| !b.is_ascii_whitespace()).copied().eq(b"[]".iter().copied())
    }
}

/// Compact arena entries. Initialise caller storage with `[Record::EMPTY; N]`.
#[derive(Clone, Copy, Debug)]
pub enum Record<'a> {
    Empty,
    Message(Message<'a>),
    Block(Block<'a>),
    Tool(Tool<'a>),
    Choice(Choice<'a>),
    Extension(Extension<'a>),
}

impl Record<'_> { pub const EMPTY: Self = Self::Empty; }
impl Default for Record<'_> { fn default() -> Self { Self::EMPTY } }

#[derive(Clone, Copy, Debug, Default)]
pub struct Parameters<'a> {
    pub temperature: Presence<RawValue<'a>>,
    pub top_p: Presence<RawValue<'a>>,
    pub top_k: Presence<RawValue<'a>>,
    pub max_output_tokens: Presence<RawValue<'a>>,
    pub stop: Presence<RawValue<'a>>,
    pub seed: Presence<RawValue<'a>>,
    pub frequency_penalty: Presence<RawValue<'a>>,
    pub presence_penalty: Presence<RawValue<'a>>,
    pub n: Presence<RawValue<'a>>,
    pub stream: Presence<RawValue<'a>>,
    pub parallel_tool_calls: Presence<RawValue<'a>>,
    pub extensions: Span,
}

#[derive(Clone, Copy, Debug)]
pub struct ContextRef<'a> { pub source: Format, pub id: Text<'a> }

#[derive(Clone, Copy, Debug)]
pub struct OutputConstraint<'a> {
    pub source: Format,
    pub value: RawValue<'a>,
    pub mode: OutputMode,
    pub schema: Presence<RawValue<'a>>,
    pub name: Presence<Text<'a>>,
    pub description: Presence<Text<'a>>,
    pub strict: Presence<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputMode { Text, JsonObject, JsonSchema, Opaque }

/// Separate request header; no response-only fields are present.
pub struct Request<'a, 's> {
    pub source: Format,
    pub model: Presence<Text<'a>>,
    pub tools: Presence<()>,
    pub functions: Presence<()>,
    pub parameters: Parameters<'a>,
    pub tool_choice: ToolChoice<'a>,
    pub context: Presence<ContextRef<'a>>,
    pub output_constraint: Presence<OutputConstraint<'a>>,
    pub extensions: Span,
    pub records: &'s mut [Record<'a>],
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Usage<'a> {
    pub input_tokens: Presence<Number<'a>>,
    pub output_tokens: Presence<Number<'a>>,
    pub total_tokens: Presence<Number<'a>>,
    pub cached_tokens: Presence<Number<'a>>,
    pub cache_write_tokens: Presence<Number<'a>>,
    pub reasoning_tokens: Presence<Number<'a>>,
    pub extensions: Span,
    pub present: bool,
    pub is_null: bool,
}

/// Complete responses carry ordered choices and blocks independently of requests.
pub struct Response<'a, 's> {
    pub source: Format,
    pub id: Presence<Text<'a>>,
    pub model: Presence<Text<'a>>,
    pub created: Presence<Number<'a>>,
    pub created_iso: Presence<Text<'a>>,
    pub updated_iso: Presence<Text<'a>>,
    pub status: Finish,
    pub usage: Usage<'a>,
    pub error: Presence<RawValue<'a>>,
    pub stop_sequence: Presence<Text<'a>>,
    pub incomplete_details: Presence<RawValue<'a>>,
    pub extensions: Span,
    pub records: &'s mut [Record<'a>],
}

/// Tool-name/ID mapping supplied when a source protocol omits a required value.
#[derive(Clone, Copy, Debug)]
pub struct ToolBinding<'a> {
    pub id: &'a str,
    pub name: &'a str,
}

/// Facts owned by the caller, never synthesized by the converter.
#[derive(Clone, Copy, Debug, Default)]
pub struct Context<'a> {
    pub model: Option<&'a str>,
    pub response_id: Option<&'a str>,
    pub created: Option<u64>,
    pub created_iso: Option<&'a str>,
    pub updated_iso: Option<&'a str>,
    pub max_tokens: Option<u64>,
    pub tool_calls: &'a [ToolBinding<'a>],
    /// Output item IDs by emitted ordinal. These are distinct from tool call IDs.
    pub item_ids: &'a [&'a str],
}

/// Each permission is narrow. All permissions are disabled by default.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Policy {
    pub drop_extensions: bool,
    pub drop_reasoning: bool,
    pub drop_context: bool,
    pub drop_metadata: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct EncodeReport<'a> {
    pub written: usize,
    pub dropped_extensions: usize,
    pub dropped_reasoning: usize,
    pub dropped_context: usize,
    pub dropped_metadata: usize,
    pub metadata_loss_fields: u32,
    pub transport_model: Presence<Text<'a>>,
    pub transport_stream: Presence<bool>,
}

/// Envelope/item metadata loss fields; counts include repeated per-item fields.
pub const METADATA_CREATED: u32 = 1;
pub const METADATA_ITEM_ID: u32 = 2;
pub const METADATA_ITEM_STATUS: u32 = 4;
pub const METADATA_ISO_CREATED: u32 = 8;
pub const METADATA_ISO_UPDATED: u32 = 16;

/// An exact, inspectable dropped field/block. Iterate without allocation.
#[derive(Clone, Copy, Debug)]
pub enum Loss<'a> {
    Extension(Extension<'a>),
    Reasoning { record: usize },
    Context { source: Format },
    Metadata { field: &'static str, record: Option<usize> },
}

pub struct Losses<'r, 'a> {
    records: &'r [Record<'a>],
    target: Format,
    policy: Policy,
    at: usize,
    context: Option<Format>,
    pending_metadata: u32,
    pending_record: Option<usize>,
    pending_reasoning: Option<usize>,
}

impl<'r, 'a> Iterator for Losses<'r, 'a> {
    type Item = Loss<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        if let Some(source) = self.context.take() { return Some(Loss::Context { source }); }
        loop {
            if self.pending_metadata != 0 {
                let bit = 1u32 << self.pending_metadata.trailing_zeros();
                self.pending_metadata &= !bit;
                let field = match bit { METADATA_CREATED => "created", METADATA_ITEM_ID => "item.id", METADATA_ITEM_STATUS => "item.status", METADATA_ISO_CREATED => "created (ISO 8601)", _ => "updated (ISO 8601)" };
                return Some(Loss::Metadata { field, record: self.pending_record });
            }
            if let Some(record) = self.pending_reasoning.take() { return Some(Loss::Reasoning { record }); }
            let record = self.records.get(self.at)?;
            let index = self.at;
            self.at += 1;
            let (id, status) = match record {
                Record::Block(b) => {
                    if matches!(b.content, Content::Reasoning { .. }) && self.policy.drop_reasoning { self.pending_reasoning = Some(index); }
                    (b.item_id, b.item_status)
                }
                Record::Message(m) => (m.item_id, m.item_status),
                Record::Extension(e) if e.source != self.target && self.policy.drop_extensions && !e.is_empty_annotation() => return Some(Loss::Extension(*e)),
                _ => (Presence::Missing, Presence::Missing),
            };
            if self.target.protocol != crate::Protocol::OpenAiResponses && self.policy.drop_metadata {
                self.pending_record = Some(index);
                if !id.is_missing() { self.pending_metadata |= METADATA_ITEM_ID; }
                if !status.is_missing() { self.pending_metadata |= METADATA_ITEM_STATUS; }
            }
        }
    }
}

impl<'a> Request<'a, '_> {
    pub fn losses(&self, target: Format, policy: Policy) -> Losses<'_, 'a> {
        let context = match self.context {
            Presence::Value(c) if c.source != target && policy.drop_context => Some(c.source),
            Presence::Null if self.source != target && policy.drop_context => Some(self.source),
            _ => None,
        };
        Losses { records: self.records, target, policy, at: 0, context, pending_metadata: 0, pending_record: None, pending_reasoning: None }
    }
}

impl<'a> Response<'a, '_> {
    pub fn losses(&self, target: Format, policy: Policy) -> Losses<'_, 'a> {
        let mut pending_metadata = 0;
        if policy.drop_metadata {
            if !matches!(target.protocol, crate::Protocol::OpenAiChat | crate::Protocol::OpenAiResponses | crate::Protocol::OpenAiCompletions) && !self.created.is_missing() { pending_metadata |= METADATA_CREATED; }
            if target.protocol != crate::Protocol::GeminiInteractions {
                if !self.created_iso.is_missing() { pending_metadata |= METADATA_ISO_CREATED; }
                if !self.updated_iso.is_missing() { pending_metadata |= METADATA_ISO_UPDATED; }
            }
        }
        Losses { records: self.records, target, policy, at: 0, context: None, pending_metadata, pending_record: None, pending_reasoning: None }
    }
}
