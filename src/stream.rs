//! Resumable SSE protocol conversion with caller-owned storage.
//!
//! Direct adapters invoke the target writer without constructing event IR. The
//! IR route first fills the caller's `events` tape, then encodes that tape. The
//! route is fixed at construction and never changes after bytes are committed.
//! Output is staged per source event; insufficient storage is recoverable by
//! replacing the named buffer. Published output is never replayed.

use crate::json::{self, JsonString, Token, Value, Writer};
use crate::sse;
use crate::{Error, Format, Path, Profile, Protocol, Strategy};

mod decode;
mod encode;
mod owned;
pub use owned::{EventEncoderBuffers, EventEncoderState};

const NONE: usize = usize::MAX;
const HEADER: usize = core::mem::size_of::<usize>() * 2;

/// An independently indexed choice and content block/tool call.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Key {
    pub choice: u32,
    pub index: u32,
    pub part: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BlockKind {
    #[default]
    Text,
    Tool,
    Reasoning,
    Refusal,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FinishReason {
    #[default]
    Stop,
    Length,
    ToolCalls,
    Safety,
    Continuation,
    Other,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Completion {
    #[default]
    Completed,
    Incomplete,
    RequiresAction,
    Failed,
    Cancelled,
}

/// An exact JSON number observed on the wire, without monetary interpretation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WireNumber {
    bytes: [u8; 96],
    len: u8,
}
impl WireNumber {
    pub fn parse(raw: &[u8]) -> Result<Self, Error> {
        if raw.len() > 96 {
            return Err(Error::Unsupported {
                feature: "usage number longer than 96 bytes",
            });
        }
        let mut tokens = [Token::EMPTY; 1];
        let document = json::parse(raw, &mut tokens, json::Limits::default())?;
        if document.root().kind() != json::Kind::Number {
            return Err(Error::InvalidInput {
                field: "cost",
                reason: "expected JSON number",
            });
        }
        let mut number = Self {
            bytes: [0; 96],
            len: raw.len() as u8,
        };
        number.bytes[..raw.len()].copy_from_slice(raw);
        Ok(number)
    }
    pub fn raw(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

/// Cumulative counters. Absence is preserved; updates replace present fields.
/// `input_basis` makes cache accounting explicit even outside an adapter.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Usage {
    pub input: Option<u64>,
    /// Observed uncached source count, independent of canonical inclusive input.
    pub uncached_input: Option<u64>,
    pub output: Option<u64>,
    pub total: Option<u64>,
    pub cached: Option<u64>,
    pub reasoning: Option<u64>,
    pub cache_write: Option<u64>,
    pub cache_write_5m: Option<u64>,
    pub cache_write_1h: Option<u64>,
    pub cache_write_30m: Option<u64>,
    pub cache_write_12h: Option<u64>,
    pub cache_miss: Option<u64>,
    pub tool_use_prompt: Option<u64>,
    pub cost: Option<WireNumber>,
    /// Preserve a present lifetime object even when every bucket is unknown.
    pub anthropic_cache_details: bool,
    pub openai_cache_details: bool,
    pub input_basis: InputCount,
    pub output_basis: OutputCount,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InputCount {
    #[default]
    Total,
    /// Input excludes separately reported cache reads and writes.
    Uncached,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OutputCount {
    #[default]
    Total,
    /// Visible output excludes the separately reported thinking count.
    Visible,
}
impl Usage {
    fn update(&mut self, next: Self) {
        if next.cache_write_5m.is_some() {
            self.cache_write_5m = next.cache_write_5m;
        }
        if next.cache_write_1h.is_some() {
            self.cache_write_1h = next.cache_write_1h;
        }
        if next.cache_write_30m.is_some() {
            self.cache_write_30m = next.cache_write_30m;
        }
        if next.cache_write_12h.is_some() {
            self.cache_write_12h = next.cache_write_12h;
        }
        if next.cache_miss.is_some() {
            self.cache_miss = next.cache_miss;
        }
        if next.tool_use_prompt.is_some() {
            self.tool_use_prompt = next.tool_use_prompt;
        }
        if next.cost.is_some() {
            self.cost = next.cost;
        }
        self.anthropic_cache_details |= next.anthropic_cache_details;
        self.openai_cache_details |= next.openai_cache_details;
        if next.uncached_input.is_some() {
            self.uncached_input = next.uncached_input;
        }
        if next.input.is_some() {
            self.input = next.input;
        }
        if next.output.is_some() {
            self.output = next.output;
        }
        if next.total.is_some() {
            self.total = next.total;
        }
        if next.cached.is_some() {
            self.cached = next.cached;
        }
        if next.reasoning.is_some() {
            self.reasoning = next.reasoning;
        }
        if next.cache_write.is_some() {
            self.cache_write = next.cache_write;
        }
    }
}

/// Checked cumulative accounting shared by event consumers and stream encoders.
///
/// Present fields replace the preceding source counters. Cache/thinking-only
/// updates recalculate canonical totals from the retained source bases; they do
/// not add earlier cumulative updates twice. Missing fields remain unknown until
/// supplied or derivable under the source's declared accounting basis.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UsageAccumulator {
    usage: Usage,
    input_base: Option<u64>,
    input_basis: InputCount,
    output_base: Option<u64>,
    output_basis: OutputCount,
}

impl UsageAccumulator {
    /// Initialize from caller-provided canonical, inclusive counters.
    pub fn new(initial: Usage) -> Result<Self, Error> {
        if initial.input_basis != InputCount::Total || initial.output_basis != OutputCount::Total {
            return Err(Error::InvalidInput {
                field: "initial_usage basis",
                reason: "context counters must be canonical totals",
            });
        }
        Ok(Self {
            usage: initial,
            input_base: initial.input,
            input_basis: initial.input_basis,
            output_base: initial.output,
            output_basis: initial.output_basis,
        })
    }

    /// The current canonical counters; this never invents an absent zero.
    pub fn usage(&self) -> Usage {
        self.usage
    }

    /// Apply a cumulative source update atomically. On overflow, retain the
    /// previously committed counters so the caller can diagnose or recover.
    pub fn update(&mut self, next: Usage) -> Result<Usage, Error> {
        let mut updated = *self;
        if let Some(input) = next.input {
            updated.input_base = Some(input);
            updated.input_basis = next.input_basis;
        }
        if let Some(output) = next.output {
            updated.output_base = Some(output);
            updated.output_basis = next.output_basis;
        }
        updated.usage.update(next);
        let overflow = Error::InvalidInput {
            field: "usage",
            reason: "counter overflow",
        };
        updated.usage.input = match (updated.input_base, updated.input_basis) {
            (Some(_), InputCount::Uncached)
                if updated.usage.anthropic_cache_details && updated.usage.cache_write.is_none() =>
            {
                None
            }
            (Some(input), InputCount::Uncached) => Some(
                input
                    .checked_add(updated.usage.cached.unwrap_or(0))
                    .and_then(|input| input.checked_add(updated.usage.cache_write.unwrap_or(0)))
                    .ok_or(overflow)?,
            ),
            (input, InputCount::Total) => input,
            (None, _) => None,
        };
        updated.usage.output = match (updated.output_base, updated.output_basis) {
            (Some(output), OutputCount::Visible) => Some(
                output
                    .checked_add(updated.usage.reasoning.unwrap_or(0))
                    .ok_or(overflow)?,
            ),
            (output, OutputCount::Total) => output,
            (None, _) => None,
        };
        updated.usage.input_basis = InputCount::Total;
        updated.usage.output_basis = OutputCount::Total;
        if updated.input_basis == InputCount::Uncached
            && let (Some(input), Some(output)) = (updated.usage.input, updated.usage.output)
        {
            updated.usage.total = Some(input.checked_add(output).ok_or(overflow)?);
        }
        *self = updated;
        Ok(self.usage)
    }
}

/// IDs for sources whose wire format omits a tool-call identifier.
#[derive(Clone, Copy, Debug)]
pub struct ToolId<'a> {
    pub key: Key,
    pub id: &'a str,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct StreamContext<'a> {
    pub response_id: Option<&'a str>,
    pub model: Option<&'a str>,
    pub created: Option<u64>,
    /// Explicit prefix for locally scoped Responses item identifiers.
    pub item_id_prefix: Option<&'a str>,
    pub initial_usage: Usage,
    pub tool_ids: &'a [ToolId<'a>],
}

/// Opt-in losses are reported as named bits in `Report`.
#[derive(Clone, Copy, Debug, Default)]
pub struct StreamPolicy {
    pub drop_reasoning: bool,
    pub drop_extensions: bool,
}

/// Exact classes of source fields that the opt-in metadata policy may discard.
/// The discriminants are bits in `Report::dropped_fields`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u32)]
pub enum DroppedField {
    #[default]
    SystemFingerprint = 1,
    ServiceTier = 2,
    SafetyRatings = 4,
    FinishMessage = 8,
    AverageLogprobs = 16,
    CacheCreationTokens = 32,
}

#[derive(Clone, Copy, Debug)]
pub struct StreamConfig<'a> {
    pub source: Format,
    pub target: Format,
    pub strategy: Strategy,
    pub json_limits: json::Limits,
    pub context: StreamContext<'a>,
    pub policy: StreamPolicy,
    /// Retain source Responses text to verify final snapshots byte-for-byte.
    /// False keeps deltas authoritative and checks snapshot lengths/lifecycle.
    /// Targets requiring snapshots always aggregate and verify them.
    pub validate_snapshots: bool,
    /// Encode only observed counters instead of requiring complete terminal usage.
    pub allow_partial_usage: bool,
}
impl<'a> StreamConfig<'a> {
    pub fn new(source: Protocol, target: Protocol) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
            strategy: Strategy::Auto,
            json_limits: json::Limits::default(),
            context: StreamContext::default(),
            policy: StreamPolicy::default(),
            validate_snapshots: false,
            allow_partial_usage: false,
        }
    }
}

/// A string span in the JSON event payload, independently editable before replay.
/// `quoted=true` means a complete JSON string, otherwise UTF-8 bytes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EventText {
    pub offset: usize,
    pub length: usize,
    pub quoted: bool,
    pub present: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EventKind {
    #[default]
    Begin,
    BlockStart,
    Delta,
    Signature,
    BlockEnd,
    Usage,
    ChoiceEnd,
    Finish,
    Failure,
    Extension,
    ToolComplete,
    Snapshot,
    BlockIncomplete,
    Images,
    Grounding,
    Continuation,
    StopSequence,
}

/// Compact event IR. All spans refer to the associated payload, never a cache.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Event {
    pub kind: EventKind,
    pub key: Key,
    pub block: BlockKind,
    pub a: EventText,
    /// Begin: model; ToolComplete: name; Signature: optional origin label.
    /// Signature origins are `anthropic`, `gemini`, `chat`, or `responses`.
    /// An absent Signature origin uses StreamConfig.source, never the target.
    pub b: EventText,
    pub c: EventText,
    /// Explicit output item identity, separate from the tool call ID in `a`.
    /// The encoder preserves this ID; absent IDs use the context prefix.
    pub item_id: EventText,
    /// Signature bound to an atomic tool call, distinct from an item ID.
    pub signature: EventText,
    pub created: Option<u64>,
    pub usage: Usage,
    pub reason: FinishReason,
    pub completion: Completion,
    pub extension: DroppedField,
}
impl Event {
    pub const EMPTY: Self = Self {
        kind: EventKind::Begin,
        key: Key {
            choice: 0,
            index: 0,
            part: 0,
        },
        block: BlockKind::Text,
        a: EventText {
            offset: 0,
            length: 0,
            quoted: false,
            present: false,
        },
        b: EventText {
            offset: 0,
            length: 0,
            quoted: false,
            present: false,
        },
        c: EventText {
            offset: 0,
            length: 0,
            quoted: false,
            present: false,
        },
        item_id: EventText {
            offset: 0,
            length: 0,
            quoted: false,
            present: false,
        },
        signature: EventText {
            offset: 0,
            length: 0,
            quoted: false,
            present: false,
        },
        created: None,
        usage: Usage {
            input: None,
            uncached_input: None,
            output: None,
            total: None,
            cached: None,
            reasoning: None,
            cache_write: None,
            cache_write_5m: None,
            cache_write_1h: None,
            cache_write_30m: None,
            cache_write_12h: None,
            cache_miss: None,
            tool_use_prompt: None,
            cost: None,
            anthropic_cache_details: false,
            openai_cache_details: false,
            input_basis: InputCount::Total,
            output_basis: OutputCount::Total,
        },
        reason: FinishReason::Stop,
        completion: Completion::Completed,
        extension: DroppedField::SystemFingerprint,
    };
}

#[derive(Clone, Copy, Debug)]
struct Span {
    start: usize,
    length: usize,
}
impl Span {
    const EMPTY: Self = Self {
        start: 0,
        length: 0,
    };
}

/// Caller-owned lifecycle slot. Initialize storage with `ActiveItem::EMPTY`.
#[derive(Clone, Copy, Debug)]
pub struct ActiveItem {
    used: bool,
    closed: bool,
    key: Key,
    kind: BlockKind,
    wire_index: u32,
    id: Span,
    name: Span,
    head: usize,
    tail: usize,
    length: usize,
    finished: bool,
    reason: FinishReason,
    saw_delta: bool,
    validated: bool,
    content_length: usize,
    incomplete: bool,
    output_id: Option<Span>,
    pending_text: Option<Span>,
    signature: Option<Span>,
    signature_provider: Option<Format>,
}
impl ActiveItem {
    pub const EMPTY: Self = Self {
        used: false,
        closed: false,
        key: Key {
            choice: 0,
            index: 0,
            part: 0,
        },
        kind: BlockKind::Text,
        wire_index: 0,
        id: Span::EMPTY,
        name: Span::EMPTY,
        head: NONE,
        tail: NONE,
        length: 0,
        finished: false,
        reason: FinishReason::Stop,
        saw_delta: false,
        validated: false,
        content_length: 0,
        incomplete: false,
        output_id: None,
        pending_text: None,
        signature: None,
        signature_provider: None,
    };
}
impl Default for ActiveItem {
    fn default() -> Self {
        Self::EMPTY
    }
}

/// Every potentially growing region belongs to the caller. `checkpoints` must
/// have at least as many slots as `items`; it permits rollback without replay.
pub struct StreamBuffers<'a> {
    pub framing: &'a mut [u8],
    pub data: &'a mut [u8],
    pub pending: &'a mut [u8],
    pub tokens: &'a mut [Token],
    pub items: &'a mut [ActiveItem],
    pub checkpoints: &'a mut [ActiveItem],
    pub events: &'a mut [Event],
    pub aggregate: &'a mut [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BufferKind {
    Framing,
    Data,
    Pending,
    Tokens,
    Items,
    Events,
    Aggregate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    NeedInput,
    NeedOutput,
    Done,
    NeedBuffer {
        buffer: BufferKind,
        needed: usize,
        exact: bool,
    },
    Failed(Error),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Report {
    pub source_events: usize,
    pub ir_events: usize,
    pub dropped_reasoning: usize,
    pub dropped_extensions: usize,
    pub dropped_fields: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Step {
    pub consumed: usize,
    pub written: usize,
    pub status: Status,
    pub path: Path,
    pub report: Report,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Memory {
    pub state: usize,
    pub framing_capacity: usize,
    pub data_capacity: usize,
    pub pending_capacity: usize,
    pub token_capacity: usize,
    pub item_capacity: usize,
    pub checkpoint_capacity: usize,
    pub event_ir_capacity: usize,
    pub aggregate_capacity: usize,
    pub framing_peak: usize,
    pub data_peak: usize,
    pub pending_peak: usize,
    pub tokens_peak: usize,
    pub items_used: usize,
    pub aggregate_used: usize,
    pub aggregate_peak: usize,
}

#[derive(Clone, Copy, Debug)]
struct State {
    begun: bool,
    ended: bool,
    seen_finish: bool,
    failure: bool,
    id: Span,
    model: Span,
    created: Option<u64>,
    usage: UsageAccumulator,
    aggregate_used: usize,
    aggregate_peak: usize,
    item_count: usize,
    sequence: u64,
    report: Report,
    stop_sequence: Option<Span>,
    continuation: Option<(u32, Span)>,
    usage_seen: bool,
}
impl State {
    fn new(context: StreamContext<'_>) -> Result<Self, Error> {
        Ok(Self {
            begun: false,
            ended: false,
            seen_finish: false,
            failure: false,
            id: Span::EMPTY,
            model: Span::EMPTY,
            created: context.created,
            usage: UsageAccumulator::new(context.initial_usage)?,
            aggregate_used: 0,
            aggregate_peak: 0,
            item_count: 0,
            sequence: 0,
            report: Report::default(),
            stop_sequence: None,
            continuation: None,
            usage_seen: false,
        })
    }
}

pub struct StreamConverter<'a, 'c> {
    config: StreamConfig<'c>,
    path: Path,
    framing: sse::Decoder<'a>,
    data: &'a mut [u8],
    pending: &'a mut [u8],
    tokens: &'a mut [Token],
    items: &'a mut [ActiveItem],
    checkpoints: &'a mut [ActiveItem],
    events: &'a mut [Event],
    aggregate: &'a mut [u8],
    state: State,
    pending_len: usize,
    pending_pos: usize,
    data_len: usize,
    ready: bool,
    eof: bool,
    error: Option<Error>,
    data_peak: usize,
    pending_peak: usize,
    token_peak: usize,
}

impl<'a, 'c> StreamConverter<'a, 'c> {
    pub fn new(config: StreamConfig<'c>, buffers: StreamBuffers<'a>) -> Result<Self, Error> {
        if config.context.initial_usage.input_basis != InputCount::Total
            || config.context.initial_usage.output_basis != OutputCount::Total
        {
            return Err(Error::InvalidInput {
                field: "initial_usage basis",
                reason: "context counters must be canonical totals",
            });
        }
        for format in [config.source, config.target] {
            validate_stream_format(format)?;
        }
        if buffers.checkpoints.len() < buffers.items.len() {
            return Err(Error::WorkspaceTooSmall {
                needed: buffers.items.len(),
            });
        }
        let direct = supports_direct(config.source.protocol, config.target.protocol);
        let path = match config.strategy {
            Strategy::Ir => Path::Ir,
            Strategy::Direct if !direct => {
                return Err(Error::DirectUnavailable {
                    feature: "stream direction",
                });
            }
            _ if direct => Path::Direct,
            _ => Path::Ir,
        };
        if config.target.protocol == Protocol::AnthropicMessages
            && config.source.protocol != Protocol::AnthropicMessages
            && config.context.initial_usage.input.is_none()
        {
            return Err(Error::ContextRequired {
                field: "initial_usage.input",
            });
        }
        if config.target.protocol == Protocol::OpenAiResponses
            && config.context.item_id_prefix.is_none()
        {
            return Err(Error::ContextRequired {
                field: "item_id_prefix",
            });
        }
        buffers.items.fill(ActiveItem::EMPTY);
        Ok(Self {
            config,
            path,
            framing: sse::Decoder::new(buffers.framing),
            data: buffers.data,
            pending: buffers.pending,
            tokens: buffers.tokens,
            items: buffers.items,
            checkpoints: buffers.checkpoints,
            events: buffers.events,
            aggregate: buffers.aggregate,
            state: State::new(config.context)?,
            pending_len: 0,
            pending_pos: 0,
            data_len: 0,
            ready: false,
            eof: false,
            error: None,
            data_peak: 0,
            pending_peak: 0,
            token_peak: 0,
        })
    }
    pub fn path(&self) -> Path {
        self.path
    }
    pub fn report(&self) -> Report {
        self.state.report
    }
    pub fn usage(&self) -> Usage {
        self.state.usage.usage()
    }
    pub fn memory(&self) -> Memory {
        Memory {
            state: core::mem::size_of::<Self>(),
            framing_capacity: self.framing.capacity(),
            data_capacity: self.data.len(),
            pending_capacity: self.pending.len(),
            token_capacity: core::mem::size_of_val(self.tokens),
            item_capacity: core::mem::size_of_val(self.items),
            checkpoint_capacity: core::mem::size_of_val(self.checkpoints),
            event_ir_capacity: core::mem::size_of_val(self.events),
            aggregate_capacity: self.aggregate.len(),
            framing_peak: self.framing.peak(),
            data_peak: self.data_peak,
            pending_peak: self.pending_peak,
            tokens_peak: self.token_peak,
            items_used: self.state.item_count,
            aggregate_used: self.state.aggregate_used,
            aggregate_peak: self.state.aggregate_peak,
        }
    }

    /// Replace a byte workspace after `NeedBuffer`. Existing state is copied.
    pub fn replace_buffer(
        &mut self,
        kind: BufferKind,
        buffer: &'a mut [u8],
    ) -> Result<&'a mut [u8], Error> {
        if kind == BufferKind::Framing {
            return self.framing.replace_buffer(buffer);
        }
        let (target, used) = match kind {
            BufferKind::Data => (&mut self.data, self.data_len),
            BufferKind::Pending => (&mut self.pending, self.pending_len),
            BufferKind::Aggregate => (&mut self.aggregate, self.state.aggregate_used),
            _ => {
                return Err(Error::InvalidInput {
                    field: "buffer",
                    reason: "use the typed workspace replacement method",
                });
            }
        };
        if buffer.len() < used {
            return Err(Error::WorkspaceTooSmall { needed: used });
        }
        buffer[..used].copy_from_slice(&target[..used]);
        Ok(core::mem::replace(target, buffer))
    }
    pub fn replace_tokens(&mut self, tokens: &'a mut [Token]) -> &'a mut [Token] {
        core::mem::replace(&mut self.tokens, tokens)
    }
    pub fn replace_events(&mut self, events: &'a mut [Event]) -> &'a mut [Event] {
        core::mem::replace(&mut self.events, events)
    }
    pub fn replace_items(
        &mut self,
        items: &'a mut [ActiveItem],
        checkpoints: &'a mut [ActiveItem],
    ) -> Result<(), Error> {
        if items.len() < self.state.item_count || checkpoints.len() < items.len() {
            return Err(Error::TooManyActiveItems {
                needed: self.state.item_count.max(items.len()),
            });
        }
        items.fill(ActiveItem::EMPTY);
        items[..self.state.item_count].copy_from_slice(&self.items[..self.state.item_count]);
        self.items = items;
        self.checkpoints = checkpoints;
        Ok(())
    }

    /// Stage caller-inspected or edited event IR. Spans borrow `payload` only for
    /// this call. Drain staged bytes with `step(&[], output, false)` before staging
    /// another batch. A capacity failure is atomic and the same batch can retry.
    /// This API requires `Strategy::Ir` and does not read SSE framing.
    pub fn stage_events(&mut self, payload: &[u8], events: &[Event]) -> Result<(), Status> {
        if self.path != Path::Ir {
            return Err(Status::Failed(Error::InvalidInput {
                field: "strategy",
                reason: "event IR staging requires Ir",
            }));
        }
        if self.pending_len != 0 || self.ready || self.framing.buffered() != 0 {
            return Err(Status::Failed(Error::InvalidInput {
                field: "event IR staging",
                reason: "drain pending conversion first",
            }));
        }
        let length = owned::stage(
            self.config,
            payload,
            events,
            EventEncoderBuffers {
                tokens: self.tokens,
                items: self.items,
                checkpoints: self.checkpoints,
                aggregate: self.aggregate,
            },
            self.pending,
            &mut self.state,
        )?;
        self.pending_len = length;
        self.pending_peak = self.pending_peak.max(length);
        Ok(())
    }

    /// `eof` applies after all supplied input has been consumed. When output is
    /// full, resend only the unconsumed suffix with the same EOF flag.
    pub fn step(&mut self, input: &[u8], output: &mut [u8], eof: bool) -> Step {
        let mut consumed = 0;
        let mut written = 0;
        let status = loop {
            if let Some(error) = self.error {
                break Status::Failed(error);
            }
            if self.pending_pos < self.pending_len {
                let length = (self.pending_len - self.pending_pos).min(output.len() - written);
                output[written..written + length]
                    .copy_from_slice(&self.pending[self.pending_pos..self.pending_pos + length]);
                self.pending_pos += length;
                written += length;
                if self.pending_pos < self.pending_len {
                    break Status::NeedOutput;
                }
                self.pending_pos = 0;
                self.pending_len = 0;
            }
            if self.ready {
                match self.convert_ready() {
                    Ok(()) => {
                        self.ready = false;
                        self.data_len = 0;
                        self.framing.consume_event();
                        continue;
                    }
                    Err(status @ Status::NeedBuffer { .. }) => break status,
                    Err(Status::Failed(error)) => {
                        self.error = Some(error);
                        break Status::Failed(error);
                    }
                    Err(other) => break other,
                }
            }
            if self.eof {
                if consumed < input.len() {
                    self.error = Some(Error::AlreadyFinished);
                    break Status::Failed(Error::AlreadyFinished);
                }
                break if self.state.ended {
                    Status::Done
                } else {
                    Status::Failed(Error::TruncatedStream)
                };
            }
            let progress = self.framing.push(&input[consumed..]);
            consumed += progress.consumed;
            match progress.status {
                sse::Status::NeedBuffer { needed } => {
                    break Status::NeedBuffer {
                        buffer: BufferKind::Framing,
                        needed,
                        exact: false,
                    };
                }
                sse::Status::Event => {
                    let frame = match self.framing.event() {
                        Some(frame) => frame,
                        None => continue,
                    };
                    match frame.data_into(self.data) {
                        Ok(Some(length)) => {
                            self.data_len = length;
                            self.data_peak = self.data_peak.max(length);
                            self.ready = true;
                        }
                        Ok(None) => self.framing.consume_event(),
                        Err(Error::OutputTooSmall { needed }) => {
                            break Status::NeedBuffer {
                                buffer: BufferKind::Data,
                                needed,
                                exact: true,
                            };
                        }
                        Err(error) => {
                            self.error = Some(error);
                            break Status::Failed(error);
                        }
                    }
                }
                sse::Status::NeedInput | sse::Status::End => {
                    if consumed == input.len() && eof {
                        let discarded = self.framing.finish();
                        if discarded {
                            self.error = Some(Error::TruncatedStream);
                            break Status::Failed(Error::TruncatedStream);
                        }
                        self.eof = true;
                        if !self.state.ended
                            && self.config.source.protocol == Protocol::GeminiGenerateContent
                            && self.state.seen_finish
                        {
                            match self.finish_google() {
                                Ok(()) => continue,
                                Err(status) => {
                                    self.eof = false;
                                    if let Status::Failed(error) = status {
                                        self.error = Some(error);
                                    }
                                    break status;
                                }
                            }
                        }
                        if !self.state.ended {
                            self.error = Some(Error::TruncatedStream);
                            break Status::Failed(Error::TruncatedStream);
                        }
                        continue;
                    }
                    break if self.state.ended {
                        Status::Done
                    } else {
                        Status::NeedInput
                    };
                }
            }
        };
        Step {
            consumed,
            written,
            status,
            path: self.path,
            report: self.state.report,
        }
    }

    fn rollback(&mut self, saved: State) {
        for item in &self.checkpoints[..saved.item_count] {
            if item.tail != NONE {
                write_usize(self.aggregate, item.tail, NONE);
            }
        }
        self.items
            .copy_from_slice(&self.checkpoints[..self.items.len()]);
        self.state = saved;
    }
    fn convert_ready(&mut self) -> Result<(), Status> {
        if self.state.ended {
            return Err(Status::Failed(Error::AlreadyFinished));
        }
        let saved = self.state;
        self.checkpoints[..self.items.len()].copy_from_slice(self.items);
        let event_type = self
            .framing
            .event()
            .map(|event| event.event_type())
            .unwrap_or(b"message");
        let result = process(
            self.config,
            self.path,
            &self.data[..self.data_len],
            event_type,
            self.tokens,
            self.events,
            self.pending,
            &mut self.state,
            self.items,
            self.aggregate,
            &mut self.token_peak,
        );
        match result {
            Ok(length) => {
                self.pending_len = length;
                self.pending_peak = self.pending_peak.max(length);
                Ok(())
            }
            Err(error) => {
                self.rollback(saved);
                Err(capacity_status(error))
            }
        }
    }
    fn finish_google(&mut self) -> Result<(), Status> {
        let saved = self.state;
        self.checkpoints[..self.items.len()].copy_from_slice(self.items);
        let mut emitter = encode::Emitter::new(
            self.config,
            Writer::new(self.pending),
            &mut self.state,
            self.items,
            self.aggregate,
        );
        let result = emitter
            .finish(Completion::Completed)
            .and_then(|()| emitter.writer.finish());
        match result {
            Ok(length) => {
                self.pending_len = length;
                self.pending_peak = self.pending_peak.max(length);
                Ok(())
            }
            Err(error) => {
                self.rollback(saved);
                Err(capacity_status(error))
            }
        }
    }
}

fn capacity_status(error: Error) -> Status {
    match error {
        Error::OutputTooSmall { needed } => Status::NeedBuffer {
            buffer: BufferKind::Pending,
            needed,
            exact: true,
        },
        Error::WorkspaceTooSmall { needed } => Status::NeedBuffer {
            buffer: BufferKind::Tokens,
            needed,
            exact: false,
        },
        Error::IrTooSmall { needed } => Status::NeedBuffer {
            buffer: BufferKind::Events,
            needed,
            exact: false,
        },
        Error::EventTooLarge { needed } => Status::NeedBuffer {
            buffer: BufferKind::Aggregate,
            needed,
            exact: false,
        },
        Error::TooManyActiveItems { needed } => Status::NeedBuffer {
            buffer: BufferKind::Items,
            needed,
            exact: false,
        },
        _ => Status::Failed(error),
    }
}

pub fn supports_direct(source: Protocol, target: Protocol) -> bool {
    source == target
        || (source == Protocol::OpenAiChat
            && matches!(
                target,
                Protocol::OpenAiResponses
                    | Protocol::AnthropicMessages
                    | Protocol::GeminiGenerateContent
            ))
        || (target == Protocol::OpenAiChat
            && matches!(
                source,
                Protocol::OpenAiResponses
                    | Protocol::AnthropicMessages
                    | Protocol::GeminiGenerateContent
            ))
}

#[derive(Clone, Copy)]
enum Text<'a> {
    Utf8(&'a [u8]),
    Json(JsonString<'a>),
}
impl<'a> Text<'a> {
    fn eq_str(self, expected: &str) -> bool {
        match self {
            Self::Utf8(value) => value == expected.as_bytes(),
            Self::Json(value) => value.eq_str(expected),
        }
    }

    fn len(self) -> usize {
        match self {
            Self::Utf8(bytes) => bytes.len(),
            Self::Json(value) => value.decoded_len(),
        }
    }
    fn write(self, writer: &mut Writer<'_>) {
        match self {
            Self::Utf8(bytes) => writer.string(bytes),
            Self::Json(value) => writer.json_string(value),
        }
    }
    fn raw(self) -> &'a [u8] {
        match self {
            Self::Utf8(bytes) => bytes,
            Self::Json(value) => value.raw(),
        }
    }
    fn copy(self, out: &mut [u8]) -> Result<usize, Error> {
        match self {
            Self::Utf8(bytes) => {
                if out.len() < bytes.len() {
                    return Err(Error::OutputTooSmall {
                        needed: bytes.len(),
                    });
                }
                out[..bytes.len()].copy_from_slice(bytes);
                Ok(bytes.len())
            }
            Self::Json(value) => value.decode_into(out),
        }
    }
}

trait Sink {
    fn begin(
        &mut self,
        id: Option<Text<'_>>,
        model: Option<Text<'_>>,
        created: Option<u64>,
    ) -> Result<(), Error>;
    fn block(
        &mut self,
        key: Key,
        kind: BlockKind,
        id: Option<Text<'_>>,
        name: Option<Text<'_>>,
        item_id: Option<Text<'_>>,
    ) -> Result<(), Error>;
    fn delta(&mut self, key: Key, kind: BlockKind, text: Text<'_>) -> Result<(), Error>;
    fn signature(
        &mut self,
        key: Key,
        text: Text<'_>,
        provider: Option<Format>,
    ) -> Result<(), Error>;
    fn close(&mut self, key: Key) -> Result<(), Error>;
    fn usage(&mut self, usage: Usage) -> Result<(), Error>;
    fn images(&mut self, choice: u32, value: json::RawValue<'_>) -> Result<(), Error>;
    fn grounding(&mut self, choice: u32, value: json::RawValue<'_>) -> Result<(), Error>;
    fn continuation(&mut self, choice: u32, token: Text<'_>) -> Result<(), Error>;
    fn stop_sequence(&mut self, sequence: Text<'_>) -> Result<(), Error>;
    fn choice(&mut self, choice: u32, reason: FinishReason) -> Result<(), Error>;
    fn finish(&mut self, completion: Completion) -> Result<(), Error>;
    fn failure(&mut self, code: Option<Text<'_>>, message: Text<'_>) -> Result<(), Error>;
    fn extension(&mut self, field: DroppedField) -> Result<(), Error>;
    fn complete_tool(
        &mut self,
        key: Key,
        id: Option<Text<'_>>,
        name: Text<'_>,
        arguments: Text<'_>,
        signature: Option<Text<'_>>,
    ) -> Result<(), Error>;
    fn snapshot(&mut self, key: Key, kind: BlockKind, text: Text<'_>) -> Result<(), Error>;
    fn incomplete(&mut self, key: Key) -> Result<(), Error>;
}

// Separate mutable regions let the parser's input borrow end before its token
// tape is reused to validate completed tool arguments.
#[allow(clippy::too_many_arguments)]
fn process(
    config: StreamConfig<'_>,
    path: Path,
    payload: &[u8],
    event_type: &[u8],
    tokens: &mut [Token],
    events: &mut [Event],
    output: &mut [u8],
    state: &mut State,
    items: &mut [ActiveItem],
    aggregate: &mut [u8],
    token_peak: &mut usize,
) -> Result<usize, Error> {
    if done_marker(config.source, event_type, payload)? {
        let mut emitter =
            encode::Emitter::new(config, Writer::new(output), state, items, aggregate);
        emitter.finish(Completion::Completed)?;
        emitter.state.report.source_events = emitter.state.report.source_events.saturating_add(1);
        let length = emitter.writer.finish()?;
        validate_arguments(config.json_limits, state, items, aggregate, tokens)?;
        return Ok(length);
    }
    let document = json::parse(payload, tokens, config.json_limits)?;
    *token_peak = (*token_peak).max(document.tokens_used());
    let root = document.root();
    validate_event_name(event_type, root)?;
    let discriminator = root
        .get("type")
        .or_else(|| root.get("event_type"))
        .and_then(Value::as_string);
    if state.begun
        && discriminator.is_some_and(|value| {
            ["message_start", "response.created", "interaction.created"]
                .iter()
                .any(|name| value.eq_str(name))
        })
    {
        return Err(Error::InvalidInput {
            field: "stream start",
            reason: "duplicate start event",
        });
    }
    let mut emitter = encode::Emitter::new(config, Writer::new(output), state, items, aggregate);
    if path == Path::Ir {
        let length = decode_ir_value(config.source, payload, root, events)?;
        for event in &events[..length] {
            replay(*event, payload, &mut emitter, tokens, config.json_limits)?;
        }
        emitter.state.report.ir_events = emitter.state.report.ir_events.saturating_add(length);
    } else {
        decode::event(config.source, root, &mut emitter)?;
    }
    emitter.state.report.source_events = emitter.state.report.source_events.saturating_add(1);
    let length = emitter.writer.finish()?;
    validate_arguments(config.json_limits, state, items, aggregate, tokens)?;
    Ok(length)
}

fn validate_arguments(
    limits: json::Limits,
    state: &mut State,
    items: &mut [ActiveItem],
    aggregate: &mut [u8],
    tokens: &mut [Token],
) -> Result<(), Error> {
    for item in items
        .iter_mut()
        .filter(|item| item.used && item.closed && item.kind == BlockKind::Tool && !item.validated)
    {
        if item.incomplete
            && item.finished
            && matches!(
                item.reason,
                FinishReason::Length | FinishReason::Safety | FinishReason::Continuation
            )
        {
            continue;
        }
        if item.head == NONE {
            return Err(Error::InvalidInput {
                field: "tool arguments",
                reason: "missing complete JSON object",
            });
        }
        let start = if item.head == item.tail {
            item.head.saturating_add(HEADER)
        } else {
            let start = state.aggregate_used;
            let needed = start
                .checked_add(item.length)
                .ok_or(Error::EventTooLarge { needed: usize::MAX })?;
            if needed > aggregate.len() {
                return Err(Error::EventTooLarge { needed });
            }
            state.aggregate_peak = state.aggregate_peak.max(needed);
            let mut at = start;
            let mut next = item.head;
            let mut visits = 0usize;
            while next != NONE {
                visits += 1;
                if visits > state.aggregate_used / HEADER + 1 {
                    return Err(Error::InvalidInput {
                        field: "aggregate",
                        reason: "cycle",
                    });
                }
                let length = read_usize(aggregate, next + core::mem::size_of::<usize>())?;
                let source = next
                    .checked_add(HEADER)
                    .ok_or(Error::EventTooLarge { needed: usize::MAX })?;
                let end = source
                    .checked_add(length)
                    .ok_or(Error::EventTooLarge { needed: usize::MAX })?;
                if end > state.aggregate_used {
                    return Err(Error::InvalidInput {
                        field: "aggregate",
                        reason: "invalid segment",
                    });
                }
                aggregate.copy_within(source..end, at);
                at += length;
                next = read_usize(aggregate, next)?;
            }
            start
        };
        let end = start
            .checked_add(item.length)
            .ok_or(Error::EventTooLarge { needed: usize::MAX })?;
        let bytes = aggregate.get(start..end).ok_or(Error::InvalidInput {
            field: "aggregate",
            reason: "invalid arguments",
        })?;
        let document = json::parse(bytes, tokens, limits)?;
        if document.root().kind() != json::Kind::Object {
            return Err(Error::InvalidInput {
                field: "tool arguments",
                reason: "expected an object",
            });
        }
        item.validated = true;
    }
    Ok(())
}

/// Decode one JSON event to caller-owned event IR. It does not update lifecycle
/// state. Validate lifecycle and encode via `StreamConverter` or an event sink.
pub fn decode_event(
    protocol: Protocol,
    payload: &[u8],
    tokens: &mut [Token],
    events: &mut [Event],
    limits: json::Limits,
) -> Result<usize, Error> {
    decode_event_format(Format::new(protocol), payload, tokens, events, limits)
}

pub fn decode_event_format(
    format: Format,
    payload: &[u8],
    tokens: &mut [Token],
    events: &mut [Event],
    limits: json::Limits,
) -> Result<usize, Error> {
    validate_stream_format(format)?;
    let document = json::parse(payload, tokens, limits)?;
    decode_ir_value(format, payload, document.root(), events)
}

/// Decode one framed SSE payload, including its event-name contract.
///
/// OpenAI Chat and Completions `[DONE]` becomes a single `Finish` event. Other
/// payloads use the same validated parser and adapters as `decode_event_format`.
/// All text spans refer to `payload`. This function is stateless: the consumer
/// must check lifecycle, tool completeness and GenerateContent EOF termination.
/// Discard the provisional event tape on error and retry capacity errors with
/// the same frame. No input bytes are retained by this function.
pub fn decode_frame_format(
    format: Format,
    event_type: &[u8],
    payload: &[u8],
    tokens: &mut [Token],
    events: &mut [Event],
    limits: json::Limits,
) -> Result<usize, Error> {
    validate_stream_format(format)?;
    if done_marker(format, event_type, payload)? {
        *events.first_mut().ok_or(Error::IrTooSmall { needed: 1 })? = Event {
            kind: EventKind::Finish,
            ..Event::EMPTY
        };
        return Ok(1);
    }
    let document = json::parse(payload, tokens, limits)?;
    validate_event_name(event_type, document.root())?;
    decode_ir_value(format, payload, document.root(), events)
}

fn done_marker(format: Format, event_type: &[u8], payload: &[u8]) -> Result<bool, Error> {
    if payload != b"[DONE]" {
        return Ok(false);
    }
    if !matches!(
        format.protocol,
        Protocol::OpenAiChat | Protocol::OpenAiCompletions
    ) {
        return Err(Error::InvalidInput {
            field: "SSE data",
            reason: "unexpected DONE marker",
        });
    }
    if event_type != b"message" {
        return Err(Error::InvalidInput {
            field: "SSE event name",
            reason: "DONE marker requires the default message event",
        });
    }
    Ok(true)
}

fn validate_event_name(event_type: &[u8], root: Value<'_, '_>) -> Result<(), Error> {
    let discriminator = root
        .get("type")
        .or_else(|| root.get("event_type"))
        .and_then(Value::as_string);
    if event_type != b"message"
        && !discriminator.is_some_and(|value| value.bytes().eq(event_type.iter().copied()))
    {
        return Err(Error::InvalidInput {
            field: "SSE event name",
            reason: "does not match JSON event discriminator",
        });
    }
    Ok(())
}

fn decode_ir_value(
    format: Format,
    payload: &[u8],
    value: Value<'_, '_>,
    events: &mut [Event],
) -> Result<usize, Error> {
    let mut collector = Collector {
        payload,
        events,
        used: 0,
    };
    decode::event(format, value, &mut collector)?;
    Ok(collector.used)
}

/// Validate the stream adapter's supported version and profile before input.
pub fn validate_stream_format(format: Format) -> Result<(), Error> {
    let profile = match format.profile {
        Profile::Standard => true,
        Profile::OpenAiCompatible(name) => {
            matches!(
                format.protocol,
                Protocol::OpenAiChat | Protocol::OpenAiResponses
            ) && !name.is_empty()
        }
    };
    if format.version != Format::new(format.protocol).version || !profile {
        return Err(Error::Unsupported {
            feature: "stream protocol version/profile",
        });
    }
    Ok(())
}

struct Collector<'a, 'b> {
    payload: &'a [u8],
    events: &'b mut [Event],
    used: usize,
}
impl Collector<'_, '_> {
    fn text(&self, text: Option<Text<'_>>) -> Result<EventText, Error> {
        let Some(text) = text else {
            return Ok(EventText::default());
        };
        let raw = text.raw();
        let offset = (raw.as_ptr() as usize)
            .checked_sub(self.payload.as_ptr() as usize)
            .ok_or(Error::InvalidInput {
                field: "event IR",
                reason: "text is outside the payload",
            })?;
        if self.payload.get(offset..offset.saturating_add(raw.len())) != Some(raw) {
            return Err(Error::InvalidInput {
                field: "event IR",
                reason: "text is outside the payload",
            });
        }
        Ok(EventText {
            offset,
            length: raw.len(),
            quoted: matches!(text, Text::Json(_)),
            present: true,
        })
    }
    fn raw_event(
        &mut self,
        kind: EventKind,
        choice: u32,
        value: json::RawValue<'_>,
    ) -> Result<(), Error> {
        self.add(Event {
            kind,
            key: Key {
                choice,
                ..Key::default()
            },
            a: self.text(Some(Text::Utf8(value.raw())))?,
            ..Event::EMPTY
        })
    }
    fn add(&mut self, event: Event) -> Result<(), Error> {
        let slot = self.events.get_mut(self.used).ok_or(Error::IrTooSmall {
            needed: self.used.saturating_add(1),
        })?;
        *slot = event;
        self.used += 1;
        Ok(())
    }
}
impl Sink for Collector<'_, '_> {
    fn begin(
        &mut self,
        id: Option<Text<'_>>,
        model: Option<Text<'_>>,
        created: Option<u64>,
    ) -> Result<(), Error> {
        self.add(Event {
            kind: EventKind::Begin,
            a: self.text(id)?,
            b: self.text(model)?,
            created,
            ..Event::EMPTY
        })
    }
    fn block(
        &mut self,
        key: Key,
        block: BlockKind,
        id: Option<Text<'_>>,
        name: Option<Text<'_>>,
        item_id: Option<Text<'_>>,
    ) -> Result<(), Error> {
        self.add(Event {
            kind: EventKind::BlockStart,
            key,
            block,
            a: self.text(id)?,
            b: self.text(name)?,
            item_id: self.text(item_id)?,
            ..Event::EMPTY
        })
    }
    fn delta(&mut self, key: Key, block: BlockKind, text: Text<'_>) -> Result<(), Error> {
        self.add(Event {
            kind: EventKind::Delta,
            key,
            block,
            a: self.text(Some(text))?,
            ..Event::EMPTY
        })
    }
    fn signature(
        &mut self,
        key: Key,
        text: Text<'_>,
        _provider: Option<Format>,
    ) -> Result<(), Error> {
        self.add(Event {
            kind: EventKind::Signature,
            key,
            a: self.text(Some(text))?,
            ..Event::EMPTY
        })
    }
    fn close(&mut self, key: Key) -> Result<(), Error> {
        self.add(Event {
            kind: EventKind::BlockEnd,
            key,
            ..Event::EMPTY
        })
    }
    fn usage(&mut self, usage: Usage) -> Result<(), Error> {
        self.add(Event {
            kind: EventKind::Usage,
            usage,
            ..Event::EMPTY
        })
    }
    fn choice(&mut self, choice: u32, reason: FinishReason) -> Result<(), Error> {
        self.add(Event {
            kind: EventKind::ChoiceEnd,
            key: Key {
                choice,
                ..Key::default()
            },
            reason,
            ..Event::EMPTY
        })
    }
    fn finish(&mut self, completion: Completion) -> Result<(), Error> {
        self.add(Event {
            kind: EventKind::Finish,
            completion,
            ..Event::EMPTY
        })
    }
    fn failure(&mut self, code: Option<Text<'_>>, message: Text<'_>) -> Result<(), Error> {
        self.add(Event {
            kind: EventKind::Failure,
            a: self.text(code)?,
            b: self.text(Some(message))?,
            ..Event::EMPTY
        })
    }
    fn extension(&mut self, field: DroppedField) -> Result<(), Error> {
        self.add(Event {
            kind: EventKind::Extension,
            extension: field,
            ..Event::EMPTY
        })
    }
    fn complete_tool(
        &mut self,
        key: Key,
        id: Option<Text<'_>>,
        name: Text<'_>,
        arguments: Text<'_>,
        signature: Option<Text<'_>>,
    ) -> Result<(), Error> {
        self.add(Event {
            kind: EventKind::ToolComplete,
            key,
            a: self.text(id)?,
            b: self.text(Some(name))?,
            c: self.text(Some(arguments))?,
            signature: self.text(signature)?,
            ..Event::EMPTY
        })
    }
    fn snapshot(&mut self, key: Key, block: BlockKind, text: Text<'_>) -> Result<(), Error> {
        self.add(Event {
            kind: EventKind::Snapshot,
            key,
            block,
            a: self.text(Some(text))?,
            ..Event::EMPTY
        })
    }
    fn images(&mut self, choice: u32, value: json::RawValue<'_>) -> Result<(), Error> {
        self.raw_event(EventKind::Images, choice, value)
    }
    fn grounding(&mut self, choice: u32, value: json::RawValue<'_>) -> Result<(), Error> {
        self.raw_event(EventKind::Grounding, choice, value)
    }
    fn continuation(&mut self, choice: u32, token: Text<'_>) -> Result<(), Error> {
        self.add(Event {
            kind: EventKind::Continuation,
            key: Key {
                choice,
                ..Key::default()
            },
            a: self.text(Some(token))?,
            ..Event::EMPTY
        })
    }
    fn stop_sequence(&mut self, sequence: Text<'_>) -> Result<(), Error> {
        self.add(Event {
            kind: EventKind::StopSequence,
            a: self.text(Some(sequence))?,
            ..Event::EMPTY
        })
    }
    fn incomplete(&mut self, key: Key) -> Result<(), Error> {
        self.add(Event {
            kind: EventKind::BlockIncomplete,
            key,
            ..Event::EMPTY
        })
    }
}

fn event_text(span: EventText, payload: &[u8]) -> Result<Option<Text<'_>>, Error> {
    if !span.present {
        return Ok(None);
    }
    let raw = payload
        .get(
            span.offset
                ..span
                    .offset
                    .checked_add(span.length)
                    .ok_or(Error::InvalidInput {
                        field: "event IR span",
                        reason: "overflow",
                    })?,
        )
        .ok_or(Error::InvalidInput {
            field: "event IR span",
            reason: "out of bounds",
        })?;
    Ok(Some(if span.quoted {
        Text::Json(JsonString::parse(raw)?)
    } else {
        core::str::from_utf8(raw).map_err(|_| Error::InvalidInput {
            field: "event IR text",
            reason: "invalid UTF-8",
        })?;
        Text::Utf8(raw)
    }))
}
fn required_text(text: Option<Text<'_>>) -> Result<Text<'_>, Error> {
    text.ok_or(Error::InvalidInput {
        field: "event IR",
        reason: "missing text",
    })
}
fn replay(
    event: Event,
    payload: &[u8],
    sink: &mut impl Sink,
    tokens: &mut [Token],
    limits: json::Limits,
) -> Result<(), Error> {
    let a = event_text(event.a, payload)?;
    let b = event_text(event.b, payload)?;
    match event.kind {
        EventKind::Images | EventKind::Grounding => {
            let raw = required_text(a)?;
            let Text::Utf8(raw) = raw else {
                return Err(Error::InvalidInput {
                    field: "event IR raw JSON",
                    reason: "must be unquoted JSON",
                });
            };
            let document = json::parse(raw, tokens, limits)?;
            let root = document.root();
            if event.kind == EventKind::Images {
                decode::validate_images(root)?;
                sink.images(event.key.choice, root.raw_value())
            } else {
                decode::validate_grounding(root)?;
                sink.grounding(event.key.choice, root.raw_value())
            }
        }
        EventKind::Continuation => sink.continuation(event.key.choice, required_text(a)?),
        EventKind::StopSequence => sink.stop_sequence(required_text(a)?),
        EventKind::Begin => sink.begin(a, b, event.created),
        EventKind::BlockStart => sink.block(
            event.key,
            event.block,
            a,
            b,
            event_text(event.item_id, payload)?,
        ),
        EventKind::Delta => sink.delta(event.key, event.block, required_text(a)?),
        EventKind::Signature => {
            let provider = b
                .map(|name| {
                    let protocol = if name.eq_str("anthropic") {
                        Protocol::AnthropicMessages
                    } else if name.eq_str("gemini") {
                        Protocol::GeminiGenerateContent
                    } else if name.eq_str("chat") {
                        Protocol::OpenAiChat
                    } else if name.eq_str("responses") {
                        Protocol::OpenAiResponses
                    } else {
                        return Err(Error::InvalidInput {
                            field: "signature provider",
                            reason: "unknown origin label",
                        });
                    };
                    Ok(Format::new(protocol))
                })
                .transpose()?;
            sink.signature(event.key, required_text(a)?, provider)
        }
        EventKind::BlockEnd => sink.close(event.key),
        EventKind::Usage => sink.usage(event.usage),
        EventKind::ChoiceEnd => sink.choice(event.key.choice, event.reason),
        EventKind::Finish => sink.finish(event.completion),
        EventKind::Failure => sink.failure(a, required_text(b)?),
        EventKind::Extension => sink.extension(event.extension),
        EventKind::ToolComplete => sink.complete_tool(
            event.key,
            a,
            required_text(b)?,
            required_text(event_text(event.c, payload)?)?,
            event_text(event.signature, payload)?,
        ),
        EventKind::Snapshot => sink.snapshot(event.key, event.block, required_text(a)?),
        EventKind::BlockIncomplete => sink.incomplete(event.key),
    }
}

fn write_usize(bytes: &mut [u8], at: usize, value: usize) {
    if let Some(dst) = bytes.get_mut(at..at.saturating_add(core::mem::size_of::<usize>())) {
        dst.copy_from_slice(&value.to_le_bytes());
    }
}
fn read_usize(bytes: &[u8], at: usize) -> Result<usize, Error> {
    let raw = bytes
        .get(at..at.saturating_add(core::mem::size_of::<usize>()))
        .ok_or(Error::InvalidInput {
            field: "aggregate",
            reason: "invalid chain",
        })?;
    let mut number = [0u8; core::mem::size_of::<usize>()];
    number.copy_from_slice(raw);
    Ok(usize::from_le_bytes(number))
}
