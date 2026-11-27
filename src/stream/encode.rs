use super::*;

pub(super) struct Emitter<'a, 'b, 'c, 'd> {
    config: StreamConfig<'a>,
    pub(super) writer: Writer<'b>,
    pub(super) state: &'c mut State,
    items: &'d mut [ActiveItem],
    aggregate: &'d mut [u8],
    chat_write_lane: usize,
    frame_grounding: Option<Span>,
}

fn number(writer: &mut Writer<'_>, mut value: u64) {
    let mut digits = [0u8; 20];
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    writer.raw(&digits[start..]);
}
fn escaped(writer: &mut Writer<'_>, bytes: &[u8]) {
    for &byte in bytes {
        match byte {
            b'"' => writer.raw(b"\\\""),
            b'\\' => writer.raw(b"\\\\"),
            b'\n' => writer.raw(b"\\n"),
            b'\r' => writer.raw(b"\\r"),
            b'\t' => writer.raw(b"\\t"),
            8 => writer.raw(b"\\b"),
            12 => writer.raw(b"\\f"),
            0..=31 => {
                const HEX: &[u8] = b"0123456789abcdef";
                writer.raw(b"\\u00");
                writer.byte(HEX[(byte >> 4) as usize]);
                writer.byte(HEX[(byte & 15) as usize]);
            }
            _ => writer.byte(byte),
        }
    }
}
fn span(aggregate: &[u8], value: Span) -> &[u8] {
    aggregate
        .get(value.start..value.start.saturating_add(value.length))
        .unwrap_or(b"")
}
fn quoted_span(writer: &mut Writer<'_>, aggregate: &[u8], value: Span) {
    writer.string(span(aggregate, value));
}

impl<'a, 'b, 'c, 'd> Emitter<'a, 'b, 'c, 'd> {
    fn require_anthropic_origin(&self) -> Result<(), Error> {
        if self.config.source != self.config.target
            || self.config.source.protocol != Protocol::AnthropicMessages
        {
            return Err(Error::Unsupported {
                feature: "provider-owned Anthropic stream data in target",
            });
        }
        Ok(())
    }
    fn provider_usage_fields(&mut self, first: &mut bool) {
        for field in AnthropicUsageField::ALL {
            if let Some(value) = self.state.provider_usage[field as usize] {
                self.usage_key(first, field.name());
                self.writer.raw(span(self.aggregate, value));
            }
        }
    }
    fn provider_name(provider: Format) -> &'static [u8] {
        match provider.protocol {
            Protocol::AnthropicMessages => b"anthropic",
            Protocol::GeminiGenerateContent => b"gemini",
            Protocol::OpenAiChat => b"chat",
            Protocol::OpenAiResponses => b"responses",
            Protocol::OpenAiCompletions => b"completions",
            Protocol::GeminiInteractions => b"interactions",
        }
    }
    fn tool_index(&self, item: ActiveItem) -> u64 {
        self.items[..item.wire_index as usize]
            .iter()
            .filter(|v| v.kind == BlockKind::Tool && v.key.choice == item.key.choice)
            .count() as u64
    }
    fn provider_metadata(&mut self, item: ActiveItem) -> Result<(), Error> {
        if let (Some(signature), Some(provider)) = (item.signature, item.signature_provider) {
            self.writer.raw(b",\"provider_metadata\":{\"protocol\":");
            self.writer.string(Self::provider_name(provider));
            self.writer.raw(b",\"signature\":");
            quoted_span(&mut self.writer, self.aggregate, signature);
            self.writer.byte(b'}');
        }
        Ok(())
    }
    fn gemini_header(&mut self) {
        self.writer.raw(b"data: {\"responseId\":");
        quoted_span(&mut self.writer, self.aggregate, self.state.id);
        self.writer.raw(b",\"modelVersion\":");
        quoted_span(&mut self.writer, self.aggregate, self.state.model);
        self.writer.raw(b",\"candidates\":[{\"index\":0");
    }
    fn gemini_text_content(&mut self, slot: usize, text: Span) {
        let item = self.items[slot];
        self.writer
            .raw(b",\"content\":{\"role\":\"model\",\"parts\":[{\"text\":");
        quoted_span(&mut self.writer, self.aggregate, text);
        if item.kind == BlockKind::ReasoningSummary {
            self.writer.raw(b",\"thought\":true");
        }
        if let Some(signature) = item.signature {
            self.writer.raw(b",\"thoughtSignature\":");
            quoted_span(&mut self.writer, self.aggregate, signature);
        }
        self.writer.raw(b"}]}");
        let end = item
            .signature
            .map_or(text.start + text.length, |signature| {
                signature.start + signature.length
            });
        let contiguous = item
            .signature
            .is_none_or(|signature| signature.start == text.start + text.length);
        // Preview and failed writes must retain the original bytes for retry.
        // In chat mode a later part in this batch can reuse the same slot.
        if !self.config.gemini_chat_history && contiguous && self.state.aggregate_used == end {
            self.state.aggregate_used = text.start;
        }
        self.items[slot].pending_text = None;
        self.items[slot].signature = None;
    }
    fn flush_gemini_text(&mut self, slot: usize) -> Result<(), Error> {
        let item = self.items[slot];
        if let Some(text) = item.pending_text {
            self.writer.raw(b"data: {\"candidates\":[{\"index\":");
            number(&mut self.writer, u64::from(item.key.choice));
            self.gemini_text_content(slot, text);
            self.writer.raw(b"}]}\n\n");
        }
        Ok(())
    }
    fn gemini_tool_content(&mut self, item: ActiveItem) -> Result<(), Error> {
        self.writer
            .raw(b",\"content\":{\"role\":\"model\",\"parts\":[{\"functionCall\":{\"id\":");
        quoted_span(&mut self.writer, self.aggregate, item.id);
        self.writer.raw(b",\"name\":");
        quoted_span(&mut self.writer, self.aggregate, item.name);
        self.writer.raw(b",\"args\":");
        self.write_chain(item, false)?;
        self.writer.byte(b'}');
        if let Some(signature) = item.signature {
            self.writer.raw(b",\"thoughtSignature\":");
            quoted_span(&mut self.writer, self.aggregate, signature);
        }
        self.writer.raw(b"}]}");
        Ok(())
    }
    fn bind_frame_grounding(&mut self) -> Result<(), Error> {
        if let Some(metadata) = self.frame_grounding.take() {
            if self.state.gemini_chat_grounding.is_some() {
                return Err(Error::Unsupported {
                    feature: "multiple grounding updates for one Gemini chat part",
                });
            }
            self.state.gemini_chat_grounding = Some(metadata);
        }
        Ok(())
    }
    pub(super) fn end_frame(&mut self) -> Result<(), Error> {
        self.bind_frame_grounding()?;
        self.flush_usage()
    }
    fn emit_gemini_chat(&mut self, finish: Option<FinishReason>) -> Result<(), Error> {
        if finish.is_some() {
            self.bind_frame_grounding()?;
        }
        let part = self.state.gemini_chat_part.take();
        if part.is_none() && finish.is_none() {
            return Ok(());
        }
        self.gemini_header();
        if let Some(slot) = part {
            let item = self.items[slot];
            if item.kind == BlockKind::Tool {
                self.gemini_tool_content(item)?;
            } else if let Some(text) = item.pending_text {
                self.gemini_text_content(slot, text);
            } else {
                return Err(Error::InvalidInput {
                    field: "Gemini content",
                    reason: "missing pending part",
                });
            }
        }
        if let Some(grounding) = self.state.gemini_chat_grounding.take() {
            self.writer.raw(b",\"groundingMetadata\":");
            self.writer.raw(span(self.aggregate, grounding));
        }
        if let Some(reason) = finish {
            self.gemini_finish_fields(0, reason)?;
        }
        self.writer.raw(b"}]");
        if finish.is_some()
            && self.state.usage_seen
            && (self.state.usage.usage() != Usage::default()
                || self.state.provider_usage.iter().any(Option::is_some))
        {
            self.writer.raw(b",\"usageMetadata\":");
            self.write_usage(Protocol::GeminiGenerateContent)?;
        }
        self.end_event();
        Ok(())
    }
    fn store_signature(
        &mut self,
        slot: usize,
        text: Text<'_>,
        provider: Format,
    ) -> Result<(), Error> {
        let item = self.items[slot];
        if item
            .signature_provider
            .is_some_and(|old| old.protocol != provider.protocol)
        {
            return Err(Error::InvalidInput {
                field: "signature provider",
                reason: "origin changed",
            });
        }
        let signature = if let Some(old) = item.signature {
            if provider.protocol == Protocol::AnthropicMessages {
                let start = self.state.aggregate_used;
                let length = old
                    .length
                    .checked_add(text.len())
                    .ok_or(Error::EventTooLarge { needed: usize::MAX })?;
                let end = start
                    .checked_add(length)
                    .ok_or(Error::EventTooLarge { needed: usize::MAX })?;
                if end > self.aggregate.len() {
                    return Err(Error::EventTooLarge { needed: end });
                }
                self.aggregate
                    .copy_within(old.start..old.start + old.length, start);
                text.copy(&mut self.aggregate[start + old.length..end])?;
                self.state.aggregate_used = end;
                self.state.aggregate_peak = self.state.aggregate_peak.max(end);
                Span { start, length }
            } else {
                let equal = match text {
                    Text::Utf8(value) => value == span(self.aggregate, old),
                    Text::Json(value) => {
                        value.bytes().eq(span(self.aggregate, old).iter().copied())
                    }
                };
                if !equal {
                    return Err(Error::InvalidInput {
                        field: "signature",
                        reason: "conflicting signature",
                    });
                }
                old
            }
        } else {
            self.store(text)?
        };
        self.items[slot].signature = Some(signature);
        self.items[slot].signature_provider = Some(provider);
        Ok(())
    }
    fn reasoning_supported(&self, kind: BlockKind) -> bool {
        match kind {
            BlockKind::ReasoningSummary => matches!(
                self.config.target.protocol,
                Protocol::OpenAiResponses
                    | Protocol::GeminiGenerateContent
                    | Protocol::GeminiInteractions
            ),
            BlockKind::Reasoning => {
                matches!(
                    self.config.target.protocol,
                    Protocol::AnthropicMessages | Protocol::OpenAiResponses
                ) || (self.config.target.protocol == Protocol::OpenAiChat
                    && matches!(self.config.target.profile, Profile::OpenAiCompatible(_)))
            }
            _ => false,
        }
    }
    pub(super) fn new(
        config: StreamConfig<'a>,
        writer: Writer<'b>,
        state: &'c mut State,
        items: &'d mut [ActiveItem],
        aggregate: &'d mut [u8],
    ) -> Self {
        // All writes in this transaction use the lane that does not hold the
        // committed part. A preview or capacity retry can restore the old state.
        let chat_write_lane = 1 - state.gemini_text_lane;
        Self {
            config,
            writer,
            chat_write_lane,
            frame_grounding: None,
            state,
            items,
            aggregate,
        }
    }
    fn store_gemini_chat_text(&mut self, text: Text<'_>) -> Result<Span, Error> {
        let mut buffer = self.state.gemini_text_buffers[self.chat_write_lane];
        let length = text.len();
        if length > buffer.length {
            // A growing lane may leave its old reservation behind. Geometric
            // growth bounds the total cost in terms of the largest delta.
            let capacity = length
                .checked_next_power_of_two()
                .ok_or(Error::EventTooLarge { needed: usize::MAX })?;
            let start = self.state.aggregate_used;
            let needed = start
                .checked_add(capacity)
                .ok_or(Error::EventTooLarge { needed: usize::MAX })?;
            if needed > self.aggregate.len() {
                return Err(Error::EventTooLarge { needed });
            }
            buffer = Span {
                start,
                length: capacity,
            };
            self.state.gemini_text_buffers[self.chat_write_lane] = buffer;
            self.state.aggregate_used = needed;
            self.state.aggregate_peak = self.state.aggregate_peak.max(needed);
        }
        let end = buffer.start + length;
        text.copy(&mut self.aggregate[buffer.start..end])?;
        self.state.gemini_text_lane = self.chat_write_lane;
        Ok(Span {
            start: buffer.start,
            length,
        })
    }
    fn store(&mut self, text: Text<'_>) -> Result<Span, Error> {
        let start = self.state.aggregate_used;
        let length = text.len();
        let needed = start
            .checked_add(length)
            .ok_or(Error::EventTooLarge { needed: usize::MAX })?;
        let dst = self
            .aggregate
            .get_mut(start..needed)
            .ok_or(Error::EventTooLarge { needed })?;
        text.copy(dst)?;
        self.state.aggregate_used = needed;
        self.state.aggregate_peak = self.state.aggregate_peak.max(needed);
        Ok(Span { start, length })
    }
    fn append(&mut self, slot: usize, text: Text<'_>) -> Result<(), Error> {
        let length = text.len();
        if length == 0 {
            self.items[slot].saw_delta = true;
            return Ok(());
        }
        let start = self.state.aggregate_used;
        let data_start = start
            .checked_add(HEADER)
            .ok_or(Error::EventTooLarge { needed: usize::MAX })?;
        let end = data_start
            .checked_add(length)
            .ok_or(Error::EventTooLarge { needed: usize::MAX })?;
        let dst = self
            .aggregate
            .get_mut(data_start..end)
            .ok_or(Error::EventTooLarge { needed: end })?;
        text.copy(dst)?;
        write_usize(self.aggregate, start, NONE);
        write_usize(
            self.aggregate,
            start + core::mem::size_of::<usize>(),
            length,
        );
        let item = &mut self.items[slot];
        if item.tail != NONE {
            write_usize(self.aggregate, item.tail, start);
        } else {
            item.head = start;
        }
        item.tail = start;
        item.length = item
            .length
            .checked_add(length)
            .ok_or(Error::EventTooLarge { needed: usize::MAX })?;
        item.saw_delta = true;
        self.state.aggregate_used = end;
        self.state.aggregate_peak = self.state.aggregate_peak.max(end);
        Ok(())
    }
    fn lookup(&self, key: Key, kind: Option<BlockKind>) -> Option<usize> {
        self.items[..self.state.item_count]
            .iter()
            .position(|item| item.key == key && kind.is_none_or(|kind| item.kind == kind))
    }
    fn write_chain(&mut self, item: ActiveItem, quoted: bool) -> Result<(), Error> {
        if quoted {
            self.writer.byte(b'"');
        }
        let mut next = item.head;
        let mut count = 0usize;
        while next != NONE {
            count += 1;
            if count > self.state.aggregate_used / HEADER + 1 {
                return Err(Error::InvalidInput {
                    field: "aggregate",
                    reason: "cycle",
                });
            }
            let length = read_usize(self.aggregate, next + core::mem::size_of::<usize>())?;
            let start = next
                .checked_add(HEADER)
                .ok_or(Error::EventTooLarge { needed: usize::MAX })?;
            let end = start
                .checked_add(length)
                .ok_or(Error::EventTooLarge { needed: usize::MAX })?;
            let bytes = self.aggregate.get(start..end).ok_or(Error::InvalidInput {
                field: "aggregate",
                reason: "invalid segment",
            })?;
            if quoted {
                escaped(&mut self.writer, bytes);
            } else {
                self.writer.raw(bytes);
            }
            next = read_usize(self.aggregate, next)?;
        }
        if quoted {
            self.writer.byte(b'"');
        }
        Ok(())
    }
    fn item_id(&mut self, item: ActiveItem) {
        if let Some(id) = item.output_id {
            quoted_span(&mut self.writer, self.aggregate, id);
            return;
        }
        self.writer.byte(b'"');
        escaped(
            &mut self.writer,
            self.config.context.item_id_prefix.unwrap_or("").as_bytes(),
        );
        number(&mut self.writer, u64::from(item.wire_index));
        self.writer.byte(b'"');
    }
    fn named(&mut self, name: &'static [u8], field: &'static [u8]) {
        self.writer.raw(b"event: ");
        self.writer.raw(name);
        self.writer.raw(b"\ndata: {\"");
        self.writer.raw(field);
        self.writer.raw(b"\":\"");
        self.writer.raw(name);
        self.writer.byte(b'"');
    }
    fn response_event(&mut self, name: &'static [u8]) -> Result<(), Error> {
        self.named(name, b"type");
        self.writer.raw(b",\"sequence_number\":");
        number(&mut self.writer, self.state.sequence);
        self.state.sequence = self
            .state
            .sequence
            .checked_add(1)
            .ok_or(Error::LimitExceeded {
                kind: "stream sequence",
                limit: usize::MAX,
            })?;
        Ok(())
    }
    fn end_event(&mut self) {
        self.writer.raw(b"}\n\n");
    }
    fn usage_ready(&self) -> bool {
        let usage = self.state.usage.usage();
        if !self.state.usage_seen {
            return false;
        }
        if usage == Usage::default() {
            return true;
        }
        if self.config.allow_partial_usage {
            return true;
        }
        match self.config.target.protocol {
            Protocol::OpenAiChat | Protocol::OpenAiCompletions => {
                usage.input.is_some() && usage.output.is_some() && usage.total.is_some()
            }
            Protocol::AnthropicMessages => usage.input.is_some() && usage.output.is_some(),
            _ => true,
        }
    }
    fn end_chat_event(&mut self) -> Result<(), Error> {
        if self.config.usage_emission == UsageEmission::Continuous && self.usage_ready() {
            self.writer.raw(b",\"usage\":");
            self.write_usage(self.config.target.protocol)?;
            self.state.usage_pending = false;
        }
        self.end_event();
        Ok(())
    }
    pub(super) fn flush_usage(&mut self) -> Result<(), Error> {
        if self.config.usage_emission != UsageEmission::Continuous
            || !self.state.begun
            || self.state.ended
            || !self.state.usage_pending
            || !self.usage_ready()
        {
            return Ok(());
        }
        match self.config.target.protocol {
            Protocol::OpenAiChat | Protocol::OpenAiCompletions => {
                self.chat_header(self.config.target.protocol == Protocol::OpenAiCompletions);
                self.writer.raw(b",\"choices\":[],\"usage\":");
            }
            Protocol::AnthropicMessages => {
                self.named(b"message_delta", b"type");
                self.writer
                    .raw(b",\"delta\":{\"stop_reason\":null},\"usage\":");
            }
            Protocol::GeminiGenerateContent => {
                self.writer
                    .raw(b"data: {\"candidates\":[],\"usageMetadata\":");
            }
            _ => {
                return Err(Error::Unsupported {
                    feature: "continuous usage in target protocol",
                });
            }
        }
        self.write_usage(self.config.target.protocol)?;
        self.end_event();
        self.state.usage_pending = false;
        Ok(())
    }
    fn chat_header(&mut self, completion: bool) {
        self.writer.raw(b"data: {\"id\":");
        quoted_span(&mut self.writer, self.aggregate, self.state.id);
        self.writer.raw(if completion {
            b",\"object\":\"text_completion\",\"created\":"
        } else {
            b",\"object\":\"chat.completion.chunk\",\"created\":"
        });
        number(&mut self.writer, self.state.created.unwrap_or(0));
        self.writer.raw(b",\"model\":");
        quoted_span(&mut self.writer, self.aggregate, self.state.model);
    }
    fn choice_header(&mut self, choice: u32, completion: bool) {
        self.chat_header(completion);
        self.writer.raw(b",\"choices\":[{\"index\":");
        number(&mut self.writer, u64::from(choice));
    }
    fn response_object_header(&mut self, status: &'static [u8]) {
        self.writer.raw(b"{\"id\":");
        quoted_span(&mut self.writer, self.aggregate, self.state.id);
        self.writer.raw(b",\"object\":\"response\",\"created_at\":");
        number(&mut self.writer, self.state.created.unwrap_or(0));
        self.writer.raw(b",\"model\":");
        quoted_span(&mut self.writer, self.aggregate, self.state.model);
        if self.config.target.profile == Profile::OpenAiCompatible("lmm") {
            self.writer.raw(b",\"store\":false");
        }
        self.writer.raw(b",\"status\":");
        self.writer.string(status);
        self.writer.raw(b",\"error\":null,\"incomplete_details\":");
        self.writer.raw(if status == b"incomplete" {
            if self.items[..self.state.item_count]
                .iter()
                .any(|item| item.reason == FinishReason::Safety)
            {
                b"{\"reason\":\"content_filter\"}"
            } else {
                b"{\"reason\":\"max_output_tokens\"}"
            }
        } else {
            b"null"
        });
    }
    fn start_item(&mut self, slot: usize) -> Result<(), Error> {
        let item = self.items[slot];
        let key = item.key;
        match self.config.target.protocol {
            Protocol::OpenAiChat => {
                if !self.items[..slot]
                    .iter()
                    .any(|earlier| earlier.key.choice == key.choice)
                {
                    self.choice_header(key.choice, false);
                    self.writer
                        .raw(b",\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]");
                    self.end_chat_event()?;
                }
                if item.kind == BlockKind::Tool {
                    self.choice_header(key.choice, false);
                    self.writer.raw(b",\"delta\":{\"tool_calls\":[{\"index\":");
                    let index = self.tool_index(item);
                    number(&mut self.writer, index);
                    self.writer.raw(b",\"id\":");
                    quoted_span(&mut self.writer, self.aggregate, item.id);
                    self.writer
                        .raw(b",\"type\":\"function\",\"function\":{\"name\":");
                    quoted_span(&mut self.writer, self.aggregate, item.name);
                    self.writer
                        .raw(b",\"arguments\":\"\"}}]},\"finish_reason\":null}]");
                    self.end_chat_event()?;
                }
            }
            Protocol::OpenAiCompletions => {
                if item.kind != BlockKind::Text {
                    return Err(Error::Unsupported {
                        feature: "Completions non-text stream",
                    });
                }
            }
            Protocol::AnthropicMessages => {
                self.named(b"content_block_start", b"type");
                self.writer.raw(b",\"index\":");
                number(&mut self.writer, u64::from(item.wire_index));
                self.writer.raw(b",\"content_block\":");
                match item.kind {
                    BlockKind::Text => self.writer.raw(b"{\"type\":\"text\",\"text\":\"\"}"),
                    BlockKind::Reasoning => self
                        .writer
                        .raw(b"{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}"),
                    BlockKind::Tool => {
                        self.writer.raw(b"{\"type\":\"tool_use\",\"id\":");
                        quoted_span(&mut self.writer, self.aggregate, item.id);
                        self.writer.raw(b",\"name\":");
                        quoted_span(&mut self.writer, self.aggregate, item.name);
                        self.writer.raw(b",\"input\":{}}");
                    }
                    BlockKind::ReasoningSummary => {
                        return Err(Error::Unsupported {
                            feature: "reasoning summary in Anthropic",
                        });
                    }
                    BlockKind::Refusal => {
                        return Err(Error::Unsupported {
                            feature: "Anthropic refusal content stream",
                        });
                    }
                }
                self.end_event();
            }
            Protocol::OpenAiResponses => {
                if item.group_head == slot {
                    self.response_event(b"response.output_item.added")?;
                    self.writer.raw(b",\"output_index\":");
                    number(&mut self.writer, u64::from(item.wire_index));
                    self.writer.raw(b",\"item\":");
                    self.response_item(item, false)?;
                    self.end_event();
                }
                if item.kind != BlockKind::Tool {
                    self.response_event(if item.kind == BlockKind::ReasoningSummary {
                        b"response.reasoning_summary_part.added"
                    } else {
                        b"response.content_part.added"
                    })?;
                    self.writer.raw(b",\"output_index\":");
                    number(&mut self.writer, u64::from(item.wire_index));
                    self.writer.raw(b",\"item_id\":");
                    self.item_id(item);
                    self.response_part_index(item);
                    self.writer.raw(b",\"part\":");
                    self.writer.raw(match item.kind {
                        BlockKind::ReasoningSummary => b"{\"type\":\"summary_text\",\"text\":\"\"}",
                        BlockKind::Reasoning => b"{\"type\":\"reasoning_text\",\"text\":\"\"}",
                        BlockKind::Refusal => b"{\"type\":\"refusal\",\"refusal\":\"\"}",
                        _ => b"{\"type\":\"output_text\",\"text\":\"\",\"annotations\":[]}",
                    });
                    self.end_event();
                }
            }
            Protocol::GeminiGenerateContent => {}
            Protocol::GeminiInteractions => {
                self.named(b"step.start", b"event_type");
                self.writer.raw(b",\"index\":");
                number(&mut self.writer, u64::from(item.wire_index));
                self.writer.raw(b",\"step\":");
                match item.kind {
                    BlockKind::Text => self.writer.raw(b"{\"type\":\"model_output\"}"),
                    BlockKind::ReasoningSummary => self.writer.raw(b"{\"type\":\"thought\"}"),
                    BlockKind::Reasoning => {
                        return Err(Error::Unsupported {
                            feature: "raw thought in Interactions",
                        });
                    }
                    BlockKind::Tool => {
                        self.writer.raw(b"{\"type\":\"function_call\",\"id\":");
                        quoted_span(&mut self.writer, self.aggregate, item.id);
                        self.writer.raw(b",\"name\":");
                        quoted_span(&mut self.writer, self.aggregate, item.name);
                        self.writer.raw(b",\"arguments\":{}}");
                    }
                    BlockKind::Refusal => {
                        return Err(Error::Unsupported {
                            feature: "Interactions refusal stream",
                        });
                    }
                }
                self.end_event();
            }
        }
        Ok(())
    }
    fn response_part_index(&mut self, item: ActiveItem) {
        self.writer
            .raw(if item.kind == BlockKind::ReasoningSummary {
                b",\"summary_index\":"
            } else {
                b",\"content_index\":"
            });
        number(&mut self.writer, u64::from(item.part_index));
    }
    fn response_group_parts(
        &mut self,
        item: ActiveItem,
        kind: Option<BlockKind>,
    ) -> Result<(), Error> {
        let count = self.items[..self.state.item_count]
            .iter()
            .filter(|member| {
                member.group_head == item.group_head && kind.is_none_or(|kind| member.kind == kind)
            })
            .count();
        for part in 0..count {
            let member = self.items[..self.state.item_count]
                .iter()
                .find(|member| {
                    member.group_head == item.group_head
                        && kind.is_none_or(|kind| member.kind == kind)
                        && member.part_index as usize == part
                })
                .copied()
                .ok_or(Error::InvalidInput {
                    field: "Responses content index",
                    reason: "non-contiguous final parts",
                })?;
            if part != 0 {
                self.writer.byte(b',');
            }
            self.response_part(member)?;
        }
        Ok(())
    }
    fn response_item(&mut self, item: ActiveItem, complete: bool) -> Result<(), Error> {
        self.writer.raw(b"{\"id\":");
        self.item_id(item);
        if item.kind == BlockKind::Tool {
            self.writer.raw(b",\"type\":\"function_call\",\"call_id\":");
            quoted_span(&mut self.writer, self.aggregate, item.id);
            self.writer.raw(b",\"name\":");
            quoted_span(&mut self.writer, self.aggregate, item.name);
            self.writer.raw(b",\"arguments\":");
            if complete {
                self.write_chain(item, true)?;
            } else {
                self.writer.raw(b"\"\"");
            }
        } else if item.kind == BlockKind::Text || item.kind == BlockKind::Refusal {
            self.writer
                .raw(b",\"type\":\"message\",\"role\":\"assistant\",\"content\":[");
            if complete {
                self.response_group_parts(item, None)?;
            }
            self.writer.byte(b']');
        } else {
            self.writer.raw(b",\"type\":\"reasoning\",\"summary\":[");
            if complete {
                self.response_group_parts(item, Some(BlockKind::ReasoningSummary))?;
            }
            self.writer.raw(b"],\"content\":[");
            if complete {
                self.response_group_parts(item, Some(BlockKind::Reasoning))?;
            }
            self.writer.byte(b']');
        }
        if complete {
            // Signatures belong to the output item. A content group must not
            // silently discard signatures attached to a later part.
            let mut signature = None;
            for member in &self.items[..self.state.item_count] {
                if member.group_head == item.group_head
                    && let Some(value) = member.signature
                {
                    if let Some((previous, previous_value)) = signature {
                        let previous: ActiveItem = previous;
                        if previous.signature_provider != member.signature_provider
                            || span(self.aggregate, previous_value) != span(self.aggregate, value)
                        {
                            return Err(Error::InvalidInput {
                                field: "signature",
                                reason: "conflicting output item signatures",
                            });
                        }
                    } else {
                        signature = Some((*member, value));
                    }
                }
            }
            if let Some((signed, _)) = signature {
                self.provider_metadata(signed)?;
            }
        }
        self.writer.raw(b",\"status\":");
        let incomplete = self.items[..self.state.item_count]
            .iter()
            .any(|member| member.group_head == item.group_head && member.incomplete);
        self.writer.string(if complete {
            if incomplete {
                b"incomplete"
            } else {
                b"completed"
            }
        } else {
            b"in_progress"
        });
        self.writer.byte(b'}');
        Ok(())
    }
    fn response_part(&mut self, item: ActiveItem) -> Result<(), Error> {
        self.writer.raw(match item.kind {
            BlockKind::ReasoningSummary => b"{\"type\":\"summary_text\",\"text\":",
            BlockKind::Reasoning => b"{\"type\":\"reasoning_text\",\"text\":",
            BlockKind::Refusal => b"{\"type\":\"refusal\",\"refusal\":",
            _ => b"{\"type\":\"output_text\",\"text\":",
        });
        self.write_chain(item, true)?;
        if item.kind == BlockKind::Text {
            self.writer.raw(b",\"annotations\":[]");
        }
        self.writer.byte(b'}');
        Ok(())
    }
    fn close_slot(&mut self, slot: usize) -> Result<(), Error> {
        if self.items[slot].closed {
            return Ok(());
        }
        if self.config.target.protocol == Protocol::GeminiGenerateContent
            && self.items[slot].kind != BlockKind::Tool
            && !self.config.gemini_chat_history
        {
            self.flush_gemini_text(slot)?;
        }
        if self.items[slot].kind.is_reasoning()
            && self.config.policy.drop_reasoning
            && !self.reasoning_supported(self.items[slot].kind)
        {
            self.items[slot].closed = true;
            return Ok(());
        }
        if self.items[slot].kind == BlockKind::Tool
            && self.items[slot].length == 0
            && !self.items[slot].incomplete
        {
            if matches!(
                self.config.source.protocol,
                Protocol::AnthropicMessages | Protocol::GeminiInteractions
            ) {
                self.append(slot, Text::Utf8(b"{}"))?;
                self.items[slot].content_length = 2;
            } else {
                return Err(Error::InvalidInput {
                    field: "tool arguments",
                    reason: "empty/incomplete JSON",
                });
            }
        }
        let item = self.items[slot];
        match self.config.target.protocol {
            Protocol::AnthropicMessages => {
                self.named(b"content_block_stop", b"type");
                self.writer.raw(b",\"index\":");
                number(&mut self.writer, u64::from(item.wire_index));
                self.end_event();
            }
            Protocol::OpenAiResponses => {
                self.response_event(match item.kind {
                    BlockKind::Tool => b"response.function_call_arguments.done",
                    BlockKind::ReasoningSummary => b"response.reasoning_summary_text.done",
                    BlockKind::Reasoning => b"response.reasoning_text.done",
                    BlockKind::Refusal => b"response.refusal.done",
                    BlockKind::Text => b"response.output_text.done",
                })?;
                self.writer.raw(b",\"item_id\":");
                self.item_id(item);
                self.writer.raw(b",\"output_index\":");
                number(&mut self.writer, u64::from(item.wire_index));
                if item.kind != BlockKind::Tool {
                    self.response_part_index(item);
                }
                self.writer.raw(match item.kind {
                    BlockKind::Tool => b",\"arguments\":",
                    BlockKind::Refusal => b",\"refusal\":",
                    _ => b",\"text\":",
                });
                self.write_chain(item, true)?;
                self.end_event();
                if item.kind != BlockKind::Tool {
                    self.response_event(if item.kind == BlockKind::ReasoningSummary {
                        b"response.reasoning_summary_part.done"
                    } else {
                        b"response.content_part.done"
                    })?;
                    self.writer.raw(b",\"item_id\":");
                    self.item_id(item);
                    self.writer.raw(b",\"output_index\":");
                    number(&mut self.writer, u64::from(item.wire_index));
                    self.response_part_index(item);
                    self.writer.raw(b",\"part\":");
                    self.response_part(item)?;
                    self.end_event();
                }
                if self.items[..self.state.item_count]
                    .iter()
                    .enumerate()
                    .all(|(index, member)| {
                        member.group_head != item.group_head || member.closed || index == slot
                    })
                {
                    self.response_event(b"response.output_item.done")?;
                    self.writer.raw(b",\"output_index\":");
                    number(&mut self.writer, u64::from(item.wire_index));
                    self.writer.raw(b",\"item\":");
                    self.response_item(self.items[item.group_head], true)?;
                    self.end_event();
                }
            }
            Protocol::GeminiGenerateContent if item.kind == BlockKind::Tool => {
                if self.config.gemini_chat_history {
                    self.emit_gemini_chat(None)?;
                    self.state.gemini_chat_part = Some(slot);
                } else {
                    self.writer.raw(b"data: {\"candidates\":[{\"index\":");
                    number(&mut self.writer, u64::from(item.key.choice));
                    self.gemini_tool_content(item)?;
                    self.writer.raw(b"}]}\n\n");
                }
            }
            Protocol::GeminiInteractions => {
                self.named(b"step.stop", b"event_type");
                self.writer.raw(b",\"index\":");
                number(&mut self.writer, u64::from(item.wire_index));
                self.end_event();
            }
            _ => {}
        }
        self.items[slot].closed = true;
        Ok(())
    }
    fn uncached_input(&self) -> Result<Option<u64>, Error> {
        let accumulator = &self.state.usage;
        if accumulator.input_basis == InputCount::Uncached {
            return Ok(accumulator.input_base);
        }
        let usage = accumulator.usage();
        if let Some(input) = usage.uncached_input {
            return Ok(Some(input));
        }
        usage
            .input
            .map(|input| {
                crate::protocol::anthropic::uncached_input(input, usage.cached, usage.cache_write)
            })
            .transpose()
    }
    fn write_usage(&mut self, protocol: Protocol) -> Result<(), Error> {
        let mut usage = self.state.usage.usage();
        if usage == Usage::default() && self.state.provider_usage.iter().all(Option::is_none) {
            self.writer.raw(b"null");
            return Ok(());
        }
        decode::validate_lifetimes(usage)?;
        let openai = matches!(
            protocol,
            Protocol::OpenAiChat | Protocol::OpenAiResponses | Protocol::OpenAiCompletions
        );
        let vendor = openai && matches!(self.config.target.profile, Profile::OpenAiCompatible(_));
        let anthropic = protocol == Protocol::AnthropicMessages;
        let google = matches!(
            protocol,
            Protocol::GeminiGenerateContent | Protocol::GeminiInteractions
        );
        let anthropic_details = usage.anthropic_cache_details
            || usage.cache_write_5m.is_some()
            || usage.cache_write_1h.is_some();
        let openai_details = usage.openai_cache_details
            || usage.cache_write_30m.is_some()
            || usage.cache_write_12h.is_some();
        if (anthropic_details && !(anthropic || vendor))
            || (openai_details && !vendor)
            || (usage.cache_miss.is_some() && !vendor)
            || (usage.cost.is_some() && !vendor)
            || (usage.tool_use_prompt.is_some() && protocol != Protocol::GeminiGenerateContent)
        {
            return Err(Error::Unsupported {
                feature: "target usage extensions",
            });
        }
        if anthropic {
            usage.input = self.uncached_input()?;
        } else if usage.cache_write.is_some() && !vendor {
            self.extension(DroppedField::CacheCreationTokens)?;
            usage.cache_write = None;
        }
        if google {
            usage.output = usage
                .output
                .map(|output| {
                    output
                        .checked_sub(usage.reasoning.unwrap_or(0))
                        .ok_or(Error::InvalidInput {
                            field: "usage",
                            reason: "reasoning tokens exceed canonical output",
                        })
                })
                .transpose()?;
        }
        if !self.config.allow_partial_usage {
            if anthropic && (usage.input.is_none() || usage.output.is_none()) {
                return Err(Error::ContextRequired {
                    field: "final usage input/output",
                });
            }
            if openai && (usage.input.is_none() || usage.output.is_none() || usage.total.is_none())
            {
                if usage == Usage::default() {
                    self.writer.raw(b"null");
                    return Ok(());
                }
                return Err(Error::ContextRequired {
                    field: "usage total (not derivable across all token-accounting schemes)",
                });
            }
        }
        let (input, output, total, cached, reasoning) = match protocol {
            Protocol::OpenAiChat | Protocol::OpenAiCompletions => (
                "prompt_tokens",
                "completion_tokens",
                "total_tokens",
                "cached_tokens",
                "reasoning_tokens",
            ),
            Protocol::OpenAiResponses => (
                "input_tokens",
                "output_tokens",
                "total_tokens",
                "cached_tokens",
                "reasoning_tokens",
            ),
            Protocol::AnthropicMessages => (
                "input_tokens",
                "output_tokens",
                "",
                "cache_read_input_tokens",
                "",
            ),
            Protocol::GeminiGenerateContent => (
                "promptTokenCount",
                "candidatesTokenCount",
                "totalTokenCount",
                "cachedContentTokenCount",
                "thoughtsTokenCount",
            ),
            Protocol::GeminiInteractions => (
                "total_input_tokens",
                "total_output_tokens",
                "total_tokens",
                "total_cached_tokens",
                "total_thought_tokens",
            ),
        };
        self.writer.byte(b'{');
        let mut first = true;
        for (name, value) in [
            (input, usage.input),
            (output, usage.output),
            (total, usage.total),
            (if openai { "" } else { cached }, usage.cached),
            (if openai { "" } else { reasoning }, usage.reasoning),
        ] {
            if name.is_empty() {
                continue;
            }
            self.usage_counter(&mut first, name, value);
        }
        if openai {
            let details = if protocol == Protocol::OpenAiResponses {
                "input_tokens_details"
            } else {
                "prompt_tokens_details"
            };
            if usage.cached.is_some() || (vendor && (usage.cache_write.is_some() || openai_details))
            {
                self.usage_key(&mut first, details);
                self.writer.byte(b'{');
                let mut nested = true;
                self.usage_counter(&mut nested, cached, usage.cached);
                if vendor {
                    self.usage_counter(&mut nested, "cache_write_tokens", usage.cache_write);
                    if openai_details {
                        self.usage_key(&mut nested, "cache_write_tokens_details");
                        self.writer.byte(b'{');
                        let mut ttl = true;
                        self.usage_counter(&mut ttl, "ttl_30m_tokens", usage.cache_write_30m);
                        self.usage_counter(&mut ttl, "ttl_12h_tokens", usage.cache_write_12h);
                        self.writer.byte(b'}');
                    }
                }
                self.writer.byte(b'}');
            }
            if usage.reasoning.is_some() {
                self.usage_key(
                    &mut first,
                    if protocol == Protocol::OpenAiResponses {
                        "output_tokens_details"
                    } else {
                        "completion_tokens_details"
                    },
                );
                self.writer.byte(b'{');
                let mut nested = true;
                self.usage_counter(&mut nested, reasoning, usage.reasoning);
                self.writer.byte(b'}');
            }
        }
        if anthropic || vendor {
            self.usage_counter(&mut first, "cache_creation_input_tokens", usage.cache_write);
            if anthropic_details {
                self.usage_key(
                    &mut first,
                    if anthropic {
                        "cache_creation"
                    } else {
                        "cache_creation_details"
                    },
                );
                self.writer.byte(b'{');
                let mut ttl = true;
                self.usage_counter(&mut ttl, "ephemeral_5m_input_tokens", usage.cache_write_5m);
                self.usage_counter(&mut ttl, "ephemeral_1h_input_tokens", usage.cache_write_1h);
                self.writer.byte(b'}');
            }
        }
        if vendor {
            self.usage_counter(&mut first, "prompt_cache_miss_tokens", usage.cache_miss);
            if let Some(cost) = usage.cost {
                self.usage_key(&mut first, "cost");
                self.writer.raw(cost.raw());
            }
        }
        if protocol == Protocol::GeminiGenerateContent {
            self.usage_counter(&mut first, "toolUsePromptTokenCount", usage.tool_use_prompt);
        }
        if protocol == Protocol::AnthropicMessages {
            self.provider_usage_fields(&mut first);
        }
        self.writer.byte(b'}');
        Ok(())
    }
    fn usage_key(&mut self, first: &mut bool, name: &str) {
        if !*first {
            self.writer.byte(b',');
        }
        *first = false;
        self.writer.string(name.as_bytes());
        self.writer.byte(b':');
    }
    fn usage_counter(&mut self, first: &mut bool, name: &str, value: Option<u64>) {
        if let Some(value) = value {
            self.usage_key(first, name);
            number(&mut self.writer, value);
        }
    }
    fn gemini_finish_fields(&mut self, choice: u32, reason: FinishReason) -> Result<(), Error> {
        if reason == FinishReason::Continuation && self.state.continuation.is_none() {
            return Err(Error::ContextRequired {
                field: "continuationToken",
            });
        }
        self.writer.raw(b",\"finishReason\":");
        self.writer.string(match reason {
            FinishReason::Stop | FinishReason::ToolCalls => b"STOP",
            FinishReason::Length => b"MAX_TOKENS",
            FinishReason::Safety => b"SAFETY",
            FinishReason::Continuation => b"CONTINUATION",
            FinishReason::Other => {
                return Err(Error::Unsupported {
                    feature: "finish reason",
                });
            }
        });
        if let Some((owner, token)) = self.state.continuation {
            if owner != choice || reason != FinishReason::Continuation {
                return Err(Error::InvalidInput {
                    field: "continuationToken",
                    reason: "choice/finish mismatch",
                });
            }
            self.writer.raw(b",\"continuationToken\":");
            quoted_span(&mut self.writer, self.aggregate, token);
            self.state.continuation = None;
        }
        Ok(())
    }
    fn emit_choice(&mut self, choice: u32, reason: FinishReason) -> Result<(), Error> {
        match self.config.target.protocol {
            Protocol::OpenAiChat | Protocol::OpenAiCompletions => {
                let legacy = self.config.target.protocol == Protocol::OpenAiCompletions;
                self.choice_header(choice, legacy);
                self.writer.raw(if legacy {
                    b",\"text\":\"\",\"logprobs\":null,\"finish_reason\":"
                } else {
                    b",\"delta\":{},\"finish_reason\":"
                });
                self.writer.string(match reason {
                    FinishReason::Stop => b"stop",
                    FinishReason::Length => b"length",
                    FinishReason::ToolCalls => b"tool_calls",
                    FinishReason::Safety => b"content_filter",
                    FinishReason::Continuation | FinishReason::Other => {
                        return Err(Error::Unsupported {
                            feature: "finish reason",
                        });
                    }
                });
                self.writer.raw(b"}]");
                self.end_chat_event()?;
            }
            Protocol::GeminiGenerateContent if !self.config.gemini_chat_history => {
                self.writer.raw(b"data: {\"candidates\":[{\"index\":");
                number(&mut self.writer, u64::from(choice));
                self.gemini_finish_fields(choice, reason)?;
                self.writer.raw(b"}]}\n\n");
            }
            _ => {}
        }
        Ok(())
    }
}

impl Sink for Emitter<'_, '_, '_, '_> {
    fn gemini_part(
        &mut self,
        key: Key,
        kind: BlockKind,
        text: Option<Text<'_>>,
        signature: Option<Text<'_>>,
        _part: Text<'_>,
    ) -> Result<(), Error> {
        // A Gemini chunk has no persistent part ID. Only its first part can
        // continue the previous chunk's last block in the same candidate.
        let last = self.items[..self.state.item_count]
            .iter()
            .rev()
            .find(|item| item.key.choice == key.choice);
        let key = match last {
            Some(item)
                if !item.closed
                    && item.kind == kind
                    && item.key.index == key.index
                    && (text.is_none() || key.part == 0) =>
            {
                item.key
            }
            _ if text.is_some() => Key {
                part: u32::try_from(self.state.item_count).map_err(|_| {
                    Error::TooManyActiveItems {
                        needed: self.state.item_count,
                    }
                })?,
                ..key
            },
            _ => {
                return Err(Error::InvalidInput {
                    field: "signature",
                    reason: "no current Gemini text part",
                });
            }
        };
        if let Some(text) = text {
            self.delta(key, kind, text)?;
        }
        if let Some(signature) = signature {
            self.signature(key, signature, None)?;
        }
        Ok(())
    }
    fn provider_block(&mut self, key: Key, value: Value<'_, '_>) -> Result<(), Error> {
        self.require_anthropic_origin()?;
        let provider_block = decode::validate_provider_block(value)?;
        if !self.state.begun
            || self.state.ended
            || self.state.seen_finish
            || key.choice != 0
            || key.part != 0
            || self.lookup(key, None).is_some()
        {
            return Err(Error::InvalidInput {
                field: "provider block",
                reason: "invalid block lifecycle",
            });
        }
        let slot = self.state.item_count;
        if slot >= self.items.len() || slot > u32::MAX as usize {
            return Err(Error::TooManyActiveItems {
                needed: slot.saturating_add(1),
            });
        }
        let kind = if provider_block == ProviderBlock::Tool {
            BlockKind::Tool
        } else {
            BlockKind::Text
        };
        let (id, name) = if kind == BlockKind::Tool {
            let id = value
                .get("id")
                .and_then(Value::as_string)
                .ok_or(Error::InvalidInput {
                    field: "id",
                    reason: "missing server tool identity",
                })?;
            if self.items[..slot].iter().any(|item| {
                item.kind == BlockKind::Tool
                    && id.bytes().eq(span(self.aggregate, item.id).iter().copied())
            }) {
                return Err(Error::InvalidInput {
                    field: "tool id",
                    reason: "duplicate call id",
                });
            }
            let name = value
                .get("name")
                .and_then(Value::as_string)
                .ok_or(Error::InvalidInput {
                    field: "name",
                    reason: "missing server tool name",
                })?;
            (self.store(Text::Json(id))?, self.store(Text::Json(name))?)
        } else {
            (Span::EMPTY, Span::EMPTY)
        };
        self.items[slot] = ActiveItem {
            used: true,
            key,
            kind,
            id,
            name,
            provider_block,
            group_head: slot,
            wire_index: slot as u32,
            ..ActiveItem::EMPTY
        };
        self.state.item_count += 1;
        if kind == BlockKind::Tool
            && let Some(input) = value.get("input").filter(|v| v.object().next().is_some())
        {
            self.append(slot, Text::Utf8(input.raw()))?;
            self.items[slot].content_length = input.raw().len();
        }
        self.named(b"content_block_start", b"type");
        self.writer.raw(b",\"index\":");
        number(&mut self.writer, slot as u64);
        self.writer.raw(b",\"content_block\":");
        self.writer.raw(value.raw());
        self.end_event();
        Ok(())
    }
    fn provider_delta(&mut self, key: Key, value: Value<'_, '_>) -> Result<(), Error> {
        self.require_anthropic_origin()?;
        decode::validate_provider_delta(value)?;
        let slot = self
            .lookup(key, Some(BlockKind::Text))
            .ok_or(Error::InvalidInput {
                field: "citation delta",
                reason: "text block missing",
            })?;
        let item = self.items[slot];
        if item.closed
            || item.finished
            || item.provider_block == ProviderBlock::Opaque
            || self.state.ended
        {
            return Err(Error::InvalidInput {
                field: "citation delta",
                reason: "outside text block lifecycle",
            });
        }
        self.named(b"content_block_delta", b"type");
        self.writer.raw(b",\"index\":");
        number(&mut self.writer, u64::from(item.wire_index));
        self.writer.raw(b",\"delta\":");
        self.writer.raw(value.raw());
        self.end_event();
        Ok(())
    }
    fn provider_usage(
        &mut self,
        field: AnthropicUsageField,
        value: Value<'_, '_>,
    ) -> Result<(), Error> {
        self.require_anthropic_origin()?;
        decode::validate_provider_usage(field, value)?;
        if self.state.ended {
            return Err(Error::AlreadyFinished);
        }
        if self.state.provider_usage[field as usize]
            .is_some_and(|old| span(self.aggregate, old) == value.raw())
        {
            return Ok(());
        }
        self.state.provider_usage[field as usize] = Some(self.store(Text::Utf8(value.raw()))?);
        Ok(())
    }
    fn begin(
        &mut self,
        id: Option<Text<'_>>,
        model: Option<Text<'_>>,
        created: Option<u64>,
    ) -> Result<(), Error> {
        if self.state.begun {
            self.bind_frame_grounding()?;
            for (supplied, stored) in [(id, self.state.id), (model, self.state.model)] {
                if let Some(supplied) = supplied {
                    let equal = match supplied {
                        Text::Utf8(bytes) => bytes == span(self.aggregate, stored),
                        Text::Json(value) => value
                            .bytes()
                            .eq(span(self.aggregate, stored).iter().copied()),
                    };
                    if !equal {
                        return Err(Error::InvalidInput {
                            field: "stream identity",
                            reason: "identity changed",
                        });
                    }
                }
            }
            if created.is_some() && created != self.state.created {
                return Err(Error::InvalidInput {
                    field: "created",
                    reason: "timestamp changed",
                });
            }
            return Ok(());
        }
        let id = id
            .or(self
                .config
                .context
                .response_id
                .map(|value| Text::Utf8(value.as_bytes())))
            .ok_or(Error::ContextRequired {
                field: "response_id",
            })?;
        let model = model
            .or(self
                .config
                .context
                .model
                .map(|value| Text::Utf8(value.as_bytes())))
            .ok_or(Error::ContextRequired { field: "model" })?;
        self.state.id = self.store(id)?;
        self.state.model = self.store(model)?;
        self.state.created = created.or(self.config.context.created);
        if matches!(
            self.config.target.protocol,
            Protocol::OpenAiChat | Protocol::OpenAiCompletions | Protocol::OpenAiResponses
        ) && self.state.created.is_none()
        {
            return Err(Error::ContextRequired { field: "created" });
        }
        self.state.begun = true;
        match self.config.target.protocol {
            Protocol::OpenAiChat => {}
            Protocol::OpenAiCompletions => {}
            Protocol::AnthropicMessages => {
                let input = self.uncached_input()?;
                if input.is_none() && !self.config.allow_partial_usage {
                    return Err(Error::ContextRequired {
                        field: "initial_usage.input",
                    });
                }
                self.named(b"message_start", b"type");
                self.writer.raw(b",\"message\":{\"id\":");
                quoted_span(&mut self.writer, self.aggregate, self.state.id);
                self.writer
                    .raw(b",\"type\":\"message\",\"role\":\"assistant\",\"model\":");
                quoted_span(&mut self.writer, self.aggregate, self.state.model);
                self.writer
                    .raw(b",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"usage\":");
                let output = if self.config.allow_partial_usage {
                    self.state.usage.usage().output
                } else {
                    Some(self.state.usage.usage().output.unwrap_or(0))
                };
                if input.is_none()
                    && output.is_none()
                    && self.state.provider_usage.iter().all(Option::is_none)
                {
                    self.writer.raw(b"null");
                } else {
                    self.writer.byte(b'{');
                    let mut first = true;
                    self.usage_counter(&mut first, "input_tokens", input);
                    self.usage_counter(&mut first, "output_tokens", output);
                    self.provider_usage_fields(&mut first);
                    self.writer.byte(b'}');
                }
                self.writer.byte(b'}');
                self.end_event();
            }
            Protocol::OpenAiResponses => {
                self.response_event(b"response.created")?;
                self.writer.raw(b",\"response\":");
                self.response_object_header(b"in_progress");
                self.writer.raw(b",\"output\":[],\"usage\":null}");
                self.end_event();
                self.response_event(b"response.in_progress")?;
                self.writer.raw(b",\"response\":");
                self.response_object_header(b"in_progress");
                self.writer.raw(b",\"output\":[],\"usage\":null}");
                self.end_event();
            }
            Protocol::GeminiGenerateContent if self.config.gemini_chat_history => {}
            Protocol::GeminiGenerateContent => {
                self.writer.raw(b"data: {\"responseId\":");
                quoted_span(&mut self.writer, self.aggregate, self.state.id);
                self.writer.raw(b",\"modelVersion\":");
                quoted_span(&mut self.writer, self.aggregate, self.state.model);
                self.writer.raw(b",\"candidates\":[]}\n\n");
            }
            Protocol::GeminiInteractions => {
                self.named(b"interaction.created", b"event_type");
                self.writer.raw(b",\"interaction\":{\"id\":");
                quoted_span(&mut self.writer, self.aggregate, self.state.id);
                self.writer.raw(b",\"model\":");
                quoted_span(&mut self.writer, self.aggregate, self.state.model);
                self.writer
                    .raw(b",\"status\":\"in_progress\",\"object\":\"interaction\"}");
                self.end_event();
            }
        }
        Ok(())
    }
    fn block(
        &mut self,
        key: Key,
        kind: BlockKind,
        id: Option<Text<'_>>,
        name: Option<Text<'_>>,
        item_id: Option<Text<'_>>,
    ) -> Result<(), Error> {
        if !self.state.begun || self.state.ended {
            return Err(Error::InvalidInput {
                field: "block start",
                reason: "outside stream lifecycle",
            });
        }
        if self.config.gemini_chat_history && key.choice != 0 {
            return Err(Error::Unsupported {
                feature: "multiple Gemini chat candidates",
            });
        }
        if self.lookup(key, Some(kind)).is_some() {
            if kind == BlockKind::Tool && self.config.source.protocol == Protocol::OpenAiChat {
                return Err(Error::Unsupported {
                    feature: "fragmented/repeated tool name or id header",
                });
            }
            return Err(Error::InvalidInput {
                field: "block start",
                reason: "duplicate block index",
            });
        }
        if matches!(
            self.config.source.protocol,
            Protocol::AnthropicMessages | Protocol::OpenAiResponses | Protocol::GeminiInteractions
        ) && self.lookup(key, None).is_some_and(|slot| {
            !(self.config.source.protocol == Protocol::OpenAiResponses
                && kind.is_reasoning()
                && self.items[slot].kind.is_reasoning())
        }) {
            return Err(Error::InvalidInput {
                field: "block start",
                reason: "index reused with a different block type",
            });
        }
        if key.choice != 0
            && !matches!(
                self.config.target.protocol,
                Protocol::OpenAiChat
                    | Protocol::OpenAiCompletions
                    | Protocol::GeminiGenerateContent
            )
        {
            return Err(Error::Unsupported {
                feature: "multiple stream choices",
            });
        }
        if kind.is_reasoning()
            && !self.reasoning_supported(kind)
            && !self.config.policy.drop_reasoning
        {
            return Err(Error::Unsupported {
                feature: "reasoning stream target",
            });
        }
        if kind == BlockKind::Refusal
            && !matches!(
                self.config.target.protocol,
                Protocol::OpenAiChat | Protocol::OpenAiResponses
            )
        {
            return Err(Error::Unsupported {
                feature: "refusal stream target",
            });
        }
        let slot = self.state.item_count;
        if slot >= self.items.len() {
            return Err(Error::TooManyActiveItems {
                needed: slot.saturating_add(1),
            });
        }
        if slot > u32::MAX as usize {
            return Err(Error::LimitExceeded {
                kind: "stream item index",
                limit: u32::MAX as usize,
            });
        }
        let (id, name) = if kind == BlockKind::Tool {
            let id = id
                .or_else(|| {
                    self.config
                        .context
                        .tool_ids
                        .iter()
                        .find(|entry| entry.key == key)
                        .map(|entry| Text::Utf8(entry.id.as_bytes()))
                })
                .ok_or(Error::ContextRequired {
                    field: "tool call id",
                })?;
            let name = name.ok_or(Error::Unsupported {
                feature: "stream tool header missing a complete function name",
            })?;
            if id.len() == 0 || name.len() == 0 {
                return Err(Error::InvalidInput {
                    field: "tool call",
                    reason: "empty id/name",
                });
            }
            for existing in &self.items[..self.state.item_count] {
                if existing.kind == BlockKind::Tool && existing.key.choice == key.choice {
                    let same = match id {
                        Text::Utf8(bytes) => bytes == span(self.aggregate, existing.id),
                        Text::Json(value) => value
                            .bytes()
                            .eq(span(self.aggregate, existing.id).iter().copied()),
                    };
                    if same {
                        return Err(Error::InvalidInput {
                            field: "tool id",
                            reason: "duplicate call id",
                        });
                    }
                }
            }
            (self.store(id)?, self.store(name)?)
        } else {
            (Span::EMPTY, Span::EMPTY)
        };
        let group_head = if self.config.source.protocol == Protocol::OpenAiResponses
            && self.config.target.protocol == Protocol::OpenAiResponses
        {
            self.items[..slot]
                .iter()
                .position(|item| item.key.index == key.index && item.key.choice == key.choice)
                .unwrap_or(slot)
        } else {
            slot
        };
        let (wire_index, part_index, output_id) = if group_head != slot {
            let head = self.items[group_head];
            if self.items[..slot]
                .iter()
                .any(|item| item.group_head == group_head && item.closed)
            {
                return Err(Error::InvalidInput {
                    field: "output item",
                    reason: "content added after item closed",
                });
            }
            if head.kind.is_reasoning() != kind.is_reasoning()
                || head.kind == BlockKind::Tool
                || kind == BlockKind::Tool
            {
                return Err(Error::InvalidInput {
                    field: "output item",
                    reason: "inconsistent content kinds",
                });
            }
            if let (Some(expected), Some(provided)) = (head.output_id, item_id) {
                let equal = match provided {
                    Text::Utf8(bytes) => bytes == span(self.aggregate, expected),
                    Text::Json(text) => text
                        .bytes()
                        .eq(span(self.aggregate, expected).iter().copied()),
                };
                if !equal {
                    return Err(Error::InvalidInput {
                        field: "item_id",
                        reason: "output item identity changed",
                    });
                }
            }
            (head.wire_index, key.part, head.output_id)
        } else {
            let groups = if self.config.target.protocol == Protocol::OpenAiResponses {
                self.items[..slot]
                    .iter()
                    .enumerate()
                    .filter(|(index, item)| item.group_head == *index)
                    .count()
            } else {
                slot
            };
            let part_index = if self.config.source.protocol == Protocol::OpenAiResponses
                && self.config.target.protocol == Protocol::OpenAiResponses
            {
                key.part
            } else {
                0
            };
            (
                groups as u32,
                part_index,
                item_id.map(|id| self.store(id)).transpose()?,
            )
        };
        self.items[slot] = ActiveItem {
            used: true,
            output_id,
            key,
            kind,
            wire_index,
            group_head,
            part_index,
            id,
            name,
            ..ActiveItem::EMPTY
        };
        self.state.item_count += 1;
        if kind.is_reasoning()
            && self.config.policy.drop_reasoning
            && !self.reasoning_supported(kind)
        {
            return Ok(());
        }
        self.start_item(slot)
    }
    fn delta(&mut self, key: Key, kind: BlockKind, text: Text<'_>) -> Result<(), Error> {
        let slot = match self.lookup(key, Some(kind)) {
            Some(slot) => slot,
            None if kind != BlockKind::Tool
                && matches!(
                    self.config.source.protocol,
                    Protocol::OpenAiChat
                        | Protocol::OpenAiCompletions
                        | Protocol::GeminiGenerateContent
                ) =>
            {
                self.block(key, kind, None, None, None)?;
                self.state.item_count - 1
            }
            None => {
                return Err(Error::InvalidInput {
                    field: "delta index",
                    reason: "block has not started",
                });
            }
        };
        if self.items[slot].closed {
            return Err(Error::InvalidInput {
                field: "delta index",
                reason: "block already closed",
            });
        }
        if self.items[slot].provider_block == ProviderBlock::Opaque {
            return Err(Error::InvalidInput {
                field: "provider block delta",
                reason: "complete provider block has no deltas",
            });
        }
        self.items[slot].content_length = self.items[slot]
            .content_length
            .checked_add(text.len())
            .ok_or(Error::EventTooLarge { needed: usize::MAX })?;
        if self.config.target.protocol == Protocol::OpenAiResponses
            || (self.config.validate_snapshots
                && self.config.source.protocol == Protocol::OpenAiResponses)
            || kind == BlockKind::Tool
        {
            self.append(slot, text)?;
        }
        if kind.is_reasoning()
            && self.config.policy.drop_reasoning
            && !self.reasoning_supported(kind)
        {
            self.state.report.dropped_reasoning =
                self.state.report.dropped_reasoning.saturating_add(1);
            return Ok(());
        }
        if self.config.target.protocol == Protocol::GeminiGenerateContent && kind != BlockKind::Tool
        {
            let stored = if self.config.gemini_chat_history {
                // Role-only Chat chunks and trailing keepalive fragments do
                // not create a model part or take an existing part's signature.
                if text.len() == 0 {
                    return Ok(());
                }
                self.emit_gemini_chat(None)?;
                self.state.gemini_chat_part = Some(slot);
                self.store_gemini_chat_text(text)?
            } else {
                self.flush_gemini_text(slot)?;
                self.store(text)?
            };
            self.items[slot].pending_text = Some(stored);
            return Ok(());
        }
        let item = self.items[slot];
        match self.config.target.protocol {
            Protocol::OpenAiChat => {
                self.choice_header(key.choice, false);
                self.writer.raw(b",\"delta\":{");
                if kind == BlockKind::Tool {
                    self.writer.raw(b"\"tool_calls\":[{\"index\":");
                    let index = self.tool_index(item);
                    number(&mut self.writer, index);
                    self.writer.raw(b",\"function\":{\"arguments\":");
                    text.write(&mut self.writer);
                    self.writer.raw(b"}}]");
                } else {
                    self.writer.raw(match kind {
                        BlockKind::Refusal => b"\"refusal\":",
                        BlockKind::Reasoning => b"\"reasoning_content\":",
                        _ => b"\"content\":",
                    });
                    text.write(&mut self.writer);
                }
                self.writer.raw(b"},\"finish_reason\":null}]");
                self.end_chat_event()?;
            }
            Protocol::OpenAiCompletions => {
                self.choice_header(key.choice, true);
                self.writer.raw(b",\"text\":");
                text.write(&mut self.writer);
                self.writer
                    .raw(b",\"logprobs\":null,\"finish_reason\":null}]");
                self.end_chat_event()?;
            }
            Protocol::AnthropicMessages => {
                self.named(b"content_block_delta", b"type");
                self.writer.raw(b",\"index\":");
                number(&mut self.writer, u64::from(item.wire_index));
                self.writer.raw(b",\"delta\":");
                self.writer.raw(match kind {
                    BlockKind::Tool => b"{\"type\":\"input_json_delta\",\"partial_json\":",
                    BlockKind::Reasoning => b"{\"type\":\"thinking_delta\",\"thinking\":",
                    _ => b"{\"type\":\"text_delta\",\"text\":",
                });
                text.write(&mut self.writer);
                self.writer.byte(b'}');
                self.end_event();
            }
            Protocol::OpenAiResponses => {
                self.response_event(match kind {
                    BlockKind::Tool => b"response.function_call_arguments.delta",
                    BlockKind::Refusal => b"response.refusal.delta",
                    BlockKind::ReasoningSummary => b"response.reasoning_summary_text.delta",
                    BlockKind::Reasoning => b"response.reasoning_text.delta",
                    _ => b"response.output_text.delta",
                })?;
                self.writer.raw(b",\"item_id\":");
                self.item_id(item);
                self.writer.raw(b",\"output_index\":");
                number(&mut self.writer, u64::from(item.wire_index));
                if kind != BlockKind::Tool {
                    self.response_part_index(item);
                }
                self.writer.raw(b",\"delta\":");
                text.write(&mut self.writer);
                self.end_event();
            }
            Protocol::GeminiGenerateContent if kind != BlockKind::Tool => {
                self.writer.raw(b"data: {\"candidates\":[{\"index\":");
                number(&mut self.writer, u64::from(key.choice));
                self.writer
                    .raw(b",\"content\":{\"role\":\"model\",\"parts\":[{\"text\":");
                text.write(&mut self.writer);
                if kind == BlockKind::ReasoningSummary {
                    self.writer.raw(b",\"thought\":true");
                }
                self.writer.raw(b"}]}}]}\n\n");
            }
            Protocol::GeminiGenerateContent => {}
            Protocol::GeminiInteractions => {
                self.named(b"step.delta", b"event_type");
                self.writer.raw(b",\"index\":");
                number(&mut self.writer, u64::from(item.wire_index));
                self.writer.raw(b",\"delta\":");
                self.writer.raw(match kind {
                    BlockKind::Tool => b"{\"type\":\"arguments_delta\",\"arguments\":",
                    BlockKind::ReasoningSummary => {
                        b"{\"type\":\"thought_summary\",\"content\":{\"type\":\"text\",\"text\":"
                    }
                    _ => b"{\"type\":\"text\",\"text\":",
                });
                text.write(&mut self.writer);
                if kind == BlockKind::ReasoningSummary {
                    self.writer.byte(b'}');
                }
                self.writer.byte(b'}');
                self.end_event();
            }
        }
        Ok(())
    }
    fn signature(
        &mut self,
        key: Key,
        text: Text<'_>,
        provider: Option<Format>,
    ) -> Result<(), Error> {
        let slot = self.lookup(key, None).ok_or(Error::InvalidInput {
            field: "signature",
            reason: "block missing",
        })?;
        if self.items[slot].closed {
            return Err(Error::InvalidInput {
                field: "signature",
                reason: "block closed",
            });
        }
        let provider = provider.unwrap_or(self.config.source);
        let target = self.config.target;
        let openai = matches!(
            target.protocol,
            Protocol::OpenAiChat | Protocol::OpenAiResponses
        ) && matches!(target.profile, Profile::OpenAiCompatible(_));
        if !openai && provider.protocol != target.protocol {
            return Err(Error::Unsupported {
                feature: "provider-bound thinking signature",
            });
        }
        self.store_signature(slot, text, provider)?;
        let item = self.items[slot];
        match target.protocol {
            Protocol::OpenAiChat if openai => {
                self.choice_header(key.choice, false);
                self.writer.raw(b",\"delta\":{");
                if item.kind == BlockKind::Tool {
                    self.writer.raw(b"\"tool_calls\":[{\"index\":");
                    let index = self.tool_index(item);
                    number(&mut self.writer, index);
                    self.provider_metadata(item)?;
                    self.writer.raw(b"}]");
                } else {
                    self.writer.raw(if item.kind.is_reasoning() {
                        b"\"reasoning_details\":[{\"type\":\"signature_delta\",\"index\":"
                    } else {
                        b"\"content_details\":[{\"type\":\"signature_delta\",\"index\":"
                    });
                    number(&mut self.writer, u64::from(item.wire_index));
                    self.writer.raw(b",\"protocol\":");
                    self.writer.string(Self::provider_name(provider));
                    self.writer.raw(b",\"signature\":");
                    text.write(&mut self.writer);
                    self.writer.raw(b"}]");
                }
                self.writer.raw(b"},\"finish_reason\":null}]");
                self.end_chat_event()?;
            }
            Protocol::OpenAiResponses if openai => {} // Retained in completed item and terminal snapshot.
            Protocol::AnthropicMessages if item.kind == BlockKind::Reasoning => {
                self.named(b"content_block_delta", b"type");
                self.writer.raw(b",\"index\":");
                number(&mut self.writer, u64::from(item.wire_index));
                self.writer
                    .raw(b",\"delta\":{\"type\":\"signature_delta\",\"signature\":");
                text.write(&mut self.writer);
                self.writer.byte(b'}');
                self.end_event();
            }
            Protocol::GeminiGenerateContent => {
                if self.config.gemini_chat_history
                    && item.kind != BlockKind::Tool
                    && self.state.gemini_chat_part != Some(slot)
                {
                    return Err(Error::InvalidInput {
                        field: "thoughtSignature",
                        reason: "no pending owning part",
                    });
                }
                if !self.config.gemini_chat_history && item.kind != BlockKind::Tool {
                    self.flush_gemini_text(slot)?;
                }
            }
            Protocol::GeminiInteractions => {
                self.named(b"step.delta", b"event_type");
                self.writer.raw(b",\"index\":");
                number(&mut self.writer, u64::from(item.wire_index));
                self.writer
                    .raw(b",\"delta\":{\"type\":\"thought_signature\",\"signature\":");
                text.write(&mut self.writer);
                self.writer.byte(b'}');
                self.end_event();
            }
            _ => {
                return Err(Error::Unsupported {
                    feature: "thinking signature target",
                });
            }
        }
        Ok(())
    }
    fn close(&mut self, key: Key) -> Result<(), Error> {
        let mut found = false;
        let mut open = false;
        for slot in 0..self.state.item_count {
            if self.items[slot].key.choice == key.choice
                && self.items[slot].key.index == key.index
                && (key.part == 0 || self.items[slot].key.part == key.part)
            {
                found = true;
                open |= !self.items[slot].closed;
                self.close_slot(slot)?;
            }
        }
        if !found {
            return Err(Error::InvalidInput {
                field: "block stop",
                reason: "block missing",
            });
        }
        if !open {
            return Err(Error::InvalidInput {
                field: "block stop",
                reason: "duplicate block stop",
            });
        }
        Ok(())
    }
    fn images(&mut self, choice: u32, value: json::RawValue<'_>) -> Result<(), Error> {
        if !self.state.begun || self.state.seen_finish {
            return Err(Error::InvalidInput {
                field: "images",
                reason: "invalid lifecycle",
            });
        }
        if self.config.target.protocol != Protocol::OpenAiChat {
            return Err(Error::Unsupported {
                feature: "Chat image output target",
            });
        }
        self.choice_header(choice, false);
        self.writer.raw(b",\"delta\":{\"images\":");
        self.writer.raw(value.raw());
        self.writer.raw(b"},\"finish_reason\":null}]");
        self.end_chat_event()?;
        Ok(())
    }
    fn grounding(&mut self, choice: u32, value: json::RawValue<'_>) -> Result<(), Error> {
        if !self.state.begun || self.state.seen_finish {
            return Err(Error::InvalidInput {
                field: "groundingMetadata",
                reason: "invalid lifecycle",
            });
        }
        if self.config.target.protocol != Protocol::GeminiGenerateContent {
            return Err(Error::Unsupported {
                feature: "Gemini grounding target",
            });
        }
        if self.config.gemini_chat_history {
            if choice != 0 {
                return Err(Error::Unsupported {
                    feature: "multiple Gemini chat candidates",
                });
            }
            if self.frame_grounding.is_some() {
                return Err(Error::Unsupported {
                    feature: "multiple grounding updates in one candidate frame",
                });
            }
            // The native decoder reports candidate-level metadata before that
            // candidate's parts. Binding now would decorate the previous frame
            // when the first new delta releases its pending text.
            self.frame_grounding = Some(self.store(Text::Utf8(value.raw()))?);
            return Ok(());
        }
        self.writer.raw(b"data: {\"candidates\":[{\"index\":");
        number(&mut self.writer, u64::from(choice));
        self.writer.raw(b",\"groundingMetadata\":");
        self.writer.raw(value.raw());
        self.writer.raw(b"}]}\n\n");
        Ok(())
    }
    fn continuation(&mut self, choice: u32, token: Text<'_>) -> Result<(), Error> {
        if !self.state.begun || self.state.seen_finish || token.len() == 0 {
            return Err(Error::InvalidInput {
                field: "continuationToken",
                reason: "invalid lifecycle or empty token",
            });
        }
        if self.config.target.protocol != Protocol::GeminiGenerateContent {
            return Err(Error::Unsupported {
                feature: "Gemini continuation target",
            });
        }
        if self.state.continuation.is_some() {
            return Err(Error::InvalidInput {
                field: "continuationToken",
                reason: "duplicate token",
            });
        }
        self.state.continuation = Some((choice, self.store(token)?));
        Ok(())
    }
    fn stop_sequence(&mut self, sequence: Text<'_>) -> Result<(), Error> {
        if !self.state.begun
            || self.state.seen_finish
            || sequence.len() == 0
            || sequence.len() > 1024
        {
            return Err(Error::InvalidInput {
                field: "stop_sequence",
                reason: "invalid lifecycle or length",
            });
        }
        if self.config.target.protocol != Protocol::AnthropicMessages {
            return Err(Error::Unsupported {
                feature: "Anthropic stop sequence target",
            });
        }
        if self.state.stop_sequence.is_some() {
            return Err(Error::InvalidInput {
                field: "stop_sequence",
                reason: "duplicate sequence",
            });
        }
        self.state.stop_sequence = Some(self.store(sequence)?);
        Ok(())
    }
    fn usage(&mut self, usage: Usage) -> Result<(), Error> {
        self.flush_usage()?;
        self.state.usage_seen = true;
        self.state.usage.update(usage)?;
        self.state.usage_pending = true;
        Ok(())
    }
    fn choice(&mut self, choice: u32, reason: FinishReason) -> Result<(), Error> {
        if !self.state.begun {
            return Err(Error::InvalidInput {
                field: "finish",
                reason: "stream not started",
            });
        }
        if self.items[..self.state.item_count]
            .iter()
            .any(|item| item.key.choice == choice && item.finished)
        {
            return Err(Error::InvalidInput {
                field: "finish",
                reason: "choice already finished",
            });
        }
        let mut found = false;
        for slot in 0..self.state.item_count {
            if self.items[slot].key.choice == choice {
                found = true;
                self.items[slot].reason = reason;
                if !self.items[slot].closed
                    && matches!(
                        reason,
                        FinishReason::Length | FinishReason::Safety | FinishReason::Continuation
                    )
                {
                    self.items[slot].incomplete = true;
                }
                if self.items[slot].kind == BlockKind::Tool
                    && !matches!(
                        self.config.target.protocol,
                        Protocol::OpenAiChat
                            | Protocol::OpenAiResponses
                            | Protocol::OpenAiCompletions
                    )
                {
                    self.items[slot].incomplete = false;
                }
                self.close_slot(slot)?;
                self.items[slot].finished = true;
                self.items[slot].reason = reason;
            }
        }
        if !found {
            self.block(
                Key {
                    choice,
                    index: u32::MAX,
                    part: 0,
                },
                BlockKind::Text,
                None,
                None,
                None,
            )?;
            let slot = self.state.item_count - 1;
            self.items[slot].reason = reason;
            self.items[slot].incomplete = matches!(
                reason,
                FinishReason::Length | FinishReason::Safety | FinishReason::Continuation
            );
            self.close_slot(slot)?;
            self.items[slot].finished = true;
            self.items[slot].reason = reason;
        }
        self.state.seen_finish = true;
        self.emit_choice(choice, reason)
    }
    fn finish(&mut self, mut completion: Completion) -> Result<(), Error> {
        if !self.state.begun || self.state.ended {
            return Err(Error::InvalidInput {
                field: "stream end",
                reason: "invalid lifecycle",
            });
        }
        if !self.state.seen_finish {
            return Err(Error::TruncatedStream);
        }
        for slot in 0..self.state.item_count {
            if !self.items[slot].finished {
                return Err(Error::TruncatedStream);
            }
            self.close_slot(slot)?;
        }
        let reason = self
            .items
            .first()
            .filter(|item| item.used)
            .map_or(FinishReason::Stop, |item| item.reason);
        if completion == Completion::Completed
            && matches!(
                reason,
                FinishReason::Length | FinishReason::Safety | FinishReason::Continuation
            )
        {
            completion = Completion::Incomplete;
        }
        match self.config.target.protocol {
            Protocol::OpenAiChat | Protocol::OpenAiCompletions => {
                if completion == Completion::Cancelled || completion == Completion::Failed {
                    return Err(Error::Unsupported {
                        feature: "cancelled/failed terminal status without error payload",
                    });
                }
                if self.state.usage_seen
                    && (self.config.usage_emission == UsageEmission::Final
                        || self.state.usage_pending)
                {
                    self.chat_header(self.config.target.protocol == Protocol::OpenAiCompletions);
                    self.writer.raw(b",\"choices\":[],\"usage\":");
                    self.write_usage(self.config.target.protocol)?;
                    self.end_event();
                }
                self.writer.raw(b"data: [DONE]\n\n");
            }
            Protocol::AnthropicMessages => {
                if completion == Completion::Cancelled || completion == Completion::Failed {
                    return Err(Error::Unsupported {
                        feature: "Anthropic cancelled/failed status without error payload",
                    });
                }
                self.named(b"message_delta", b"type");
                self.writer.raw(b",\"delta\":{\"stop_reason\":");
                self.writer.string(match reason {
                    FinishReason::Stop if self.state.stop_sequence.is_some() => b"stop_sequence",
                    FinishReason::Stop => b"end_turn",
                    FinishReason::Length => b"max_tokens",
                    FinishReason::ToolCalls => b"tool_use",
                    FinishReason::Safety => b"refusal",
                    FinishReason::Continuation | FinishReason::Other => {
                        return Err(Error::Unsupported {
                            feature: "stop reason",
                        });
                    }
                });
                self.writer.raw(b",\"stop_sequence\":");
                if let Some(sequence) = self.state.stop_sequence {
                    quoted_span(&mut self.writer, self.aggregate, sequence);
                } else {
                    self.writer.raw(b"null");
                }
                self.writer.raw(b"},\"usage\":");
                self.write_usage(Protocol::AnthropicMessages)?;
                self.end_event();
                self.named(b"message_stop", b"type");
                self.end_event();
            }
            Protocol::OpenAiResponses => {
                if completion == Completion::Cancelled {
                    return Err(Error::Unsupported {
                        feature: "Responses SSE cancellation terminal event",
                    });
                }
                let status = match completion {
                    Completion::Incomplete => b"incomplete".as_slice(),
                    Completion::Failed => b"failed",
                    Completion::Cancelled => b"cancelled",
                    _ => b"completed",
                };
                self.response_event(match completion {
                    Completion::Incomplete => b"response.incomplete",
                    Completion::Failed => b"response.failed",
                    Completion::Cancelled => {
                        return Err(Error::Unsupported {
                            feature: "Responses SSE cancellation terminal event",
                        });
                    }
                    _ => b"response.completed",
                })?;
                self.writer.raw(b",\"response\":");
                self.response_object_header(status);
                self.writer.raw(b",\"output\":[");
                let mut first = true;
                for slot in 0..self.state.item_count {
                    if self.items[slot].group_head != slot {
                        continue;
                    }
                    if !first {
                        self.writer.byte(b',');
                    }
                    first = false;
                    self.response_item(self.items[slot], true)?;
                }
                self.writer.raw(b"],\"usage\":");
                self.write_usage(Protocol::OpenAiResponses)?;
                self.writer.byte(b'}');
                self.end_event();
            }
            Protocol::GeminiGenerateContent => {
                if completion == Completion::Cancelled || completion == Completion::Failed {
                    return Err(Error::Unsupported {
                        feature: "Gemini cancelled/failed status without error payload",
                    });
                }
                if self.config.gemini_chat_history {
                    self.emit_gemini_chat(Some(reason))?;
                } else if self.state.usage_seen {
                    self.writer
                        .raw(b"data: {\"candidates\":[],\"usageMetadata\":");
                    self.write_usage(Protocol::GeminiGenerateContent)?;
                    self.end_event();
                }
            }
            Protocol::GeminiInteractions => {
                self.named(b"interaction.completed", b"event_type");
                self.writer.raw(b",\"interaction\":{\"id\":");
                quoted_span(&mut self.writer, self.aggregate, self.state.id);
                self.writer.raw(b",\"status\":");
                self.writer.string(match completion {
                    Completion::Incomplete => b"incomplete",
                    Completion::RequiresAction => b"requires_action",
                    Completion::Failed => b"failed",
                    Completion::Cancelled => b"cancelled",
                    Completion::Completed if reason == FinishReason::ToolCalls => {
                        b"requires_action"
                    }
                    _ => b"completed",
                });
                self.writer.raw(b",\"usage\":");
                self.write_usage(Protocol::GeminiInteractions)?;
                self.writer.byte(b'}');
                self.end_event();
            }
        }
        self.state.ended = true;
        Ok(())
    }
    fn failure(&mut self, code: Option<Text<'_>>, message: Text<'_>) -> Result<(), Error> {
        if self.config.gemini_chat_history {
            self.bind_frame_grounding()?;
            self.emit_gemini_chat(None)?;
        }
        if self.config.target.protocol == Protocol::OpenAiResponses {
            self.response_event(b"error")?;
            if let Some(code) = code {
                self.writer.raw(b",\"code\":");
                code.write(&mut self.writer);
            }
            self.writer.raw(b",\"message\":");
            message.write(&mut self.writer);
            self.writer.raw(b",\"param\":null");
            self.end_event();
            self.state.ended = true;
            self.state.failure = true;
            return Ok(());
        }
        match self.config.target.protocol {
            Protocol::AnthropicMessages => self.named(b"error", b"type"),
            Protocol::GeminiInteractions => self.named(b"error", b"event_type"),
            Protocol::OpenAiResponses => self.response_event(b"error")?,
            _ => self.writer.raw(b"data: {"),
        }
        if matches!(
            self.config.target.protocol,
            Protocol::AnthropicMessages | Protocol::GeminiInteractions | Protocol::OpenAiResponses
        ) {
            self.writer.byte(b',');
        }
        self.writer.raw(b"\"error\":{");
        if let Some(code) = code {
            self.writer.raw(
                if self.config.target.protocol == Protocol::AnthropicMessages {
                    b"\"type\":"
                } else {
                    b"\"code\":"
                },
            );
            code.write(&mut self.writer);
            self.writer.byte(b',');
        }
        self.writer.raw(b"\"message\":");
        message.write(&mut self.writer);
        self.writer.byte(b'}');
        self.end_event();
        self.state.ended = true;
        self.state.failure = true;
        Ok(())
    }
    fn extension(&mut self, field: DroppedField) -> Result<(), Error> {
        if !self.config.policy.drop_extensions {
            return Err(Error::PolicyViolation {
                feature: match field {
                    DroppedField::SystemFingerprint => "system_fingerprint",
                    DroppedField::ServiceTier => "service_tier",
                    DroppedField::SafetyRatings => "safetyRatings",
                    DroppedField::FinishMessage => "finishMessage",
                    DroppedField::AverageLogprobs => "avgLogprobs",
                    DroppedField::CacheCreationTokens => "cache_creation_input_tokens",
                },
            });
        }
        self.state.report.dropped_fields |= field as u32;
        self.state.report.dropped_extensions =
            self.state.report.dropped_extensions.saturating_add(1);
        Ok(())
    }
    fn complete_tool(
        &mut self,
        key: Key,
        id: Option<Text<'_>>,
        name: Text<'_>,
        arguments: Text<'_>,
        signature: Option<Text<'_>>,
    ) -> Result<(), Error> {
        let id = id.or_else(|| {
            self.config
                .context
                .tool_ids
                .iter()
                .find(|entry| entry.key == key)
                .map(|entry| Text::Utf8(entry.id.as_bytes()))
        });
        let key = Key {
            choice: key.choice,
            index: u32::try_from(self.state.item_count).map_err(|_| Error::TooManyActiveItems {
                needed: self.state.item_count,
            })?,
            part: 0,
        };
        self.block(key, BlockKind::Tool, id, Some(name), None)?;
        self.delta(key, BlockKind::Tool, arguments)?;
        if let Some(signature) = signature {
            if self.config.source != self.config.target
                || self.config.target.protocol != Protocol::GeminiGenerateContent
            {
                return Err(Error::Unsupported {
                    feature: "provider-bound tool signature target",
                });
            }
            let slot = self.state.item_count - 1;
            if self.config.gemini_chat_history {
                self.store_signature(slot, signature, self.config.source)?;
                return self.close(key);
            }
            let item = self.items[slot];
            self.writer.raw(b"data: {\"candidates\":[{\"index\":");
            number(&mut self.writer, u64::from(key.choice));
            self.writer
                .raw(b",\"content\":{\"role\":\"model\",\"parts\":[{\"functionCall\":{\"id\":");
            quoted_span(&mut self.writer, self.aggregate, item.id);
            self.writer.raw(b",\"name\":");
            quoted_span(&mut self.writer, self.aggregate, item.name);
            self.writer.raw(b",\"args\":");
            self.write_chain(item, false)?;
            self.writer.raw(b"},\"thoughtSignature\":");
            signature.write(&mut self.writer);
            self.writer.raw(b"}]}}]}\n\n");
            self.items[slot].closed = true;
            Ok(())
        } else {
            self.close(key)
        }
    }
    fn snapshot(&mut self, key: Key, kind: BlockKind, text: Text<'_>) -> Result<(), Error> {
        let slot = self.lookup(key, Some(kind)).ok_or(Error::InvalidInput {
            field: "final snapshot",
            reason: "block was never started",
        })?;
        let item = self.items[slot];
        if item.content_length != text.len() {
            return Err(Error::InvalidInput {
                field: "final snapshot",
                reason: "differs from accumulated deltas",
            });
        }
        if !self.config.validate_snapshots
            && self.config.target.protocol != Protocol::OpenAiResponses
            && kind != BlockKind::Tool
        {
            return Ok(());
        }
        let equal = match text {
            Text::Utf8(bytes) => chain_equals(self.aggregate, item, bytes.iter().copied())?,
            Text::Json(value) => chain_equals(self.aggregate, item, value.bytes())?,
        };
        if !equal {
            return Err(Error::InvalidInput {
                field: "final snapshot",
                reason: "differs from accumulated deltas",
            });
        }
        Ok(())
    }
    fn incomplete(&mut self, key: Key) -> Result<(), Error> {
        let mut found = false;
        for item in &mut self.items[..self.state.item_count] {
            if item.key.choice == key.choice && item.key.index == key.index {
                item.incomplete = true;
                found = true;
            }
        }
        if found {
            Ok(())
        } else {
            Err(Error::InvalidInput {
                field: "incomplete block",
                reason: "block missing",
            })
        }
    }
}

fn chain_equals(
    aggregate: &[u8],
    item: ActiveItem,
    mut expected: impl Iterator<Item = u8>,
) -> Result<bool, Error> {
    let mut next = item.head;
    let mut count = 0usize;
    while next != NONE {
        count += 1;
        if count > aggregate.len() / HEADER + 1 {
            return Err(Error::InvalidInput {
                field: "aggregate",
                reason: "cycle",
            });
        }
        let length = read_usize(aggregate, next + core::mem::size_of::<usize>())?;
        let start = next
            .checked_add(HEADER)
            .ok_or(Error::EventTooLarge { needed: usize::MAX })?;
        let end = start
            .checked_add(length)
            .ok_or(Error::EventTooLarge { needed: usize::MAX })?;
        let bytes = aggregate.get(start..end).ok_or(Error::InvalidInput {
            field: "aggregate",
            reason: "invalid segment",
        })?;
        for &byte in bytes {
            if expected.next() != Some(byte) {
                return Ok(false);
            }
        }
        next = read_usize(aggregate, next)?;
    }
    Ok(expected.next().is_none())
}
