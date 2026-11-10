use super::{invalid, validate_format};
use crate::{
    ir::*,
    json::{self, Kind, RawValue, Token, Writer},
    Error, Format, Profile, Protocol,
};

fn write_text(w: &mut Writer<'_>, text: Text<'_>) {
    match text {
        Text::Json(s) => w.json_string(s),
        Text::Utf8(s) => w.string(s.as_bytes()),
    }
}

fn number(w: &mut Writer<'_>, number: Number<'_>) -> Result<(), Error> {
    match number {
        Number::Json(v) => {
            if v.kind() != Kind::Number {
                return Err(invalid("number", "edited number is not numeric"));
            }
            w.raw(v.raw());
        }
        Number::Unsigned(n) => uint(w, n),
    }
    Ok(())
}

fn uint(w: &mut Writer<'_>, mut n: u64) {
    let mut buf = [0u8; 20];
    let mut at = buf.len();
    loop {
        at -= 1;
        buf[at] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    w.raw(&buf[at..]);
}

fn key(w: &mut Writer<'_>, comma: &mut bool, name: &str) {
    if *comma {
        w.byte(b',');
    }
    *comma = true;
    w.string(name.as_bytes());
    w.byte(b':');
}

fn ptext(w: &mut Writer<'_>, comma: &mut bool, name: &str, p: Presence<Text<'_>>) {
    match p {
        Presence::Missing => {}
        Presence::Null => {
            key(w, comma, name);
            w.raw(b"null");
        }
        Presence::Value(t) => {
            key(w, comma, name);
            write_text(w, t);
        }
    }
}

fn praw(w: &mut Writer<'_>, comma: &mut bool, name: &str, p: Presence<RawValue<'_>>) {
    match p {
        Presence::Missing => {}
        Presence::Null => {
            key(w, comma, name);
            w.raw(b"null");
        }
        Presence::Value(v) => {
            key(w, comma, name);
            w.raw(v.raw());
        }
    }
}

fn pbool(w: &mut Writer<'_>, comma: &mut bool, name: &str, p: Presence<bool>) {
    match p {
        Presence::Missing => {}
        Presence::Null => {
            key(w, comma, name);
            w.raw(b"null");
        }
        Presence::Value(v) => {
            key(w, comma, name);
            w.raw(if v { b"true" } else { b"false" });
        }
    }
}

fn pnumber(
    w: &mut Writer<'_>,
    comma: &mut bool,
    name: &str,
    p: Presence<Number<'_>>,
) -> Result<(), Error> {
    match p {
        Presence::Missing => {}
        Presence::Null => {
            key(w, comma, name);
            w.raw(b"null");
        }
        Presence::Value(v) => {
            key(w, comma, name);
            number(w, v)?;
        }
    }
    Ok(())
}

fn equivalent_text(a: Text<'_>, b: &str) -> bool {
    a.eq_str(b)
}

struct Encoder<'a, 'r, 'o, 's, 't, 'c> {
    source: Format,
    target: Format,
    records: &'r [Record<'a>],
    context: &'c Context<'a>,
    policy: Policy,
    w: Writer<'o>,
    scratch: &'s mut [u8],
    tokens: &'t mut [Token],
    report: EncodeReport<'a>,
    next_item: usize,
}

impl<'a> Encoder<'a, '_, '_, '_, '_, '_> {
    fn output_id(
        &mut self,
        original: Presence<Text<'a>>,
        response: bool,
    ) -> Result<Presence<Text<'a>>, Error> {
        if !response || self.target.protocol != Protocol::OpenAiResponses {
            return Ok(original);
        }
        let ordinal = self.next_item;
        self.next_item = self.next_item.checked_add(1).ok_or(Error::LimitExceeded {
            kind: "output items",
            limit: usize::MAX,
        })?;
        match original {
            Presence::Value(v) => Ok(Presence::Value(v)),
            Presence::Missing => self
                .context
                .item_ids
                .get(ordinal)
                .map(|s| Presence::Value(Text::Utf8(s)))
                .ok_or(Error::ContextRequired {
                    field: "output item id",
                }),
            Presence::Null => Err(invalid("output item id", "required string cannot be null")),
        }
    }
    fn metadata_loss(&mut self, present: bool, feature: &'static str) -> Result<(), Error> {
        if present {
            if !self.policy.drop_metadata {
                return Err(Error::PolicyViolation { feature });
            }
            self.report.dropped_metadata += 1;
            self.report.metadata_loss_fields |= match feature {
                "creation timestamp" | "Unix creation timestamp" => METADATA_CREATED,
                "ISO creation timestamp" => METADATA_ISO_CREATED,
                "ISO update timestamp" => METADATA_ISO_UPDATED,
                "response item status" => METADATA_ITEM_STATUS,
                _ => METADATA_ITEM_ID,
            };
        }
        Ok(())
    }
    fn preflight(&mut self) -> Result<(), Error> {
        validate_format(self.target)?;
        if self.context.tool_calls.len() > 64 {
            return Err(Error::LimitExceeded {
                kind: "external tool bindings",
                limit: 64,
            });
        }
        let mut choice_end = self.records.len();
        let mut message_end = self.records.len();
        for (index, record) in self.records.iter().copied().enumerate() {
            if index >= choice_end {
                choice_end = self.records.len();
            }
            if index >= message_end {
                message_end = choice_end;
            }
            match record {
                Record::Extension(e) if extension_is_reserved(e) => {
                    return Err(invalid(
                        "IR extension",
                        "extension collides with normalized field",
                    ));
                }
                Record::Extension(e) if e.source != self.target && !e.is_empty_annotation() => {
                    if !self.policy.drop_extensions {
                        return Err(Error::PolicyViolation {
                            feature: "source-owned extension",
                        });
                    }
                    self.report.dropped_extensions += 1;
                }
                Record::Block(b) => {
                    if b.end <= index || b.end > message_end {
                        return Err(invalid("IR block end", "invalid arena range"));
                    }
                    self.check_span(b.extensions, index, b.end)?;
                    if !b.item_id.is_missing() && self.target.protocol != Protocol::OpenAiResponses
                    {
                        self.metadata_loss(true, "response item ID")?;
                    }
                    if !b.item_status.is_missing()
                        && self.target.protocol != Protocol::OpenAiResponses
                    {
                        self.metadata_loss(true, "response item status")?;
                    }
                    match b.content {
                        Content::Reasoning {
                            signature, kind, ..
                        } => {
                            if self.policy.drop_reasoning {
                                self.report.dropped_reasoning += 1;
                            } else if self.source != self.target
                                && ((kind == ReasoningKind::Thought
                                    && matches!(
                                        self.target.protocol,
                                        Protocol::OpenAiResponses
                                            | Protocol::GeminiInteractions
                                            | Protocol::GeminiGenerateContent
                                    ))
                                    || (kind == ReasoningKind::Summary
                                        && matches!(
                                            self.target.protocol,
                                            Protocol::AnthropicMessages | Protocol::OpenAiChat
                                        )))
                            {
                                return Err(Error::Unsupported {
                                    feature: "reasoning summary and provider thought differ",
                                });
                            } else if self.source != self.target && !signature.is_missing() {
                                return Err(Error::PolicyViolation {
                                    feature: "reasoning signature belongs to source",
                                });
                            }
                        }
                        Content::Opaque { source, .. } if source != self.target => {
                            return Err(Error::Unsupported {
                                feature: "opaque content block",
                            })
                        }
                        Content::Media(Media {
                            source: MediaSource::FileId { namespace, .. },
                            ..
                        }) if namespace != self.target => {
                            return Err(Error::ContextRequired {
                                field: "mapped provider file reference",
                            })
                        }
                        _ => {}
                    }
                }
                Record::Message(m) => {
                    if m.end <= index || m.end > choice_end {
                        return Err(invalid("IR message end", "invalid arena range"));
                    }
                    self.check_span(m.extensions, index, m.end)?;
                    message_end = m.end;
                    if !m.item_id.is_missing() && self.target.protocol != Protocol::OpenAiResponses
                    {
                        self.metadata_loss(true, "response message ID")?;
                    }
                    if !m.item_status.is_missing()
                        && self.target.protocol != Protocol::OpenAiResponses
                    {
                        self.metadata_loss(true, "response item status")?;
                    }
                }
                Record::Choice(c) => {
                    if c.end <= index || c.end > self.records.len() {
                        return Err(invalid("IR choice end", "invalid arena range"));
                    }
                    self.check_span(c.extensions, index, c.end)?;
                    choice_end = c.end;
                    message_end = c.end;
                }
                Record::Tool(t) => {
                    self.check_span(t.extensions, index, self.records.len())?;
                }
                _ => {}
            }
        }
        Ok(())
    }
    fn check_span(&self, span: Span, owner: usize, parent_end: usize) -> Result<(), Error> {
        if span.start == 0 && span.end == 0 {
            return Ok(());
        }
        if span.start <= owner || span.end > parent_end {
            return Err(invalid("IR extensions", "extension range outside owner"));
        }
        let entries = self
            .records
            .get(span.start..span.end)
            .ok_or_else(|| invalid("IR extensions", "invalid arena range"))?;
        for entry in entries {
            if !matches!(entry,Record::Extension(e)if e.owner==owner) {
                return Err(invalid("IR extensions", "extension owner mismatch"));
            }
        }
        Ok(())
    }
    fn extras(&mut self, span: Span, comma: &mut bool) -> Result<(), Error> {
        let range = self
            .records
            .get(span.start..span.end)
            .ok_or_else(|| invalid("IR extensions", "invalid arena range"))?;
        for record in range {
            if let Record::Extension(e) = record {
                if e.source == self.target {
                    if *comma {
                        self.w.byte(b',');
                    }
                    *comma = true;
                    self.w.json_string(e.key);
                    self.w.byte(b':');
                    self.w.raw(e.value.raw());
                }
            } else {
                return Err(invalid(
                    "IR extensions",
                    "range contains a non-extension record",
                ));
            }
        }
        Ok(())
    }
    fn bind_id(&self, id: Presence<Text<'a>>, name: Text<'a>) -> Result<Text<'a>, Error> {
        if let Presence::Value(id) = id {
            return Ok(id);
        }
        let mut found = None;
        for binding in self.context.tool_calls.iter().take(64) {
            if equivalent_text(name, binding.name) {
                if found.is_some() {
                    return Err(Error::ContextRequired {
                        field: "unambiguous tool call ID",
                    });
                }
                found = Some(Text::Utf8(binding.id));
            }
        }
        found.ok_or(Error::ContextRequired {
            field: "tool call ID",
        })
    }
    fn bind_name(
        &self,
        name: Presence<Text<'a>>,
        id: Presence<Text<'a>>,
    ) -> Result<Text<'a>, Error> {
        if let Presence::Value(name) = name {
            return Ok(name);
        }
        if let Presence::Value(id) = id {
            for binding in self.context.tool_calls.iter().take(64) {
                if equivalent_text(id, binding.id) {
                    return Ok(Text::Utf8(binding.name));
                }
            }
            for record in self.records.iter().take(256) {
                if let Record::Block(Block {
                    content:
                        Content::ToolCall {
                            id: Presence::Value(call_id),
                            name,
                            ..
                        },
                    ..
                }) = record
                {
                    let same = match (id, *call_id) {
                        (Text::Json(a), Text::Json(b)) => a.bytes().eq(b.bytes()),
                        _ => id
                            .as_str()
                            .zip(call_id.as_str())
                            .map(|(a, b)| a == b)
                            .unwrap_or(false),
                    };
                    if same {
                        return Ok(*name);
                    }
                }
            }
        }
        Err(Error::ContextRequired {
            field: "tool function name",
        })
    }
    fn data(
        &mut self,
        data: Data<'a>,
        stringify: bool,
        object_required: bool,
    ) -> Result<(), Error> {
        match data {
            Data::EmptyObject => {
                if stringify {
                    self.w.string(b"{}");
                } else {
                    self.w.raw(b"{}");
                }
            }
            Data::Json(v) => {
                if object_required && v.kind() != Kind::Object {
                    return Err(Error::Unsupported {
                        feature: "tool result or arguments must be object in target",
                    });
                }
                if !object_required && v.kind() == Kind::Array && self.source != self.target {
                    return Err(Error::Unsupported {
                        feature: "typed tool-result content array across protocols",
                    });
                }
                if stringify {
                    self.w.string(v.raw());
                } else {
                    self.w.raw(v.raw());
                }
            }
            Data::Text(t) => {
                if object_required {
                    return self.data(Data::JsonText(t), false, true);
                }
                write_text(&mut self.w, t);
            }
            Data::JsonText(t) => {
                if stringify {
                    write_text(&mut self.w, t);
                    return Ok(());
                }
                let len = match t {
                    Text::Json(s) => s.decode_into(self.scratch).map_err(|e| match e {
                        Error::OutputTooSmall { needed } => Error::ScratchTooSmall { needed },
                        e => e,
                    })?,
                    Text::Utf8(s) => {
                        if s.len() > self.scratch.len() {
                            return Err(Error::ScratchTooSmall { needed: s.len() });
                        }
                        self.scratch[..s.len()].copy_from_slice(s.as_bytes());
                        s.len()
                    }
                };
                let doc = json::parse(&self.scratch[..len], self.tokens, json::Limits::default())?;
                if object_required && doc.root().kind() != Kind::Object {
                    return Err(invalid("arguments", "target requires JSON object"));
                }
                if stringify {
                    write_text(&mut self.w, t);
                } else {
                    self.w.raw(&self.scratch[..len]);
                }
            }
        }
        Ok(())
    }
    fn media(&mut self, media: Media<'a>, response: bool) -> Result<(), Error> {
        let p = self.target.protocol;
        let mut comma = false;
        self.w.byte(b'{');
        match p {
            Protocol::OpenAiChat | Protocol::OpenAiResponses => {
                if media.kind == MediaKind::Image {
                    if p == Protocol::OpenAiChat {
                        key(&mut self.w, &mut comma, "type");
                        self.w.string(b"image_url");
                        key(&mut self.w, &mut comma, "image_url");
                        self.w.byte(b'{');
                        comma = false;
                        key(&mut self.w, &mut comma, "url");
                    } else {
                        key(&mut self.w, &mut comma, "type");
                        self.w.string(b"input_image");
                        key(
                            &mut self.w,
                            &mut comma,
                            if matches!(media.source, MediaSource::FileId { .. }) {
                                "file_id"
                            } else {
                                "image_url"
                            },
                        );
                    }
                    self.media_url(media.source)?;
                    ptext(&mut self.w, &mut comma, "detail", media.detail);
                    if p == Protocol::OpenAiChat {
                        self.w.byte(b'}');
                    }
                } else if media.kind == MediaKind::Audio && p == Protocol::OpenAiChat {
                    let (data, mime_type) = match media.source {
                        MediaSource::Base64 { data, mime_type } => (data, mime_type),
                        _ => {
                            return Err(Error::Unsupported {
                                feature: "Chat audio requires inline base64",
                            })
                        }
                    };
                    key(&mut self.w, &mut comma, "type");
                    self.w.string(b"input_audio");
                    key(&mut self.w, &mut comma, "input_audio");
                    self.w.byte(b'{');
                    let mut nested = false;
                    key(&mut self.w, &mut nested, "data");
                    write_text(&mut self.w, data);
                    key(&mut self.w, &mut nested, "format");
                    self.w.string(if mime_type.eq_str("audio/wav") {
                        b"wav"
                    } else if mime_type.eq_str("audio/mpeg") || mime_type.eq_str("audio/mp3") {
                        b"mp3"
                    } else {
                        return Err(Error::Unsupported {
                            feature: "audio MIME type",
                        });
                    });
                    self.w.byte(b'}');
                } else if media.kind == MediaKind::File {
                    key(&mut self.w, &mut comma, "type");
                    self.w.string(if p == Protocol::OpenAiChat {
                        b"file"
                    } else {
                        b"input_file"
                    });
                    if p == Protocol::OpenAiChat {
                        key(&mut self.w, &mut comma, "file");
                        self.w.byte(b'{');
                        comma = false;
                    }
                    match media.source {
                        MediaSource::FileId { id, .. } => {
                            key(&mut self.w, &mut comma, "file_id");
                            write_text(&mut self.w, id);
                        }
                        MediaSource::Url(url) if p == Protocol::OpenAiResponses => {
                            key(&mut self.w, &mut comma, "file_url");
                            write_text(&mut self.w, url);
                        }
                        MediaSource::Base64 { .. } => {
                            key(&mut self.w, &mut comma, "file_data");
                            self.media_url(media.source)?;
                        }
                        _ => {
                            return Err(Error::Unsupported {
                                feature: "file reference target representation",
                            })
                        }
                    }
                    ptext(&mut self.w, &mut comma, "filename", media.filename);
                    if p == Protocol::OpenAiChat {
                        self.w.byte(b'}');
                    }
                } else {
                    return Err(Error::Unsupported {
                        feature: "media kind in OpenAI target",
                    });
                }
                if response {
                    return Err(Error::Unsupported {
                        feature: "media output in OpenAI target",
                    });
                }
            }
            Protocol::AnthropicMessages => {
                if !matches!(media.kind, MediaKind::Image | MediaKind::File) {
                    return Err(Error::Unsupported {
                        feature: "Anthropic audio/video block",
                    });
                }
                key(&mut self.w, &mut comma, "type");
                self.w.string(if media.kind == MediaKind::Image {
                    b"image"
                } else {
                    b"document"
                });
                key(&mut self.w, &mut comma, "source");
                self.w.byte(b'{');
                let mut nested = false;
                match media.source {
                    MediaSource::Url(url) => {
                        key(&mut self.w, &mut nested, "type");
                        self.w.string(b"url");
                        key(&mut self.w, &mut nested, "url");
                        write_text(&mut self.w, url);
                    }
                    MediaSource::DataUrl(url) => {
                        let (mime_end, data_start) = data_url_header(url)?;
                        key(&mut self.w, &mut nested, "type");
                        self.w.string(b"base64");
                        key(&mut self.w, &mut nested, "media_type");
                        write_text_range(&mut self.w, url, 5, mime_end);
                        key(&mut self.w, &mut nested, "data");
                        write_text_range(&mut self.w, url, data_start, usize::MAX);
                    }
                    MediaSource::Base64 { data, mime_type } => {
                        key(&mut self.w, &mut nested, "type");
                        self.w.string(b"base64");
                        key(&mut self.w, &mut nested, "media_type");
                        write_text(&mut self.w, mime_type);
                        key(&mut self.w, &mut nested, "data");
                        write_text(&mut self.w, data);
                    }
                    MediaSource::FileId { id, .. } => {
                        key(&mut self.w, &mut nested, "type");
                        self.w.string(b"file");
                        key(&mut self.w, &mut nested, "file_id");
                        write_text(&mut self.w, id);
                    }
                }
                self.w.byte(b'}');
                if !media.detail.is_missing() || !media.filename.is_missing() {
                    return Err(Error::Unsupported {
                        feature: "Anthropic media detail or filename",
                    });
                }
            }
            Protocol::GeminiGenerateContent => match media.source {
                MediaSource::Base64 { data, mime_type } => {
                    key(&mut self.w, &mut comma, "inlineData");
                    self.w.byte(b'{');
                    let mut c = false;
                    key(&mut self.w, &mut c, "mimeType");
                    write_text(&mut self.w, mime_type);
                    key(&mut self.w, &mut c, "data");
                    write_text(&mut self.w, data);
                    self.w.byte(b'}');
                }
                MediaSource::DataUrl(url) => {
                    let (mime_end, data_start) = data_url_header(url)?;
                    key(&mut self.w, &mut comma, "inlineData");
                    self.w.byte(b'{');
                    let mut c = false;
                    key(&mut self.w, &mut c, "mimeType");
                    write_text_range(&mut self.w, url, 5, mime_end);
                    key(&mut self.w, &mut c, "data");
                    write_text_range(&mut self.w, url, data_start, usize::MAX);
                    self.w.byte(b'}');
                }
                MediaSource::Url(url) => {
                    let mime = media.mime_type.value().ok_or(Error::ContextRequired {
                        field: "media MIME type",
                    })?;
                    key(&mut self.w, &mut comma, "fileData");
                    self.w.byte(b'{');
                    let mut c = false;
                    key(&mut self.w, &mut c, "mimeType");
                    write_text(&mut self.w, mime);
                    key(&mut self.w, &mut c, "fileUri");
                    write_text(&mut self.w, url);
                    self.w.byte(b'}');
                }
                _ => {
                    return Err(Error::Unsupported {
                        feature: "provider file reference in Gemini",
                    })
                }
            },
            Protocol::GeminiInteractions => {
                key(&mut self.w, &mut comma, "type");
                self.w.string(match media.kind {
                    MediaKind::Image => b"image",
                    MediaKind::Audio => b"audio",
                    MediaKind::Video => b"video",
                    MediaKind::File => b"document",
                });
                match media.source {
                    MediaSource::Url(uri) => {
                        key(&mut self.w, &mut comma, "uri");
                        write_text(&mut self.w, uri);
                    }
                    MediaSource::DataUrl(url) => {
                        let (mime_end, data_start) = data_url_header(url)?;
                        key(&mut self.w, &mut comma, "mime_type");
                        write_text_range(&mut self.w, url, 5, mime_end);
                        key(&mut self.w, &mut comma, "data");
                        write_text_range(&mut self.w, url, data_start, usize::MAX);
                    }
                    MediaSource::Base64 { data, mime_type } => {
                        key(&mut self.w, &mut comma, "data");
                        write_text(&mut self.w, data);
                        key(&mut self.w, &mut comma, "mime_type");
                        write_text(&mut self.w, mime_type);
                    }
                    _ => {
                        return Err(Error::Unsupported {
                            feature: "provider file reference in Interactions",
                        })
                    }
                }
                ptext(&mut self.w, &mut comma, "resolution", media.detail);
            }
            _ => {
                return Err(Error::Unsupported {
                    feature: "media in legacy Completions",
                })
            }
        }
        self.w.byte(b'}');
        Ok(())
    }
    fn media_url(&mut self, source: MediaSource<'a>) -> Result<(), Error> {
        match source {
            MediaSource::Url(url)
            | MediaSource::DataUrl(url)
            | MediaSource::FileId { id: url, .. } => write_text(&mut self.w, url),
            MediaSource::Base64 { data, mime_type } => {
                self.w.byte(b'"');
                self.w.raw(b"data:");
                text_inner(&mut self.w, mime_type);
                self.w.raw(b";base64,");
                text_inner(&mut self.w, data);
                self.w.byte(b'"');
            }
        }
        Ok(())
    }
    fn content(&mut self, block: Block<'a>, role: Role, response: bool) -> Result<bool, Error> {
        let p = self.target.protocol;
        if matches!(block.content, Content::Reasoning { .. }) && self.policy.drop_reasoning {
            return Ok(false);
        }
        if let Content::Opaque { source, value } = block.content {
            if source == self.target {
                self.w.raw(value.raw());
                return Ok(true);
            }
            return Err(Error::Unsupported {
                feature: "opaque block",
            });
        }
        if let Content::Media(media) = block.content {
            self.media(media, response)?;
            return Ok(true);
        }
        self.w.byte(b'{');
        let mut c = false;
        match block.content {
            Content::Text(t) => {
                if p != Protocol::GeminiGenerateContent {
                    key(&mut self.w, &mut c, "type");
                    self.w.string(if p == Protocol::OpenAiResponses {
                        if response || role == Role::Assistant {
                            b"output_text"
                        } else {
                            b"input_text"
                        }
                    } else {
                        b"text"
                    });
                }
                key(&mut self.w, &mut c, "text");
                write_text(&mut self.w, t);
            }
            Content::Refusal(t) => {
                if p == Protocol::AnthropicMessages {
                    key(&mut self.w, &mut c, "type");
                    self.w.string(b"text");
                    key(&mut self.w, &mut c, "text");
                    write_text(&mut self.w, t);
                } else {
                    if !matches!(p, Protocol::OpenAiResponses | Protocol::OpenAiChat) {
                        return Err(Error::Unsupported {
                            feature: "refusal content in target",
                        });
                    }
                    key(&mut self.w, &mut c, "type");
                    self.w.string(b"refusal");
                    key(&mut self.w, &mut c, "refusal");
                    write_text(&mut self.w, t);
                }
            }
            Content::Reasoning {
                text, signature, ..
            } => {
                if p == Protocol::AnthropicMessages {
                    key(&mut self.w, &mut c, "type");
                    self.w.string(if text.is_missing() {
                        b"redacted_thinking"
                    } else {
                        b"thinking"
                    });
                    ptext(&mut self.w, &mut c, "thinking", text);
                    ptext(
                        &mut self.w,
                        &mut c,
                        if text.is_missing() {
                            "data"
                        } else {
                            "signature"
                        },
                        signature,
                    );
                } else if p == Protocol::GeminiGenerateContent {
                    ptext(&mut self.w, &mut c, "text", text);
                    key(&mut self.w, &mut c, "thought");
                    self.w.raw(b"true");
                    ptext(&mut self.w, &mut c, "thoughtSignature", signature);
                } else {
                    return Err(Error::Unsupported {
                        feature: "reasoning content requires target reasoning item",
                    });
                }
            }
            Content::ToolCall {
                id,
                name,
                arguments,
                ..
            } => {
                if p == Protocol::AnthropicMessages {
                    let id = self.bind_id(id, name)?;
                    key(&mut self.w, &mut c, "type");
                    self.w.string(b"tool_use");
                    key(&mut self.w, &mut c, "id");
                    write_text(&mut self.w, id);
                    key(&mut self.w, &mut c, "name");
                    write_text(&mut self.w, name);
                    key(&mut self.w, &mut c, "input");
                    self.data(arguments, false, true)?;
                } else if p == Protocol::GeminiGenerateContent {
                    key(&mut self.w, &mut c, "functionCall");
                    self.w.byte(b'{');
                    let mut n = false;
                    ptext(&mut self.w, &mut n, "id", id);
                    key(&mut self.w, &mut n, "name");
                    write_text(&mut self.w, name);
                    key(&mut self.w, &mut n, "args");
                    self.data(arguments, false, true)?;
                    self.w.byte(b'}');
                } else {
                    return Err(Error::Unsupported {
                        feature: "tool call in target content array",
                    });
                }
            }
            Content::ToolResult {
                id,
                name,
                result,
                is_error,
            } => {
                if p == Protocol::AnthropicMessages {
                    let id = match id {
                        Presence::Value(v) => v,
                        _ => self.bind_id(id, self.bind_name(name, id)?)?,
                    };
                    key(&mut self.w, &mut c, "type");
                    self.w.string(b"tool_result");
                    key(&mut self.w, &mut c, "tool_use_id");
                    write_text(&mut self.w, id);
                    key(&mut self.w, &mut c, "content");
                    self.data(
                        result,
                        matches!(result,Data::Json(v)if v.kind()==Kind::Object),
                        false,
                    )?;
                    pbool(&mut self.w, &mut c, "is_error", is_error);
                } else if p == Protocol::GeminiGenerateContent {
                    if !is_error.is_missing() {
                        return Err(Error::Unsupported {
                            feature: "Gemini function result is_error flag",
                        });
                    }
                    let name = self.bind_name(name, id)?;
                    key(&mut self.w, &mut c, "functionResponse");
                    self.w.byte(b'{');
                    let mut n = false;
                    ptext(&mut self.w, &mut n, "id", id);
                    key(&mut self.w, &mut n, "name");
                    write_text(&mut self.w, name);
                    key(&mut self.w, &mut n, "response");
                    self.data(result, false, true)?;
                    self.w.byte(b'}');
                } else {
                    return Err(Error::Unsupported {
                        feature: "tool result in target content array",
                    });
                }
            }
            _ => {
                return Err(Error::Unsupported {
                    feature: "content representation",
                })
            }
        }
        self.extras(block.extensions, &mut c)?;
        self.w.byte(b'}');
        Ok(true)
    }
    fn blocks(
        &mut self,
        start: usize,
        end: usize,
        role: Role,
        response: bool,
    ) -> Result<(), Error> {
        self.w.byte(b'[');
        let mut comma = false;
        let mut at = start;
        while at < end {
            let r = *self
                .records
                .get(at)
                .ok_or_else(|| invalid("IR content", "invalid arena range"))?;
            match r {
                Record::Block(b) => {
                    if !(matches!(b.content, Content::Reasoning { .. })
                        && self.policy.drop_reasoning)
                    {
                        if comma {
                            self.w.byte(b',');
                        }
                        comma = true;
                        self.content(b, role, response)?;
                    }
                    at = b.end;
                }
                _ => at += 1,
            }
        }
        self.w.byte(b']');
        Ok(())
    }
    fn message(&mut self, index: usize, m: Message<'a>, response: bool) -> Result<(), Error> {
        match self.target.protocol {
            Protocol::OpenAiChat => self.chat_message(index, m, response),
            Protocol::AnthropicMessages | Protocol::GeminiGenerateContent => {
                if m.role == Role::Developer {
                    return Err(Error::Unsupported {
                        feature: "developer role in target",
                    });
                }
                if !m.name.is_missing() {
                    return Err(Error::Unsupported {
                        feature: "message name in target",
                    });
                }
                self.w.byte(b'{');
                let mut c = false;
                key(&mut self.w, &mut c, "role");
                self.w.string(if m.role == Role::Assistant {
                    if self.target.protocol == Protocol::GeminiGenerateContent {
                        b"model"
                    } else {
                        b"assistant"
                    }
                } else {
                    b"user"
                });
                key(
                    &mut self.w,
                    &mut c,
                    if self.target.protocol == Protocol::GeminiGenerateContent {
                        "parts"
                    } else {
                        "content"
                    },
                );
                self.blocks(index + 1, m.end, m.role, response)?;
                self.extras(m.extensions, &mut c)?;
                self.w.byte(b'}');
                Ok(())
            }
            _ => Err(Error::Unsupported {
                feature: "message target",
            }),
        }
    }
    fn chat_message(&mut self, index: usize, m: Message<'a>, response: bool) -> Result<(), Error> {
        let result_count = self.records[index + 1..m.end]
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    Record::Block(Block {
                        content: Content::ToolResult { .. },
                        ..
                    })
                )
            })
            .count();
        if result_count > 1 {
            return Err(Error::Unsupported {
                feature: "multiple tool results require separate Chat messages",
            });
        }
        self.w.byte(b'{');
        let mut c = false;
        key(&mut self.w, &mut c, "role");
        self.w.string(
            if m.legacy_function && self.source.protocol == Protocol::OpenAiChat {
                b"function"
            } else if result_count == 1 {
                b"tool"
            } else {
                role_name(m.role)
            },
        );
        ptext(&mut self.w, &mut c, "name", m.name);
        let mut seen_call = false;
        for record in &self.records[index + 1..m.end] {
            if let Record::Block(b) = record {
                if matches!(b.content, Content::ToolCall { .. }) {
                    seen_call = true;
                } else if seen_call
                    && matches!(
                        b.content,
                        Content::Text(_) | Content::Media(_) | Content::Refusal(_)
                    )
                {
                    return Err(Error::Unsupported {
                        feature: "interleaved content/tool order in Chat",
                    });
                }
            }
        }
        let mut plain = 0;
        let mut calls = 0;
        let mut other = 0;
        let mut text_value = None;
        let mut refusal = None;
        let mut reasoning = None;
        let mut result = None;
        for r in self
            .records
            .get(index + 1..m.end)
            .ok_or_else(|| invalid("IR message", "invalid range"))?
        {
            if let Record::Block(b) = r {
                match b.content {
                    Content::Text(t) => {
                        plain += 1;
                        text_value = Some(t);
                    }
                    Content::ToolCall { .. } => calls += 1,
                    Content::ToolResult { .. } => result = Some(*b),
                    Content::Refusal(t) => {
                        if refusal.is_some() {
                            return Err(Error::Unsupported {
                                feature: "multiple refusal blocks in Chat",
                            });
                        }
                        refusal = Some(t);
                    }
                    Content::Reasoning { text, .. } => {
                        if !self.policy.drop_reasoning {
                            if reasoning.is_some() {
                                return Err(Error::Unsupported {
                                    feature: "multiple reasoning blocks in Chat",
                                });
                            }
                            reasoning = text.value();
                        }
                    }
                    _ => other += 1,
                }
            }
        }
        if let Some(b) = result {
            if plain + calls + other > 0 {
                return Err(Error::Unsupported {
                    feature: "mixed tool result Chat message",
                });
            }
            if let Content::ToolResult {
                id,
                name,
                result,
                is_error,
            } = b.content
            {
                if !is_error.is_missing() {
                    return Err(Error::Unsupported {
                        feature: "Chat tool error flag",
                    });
                }
                if !m.legacy_function {
                    let id = match id {
                        Presence::Value(id) => id,
                        _ => self.bind_id(id, self.bind_name(name, id)?)?,
                    };
                    key(&mut self.w, &mut c, "tool_call_id");
                    write_text(&mut self.w, id);
                }
                key(&mut self.w, &mut c, "content");
                self.data(
                    result,
                    matches!(result,Data::Json(v)if v.kind()==Kind::Object),
                    false,
                )?;
            }
        } else {
            if response && plain > 1 && other == 0 {
                key(&mut self.w, &mut c, "content");
                self.w.byte(b'"');
                for record in &self.records[index + 1..m.end] {
                    if let Record::Block(Block {
                        content: Content::Text(t),
                        ..
                    }) = record
                    {
                        text_inner(&mut self.w, *t);
                    }
                }
                self.w.byte(b'"');
            } else if plain == 1 && other == 0 && (m.shape == ContentShape::String || response) {
                key(&mut self.w, &mut c, "content");
                if let Some(t) = text_value {
                    write_text(&mut self.w, t);
                }
            } else if plain + other > 0 {
                key(&mut self.w, &mut c, "content");
                self.w.byte(b'[');
                let mut bc = false;
                for at in index + 1..m.end {
                    if let Record::Block(b) = self.records[at] {
                        if matches!(
                            b.content,
                            Content::Text(_) | Content::Media(_) | Content::Opaque { .. }
                        ) {
                            if bc {
                                self.w.byte(b',');
                            }
                            bc = true;
                            self.content(b, m.role, response)?;
                        }
                    }
                }
                self.w.byte(b']');
            } else {
                match m.shape {
                    ContentShape::Missing => {}
                    ContentShape::Null => {
                        key(&mut self.w, &mut c, "content");
                        self.w.raw(b"null");
                    }
                    _ => {
                        key(&mut self.w, &mut c, "content");
                        self.w.raw(b"[]");
                    }
                }
            }
            if refusal.is_none() && m.refusal.is_null() {
                key(&mut self.w, &mut c, "refusal");
                self.w.raw(b"null");
            }
            if reasoning.is_none() && m.reasoning.is_null() {
                key(&mut self.w, &mut c, "reasoning_content");
                self.w.raw(b"null");
            }
            if let Some(r) = refusal {
                key(&mut self.w, &mut c, "refusal");
                write_text(&mut self.w, r);
            }
            if let Some(r) = reasoning {
                if matches!(self.target.profile, Profile::Standard) {
                    return Err(Error::Unsupported {
                        feature: "reasoning_content requires explicit vendor profile",
                    });
                }
                key(&mut self.w, &mut c, "reasoning_content");
                write_text(&mut self.w, r);
            }
            if calls > 0
                && matches!(m.function_call, Presence::Value(()))
                && self.source.protocol == Protocol::OpenAiChat
            {
                if calls != 1 {
                    return Err(Error::Unsupported {
                        feature: "multiple calls with legacy function_call",
                    });
                }
                for at in index + 1..m.end {
                    if let Record::Block(Block {
                        content:
                            Content::ToolCall {
                                name, arguments, ..
                            },
                        ..
                    }) = self.records[at]
                    {
                        key(&mut self.w, &mut c, "function_call");
                        self.w.raw(b"{\"name\":");
                        write_text(&mut self.w, name);
                        self.w.raw(b",\"arguments\":");
                        self.data(arguments, true, false)?;
                        self.w.byte(b'}');
                    }
                }
            } else if calls > 0 {
                key(&mut self.w, &mut c, "tool_calls");
                self.w.byte(b'[');
                let mut tc = false;
                for at in index + 1..m.end {
                    if let Record::Block(b) = self.records[at] {
                        if let Content::ToolCall {
                            id,
                            name,
                            arguments,
                            ..
                        } = b.content
                        {
                            if tc {
                                self.w.byte(b',');
                            }
                            tc = true;
                            let id = self.bind_id(id, name)?;
                            self.w.raw(b"{\"id\":");
                            write_text(&mut self.w, id);
                            self.w
                                .raw(b",\"type\":\"function\",\"function\":{\"name\":");
                            write_text(&mut self.w, name);
                            self.w.raw(b",\"arguments\":");
                            self.data(arguments, true, false)?;
                            let mut ec = true;
                            self.extras(b.extensions, &mut ec)?;
                            self.w.raw(b"}}");
                        }
                    }
                }
                self.w.byte(b']');
            } else {
                match m.tool_calls {
                    Presence::Null => {
                        key(&mut self.w, &mut c, "tool_calls");
                        self.w.raw(b"null");
                    }
                    Presence::Value(()) => {
                        key(&mut self.w, &mut c, "tool_calls");
                        self.w.raw(b"[]");
                    }
                    _ => {}
                }
                if m.function_call.is_null() {
                    key(&mut self.w, &mut c, "function_call");
                    self.w.raw(b"null");
                }
            }
        }
        self.extras(m.extensions, &mut c)?;
        self.w.byte(b'}');
        Ok(())
    }
    fn items(&mut self, start: usize, end: usize, response: bool) -> Result<(), Error> {
        self.w.byte(b'[');
        let mut comma = false;
        let mut at = start;
        while at < end {
            if let Record::Message(m) = self.records[at] {
                if m.role == Role::System
                    && (self.target.protocol == Protocol::GeminiInteractions
                        || m.system_instruction)
                {
                    at = m.end;
                    continue;
                }
                let only_summaries = self.records[at + 1..m.end]
                    .iter()
                    .filter_map(|r| {
                        if let Record::Block(b) = r {
                            Some(b)
                        } else {
                            None
                        }
                    })
                    .all(|b| {
                        matches!(
                            b.content,
                            Content::Reasoning {
                                kind: ReasoningKind::Summary,
                                ..
                            }
                        )
                    });
                let has_summaries = self.records[at + 1..m.end].iter().any(|r| {
                    matches!(
                        r,
                        Record::Block(Block {
                            content: Content::Reasoning {
                                kind: ReasoningKind::Summary,
                                ..
                            },
                            ..
                        })
                    )
                });
                if self.target.protocol == Protocol::OpenAiResponses
                    && only_summaries
                    && has_summaries
                {
                    if !self.policy.drop_reasoning {
                        if comma {
                            self.w.byte(b',');
                        }
                        comma = true;
                        self.reasoning_item(at + 1, m.end, response)?;
                    }
                    at = m.end;
                    continue;
                }
                let mut run_start = at + 1;
                for bi in at + 1..m.end {
                    if let Record::Block(b) = self.records[bi] {
                        let special = matches!(
                            b.content,
                            Content::ToolCall { .. }
                                | Content::ToolResult { .. }
                                | Content::Reasoning { .. }
                                | Content::Opaque { .. }
                        );
                        if special {
                            if run_start < bi {
                                self.item_message(m, run_start, bi, response, &mut comma)?;
                            }
                            if !(matches!(b.content, Content::Reasoning { .. })
                                && self.policy.drop_reasoning)
                            {
                                if comma {
                                    self.w.byte(b',');
                                }
                                comma = true;
                                self.item(b, response)?;
                            }
                            run_start = b.end;
                        }
                    }
                }
                if run_start < m.end
                    || !self.records[at + 1..m.end]
                        .iter()
                        .any(|r| matches!(r, Record::Block(_)))
                {
                    self.item_message(m, run_start, m.end, response, &mut comma)?;
                }
                at = m.end;
            } else {
                at += 1;
            }
        }
        self.w.byte(b']');
        Ok(())
    }
    fn reasoning_item(&mut self, start: usize, end: usize, response: bool) -> Result<(), Error> {
        let mut id = Presence::Missing;
        let mut signature = Presence::Missing;
        let mut item_status = Presence::Missing;
        let mut item_extras = Span::EMPTY;
        for record in &self.records[start..end] {
            if let Record::Block(b) = record {
                if !b.item_id.is_missing() {
                    id = b.item_id;
                    item_status = b.item_status;
                }
                if let Content::Reasoning {
                    signature: s, text, ..
                } = b.content
                {
                    if !s.is_missing() {
                        if !signature.is_missing() {
                            return Err(Error::Unsupported {
                                feature: "multiple reasoning signatures in one item",
                            });
                        }
                        signature = s;
                    }
                    if text.is_missing() {
                        item_extras = b.extensions;
                    }
                }
            }
        }
        let id = self.output_id(id, response)?;
        self.w.raw(b"{\"type\":\"reasoning\"");
        let mut c = true;
        ptext(&mut self.w, &mut c, "id", id);
        if response {
            ptext(
                &mut self.w,
                &mut c,
                "status",
                if item_status.is_missing() {
                    Presence::Value(Text::Utf8("completed"))
                } else {
                    item_status
                },
            );
        }
        key(&mut self.w, &mut c, "summary");
        self.w.byte(b'[');
        let mut sc = false;
        for record in &self.records[start..end] {
            if let Record::Block(b) = record {
                if let Content::Reasoning {
                    text: Presence::Value(t),
                    ..
                } = b.content
                {
                    if sc {
                        self.w.byte(b',');
                    }
                    sc = true;
                    self.w.raw(b"{\"type\":\"summary_text\",\"text\":");
                    write_text(&mut self.w, t);
                    let mut c = true;
                    self.extras(b.extensions, &mut c)?;
                    self.w.byte(b'}');
                }
            }
        }
        self.w.byte(b']');
        ptext(&mut self.w, &mut c, "encrypted_content", signature);
        self.extras(item_extras, &mut c)?;
        self.w.byte(b'}');
        Ok(())
    }
    fn item_message(
        &mut self,
        m: Message<'a>,
        start: usize,
        end: usize,
        response: bool,
        comma: &mut bool,
    ) -> Result<(), Error> {
        if !self.records[start..end]
            .iter()
            .any(|r| matches!(r, Record::Block(_)))
            && m.shape != ContentShape::Array
        {
            return Err(Error::Unsupported {
                feature: "missing or null content in target item",
            });
        }
        if *comma {
            self.w.byte(b',');
        }
        *comma = true;
        self.w.byte(b'{');
        let mut c = false;
        key(&mut self.w, &mut c, "type");
        let interactions = self.target.protocol == Protocol::GeminiInteractions;
        if interactions {
            if !matches!(m.role, Role::User | Role::Assistant) {
                return Err(Error::Unsupported {
                    feature: "role in Interactions",
                });
            }
            self.w.string(if m.role == Role::Assistant {
                b"model_output"
            } else {
                b"user_input"
            });
        } else {
            self.w.string(b"message");
            key(&mut self.w, &mut c, "role");
            self.w.string(role_name(m.role));
            let item_id = self.output_id(m.item_id, response)?;
            ptext(&mut self.w, &mut c, "id", item_id);
            if response {
                ptext(
                    &mut self.w,
                    &mut c,
                    "status",
                    if m.item_status.is_missing() {
                        Presence::Value(Text::Utf8("completed"))
                    } else {
                        m.item_status
                    },
                );
            }
        }
        key(&mut self.w, &mut c, "content");
        self.blocks(start, end, m.role, response)?;
        self.extras(m.extensions, &mut c)?;
        self.w.byte(b'}');
        Ok(())
    }
    fn item(&mut self, b: Block<'a>, response: bool) -> Result<(), Error> {
        let interactions = self.target.protocol == Protocol::GeminiInteractions;
        if let Content::Opaque { source, value } = b.content {
            if source == self.target {
                self.w.raw(value.raw());
                return Ok(());
            }
            return Err(Error::Unsupported {
                feature: "opaque item",
            });
        }
        self.w.byte(b'{');
        let mut c = false;
        match b.content {
            Content::ToolCall {
                id,
                name,
                arguments,
                ..
            } => {
                let id = self.bind_id(id, name)?;
                key(&mut self.w, &mut c, "type");
                self.w.string(b"function_call");
                key(
                    &mut self.w,
                    &mut c,
                    if interactions { "id" } else { "call_id" },
                );
                write_text(&mut self.w, id);
                key(&mut self.w, &mut c, "name");
                write_text(&mut self.w, name);
                key(&mut self.w, &mut c, "arguments");
                self.data(arguments, !interactions, interactions)?;
            }
            Content::ToolResult {
                id,
                name,
                result,
                is_error,
            } => {
                let id = match id {
                    Presence::Value(id) => id,
                    _ => self.bind_id(id, self.bind_name(name, id)?)?,
                };
                key(&mut self.w, &mut c, "type");
                self.w.string(if interactions {
                    b"function_result"
                } else {
                    b"function_call_output"
                });
                key(&mut self.w, &mut c, "call_id");
                write_text(&mut self.w, id);
                key(
                    &mut self.w,
                    &mut c,
                    if interactions { "result" } else { "output" },
                );
                self.data(
                    result,
                    !interactions && matches!(result,Data::Json(v)if v.kind()==Kind::Object),
                    false,
                )?;
                if interactions {
                    ptext(&mut self.w, &mut c, "name", name);
                    pbool(&mut self.w, &mut c, "is_error", is_error);
                } else if !is_error.is_missing() {
                    return Err(Error::Unsupported {
                        feature: "Responses tool result error flag",
                    });
                }
            }
            Content::Reasoning {
                text, signature, ..
            } => {
                key(&mut self.w, &mut c, "type");
                self.w.string(if interactions {
                    b"thought"
                } else {
                    b"reasoning"
                });
                if interactions {
                    ptext(&mut self.w, &mut c, "summary", text);
                    ptext(&mut self.w, &mut c, "signature", signature);
                } else {
                    key(&mut self.w, &mut c, "summary");
                    self.w.byte(b'[');
                    if let Presence::Value(t) = text {
                        self.w.raw(b"{\"type\":\"summary_text\",\"text\":");
                        write_text(&mut self.w, t);
                        self.w.byte(b'}');
                    }
                    self.w.byte(b']');
                    ptext(&mut self.w, &mut c, "encrypted_content", signature);
                }
            }
            _ => {
                return Err(Error::Unsupported {
                    feature: "non-item content",
                })
            }
        }
        if !interactions {
            let item_id = self.output_id(b.item_id, response)?;
            ptext(&mut self.w, &mut c, "id", item_id);
            if response {
                ptext(
                    &mut self.w,
                    &mut c,
                    "status",
                    if b.item_status.is_missing() {
                        Presence::Value(Text::Utf8("completed"))
                    } else {
                        b.item_status
                    },
                );
            }
        }
        self.extras(b.extensions, &mut c)?;
        self.w.byte(b'}');
        Ok(())
    }
    fn tools(
        &mut self,
        comma: &mut bool,
        shape: Presence<()>,
        legacy_shape: Presence<()>,
    ) -> Result<(), Error> {
        if !self.records.iter().any(|r| matches!(r, Record::Tool(_))) {
            let legacy = self.source.protocol == Protocol::OpenAiChat
                && self.target.protocol == Protocol::OpenAiChat
                && !legacy_shape.is_missing();
            let shape = if legacy || shape.is_missing() {
                legacy_shape
            } else {
                shape
            };
            match shape {
                Presence::Missing => {}
                Presence::Null => {
                    key(
                        &mut self.w,
                        comma,
                        if legacy { "functions" } else { "tools" },
                    );
                    self.w.raw(b"null");
                }
                Presence::Value(()) => {
                    key(
                        &mut self.w,
                        comma,
                        if legacy { "functions" } else { "tools" },
                    );
                    self.w.raw(b"[]");
                }
            }
            return Ok(());
        }
        if self.target.protocol == Protocol::OpenAiCompletions {
            return Err(Error::Unsupported {
                feature: "tools in legacy Completions",
            });
        }
        let legacy = self.target.protocol == Protocol::OpenAiChat
            && self.source.protocol == Protocol::OpenAiChat
            && shape.is_missing()
            && !legacy_shape.is_missing();
        key(
            &mut self.w,
            comma,
            if legacy { "functions" } else { "tools" },
        );
        self.w.byte(b'[');
        let google = self.target.protocol == Protocol::GeminiGenerateContent;
        if google {
            self.w.raw(b"{\"functionDeclarations\":[");
        }
        let mut tc = false;
        for record in self.records {
            if let Record::Tool(tool) = record {
                if tc {
                    self.w.byte(b',');
                }
                tc = true;
                self.w.byte(b'{');
                let mut c = false;
                if !legacy
                    && matches!(
                        self.target.protocol,
                        Protocol::OpenAiChat
                            | Protocol::OpenAiResponses
                            | Protocol::GeminiInteractions
                    )
                {
                    key(&mut self.w, &mut c, "type");
                    self.w.string(b"function");
                }
                if self.target.protocol == Protocol::OpenAiChat && !legacy {
                    key(&mut self.w, &mut c, "function");
                    self.w.byte(b'{');
                    c = false;
                }
                key(&mut self.w, &mut c, "name");
                write_text(&mut self.w, tool.name);
                ptext(&mut self.w, &mut c, "description", tool.description);
                praw(
                    &mut self.w,
                    &mut c,
                    if self.target.protocol == Protocol::AnthropicMessages {
                        "input_schema"
                    } else {
                        "parameters"
                    },
                    tool.parameters,
                );
                // Responses may normalize an implicit schema into strict mode.
                // Other providers' best-effort defaults must not enable that
                // normalization. In the reverse direction only the caller can
                // resolve which mode the Responses service actually selected.
                let strict = if self.source.protocol == Protocol::OpenAiResponses
                    && self.target.protocol != Protocol::OpenAiResponses
                    && !matches!(tool.strict, Presence::Value(_))
                {
                    return Err(Error::ContextRequired {
                        field: "resolved tool strict mode",
                    });
                } else if self.target.protocol == Protocol::OpenAiResponses
                    && self.source.protocol != Protocol::OpenAiResponses
                    && !matches!(tool.strict, Presence::Value(_))
                {
                    Presence::Value(false)
                } else {
                    tool.strict
                };
                if matches!(
                    self.target.protocol,
                    Protocol::OpenAiChat | Protocol::OpenAiResponses
                ) {
                    pbool(&mut self.w, &mut c, "strict", strict);
                } else if matches!(strict, Presence::Value(true)) {
                    return Err(Error::Unsupported {
                        feature: "strict tool schema guarantee in target",
                    });
                }
                self.extras(tool.extensions, &mut c)?;
                if self.target.protocol == Protocol::OpenAiChat && !legacy {
                    self.w.byte(b'}');
                }
                self.w.byte(b'}');
            }
        }
        if google {
            self.w.raw(b"]}");
        }
        self.w.byte(b']');
        Ok(())
    }
    fn choice(&mut self, choice: ToolChoice<'a>) -> Result<(), Error> {
        let p = self.target.protocol;
        match choice {
            ToolChoice::Missing => {
                return Err(invalid("tool_choice", "missing choice cannot be written"))
            }
            ToolChoice::Null => self.w.raw(b"null"),
            ToolChoice::Opaque { source, value } => {
                if source != self.target {
                    return Err(Error::Unsupported {
                        feature: "provider tool choice",
                    });
                }
                self.w.raw(value.raw());
            }
            ToolChoice::Auto | ToolChoice::None | ToolChoice::Required => {
                let name = match choice {
                    ToolChoice::Auto => "auto",
                    ToolChoice::None => "none",
                    _ => "required",
                };
                if p == Protocol::AnthropicMessages {
                    self.w.raw(b"{\"type\":");
                    self.w.string(if name == "required" {
                        b"any"
                    } else {
                        name.as_bytes()
                    });
                    self.w.byte(b'}');
                } else if p == Protocol::GeminiGenerateContent {
                    self.w.raw(b"{\"mode\":");
                    self.w.string(match choice {
                        ToolChoice::Auto => b"AUTO",
                        ToolChoice::None => b"NONE",
                        _ => b"ANY",
                    });
                    self.w.byte(b'}');
                } else {
                    self.w
                        .string(if p == Protocol::GeminiInteractions && name == "required" {
                            b"any"
                        } else {
                            name.as_bytes()
                        });
                }
            }
            ToolChoice::Named(name) => match p {
                Protocol::OpenAiChat => {
                    self.w
                        .raw(b"{\"type\":\"function\",\"function\":{\"name\":");
                    write_text(&mut self.w, name);
                    self.w.raw(b"}}");
                }
                Protocol::OpenAiResponses => {
                    self.w.raw(b"{\"type\":\"function\",\"name\":");
                    write_text(&mut self.w, name);
                    self.w.byte(b'}');
                }
                Protocol::AnthropicMessages => {
                    self.w.raw(b"{\"type\":\"tool\",\"name\":");
                    write_text(&mut self.w, name);
                    self.w.byte(b'}');
                }
                Protocol::GeminiGenerateContent => {
                    self.w.raw(b"{\"mode\":\"ANY\",\"allowedFunctionNames\":[");
                    write_text(&mut self.w, name);
                    self.w.raw(b"]}");
                }
                Protocol::GeminiInteractions => {
                    self.w
                        .raw(b"{\"allowed_tools\":{\"mode\":\"any\",\"tools\":[");
                    write_text(&mut self.w, name);
                    self.w.raw(b"]}}");
                }
                _ => {
                    return Err(Error::Unsupported {
                        feature: "tool choice in Completions",
                    })
                }
            },
        }
        Ok(())
    }
    fn parameters(
        &mut self,
        p: Parameters<'a>,
        choice: ToolChoice<'a>,
        constraint: Presence<OutputConstraint<'a>>,
        legacy_choice: bool,
        comma: &mut bool,
    ) -> Result<(), Error> {
        let target = self.target.protocol;
        let google = target == Protocol::GeminiGenerateContent;
        let interactions = target == Protocol::GeminiInteractions;
        if self.target.protocol == Protocol::AnthropicMessages
            && self.target.version == "2026-10-21"
            && (!p.temperature.is_missing() || !p.top_p.is_missing() || !p.top_k.is_missing())
        {
            return Err(Error::Unsupported {
                feature: "sampling controls in Anthropic 2026-10-21",
            });
        }
        if interactions
            && (!p.temperature.is_missing() || !p.top_p.is_missing() || !p.top_k.is_missing())
        {
            return Err(Error::Unsupported {
                feature: "sampling controls in Interactions v1",
            });
        }
        if !p.top_k.is_missing()
            && !matches!(
                target,
                Protocol::AnthropicMessages | Protocol::GeminiGenerateContent
            )
        {
            return Err(Error::Unsupported { feature: "top_k" });
        }
        if !p.n.is_missing()
            && !matches!(
                target,
                Protocol::OpenAiChat
                    | Protocol::OpenAiCompletions
                    | Protocol::GeminiGenerateContent
            )
        {
            return Err(Error::Unsupported {
                feature: "multiple candidate request",
            });
        }
        if !p.seed.is_missing()
            && matches!(
                target,
                Protocol::AnthropicMessages | Protocol::OpenAiResponses
            )
        {
            return Err(Error::Unsupported { feature: "seed" });
        }
        if (!p.frequency_penalty.is_missing() || !p.presence_penalty.is_missing())
            && matches!(
                target,
                Protocol::AnthropicMessages
                    | Protocol::OpenAiResponses
                    | Protocol::GeminiInteractions
            )
        {
            return Err(Error::Unsupported {
                feature: "token penalties",
            });
        }
        if !p.stop.is_missing() && target == Protocol::OpenAiResponses {
            return Err(Error::Unsupported {
                feature: "stop sequences in Responses",
            });
        }
        let has_config = (google && !constraint.is_missing())
            || !p.temperature.is_missing()
            || !p.top_p.is_missing()
            || !p.top_k.is_missing()
            || !p.max_output_tokens.is_missing()
            || !p.stop.is_missing()
            || !p.seed.is_missing()
            || !p.frequency_penalty.is_missing()
            || !p.presence_penalty.is_missing()
            || !p.n.is_missing()
            || p.extensions.start != p.extensions.end
            || (interactions && !matches!(choice, ToolChoice::Missing));
        let mut c = *comma;
        if (google || interactions) && has_config {
            key(
                &mut self.w,
                comma,
                if google {
                    "generationConfig"
                } else {
                    "generation_config"
                },
            );
            self.w.byte(b'{');
            c = false;
        }
        if !(google || interactions) || has_config {
            praw(&mut self.w, &mut c, "temperature", p.temperature);
            praw(
                &mut self.w,
                &mut c,
                if google { "topP" } else { "top_p" },
                p.top_p,
            );
            praw(
                &mut self.w,
                &mut c,
                if google { "topK" } else { "top_k" },
                p.top_k,
            );
            let maxkey = match target {
                Protocol::OpenAiResponses | Protocol::GeminiInteractions => "max_output_tokens",
                Protocol::GeminiGenerateContent => "maxOutputTokens",
                Protocol::OpenAiChat => "max_completion_tokens",
                _ => "max_tokens",
            };
            if target == Protocol::AnthropicMessages && p.max_output_tokens.is_missing() {
                let n = self.context.max_tokens.ok_or(Error::ContextRequired {
                    field: "max_tokens",
                })?;
                key(&mut self.w, &mut c, maxkey);
                uint(&mut self.w, n);
            } else {
                praw(&mut self.w, &mut c, maxkey, p.max_output_tokens);
            }
            if let Presence::Value(stop) = p.stop {
                if stop.kind() == Kind::String
                    && matches!(
                        target,
                        Protocol::AnthropicMessages
                            | Protocol::GeminiGenerateContent
                            | Protocol::GeminiInteractions
                    )
                {
                    key(
                        &mut self.w,
                        &mut c,
                        if google {
                            "stopSequences"
                        } else {
                            "stop_sequences"
                        },
                    );
                    self.w.byte(b'[');
                    self.w.raw(stop.raw());
                    self.w.byte(b']');
                } else {
                    praw(
                        &mut self.w,
                        &mut c,
                        if google {
                            "stopSequences"
                        } else if matches!(
                            target,
                            Protocol::AnthropicMessages | Protocol::GeminiInteractions
                        ) {
                            "stop_sequences"
                        } else {
                            "stop"
                        },
                        p.stop,
                    );
                }
            } else {
                praw(
                    &mut self.w,
                    &mut c,
                    if google {
                        "stopSequences"
                    } else if matches!(
                        target,
                        Protocol::AnthropicMessages | Protocol::GeminiInteractions
                    ) {
                        "stop_sequences"
                    } else {
                        "stop"
                    },
                    p.stop,
                );
            }
            praw(&mut self.w, &mut c, "seed", p.seed);
            praw(
                &mut self.w,
                &mut c,
                if google {
                    "frequencyPenalty"
                } else {
                    "frequency_penalty"
                },
                p.frequency_penalty,
            );
            praw(
                &mut self.w,
                &mut c,
                if google {
                    "presencePenalty"
                } else {
                    "presence_penalty"
                },
                p.presence_penalty,
            );
            praw(
                &mut self.w,
                &mut c,
                if google { "candidateCount" } else { "n" },
                p.n,
            );
            if interactions && !matches!(choice, ToolChoice::Missing) {
                key(&mut self.w, &mut c, "tool_choice");
                self.choice(choice)?;
            }
            if google {
                match constraint {
                    Presence::Missing => {}
                    Presence::Null => {
                        return Err(Error::Unsupported {
                            feature: "null output constraint in Gemini",
                        })
                    }
                    Presence::Value(v) => {
                        if v.source != self.target && v.mode == OutputMode::JsonSchema {
                            return Err(Error::Unsupported {
                                feature: "cross-provider schema constraint guarantee",
                            });
                        }
                        key(&mut self.w, &mut c, "responseMimeType");
                        self.w.string(if v.mode == OutputMode::Text {
                            b"text/plain"
                        } else {
                            b"application/json"
                        });
                        if v.mode == OutputMode::JsonSchema {
                            key(&mut self.w, &mut c, "responseJsonSchema");
                            self.w.raw(
                                v.schema
                                    .value()
                                    .ok_or_else(|| invalid("output schema", "schema missing"))?
                                    .raw(),
                            );
                        } else if v.mode == OutputMode::Opaque {
                            return Err(Error::Unsupported {
                                feature: "opaque Gemini output format",
                            });
                        }
                    }
                }
            }
            self.extras(p.extensions, &mut c)?;
            if google || interactions {
                self.w.byte(b'}');
            } else {
                *comma = c;
            }
        }
        if google {
            self.report.transport_stream = match p.stream {
                Presence::Missing => Presence::Missing,
                Presence::Null => Presence::Null,
                Presence::Value(v) => Presence::Value(
                    v.as_bool()
                        .ok_or_else(|| invalid("stream", "expected boolean"))?,
                ),
            };
        } else {
            praw(&mut self.w, comma, "stream", p.stream);
        }
        if !p.parallel_tool_calls.is_missing() {
            if matches!(target, Protocol::OpenAiChat | Protocol::OpenAiResponses) {
                praw(
                    &mut self.w,
                    comma,
                    "parallel_tool_calls",
                    p.parallel_tool_calls,
                );
            } else {
                return Err(Error::Unsupported {
                    feature: "parallel_tool_calls in target",
                });
            }
        }
        if !interactions && !matches!(choice, ToolChoice::Missing) {
            if google {
                key(&mut self.w, comma, "toolConfig");
                self.w.raw(b"{\"functionCallingConfig\":");
                self.choice(choice)?;
                self.w.byte(b'}');
            } else {
                key(
                    &mut self.w,
                    comma,
                    if legacy_choice && target == Protocol::OpenAiChat {
                        "function_call"
                    } else {
                        "tool_choice"
                    },
                );
                if legacy_choice && target == Protocol::OpenAiChat {
                    if let ToolChoice::Named(name) = choice {
                        self.w.raw(b"{\"name\":");
                        write_text(&mut self.w, name);
                        self.w.byte(b'}');
                    } else {
                        self.choice(choice)?;
                    }
                } else {
                    self.choice(choice)?;
                }
            }
        }
        Ok(())
    }
}

fn extension_is_reserved(e: Extension<'_>) -> bool {
    let keys: &[&str] = match e.location {
        "request" => super::decode::request_keys(e.source.protocol),
        "response" => super::decode::response_keys(e.source.protocol),
        "message" => &[
            "role",
            "name",
            "content",
            "parts",
            "id",
            "status",
            "tool_calls",
            "function_call",
            "tool_call_id",
            "refusal",
        ],
        "tool" => &[
            "type",
            "name",
            "description",
            "parameters",
            "input_schema",
            "strict",
        ],
        "choice" => &[
            "index",
            "message",
            "content",
            "text",
            "finish_reason",
            "finishReason",
        ],
        _ => &[],
    };
    keys.iter().any(|key| e.key.eq_str(key))
}

enum TextBytes<'a> {
    Raw(core::iter::Copied<core::slice::Iter<'a, u8>>),
    Json(json::DecodedBytes<'a>),
}
impl Iterator for TextBytes<'_> {
    type Item = u8;
    fn next(&mut self) -> Option<u8> {
        match self {
            Self::Raw(v) => v.next(),
            Self::Json(v) => v.next(),
        }
    }
}
fn text_bytes(text: Text<'_>) -> TextBytes<'_> {
    match text {
        Text::Utf8(s) => TextBytes::Raw(s.as_bytes().iter().copied()),
        Text::Json(s) => TextBytes::Json(s.bytes()),
    }
}
fn data_url_header(text: Text<'_>) -> Result<(usize, usize), Error> {
    let mut bytes = text_bytes(text);
    for expected in b"data:" {
        if bytes.next() != Some(*expected) {
            return Err(invalid("data URL", "missing data prefix"));
        }
    }
    let mut end = 5usize;
    let mut slash = false;
    loop {
        match bytes.next() {
            Some(b';') => break,
            Some(b'/') => slash = true,
            Some(b) if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'+' | b'.') => {}
            _ => {
                return Err(Error::Unsupported {
                    feature: "data URL MIME or non-base64 encoding",
                })
            }
        }
        end = end
            .checked_add(1)
            .ok_or(Error::InputTooLarge { limit: usize::MAX })?;
    }
    if !slash {
        return Err(invalid("data URL", "MIME type required"));
    }
    for expected in b"base64," {
        if bytes.next() != Some(*expected) {
            return Err(Error::Unsupported {
                feature: "non-base64 data URL",
            });
        }
    }
    Ok((
        end,
        end.checked_add(8)
            .ok_or(Error::InputTooLarge { limit: usize::MAX })?,
    ))
}
fn write_text_range(w: &mut Writer<'_>, text: Text<'_>, start: usize, end: usize) {
    w.byte(b'"');
    for (index, b) in text_bytes(text).enumerate() {
        if index >= end {
            break;
        }
        if index >= start {
            match b {
                b'"' => w.raw(b"\\\""),
                b'\\' => w.raw(b"\\\\"),
                0..=31 => {
                    const HEX: &[u8; 16] = b"0123456789abcdef";
                    w.raw(b"\\u00");
                    w.byte(HEX[(b >> 4) as usize]);
                    w.byte(HEX[(b & 15) as usize]);
                }
                _ => w.byte(b),
            }
        }
    }
    w.byte(b'"');
}

fn role_name(role: Role) -> &'static [u8] {
    match role {
        Role::System => b"system",
        Role::Developer => b"developer",
        Role::User => b"user",
        Role::Assistant => b"assistant",
        Role::Tool => b"tool",
    }
}

// Escaping an assembled string reuses the JSON writer's escaping for each byte.
fn text_inner(w: &mut Writer<'_>, text: Text<'_>) {
    fn byte(w: &mut Writer<'_>, b: u8) {
        match b {
            b'"' => w.raw(b"\\\""),
            b'\\' => w.raw(b"\\\\"),
            b'\n' => w.raw(b"\\n"),
            b'\r' => w.raw(b"\\r"),
            b'\t' => w.raw(b"\\t"),
            0..=31 => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                w.raw(b"\\u00");
                w.byte(HEX[(b >> 4) as usize]);
                w.byte(HEX[(b & 15) as usize]);
            }
            _ => w.byte(b),
        }
    }
    match text {
        Text::Json(s) => {
            for b in s.bytes() {
                byte(w, b);
            }
        }
        Text::Utf8(s) => {
            for &b in s.as_bytes() {
                byte(w, b);
            }
        }
    }
}

/// Encode a request. On error discard output; retry capacity errors with larger
/// buffers. `scratch` and `tokens` validate JSON embedded in tool-argument strings.
pub fn encode_request<'a>(
    request: &Request<'a, '_>,
    target: Format,
    context: &Context<'a>,
    policy: Policy,
    output: &mut [u8],
    scratch: &mut [u8],
    tokens: &mut [Token],
) -> Result<EncodeReport<'a>, Error> {
    let mut e = Encoder {
        source: request.source,
        target,
        records: request.records,
        context,
        policy,
        w: Writer::new(output),
        scratch,
        tokens,
        report: EncodeReport::default(),
        next_item: 0,
    };
    e.preflight()?;
    e.w.byte(b'{');
    let mut comma = false;
    let model = match request.model {
        Presence::Missing => context
            .model
            .map(|m| Presence::Value(Text::Utf8(m)))
            .unwrap_or(Presence::Missing),
        v => v,
    };
    if target.protocol == Protocol::GeminiGenerateContent {
        e.report.transport_model = model;
    } else {
        if !matches!(model, Presence::Value(_)) {
            return Err(Error::ContextRequired { field: "model" });
        }
        ptext(&mut e.w, &mut comma, "model", model);
    }
    let mut system = None;
    let mut system_count = 0;
    for (i, r) in request.records.iter().enumerate() {
        if let Record::Message(m) = r {
            if m.role == Role::System
                && (target.protocol != Protocol::OpenAiResponses || m.system_instruction)
            {
                system = Some((i, *m));
                system_count += 1;
            }
        }
    }
    if target.protocol == Protocol::OpenAiCompletions {
        let mut count = 0;
        let mut content = None;
        for r in request.records.iter() {
            match r {
                Record::Message(m) => {
                    count += 1;
                    if m.role != Role::User {
                        return Err(Error::Unsupported {
                            feature: "role-bearing history to completion prompt",
                        });
                    }
                }
                Record::Block(Block {
                    content: Content::Text(t),
                    ..
                }) => {
                    if content.is_some() {
                        return Err(Error::Unsupported {
                            feature: "multiple content blocks to completion prompt",
                        });
                    }
                    content = Some(*t);
                }
                Record::Block(_) => {
                    return Err(Error::Unsupported {
                        feature: "non-text completion prompt",
                    })
                }
                _ => {}
            }
        }
        if count != 1 {
            return Err(Error::Unsupported {
                feature: "multiple messages to completion prompt",
            });
        }
        key(&mut e.w, &mut comma, "prompt");
        write_text(
            &mut e.w,
            content.ok_or_else(|| invalid("prompt", "text missing"))?,
        );
    } else {
        if target.protocol != Protocol::OpenAiChat && system_count > 1 {
            return Err(Error::Unsupported {
                feature: "multiple ordered system messages in target",
            });
        }
        if target.protocol != Protocol::OpenAiChat {
            if let Some((i, m)) = system {
                let first_message = request
                    .records
                    .iter()
                    .position(|r| matches!(r, Record::Message(_)))
                    .unwrap_or(i);
                if i != first_message {
                    return Err(Error::Unsupported {
                        feature: "non-leading system message",
                    });
                }
                key(
                    &mut e.w,
                    &mut comma,
                    match target.protocol {
                        Protocol::AnthropicMessages => "system",
                        Protocol::GeminiGenerateContent => "systemInstruction",
                        Protocol::OpenAiResponses => "instructions",
                        _ => "system_instruction",
                    },
                );
                if target.protocol == Protocol::AnthropicMessages {
                    e.blocks(i + 1, m.end, Role::System, false)?;
                } else if target.protocol == Protocol::GeminiGenerateContent {
                    e.w.raw(b"{\"parts\":");
                    e.blocks(i + 1, m.end, Role::System, false)?;
                    e.w.byte(b'}');
                } else {
                    let mut one = None;
                    for record in &request.records[i + 1..m.end] {
                        if let Record::Block(b) = record {
                            match b.content {
                                Content::Text(t) if one.is_none() => one = Some(t),
                                _ => {
                                    return Err(Error::Unsupported {
                                        feature: "system instruction requires single text",
                                    })
                                }
                            }
                        }
                    }
                    write_text(
                        &mut e.w,
                        one.ok_or(Error::Unsupported {
                            feature: "empty system instruction array",
                        })?,
                    );
                }
            }
        }
        key(
            &mut e.w,
            &mut comma,
            match target.protocol {
                Protocol::OpenAiResponses | Protocol::GeminiInteractions => "input",
                Protocol::GeminiGenerateContent => "contents",
                _ => "messages",
            },
        );
        if matches!(
            target.protocol,
            Protocol::OpenAiResponses | Protocol::GeminiInteractions
        ) {
            e.items(0, request.records.len(), false)?;
        } else {
            e.w.byte(b'[');
            let mut mc = false;
            let mut at = 0;
            while at < request.records.len() {
                if let Record::Message(m) = request.records[at] {
                    if m.role != Role::System || target.protocol == Protocol::OpenAiChat {
                        let results = request.records[at + 1..m.end]
                            .iter()
                            .filter(|r| {
                                matches!(
                                    r,
                                    Record::Block(Block {
                                        content: Content::ToolResult { .. },
                                        ..
                                    })
                                )
                            })
                            .count();
                        if target.protocol == Protocol::OpenAiChat && results > 1 {
                            if request.records[at+1..m.end].iter().any(|r|matches!(r,Record::Block(Block{content, ..})if !matches!(content,Content::ToolResult{..}))){return Err(Error::Unsupported{feature:"mixed content and multiple tool results in Chat"});}
                            for bi in at + 1..m.end {
                                if let Record::Block(b) = request.records[bi] {
                                    if matches!(b.content, Content::ToolResult { .. }) {
                                        if mc {
                                            e.w.byte(b',');
                                        }
                                        mc = true;
                                        let mut single = m;
                                        single.end = b.end;
                                        single.extensions = Span::EMPTY;
                                        single.role = Role::Tool;
                                        e.chat_message(bi - 1, single, false)?;
                                    }
                                }
                            }
                        } else {
                            if mc {
                                e.w.byte(b',');
                            }
                            mc = true;
                            e.message(at, m, false)?;
                        }
                    }
                    at = m.end;
                } else {
                    at += 1;
                }
            }
            e.w.byte(b']');
        }
    }
    if request.source == target && system_count == 0 && request.instruction_presence.is_null() {
        let name = match target.protocol {
            Protocol::OpenAiResponses => "instructions",
            Protocol::AnthropicMessages => "system",
            Protocol::GeminiGenerateContent => "systemInstruction",
            Protocol::GeminiInteractions => "system_instruction",
            _ => {
                return Err(invalid(
                    "instruction presence",
                    "invalid instruction source",
                ))
            }
        };
        key(&mut e.w, &mut comma, name);
        e.w.raw(b"null");
    }
    e.tools(&mut comma, request.tools, request.functions)?;
    e.parameters(
        request.parameters,
        request.tool_choice,
        request.output_constraint,
        request.tools.is_missing() && !request.functions.is_missing(),
        &mut comma,
    )?;
    match request.context {
        Presence::Missing => {}
        Presence::Null => {
            if matches!(
                target.protocol,
                Protocol::OpenAiResponses | Protocol::GeminiInteractions
            ) {
                key(
                    &mut e.w,
                    &mut comma,
                    if target.protocol == Protocol::GeminiInteractions {
                        "previous_interaction_id"
                    } else {
                        "previous_response_id"
                    },
                );
                e.w.raw(b"null");
            } else if !policy.drop_context {
                return Err(Error::PolicyViolation {
                    feature: "explicit null context field",
                });
            } else {
                e.report.dropped_context += 1;
            }
        }
        Presence::Value(c) => {
            if c.source == target {
                key(
                    &mut e.w,
                    &mut comma,
                    if target.protocol == Protocol::GeminiInteractions {
                        "previous_interaction_id"
                    } else {
                        "previous_response_id"
                    },
                );
                write_text(&mut e.w, c.id);
            } else if policy.drop_context {
                e.report.dropped_context += 1;
            } else {
                return Err(Error::ContextRequired {
                    field: "resolved source conversation history",
                });
            }
        }
    }
    if target.protocol != Protocol::GeminiGenerateContent {
        match request.output_constraint {
            Presence::Missing => {}
            Presence::Null => {
                if request.source != target {
                    return Err(Error::Unsupported {
                        feature: "null structured output constraint",
                    });
                }
                if target.protocol == Protocol::OpenAiResponses {
                    key(&mut e.w, &mut comma, "text");
                    e.w.raw(b"{\"format\":null}");
                } else {
                    key(&mut e.w, &mut comma, "response_format");
                    e.w.raw(b"null");
                }
            }
            Presence::Value(v) => {
                let openai = matches!(
                    target.protocol,
                    Protocol::OpenAiChat | Protocol::OpenAiResponses
                );
                let source_openai = matches!(
                    v.source.protocol,
                    Protocol::OpenAiChat | Protocol::OpenAiResponses
                );
                if openai && source_openai && v.mode != OutputMode::Opaque {
                    let responses = target.protocol == Protocol::OpenAiResponses;
                    key(
                        &mut e.w,
                        &mut comma,
                        if responses { "text" } else { "response_format" },
                    );
                    if responses {
                        e.w.raw(b"{\"format\":");
                    }
                    e.w.raw(b"{\"type\":");
                    e.w.string(match v.mode {
                        OutputMode::Text => b"text",
                        OutputMode::JsonObject => b"json_object",
                        _ => b"json_schema",
                    });
                    if v.mode == OutputMode::JsonSchema {
                        if !responses {
                            e.w.raw(b",\"json_schema\":{");
                        }
                        let mut c = responses;
                        ptext(&mut e.w, &mut c, "name", v.name);
                        ptext(&mut e.w, &mut c, "description", v.description);
                        pbool(&mut e.w, &mut c, "strict", v.strict);
                        praw(&mut e.w, &mut c, "schema", v.schema);
                        if !responses {
                            e.w.byte(b'}');
                        }
                    }
                    e.w.byte(b'}');
                    if responses {
                        e.w.byte(b'}');
                    }
                } else if v.source == target {
                    key(&mut e.w, &mut comma, "response_format");
                    e.w.raw(v.value.raw());
                } else {
                    return Err(Error::Unsupported {
                        feature: "structured output schema dialect conversion",
                    });
                }
            }
        }
    }

    e.extras(request.extensions, &mut comma)?;
    e.w.byte(b'}');
    e.report.written = e.w.finish()?;
    Ok(e.report)
}

/// Encode a complete response, preserving every supported candidate and usage value.
pub fn encode_response<'a>(
    response: &Response<'a, '_>,
    target: Format,
    context: &Context<'a>,
    policy: Policy,
    output: &mut [u8],
    scratch: &mut [u8],
    tokens: &mut [Token],
) -> Result<EncodeReport<'a>, Error> {
    let mut e = Encoder {
        source: response.source,
        target,
        records: response.records,
        context,
        policy,
        w: Writer::new(output),
        scratch,
        tokens,
        report: EncodeReport::default(),
        next_item: 0,
    };
    e.preflight()?;
    let count = response
        .records
        .iter()
        .filter(|r| matches!(r, Record::Choice(_)))
        .count();
    if count > 1
        && !matches!(
            target.protocol,
            Protocol::OpenAiChat | Protocol::OpenAiCompletions | Protocol::GeminiGenerateContent
        )
    {
        return Err(Error::Unsupported {
            feature: "multiple candidates in single-output protocol",
        });
    }
    if !matches!(
        target.protocol,
        Protocol::OpenAiChat | Protocol::OpenAiCompletions | Protocol::GeminiGenerateContent
    ) && response
        .records
        .iter()
        .any(|r| matches!(r,Record::Choice(c)if c.index!=0))
    {
        return Err(Error::Unsupported {
            feature: "nonzero candidate index in single-output protocol",
        });
    }
    let id = match response.id {
        Presence::Missing => context
            .response_id
            .map(|s| Presence::Value(Text::Utf8(s)))
            .unwrap_or(Presence::Missing),
        v => v,
    };
    let model = match response.model {
        Presence::Missing => context
            .model
            .map(|s| Presence::Value(Text::Utf8(s)))
            .unwrap_or(Presence::Missing),
        v => v,
    };
    if !matches!(id, Presence::Value(_)) && target.protocol != Protocol::GeminiGenerateContent {
        return Err(Error::ContextRequired {
            field: "response_id",
        });
    }
    if !matches!(model, Presence::Value(_)) && target.protocol != Protocol::GeminiGenerateContent {
        return Err(Error::ContextRequired { field: "model" });
    }
    e.w.byte(b'{');
    let mut comma = false;
    ptext(
        &mut e.w,
        &mut comma,
        if target.protocol == Protocol::GeminiGenerateContent {
            "responseId"
        } else {
            "id"
        },
        id,
    );
    ptext(
        &mut e.w,
        &mut comma,
        if target.protocol == Protocol::GeminiGenerateContent {
            "modelVersion"
        } else {
            "model"
        },
        model,
    );
    match target.protocol {
        Protocol::OpenAiChat | Protocol::OpenAiCompletions | Protocol::OpenAiResponses => {
            key(&mut e.w, &mut comma, "object");
            e.w.string(match target.protocol {
                Protocol::OpenAiChat => b"chat.completion",
                Protocol::OpenAiCompletions => b"text_completion",
                _ => b"response",
            });
            let created = match response.created {
                Presence::Missing => context
                    .created
                    .map(|n| Presence::Value(Number::Unsigned(n)))
                    .ok_or(Error::ContextRequired {
                        field: "created Unix timestamp",
                    })?,
                v => v,
            };
            pnumber(
                &mut e.w,
                &mut comma,
                if target.protocol == Protocol::OpenAiResponses {
                    "created_at"
                } else {
                    "created"
                },
                created,
            )?;
            e.metadata_loss(!response.created_iso.is_missing(), "ISO creation timestamp")?;
            e.metadata_loss(!response.updated_iso.is_missing(), "ISO update timestamp")?;
        }
        Protocol::GeminiInteractions => {
            let created = match response.created_iso {
                Presence::Missing => context
                    .created_iso
                    .map(|s| Presence::Value(Text::Utf8(s)))
                    .ok_or(Error::ContextRequired {
                        field: "created ISO timestamp",
                    })?,
                v => v,
            };
            let updated = match response.updated_iso {
                Presence::Missing => context
                    .updated_iso
                    .map(|s| Presence::Value(Text::Utf8(s)))
                    .ok_or(Error::ContextRequired {
                        field: "updated ISO timestamp",
                    })?,
                v => v,
            };
            ptext(&mut e.w, &mut comma, "created", created);
            ptext(&mut e.w, &mut comma, "updated", updated);
            e.metadata_loss(!response.created.is_missing(), "Unix creation timestamp")?;
        }
        _ => {
            e.metadata_loss(!response.created.is_missing(), "creation timestamp")?;
            e.metadata_loss(!response.created_iso.is_missing(), "ISO creation timestamp")?;
            e.metadata_loss(!response.updated_iso.is_missing(), "ISO update timestamp")?;
        }
    }
    match target.protocol {
        Protocol::OpenAiChat | Protocol::OpenAiCompletions | Protocol::GeminiGenerateContent => {
            key(
                &mut e.w,
                &mut comma,
                if target.protocol == Protocol::GeminiGenerateContent {
                    "candidates"
                } else {
                    "choices"
                },
            );
            e.w.byte(b'[');
            let mut cc = false;
            for (at, r) in response.records.iter().copied().enumerate() {
                if let Record::Choice(choice) = r {
                    if cc {
                        e.w.byte(b',');
                    }
                    cc = true;
                    e.w.byte(b'{');
                    let mut c = false;
                    key(&mut e.w, &mut c, "index");
                    uint(&mut e.w, choice.index);
                    if target.protocol == Protocol::OpenAiCompletions {
                        let mut t = None;
                        for item in &response.records[at + 1..choice.end] {
                            if let Record::Block(b) = item {
                                match b.content {
                                    Content::Text(s) if t.is_none() => t = Some(s),
                                    _ => {
                                        return Err(Error::Unsupported {
                                            feature: "multi-block or non-text completion response",
                                        })
                                    }
                                }
                            }
                        }
                        key(&mut e.w, &mut c, "text");
                        write_text(
                            &mut e.w,
                            t.ok_or(Error::Unsupported {
                                feature: "empty completion choice",
                            })?,
                        );
                    } else {
                        let mut m = None;
                        let mut messages = 0;
                        for (i, r) in response
                            .records
                            .iter()
                            .enumerate()
                            .take(choice.end)
                            .skip(at + 1)
                        {
                            if let Record::Message(message) = r {
                                if message.role != Role::Assistant {
                                    return Err(Error::Unsupported {
                                        feature: "non-assistant response output",
                                    });
                                }
                                messages += 1;
                                m = Some((i, *message));
                            }
                        }
                        if messages > 1 {
                            m = Some((
                                at,
                                Message {
                                    role: Role::Assistant,
                                    legacy_function: false,
                                    system_instruction: false,
                                    tool_calls: Presence::Missing,
                                    function_call: Presence::Missing,
                                    refusal: Presence::Missing,
                                    reasoning: Presence::Missing,
                                    name: Presence::Missing,
                                    shape: ContentShape::Array,
                                    end: choice.end,
                                    extensions: Span::EMPTY,
                                    item_id: Presence::Missing,
                                    item_status: Presence::Missing,
                                },
                            ));
                        }
                        if let Some((i, m)) = m {
                            key(
                                &mut e.w,
                                &mut c,
                                if target.protocol == Protocol::GeminiGenerateContent {
                                    "content"
                                } else {
                                    "message"
                                },
                            );
                            e.message(i, m, true)?;
                        }
                    }
                    write_finish(&mut e, choice, &mut c)?;
                    e.extras(choice.extensions, &mut c)?;
                    e.w.byte(b'}');
                }
            }
            e.w.byte(b']');
        }
        Protocol::AnthropicMessages => {
            key(&mut e.w, &mut comma, "type");
            e.w.string(b"message");
            key(&mut e.w, &mut comma, "role");
            e.w.string(b"assistant");
            key(&mut e.w, &mut comma, "content");
            e.blocks(0, response.records.len(), Role::Assistant, true)?;
            if let Some(Record::Choice(c)) = response
                .records
                .iter()
                .find(|r| matches!(r, Record::Choice(_)))
            {
                write_finish(&mut e, *c, &mut comma)?;
            }
        }
        Protocol::OpenAiResponses | Protocol::GeminiInteractions => {
            key(&mut e.w, &mut comma, "status");
            let status = response
                .records
                .iter()
                .find_map(|r| {
                    if let Record::Choice(c) = r {
                        Some(c.finish)
                    } else {
                        None
                    }
                })
                .unwrap_or(response.status);
            e.w.string(status_name(status, target.protocol)?);
            key(
                &mut e.w,
                &mut comma,
                if target.protocol == Protocol::GeminiInteractions {
                    "steps"
                } else {
                    "output"
                },
            );
            e.items(0, response.records.len(), true)?;
        }
    }
    if target.protocol == Protocol::AnthropicMessages {
        ptext(
            &mut e.w,
            &mut comma,
            "stop_sequence",
            response.stop_sequence,
        );
    } else if matches!(response.stop_sequence, Presence::Value(_)) {
        return Err(Error::Unsupported {
            feature: "matched Anthropic stop sequence in target",
        });
    }
    if target.protocol == Protocol::OpenAiResponses {
        let finish = response
            .records
            .iter()
            .find_map(|r| {
                if let Record::Choice(c) = r {
                    Some(c.finish)
                } else {
                    None
                }
            })
            .unwrap_or(response.status);
        if response.source == target {
            praw(
                &mut e.w,
                &mut comma,
                "incomplete_details",
                response.incomplete_details,
            );
        } else if matches!(finish, Finish::Length | Finish::Safety) {
            key(&mut e.w, &mut comma, "incomplete_details");
            e.w.raw(b"{\"reason\":");
            e.w.string(if finish == Finish::Length {
                b"max_output_tokens"
            } else {
                b"content_filter"
            });
            e.w.byte(b'}');
        }
    }
    write_usage(&mut e, response.usage, &mut comma)?;
    if !response.error.is_missing() {
        if matches!(
            target.protocol,
            Protocol::OpenAiResponses | Protocol::GeminiInteractions
        ) {
            praw(&mut e.w, &mut comma, "error", response.error);
        } else if !response.error.is_null() {
            return Err(Error::Unsupported {
                feature: "response error in target success envelope",
            });
        }
    }
    e.extras(response.extensions, &mut comma)?;
    e.w.byte(b'}');
    e.report.written = e.w.finish()?;
    Ok(e.report)
}

fn status_name(finish: Finish, target: Protocol) -> Result<&'static [u8], Error> {
    Ok(match finish {
        Finish::Stop | Finish::Null | Finish::Missing => b"completed",
        Finish::ToolCalls => {
            if target == Protocol::OpenAiResponses {
                b"completed"
            } else {
                b"requires_action"
            }
        }
        Finish::Length => b"incomplete",
        Finish::Error => b"failed",
        Finish::Cancelled => b"cancelled",
        Finish::InProgress => b"in_progress",
        Finish::Safety if target == Protocol::OpenAiResponses => b"incomplete",
        Finish::Safety | Finish::Refusal => {
            return Err(Error::Unsupported {
                feature: "safety finish requires target blocking detail",
            })
        }
        Finish::Other => {
            return Err(Error::Unsupported {
                feature: "unknown finish reason",
            })
        }
    })
}

fn write_finish(
    e: &mut Encoder<'_, '_, '_, '_, '_, '_>,
    choice: Choice<'_>,
    comma: &mut bool,
) -> Result<(), Error> {
    let p = e.target.protocol;
    let field = match p {
        Protocol::AnthropicMessages => "stop_reason",
        Protocol::GeminiGenerateContent => "finishReason",
        _ => "finish_reason",
    };
    if e.source == e.target && !choice.raw_finish.is_missing() {
        ptext(&mut e.w, comma, field, choice.raw_finish);
        return Ok(());
    }
    if choice.finish == Finish::Missing {
        return Ok(());
    }
    key(&mut e.w, comma, field);
    if choice.finish == Finish::Null {
        e.w.raw(b"null");
        return Ok(());
    }
    let v = match p {
        Protocol::AnthropicMessages => match choice.finish {
            Finish::Stop => "end_turn",
            Finish::Length => "max_tokens",
            Finish::ToolCalls => "tool_use",
            Finish::Refusal => "refusal",
            _ => {
                return Err(Error::Unsupported {
                    feature: "finish reason in Anthropic",
                })
            }
        },
        Protocol::GeminiGenerateContent => match choice.finish {
            Finish::Stop => "STOP",
            Finish::Length => "MAX_TOKENS",
            Finish::Safety => "SAFETY",
            Finish::ToolCalls => "STOP",
            _ => {
                return Err(Error::Unsupported {
                    feature: "finish reason in Gemini",
                })
            }
        },
        _ => match choice.finish {
            Finish::Stop => "stop",
            Finish::Length => "length",
            Finish::ToolCalls => "tool_calls",
            Finish::Safety => "content_filter",
            Finish::Refusal => "stop",
            _ => {
                return Err(Error::Unsupported {
                    feature: "finish reason in Chat",
                })
            }
        },
    };
    e.w.string(v.as_bytes());
    Ok(())
}

fn write_usage(
    e: &mut Encoder<'_, '_, '_, '_, '_, '_>,
    usage: Usage<'_>,
    comma: &mut bool,
) -> Result<(), Error> {
    if !usage.present {
        return Ok(());
    }
    let p = e.target.protocol;
    key(
        &mut e.w,
        comma,
        if p == Protocol::GeminiGenerateContent {
            "usageMetadata"
        } else {
            "usage"
        },
    );
    if usage.is_null {
        e.w.raw(b"null");
        return Ok(());
    }
    e.w.byte(b'{');
    let mut c = false;
    let (input, output, total) = match p {
        Protocol::OpenAiChat | Protocol::OpenAiCompletions => {
            ("prompt_tokens", "completion_tokens", "total_tokens")
        }
        Protocol::GeminiGenerateContent => (
            "promptTokenCount",
            "candidatesTokenCount",
            "totalTokenCount",
        ),
        Protocol::GeminiInteractions => {
            ("total_input_tokens", "total_output_tokens", "total_tokens")
        }
        _ => ("input_tokens", "output_tokens", "total_tokens"),
    };
    let mut input_count = usage.input_tokens;
    if p == Protocol::AnthropicMessages
        && (!usage.cached_tokens.is_missing() || !usage.cache_write_tokens.is_missing())
    {
        let n = usage
            .input_tokens
            .value()
            .and_then(Number::as_u64)
            .ok_or(Error::Unsupported {
                feature: "input usage arithmetic",
            })?;
        let cache = match usage.cached_tokens {
            Presence::Missing => 0,
            Presence::Value(n) => n.as_u64().ok_or(Error::Unsupported {
                feature: "cache usage arithmetic",
            })?,
            Presence::Null => {
                return Err(invalid(
                    "cached_tokens",
                    "null cannot participate in usage arithmetic",
                ))
            }
        };
        let write = match usage.cache_write_tokens {
            Presence::Missing => 0,
            Presence::Value(n) => n.as_u64().ok_or(Error::Unsupported {
                feature: "cache write usage arithmetic",
            })?,
            Presence::Null => {
                return Err(invalid(
                    "cache_write_tokens",
                    "null cannot participate in usage arithmetic",
                ))
            }
        };
        input_count = Presence::Value(Number::Unsigned(
            n.checked_sub(cache)
                .and_then(|n| n.checked_sub(write))
                .ok_or_else(|| invalid("usage", "cache counts exceed inclusive input count"))?,
        ));
    }
    pnumber(&mut e.w, &mut c, input, input_count)?;
    let mut output_count = usage.output_tokens;
    if matches!(
        p,
        Protocol::GeminiGenerateContent | Protocol::GeminiInteractions
    ) && !usage.reasoning_tokens.is_missing()
    {
        let inclusive =
            usage
                .output_tokens
                .value()
                .and_then(Number::as_u64)
                .ok_or(Error::Unsupported {
                    feature: "generated usage arithmetic",
                })?;
        let thoughts = usage
            .reasoning_tokens
            .value()
            .and_then(Number::as_u64)
            .ok_or(Error::Unsupported {
                feature: "thought usage arithmetic",
            })?;
        output_count = Presence::Value(Number::Unsigned(
            inclusive
                .checked_sub(thoughts)
                .ok_or_else(|| invalid("usage", "thoughts exceed inclusive generated count"))?,
        ));
    }
    pnumber(&mut e.w, &mut c, output, output_count)?;
    if p == Protocol::AnthropicMessages && !usage.total_tokens.is_missing() {
        let exact = match (usage.input_tokens, usage.output_tokens, usage.total_tokens) {
            (Presence::Value(i), Presence::Value(o), Presence::Value(t)) => i
                .as_u64()
                .zip(o.as_u64())
                .and_then(|(i, o)| i.checked_add(o))
                .zip(t.as_u64())
                .map(|(sum, t)| sum == t)
                .unwrap_or(false),
            _ => false,
        };
        if !exact {
            return Err(Error::Unsupported {
                feature: "non-reconstructible total usage count in Anthropic",
            });
        }
    } else {
        let total_count = if usage.total_tokens.is_missing() {
            match (
                usage.input_tokens.value().and_then(Number::as_u64),
                usage.output_tokens.value().and_then(Number::as_u64),
            ) {
                (Some(i), Some(o)) => Presence::Value(Number::Unsigned(i.checked_add(o).ok_or(
                    Error::Unsupported {
                        feature: "total usage exceeds u64 arithmetic",
                    },
                )?)),
                _ => Presence::Missing,
            }
        } else {
            usage.total_tokens
        };
        pnumber(&mut e.w, &mut c, total, total_count)?;
    }
    if !usage.cached_tokens.is_missing() {
        match p {
            Protocol::OpenAiChat | Protocol::OpenAiCompletions | Protocol::OpenAiResponses => {
                key(
                    &mut e.w,
                    &mut c,
                    if p == Protocol::OpenAiResponses {
                        "input_tokens_details"
                    } else {
                        "prompt_tokens_details"
                    },
                );
                e.w.byte(b'{');
                let mut n = false;
                pnumber(&mut e.w, &mut n, "cached_tokens", usage.cached_tokens)?;
                e.w.byte(b'}');
            }
            _ => pnumber(
                &mut e.w,
                &mut c,
                match p {
                    Protocol::GeminiGenerateContent => "cachedContentTokenCount",
                    Protocol::GeminiInteractions => "total_cached_tokens",
                    _ => "cache_read_input_tokens",
                },
                usage.cached_tokens,
            )?,
        }
    }
    if !usage.reasoning_tokens.is_missing() {
        match p {
            Protocol::OpenAiChat | Protocol::OpenAiCompletions | Protocol::OpenAiResponses => {
                key(
                    &mut e.w,
                    &mut c,
                    if p == Protocol::OpenAiResponses {
                        "output_tokens_details"
                    } else {
                        "completion_tokens_details"
                    },
                );
                e.w.byte(b'{');
                let mut n = false;
                pnumber(&mut e.w, &mut n, "reasoning_tokens", usage.reasoning_tokens)?;
                e.w.byte(b'}');
            }
            Protocol::AnthropicMessages => {
                return Err(Error::Unsupported {
                    feature: "reasoning usage count in Anthropic",
                })
            }
            _ => pnumber(
                &mut e.w,
                &mut c,
                if p == Protocol::GeminiGenerateContent {
                    "thoughtsTokenCount"
                } else {
                    "total_thought_tokens"
                },
                usage.reasoning_tokens,
            )?,
        }
    }
    if !usage.cache_write_tokens.is_missing() {
        if p != Protocol::AnthropicMessages {
            return Err(Error::Unsupported {
                feature: "cache creation usage count in target",
            });
        }
        pnumber(
            &mut e.w,
            &mut c,
            "cache_creation_input_tokens",
            usage.cache_write_tokens,
        )?;
    }
    e.extras(usage.extensions, &mut c)?;
    e.w.byte(b'}');
    Ok(())
}
