use super::{invalid, validate_format};
use crate::{
    Error, Format, Operation, Protocol,
    ir::*,
    json::{Kind, RawValue, Value},
};

type V<'a, 't> = Value<'a, 't>;

fn required<'a, 't>(v: V<'a, 't>, key: &'static str) -> Result<V<'a, 't>, Error> {
    v.get(key)
        .ok_or_else(|| invalid(key, "required field missing"))
}

pub(crate) fn object(v: V<'_, '_>, field: &'static str) -> Result<(), Error> {
    if v.kind() == Kind::Object {
        Ok(())
    } else {
        Err(invalid(field, "expected object"))
    }
}

fn array(v: V<'_, '_>, field: &'static str) -> Result<(), Error> {
    if v.kind() == Kind::Array {
        Ok(())
    } else {
        Err(invalid(field, "expected array"))
    }
}

fn text<'a>(v: V<'a, '_>, field: &'static str) -> Result<Text<'a>, Error> {
    v.as_string()
        .map(Text::Json)
        .ok_or_else(|| invalid(field, "expected string"))
}

fn req_text<'a>(v: V<'a, '_>, field: &'static str) -> Result<Text<'a>, Error> {
    text(required(v, field)?, field)
}

fn presence<'a, 't>(v: V<'a, 't>, key: &str) -> Presence<V<'a, 't>> {
    match v.get(key) {
        None => Presence::Missing,
        Some(v) if v.is_null() => Presence::Null,
        Some(v) => Presence::Value(v),
    }
}

fn raw_presence<'a>(v: V<'a, '_>, key: &str) -> Presence<RawValue<'a>> {
    match presence(v, key) {
        Presence::Missing => Presence::Missing,
        Presence::Null => Presence::Null,
        Presence::Value(v) => Presence::Value(v.raw_value()),
    }
}

fn marker(v: Option<V<'_, '_>>) -> Presence<()> {
    match v {
        None => Presence::Missing,
        Some(v) if v.is_null() => Presence::Null,
        _ => Presence::Value(()),
    }
}

fn ptext<'a>(v: V<'a, '_>, key: &'static str) -> Result<Presence<Text<'a>>, Error> {
    Ok(match presence(v, key) {
        Presence::Missing => Presence::Missing,
        Presence::Null => Presence::Null,
        Presence::Value(v) => Presence::Value(text(v, key)?),
    })
}

fn pbool(v: V<'_, '_>, key: &'static str) -> Result<Presence<bool>, Error> {
    Ok(match presence(v, key) {
        Presence::Missing => Presence::Missing,
        Presence::Null => Presence::Null,
        Presence::Value(v) => Presence::Value(
            v.as_bool()
                .ok_or_else(|| invalid(key, "expected boolean"))?,
        ),
    })
}

fn pnumber<'a, 't>(v: V<'a, 't>, key: &'static str) -> Result<Presence<Number<'a>>, Error> {
    Ok(match presence(v, key) {
        Presence::Missing => Presence::Missing,
        Presence::Null => Presence::Null,
        Presence::Value(v) => {
            if v.kind() != Kind::Number || !v.raw().iter().all(u8::is_ascii_digit) {
                return Err(invalid(key, "expected nonnegative integer"));
            }
            Presence::Value(Number::Json(v.raw_value()))
        }
    })
}

fn shape(value: Option<V<'_, '_>>) -> ContentShape {
    match value {
        None => ContentShape::Missing,
        Some(v) if v.is_null() => ContentShape::Null,
        Some(v) if v.kind() == Kind::String => ContentShape::String,
        _ => ContentShape::Array,
    }
}

fn role(v: V<'_, '_>) -> Result<Role, Error> {
    let v = req_text(v, "role")?;
    if v.eq_str("system") {
        Ok(Role::System)
    } else if v.eq_str("developer") {
        Ok(Role::Developer)
    } else if v.eq_str("user") {
        Ok(Role::User)
    } else if v.eq_str("assistant") || v.eq_str("model") {
        Ok(Role::Assistant)
    } else if v.eq_str("tool") || v.eq_str("function") {
        Ok(Role::Tool)
    } else {
        Err(invalid("role", "unknown message role"))
    }
}

struct Arena<'s, 'a> {
    records: &'s mut [Record<'a>],
    used: usize,
    source: Format,
    count_only: bool,
}

impl<'s, 'a> Arena<'s, 'a> {
    fn push(&mut self, record: Record<'a>) -> Result<usize, Error> {
        let index = self.used;
        self.used = self
            .used
            .checked_add(1)
            .ok_or(Error::IrTooSmall { needed: usize::MAX })?;
        if !self.count_only {
            let target = self
                .records
                .get_mut(index)
                .ok_or(Error::IrTooSmall { needed: self.used })?;
            *target = record;
        }
        Ok(index)
    }
    fn set(&mut self, index: usize, record: Record<'a>) {
        if let Some(target) = self.records.get_mut(index) {
            *target = record;
        }
    }
    fn extras(
        &mut self,
        v: V<'a, '_>,
        known: &[&str],
        location: &'static str,
        owner: usize,
    ) -> Result<Span, Error> {
        let start = self.used;
        for (key, value) in v.object() {
            if !known.iter().any(|name| key.eq_str(name)) {
                self.push(Record::Extension(Extension {
                    source: self.source,
                    key,
                    value: value.raw_value(),
                    location,
                    owner,
                }))?;
            }
        }
        Ok(Span {
            start,
            end: self.used,
        })
    }
    fn block(
        &mut self,
        content: Content<'a>,
        raw: Option<V<'a, '_>>,
        known: &[&str],
        item_id: Presence<Text<'a>>,
    ) -> Result<(), Error> {
        let owner = self.push(Record::Empty)?;
        let extensions = if let Some(raw) = raw {
            self.extras(raw, known, "content", owner)?
        } else {
            Span::EMPTY
        };
        self.set(
            owner,
            Record::Block(Block {
                content,
                extensions,
                end: self.used,
                item_id,
                item_status: match raw {
                    Some(v) => ptext(v, "status")?,
                    None => Presence::Missing,
                },
            }),
        );
        Ok(())
    }
    fn message_start(&mut self) -> Result<usize, Error> {
        self.push(Record::Empty)
    }
    #[allow(clippy::too_many_arguments)]
    fn message_end(
        &mut self,
        owner: usize,
        role: Role,
        name: Presence<Text<'a>>,
        shape: ContentShape,
        raw: Option<V<'a, '_>>,
        known: &[&str],
        item_id: Presence<Text<'a>>,
    ) -> Result<(), Error> {
        let extensions = if let Some(raw) = raw {
            self.extras(raw, known, "message", owner)?
        } else {
            Span::EMPTY
        };
        self.set(
            owner,
            Record::Message(Message {
                role,
                system_instruction: false,
                legacy_function: raw
                    .and_then(|v| v.get("role"))
                    .and_then(|v| v.as_string())
                    .map(|s| s.eq_str("function"))
                    .unwrap_or(false),
                tool_calls: marker(raw.and_then(|v| v.get("tool_calls"))),
                function_call: marker(raw.and_then(|v| v.get("function_call"))),
                refusal: marker(raw.and_then(|v| v.get("refusal"))),
                reasoning: if matches!(self.source.profile, crate::Profile::Standard) {
                    Presence::Missing
                } else {
                    marker(raw.and_then(|v| v.get("reasoning_content")))
                },
                name,
                shape,
                end: self.used,
                extensions,
                item_id,
                item_status: match raw {
                    Some(v) => ptext(v, "status")?,
                    None => Presence::Missing,
                },
            }),
        );
        Ok(())
    }
    fn content(&mut self, v: V<'a, '_>, role: Role) -> Result<(), Error> {
        if v.is_null() {
            return Ok(());
        }
        if v.kind() == Kind::String {
            return self.block(
                Content::Text(text(v, "content")?),
                None,
                &[],
                Presence::Missing,
            );
        }
        array(v, "content")?;
        for block in v.array() {
            self.content_block(block, role)?;
        }
        Ok(())
    }
    fn content_block(&mut self, v: V<'a, '_>, role: Role) -> Result<(), Error> {
        object(v, "content block")?;
        if self.source.protocol == Protocol::GeminiGenerateContent {
            return self.gemini_part(v);
        }
        let kind = req_text(v, "type")?;
        if kind.eq_str("text") || kind.eq_str("input_text") || kind.eq_str("output_text") {
            if self.source.protocol == Protocol::AnthropicMessages
                && self.source.version == "2026-10-21"
                && role == Role::User
            {
                if let Some(c) = v.get("citations") {
                    if c.array().next().is_some() {
                        return Err(invalid(
                            "citations",
                            "user citations are disallowed in 2026-10-21",
                        ));
                    }
                }
            }
            self.block(
                Content::Text(req_text(v, "text")?),
                Some(v),
                &["type", "text"],
                Presence::Missing,
            )
        } else if kind.eq_str("refusal") {
            self.block(
                Content::Refusal(req_text(v, "refusal")?),
                Some(v),
                &["type", "refusal"],
                Presence::Missing,
            )
        } else if kind.eq_str("thinking") || kind.eq_str("redacted_thinking") {
            let signature = if kind.eq_str("redacted_thinking") {
                ptext(v, "data")?
            } else {
                ptext(v, "signature")?
            };
            self.block(
                Content::Reasoning {
                    kind: ReasoningKind::Thought,
                    text: ptext(v, "thinking")?,
                    signature,
                },
                Some(v),
                &["type", "thinking", "signature", "data"],
                Presence::Missing,
            )
        } else if kind.eq_str("tool_use") {
            if self.source.protocol == Protocol::AnthropicMessages && role != Role::Assistant {
                return Err(invalid("tool_use", "must belong to assistant"));
            }
            let arguments = required(v, "input")?;
            object(arguments, "input")?;
            self.block(
                Content::ToolCall {
                    id: Presence::Value(req_text(v, "id")?),
                    name: req_text(v, "name")?,
                    arguments: Data::Json(arguments.raw_value()),
                    legacy: false,
                },
                Some(v),
                &["type", "id", "name", "input"],
                Presence::Missing,
            )
        } else if kind.eq_str("tool_result") {
            if self.source.protocol == Protocol::AnthropicMessages && role != Role::User {
                return Err(invalid("tool_result", "must belong to user"));
            }
            let result = required(v, "content")?;
            self.block(
                Content::ToolResult {
                    id: Presence::Value(req_text(v, "tool_use_id")?),
                    name: Presence::Missing,
                    result: data(result),
                    is_error: pbool(v, "is_error")?,
                },
                Some(v),
                &["type", "tool_use_id", "content", "is_error"],
                Presence::Missing,
            )
        } else if kind.eq_str("image_url") {
            let image = required(v, "image_url")?;
            object(image, "image_url")?;
            let media = Media {
                mime_type: Presence::Missing,
                kind: MediaKind::Image,
                source: url_source(req_text(image, "url")?),
                detail: ptext(image, "detail")?,
                filename: Presence::Missing,
            };
            self.block(
                Content::Media(media),
                Some(v),
                &["type", "image_url"],
                Presence::Missing,
            )?;
            reject_extra(image, &["url", "detail"], "image_url extension")?;
            Ok(())
        } else if kind.eq_str("input_image") {
            let source = if let Some(url) = v.get("image_url") {
                url_source(text(url, "image_url")?)
            } else {
                MediaSource::FileId {
                    id: req_text(v, "file_id")?,
                    namespace: self.source,
                }
            };
            self.block(
                Content::Media(Media {
                    mime_type: Presence::Missing,
                    kind: MediaKind::Image,
                    source,
                    detail: ptext(v, "detail")?,
                    filename: Presence::Missing,
                }),
                Some(v),
                &["type", "image_url", "file_id", "detail"],
                Presence::Missing,
            )
        } else if kind.eq_str("image")
            || kind.eq_str("document")
            || kind.eq_str("audio")
            || kind.eq_str("video")
        {
            if self.source.protocol == Protocol::AnthropicMessages
                && self.source.version == "2026-10-21"
                && role == Role::Assistant
                && kind.eq_str("document")
            {
                return Err(invalid(
                    "document",
                    "assistant documents are disallowed in 2026-10-21",
                ));
            }
            let media_kind = if kind.eq_str("image") {
                MediaKind::Image
            } else if kind.eq_str("audio") {
                MediaKind::Audio
            } else if kind.eq_str("video") {
                MediaKind::Video
            } else {
                MediaKind::File
            };
            let source = if let Some(source) = v.get("source") {
                object(source, "source")?;
                reject_extra(
                    source,
                    &["type", "url", "data", "media_type", "file_id"],
                    "media source extension",
                )?;
                let ty = req_text(source, "type")?;
                if ty.eq_str("base64") {
                    MediaSource::Base64 {
                        data: req_text(source, "data")?,
                        mime_type: req_text(source, "media_type")?,
                    }
                } else if ty.eq_str("url") {
                    MediaSource::Url(req_text(source, "url")?)
                } else if ty.eq_str("file") {
                    MediaSource::FileId {
                        id: req_text(source, "file_id")?,
                        namespace: self.source,
                    }
                } else {
                    return self.block(
                        Content::Opaque {
                            source: self.source,
                            value: v.raw_value(),
                        },
                        None,
                        &[],
                        Presence::Missing,
                    );
                }
            } else if let Some(uri) = v.get("uri") {
                MediaSource::Url(text(uri, "uri")?)
            } else {
                MediaSource::Base64 {
                    data: req_text(v, "data")?,
                    mime_type: req_text(v, "mime_type")?,
                }
            };
            self.block(
                Content::Media(Media {
                    mime_type: Presence::Missing,
                    kind: media_kind,
                    source,
                    detail: ptext(v, "resolution")?,
                    filename: ptext(v, "filename")?,
                }),
                Some(v),
                &[
                    "type",
                    "source",
                    "uri",
                    "data",
                    "mime_type",
                    "resolution",
                    "filename",
                ],
                Presence::Missing,
            )
        } else if kind.eq_str("input_audio") {
            let audio = required(v, "input_audio")?;
            object(audio, "input_audio")?;
            reject_extra(audio, &["data", "format"], "input audio extension")?;
            let format = req_text(audio, "format")?;
            let mime = if format.eq_str("wav") {
                Text::Utf8("audio/wav")
            } else if format.eq_str("mp3") {
                Text::Utf8("audio/mpeg")
            } else {
                return Err(invalid("format", "unsupported input_audio format"));
            };
            self.block(
                Content::Media(Media {
                    mime_type: Presence::Missing,
                    kind: MediaKind::Audio,
                    source: MediaSource::Base64 {
                        data: req_text(audio, "data")?,
                        mime_type: mime,
                    },
                    detail: Presence::Missing,
                    filename: Presence::Missing,
                }),
                Some(v),
                &["type", "input_audio"],
                Presence::Missing,
            )
        } else if kind.eq_str("input_file") || kind.eq_str("file") {
            let file = v.get("file").unwrap_or(v);
            let source = if let Some(id) = file.get("file_id") {
                MediaSource::FileId {
                    id: text(id, "file_id")?,
                    namespace: self.source,
                }
            } else if let Some(url) = file.get("file_url") {
                MediaSource::Url(text(url, "file_url")?)
            } else if let Some(data) = file.get("file_data") {
                url_source(text(data, "file_data")?)
            } else {
                return self.block(
                    Content::Opaque {
                        source: self.source,
                        value: v.raw_value(),
                    },
                    None,
                    &[],
                    Presence::Missing,
                );
            };
            self.block(
                Content::Media(Media {
                    mime_type: Presence::Missing,
                    kind: MediaKind::File,
                    source,
                    detail: Presence::Missing,
                    filename: ptext(file, "filename")?,
                }),
                Some(v),
                &[
                    "type",
                    "file",
                    "file_id",
                    "file_url",
                    "file_data",
                    "filename",
                ],
                Presence::Missing,
            )
        } else {
            self.block(
                Content::Opaque {
                    source: self.source,
                    value: v.raw_value(),
                },
                None,
                &[],
                Presence::Missing,
            )
        }
    }

    fn gemini_part(&mut self, v: V<'a, '_>) -> Result<(), Error> {
        let alternatives = [
            "text",
            "functionCall",
            "functionResponse",
            "inlineData",
            "fileData",
            "executableCode",
            "codeExecutionResult",
        ];
        if alternatives
            .iter()
            .filter(|key| v.get(key).is_some())
            .count()
            > 1
        {
            return Err(invalid("part", "multiple content alternatives"));
        }
        if let Some(t) = v.get("text") {
            let reasoning = v.get("thought").and_then(|v| v.as_bool()).unwrap_or(false);
            let content = if reasoning {
                Content::Reasoning {
                    kind: ReasoningKind::Summary,
                    text: Presence::Value(text(t, "text")?),
                    signature: ptext(v, "thoughtSignature")?,
                }
            } else {
                Content::Text(text(t, "text")?)
            };
            self.block(
                content,
                Some(v),
                if reasoning {
                    &["text", "thought", "thoughtSignature"]
                } else {
                    &["text"]
                },
                Presence::Missing,
            )
        } else if let Some(call) = v.get("functionCall") {
            object(call, "functionCall")?;
            reject_extra(call, &["id", "name", "args"], "functionCall extension")?;
            let args = required(call, "args")?;
            object(args, "args")?;
            self.block(
                Content::ToolCall {
                    id: ptext(call, "id")?,
                    name: req_text(call, "name")?,
                    arguments: Data::Json(args.raw_value()),
                    legacy: false,
                },
                Some(v),
                &["functionCall"],
                Presence::Missing,
            )
        } else if let Some(result) = v.get("functionResponse") {
            object(result, "functionResponse")?;
            reject_extra(
                result,
                &["id", "name", "response"],
                "functionResponse extension",
            )?;
            let response = required(result, "response")?;
            object(response, "response")?;
            self.block(
                Content::ToolResult {
                    id: ptext(result, "id")?,
                    name: Presence::Value(req_text(result, "name")?),
                    result: Data::Json(response.raw_value()),
                    is_error: Presence::Missing,
                },
                Some(v),
                &["functionResponse"],
                Presence::Missing,
            )
        } else if let Some(inline) = v.get("inlineData") {
            object(inline, "inlineData")?;
            let mime = req_text(inline, "mimeType")?;
            reject_extra(inline, &["data", "mimeType"], "inlineData extension")?;
            self.block(
                Content::Media(Media {
                    mime_type: Presence::Missing,
                    kind: media_kind(mime),
                    source: MediaSource::Base64 {
                        data: req_text(inline, "data")?,
                        mime_type: mime,
                    },
                    detail: Presence::Missing,
                    filename: Presence::Missing,
                }),
                Some(v),
                &["inlineData"],
                Presence::Missing,
            )
        } else if let Some(file) = v.get("fileData") {
            object(file, "fileData")?;
            let mime = req_text(file, "mimeType")?;
            reject_extra(file, &["fileUri", "mimeType"], "fileData extension")?;
            self.block(
                Content::Media(Media {
                    mime_type: Presence::Value(mime),
                    kind: media_kind(mime),
                    source: MediaSource::Url(req_text(file, "fileUri")?),
                    detail: Presence::Missing,
                    filename: Presence::Missing,
                }),
                Some(v),
                &["fileData"],
                Presence::Missing,
            )
        } else {
            self.block(
                Content::Opaque {
                    source: self.source,
                    value: v.raw_value(),
                },
                None,
                &[],
                Presence::Missing,
            )
        }
    }

    fn chat_message(&mut self, v: V<'a, '_>) -> Result<(), Error> {
        object(v, "message")?;
        let r = role(v)?;
        if req_text(v, "role")?.eq_str("model") {
            return Err(invalid("role", "Chat has no model role"));
        }
        let start = self.message_start()?;
        if r == Role::Tool {
            let result = required(v, "content")?;
            let legacy = v
                .get("role")
                .and_then(|v| v.as_string())
                .map(|s| s.eq_str("function"))
                .unwrap_or(false);
            let id = ptext(v, "tool_call_id")?;
            let name = ptext(v, "name")?;
            if !legacy && id.is_missing() {
                return Err(invalid("tool_call_id", "required for tool message"));
            }
            if legacy && name.is_missing() {
                return Err(invalid("name", "required for function message"));
            }
            self.block(
                Content::ToolResult {
                    id,
                    name,
                    result: data(result),
                    is_error: Presence::Missing,
                },
                None,
                &[],
                Presence::Missing,
            )?;
        } else {
            if let Some(content) = v.get("content") {
                self.content(content, r)?;
            }
            if let Some(reasoning) = v
                .get("reasoning_content")
                .filter(|_| !matches!(self.source.profile, crate::Profile::Standard))
            {
                if !reasoning.is_null() {
                    self.block(
                        Content::Reasoning {
                            kind: ReasoningKind::Thought,
                            text: Presence::Value(text(reasoning, "reasoning_content")?),
                            signature: Presence::Missing,
                        },
                        None,
                        &[],
                        Presence::Missing,
                    )?;
                }
            }
            if let Some(refusal) = v.get("refusal") {
                if !refusal.is_null() {
                    self.block(
                        Content::Refusal(text(refusal, "refusal")?),
                        None,
                        &[],
                        Presence::Missing,
                    )?;
                }
            }
            if let Some(calls) = v.get("tool_calls") {
                if !calls.is_null() {
                    if r != Role::Assistant {
                        return Err(invalid("tool_calls", "must belong to assistant"));
                    }
                    array(calls, "tool_calls")?;
                    for call in calls.array() {
                        object(call, "tool call")?;
                        if !req_text(call, "type")?.eq_str("function") {
                            return Err(Error::Unsupported {
                                feature: "non-function Chat tool call",
                            });
                        }
                        reject_extra(
                            call,
                            &["id", "type", "function"],
                            "tool call wrapper extension",
                        )?;
                        self.chat_call(required(call, "function")?, ptext(call, "id")?, false)?;
                    }
                }
            }
            if let Some(call) = v.get("function_call") {
                if !call.is_null() {
                    if r != Role::Assistant {
                        return Err(invalid("function_call", "must belong to assistant"));
                    }
                    self.chat_call(call, Presence::Missing, true)?;
                }
            }
        }
        self.message_end(
            start,
            r,
            ptext(v, "name")?,
            shape(v.get("content")),
            Some(v),
            if matches!(self.source.profile, crate::Profile::Standard) {
                &[
                    "role",
                    "name",
                    "content",
                    "tool_call_id",
                    "tool_calls",
                    "function_call",
                    "refusal",
                ]
            } else {
                &[
                    "role",
                    "name",
                    "content",
                    "tool_call_id",
                    "tool_calls",
                    "function_call",
                    "refusal",
                    "reasoning_content",
                ]
            },
            Presence::Missing,
        )
    }

    fn chat_call(
        &mut self,
        v: V<'a, '_>,
        id: Presence<Text<'a>>,
        legacy: bool,
    ) -> Result<(), Error> {
        object(v, "function")?;
        self.block(
            Content::ToolCall {
                id,
                name: req_text(v, "name")?,
                arguments: Data::JsonText(req_text(v, "arguments")?),
                legacy,
            },
            Some(v),
            &["name", "arguments"],
            Presence::Missing,
        )
    }

    fn simple_message(&mut self, v: V<'a, '_>, google: bool) -> Result<(), Error> {
        object(v, "message")?;
        let r = if google && v.get("role").is_none() {
            Role::User
        } else {
            role(v)?
        };
        if let Some(raw) = v.get("role") {
            let name = text(raw, "role")?;
            if google && !name.eq_str("user") && !name.eq_str("model") {
                return Err(invalid(
                    "role",
                    "GenerateContent role must be user or model",
                ));
            }
            if self.source.protocol == Protocol::AnthropicMessages
                && !name.eq_str("user")
                && !name.eq_str("assistant")
            {
                return Err(invalid("role", "Anthropic role must be user or assistant"));
            }
        }
        if google && !matches!(r, Role::User | Role::Assistant) {
            return Err(invalid(
                "role",
                "GenerateContent role must be user or model",
            ));
        }
        if self.source.protocol == Protocol::AnthropicMessages
            && !matches!(r, Role::User | Role::Assistant)
        {
            return Err(invalid(
                "role",
                "Anthropic messages must be user or assistant",
            ));
        }
        let content = required(v, if google { "parts" } else { "content" })?;
        let start = self.message_start()?;
        self.content(content, r)?;
        self.message_end(
            start,
            r,
            Presence::Missing,
            shape(Some(content)),
            Some(v),
            if google {
                &["role", "parts"]
            } else {
                &["role", "content"]
            },
            Presence::Missing,
        )
    }

    fn response_item(&mut self, v: V<'a, '_>, interactions: bool) -> Result<(), Error> {
        object(v, "item")?;
        let ty = v
            .get("type")
            .map(|v| text(v, "type"))
            .transpose()?
            .unwrap_or(Text::Utf8("message"));
        if ty.eq_str("message") || ty.eq_str("user_input") || ty.eq_str("model_output") {
            let r = if ty.eq_str("user_input") {
                Role::User
            } else if ty.eq_str("model_output") {
                Role::Assistant
            } else {
                role(v)?
            };
            if !interactions
                && !matches!(
                    r,
                    Role::System | Role::Developer | Role::User | Role::Assistant
                )
            {
                return Err(invalid("role", "Responses message role is unsupported"));
            }
            if !interactions
                && v.get("role")
                    .and_then(|v| v.as_string())
                    .map(|s| s.eq_str("model"))
                    .unwrap_or(false)
            {
                return Err(invalid("role", "Responses has no model role"));
            }
            let content = required(v, "content")?;
            let start = self.message_start()?;
            self.content(content, r)?;
            self.message_end(
                start,
                r,
                Presence::Missing,
                shape(Some(content)),
                Some(v),
                &["type", "role", "content", "id", "status"],
                ptext(v, "id")?,
            )
        } else if ty.eq_str("function_call") {
            let args = required(v, "arguments")?;
            let arguments = if interactions {
                object(args, "arguments")?;
                Data::Json(args.raw_value())
            } else {
                Data::JsonText(text(args, "arguments")?)
            };
            let start = self.message_start()?;
            self.block(
                Content::ToolCall {
                    id: ptext(v, if interactions { "id" } else { "call_id" })?,
                    name: req_text(v, "name")?,
                    arguments,
                    legacy: false,
                },
                Some(v),
                &["type", "id", "call_id", "name", "arguments", "status"],
                if interactions {
                    Presence::Missing
                } else {
                    ptext(v, "id")?
                },
            )?;
            self.message_end(
                start,
                Role::Assistant,
                Presence::Missing,
                ContentShape::Missing,
                None,
                &[],
                Presence::Missing,
            )
        } else if ty.eq_str("function_call_output") || ty.eq_str("function_result") {
            let result = required(v, if interactions { "result" } else { "output" })?;
            let start = self.message_start()?;
            self.block(
                Content::ToolResult {
                    id: Presence::Value(req_text(v, "call_id")?),
                    name: ptext(v, "name")?,
                    result: data(result),
                    is_error: pbool(v, "is_error")?,
                },
                Some(v),
                &[
                    "type", "id", "call_id", "name", "result", "output", "is_error", "status",
                ],
                ptext(v, "id")?,
            )?;
            self.message_end(
                start,
                Role::Tool,
                Presence::Missing,
                ContentShape::Array,
                None,
                &[],
                Presence::Missing,
            )
        } else if ty.eq_str("reasoning") || ty.eq_str("thought") {
            let start = self.message_start()?;
            let mut had_text = false;
            let mut included_signature = false;
            if let Some(summary) = v.get("summary") {
                if summary.kind() == Kind::String {
                    self.block(
                        Content::Reasoning {
                            kind: ReasoningKind::Summary,
                            text: Presence::Value(text(summary, "summary")?),
                            signature: ptext(
                                v,
                                if interactions {
                                    "signature"
                                } else {
                                    "encrypted_content"
                                },
                            )?,
                        },
                        Some(v),
                        &[
                            "type",
                            "id",
                            "summary",
                            "signature",
                            "encrypted_content",
                            "status",
                        ],
                        ptext(v, "id")?,
                    )?;
                    had_text = true;
                    included_signature = true;
                } else {
                    array(summary, "summary")?;
                    for part in summary.array() {
                        let t = req_text(part, "text")?;
                        self.block(
                            Content::Reasoning {
                                kind: ReasoningKind::Summary,
                                text: Presence::Value(t),
                                signature: Presence::Missing,
                            },
                            Some(part),
                            &["type", "text"],
                            Presence::Missing,
                        )?;
                        had_text = true;
                    }
                }
            }
            if !had_text || !included_signature {
                self.block(
                    Content::Reasoning {
                        kind: ReasoningKind::Summary,
                        text: Presence::Missing,
                        signature: ptext(
                            v,
                            if interactions {
                                "signature"
                            } else {
                                "encrypted_content"
                            },
                        )?,
                    },
                    Some(v),
                    &[
                        "type",
                        "id",
                        "summary",
                        "signature",
                        "encrypted_content",
                        "status",
                    ],
                    ptext(v, "id")?,
                )?;
            }
            self.message_end(
                start,
                Role::Assistant,
                Presence::Missing,
                ContentShape::Array,
                None,
                &[],
                Presence::Missing,
            )
        } else {
            let start = self.message_start()?;
            self.block(
                Content::Opaque {
                    source: self.source,
                    value: v.raw_value(),
                },
                None,
                &[],
                Presence::Missing,
            )?;
            self.message_end(
                start,
                Role::Assistant,
                Presence::Missing,
                ContentShape::Array,
                None,
                &[],
                Presence::Missing,
            )
        }
    }

    fn tools(&mut self, root: V<'a, '_>) -> Result<(), Error> {
        if let Some(tools) = root.get("tools") {
            if !tools.is_null() {
                array(tools, "tools")?;
                for tool in tools.array() {
                    object(tool, "tool")?;
                    if self.source.protocol == Protocol::GeminiGenerateContent {
                        if let Some(decls) = tool.get("functionDeclarations") {
                            array(decls, "functionDeclarations")?;
                            for d in decls.array() {
                                self.tool(d, false)?;
                            }
                            reject_extra(
                                tool,
                                &["functionDeclarations"],
                                "Gemini tool wrapper extension",
                            )?;
                        } else {
                            return Err(Error::Unsupported {
                                feature: "Gemini built-in tool",
                            });
                        }
                    } else if self.source.protocol == Protocol::OpenAiChat {
                        if !req_text(tool, "type")?.eq_str("function") {
                            return Err(Error::Unsupported {
                                feature: "Chat built-in tool",
                            });
                        } else {
                            self.tool(required(tool, "function")?, false)?;
                            reject_extra(
                                tool,
                                &["type", "function"],
                                "Chat tool wrapper extension",
                            )?;
                        }
                    } else {
                        self.tool(tool, false)?;
                    }
                }
            }
        }
        if let Some(tools) = root.get("functions") {
            array(tools, "functions")?;
            for tool in tools.array() {
                self.tool(tool, true)?;
            }
        }
        Ok(())
    }

    fn tool(&mut self, v: V<'a, '_>, legacy: bool) -> Result<(), Error> {
        object(v, "tool")?;
        if matches!(
            self.source.protocol,
            Protocol::OpenAiResponses | Protocol::GeminiInteractions
        ) && v.get("type").is_none()
        {
            return Err(invalid("type", "function tool type required"));
        }
        if let Some(ty) = v.get("type") {
            if !text(ty, "type")?.eq_str("function") {
                return Err(Error::Unsupported {
                    feature: "built-in tool definition",
                });
            }
        }
        let params = if self.source.protocol == Protocol::AnthropicMessages {
            raw_presence(v, "input_schema")
        } else {
            raw_presence(v, "parameters")
        };
        if let Presence::Value(p) = params {
            if p.kind() != Kind::Object {
                return Err(invalid("parameters", "expected object"));
            }
        }
        let owner = self.push(Record::Empty)?;
        let extensions = self.extras(
            v,
            &[
                "type",
                "name",
                "description",
                "parameters",
                "input_schema",
                "strict",
            ],
            "tool",
            owner,
        )?;
        self.set(
            owner,
            Record::Tool(Tool {
                name: req_text(v, "name")?,
                description: ptext(v, "description")?,
                parameters: params,
                strict: pbool(v, "strict")?,
                extensions,
                legacy,
            }),
        );
        Ok(())
    }
}

fn reject_extra(v: V<'_, '_>, known: &[&str], feature: &'static str) -> Result<(), Error> {
    for (key, _) in v.object() {
        if !known.iter().any(|name| key.eq_str(name)) {
            return Err(Error::Unsupported { feature });
        }
    }
    Ok(())
}

fn url_source(text: Text<'_>) -> MediaSource<'_> {
    let is_data = match text {
        Text::Utf8(s) => s.starts_with("data:"),
        Text::Json(s) => s.bytes().take(5).eq(b"data:".iter().copied()),
    };
    if is_data {
        MediaSource::DataUrl(text)
    } else {
        MediaSource::Url(text)
    }
}

fn media_kind(mime: Text<'_>) -> MediaKind {
    match mime.as_str() {
        Some(s) if s.starts_with("image/") => MediaKind::Image,
        Some(s) if s.starts_with("audio/") => MediaKind::Audio,
        Some(s) if s.starts_with("video/") => MediaKind::Video,
        _ => MediaKind::File,
    }
}

fn data<'a>(v: V<'a, '_>) -> Data<'a> {
    if let Some(s) = v.as_string() {
        Data::Text(Text::Json(s))
    } else {
        Data::Json(v.raw_value())
    }
}

fn tool_choice<'a, 't>(source: Format, v: Option<V<'a, 't>>) -> Result<ToolChoice<'a>, Error> {
    let v = match v {
        None => return Ok(ToolChoice::Missing),
        Some(v) if v.is_null() => return Ok(ToolChoice::Null),
        Some(v) => v,
    };
    if let Some(s) = v.as_string() {
        return Ok(if s.eq_str("auto") || s.eq_str("AUTO") {
            ToolChoice::Auto
        } else if s.eq_str("none") || s.eq_str("NONE") {
            ToolChoice::None
        } else if s.eq_str("required") || s.eq_str("any") || s.eq_str("ANY") {
            ToolChoice::Required
        } else {
            ToolChoice::Opaque {
                source,
                value: v.raw_value(),
            }
        });
    }
    object(v, "tool_choice")?;
    let name = v
        .get("name")
        .or_else(|| v.get("function").and_then(|f| f.get("name")));
    if let Some(name) = name {
        return Ok(ToolChoice::Named(text(name, "name")?));
    }
    if let Some(mode) = v.get("type").or_else(|| v.get("mode")) {
        let mode = text(mode, "tool_choice")?;
        if mode.eq_str("auto") || mode.eq_str("AUTO") {
            return Ok(ToolChoice::Auto);
        }
        if mode.eq_str("none") || mode.eq_str("NONE") {
            return Ok(ToolChoice::None);
        }
        if mode.eq_str("any") || mode.eq_str("ANY") {
            if let Some(names) = v.get("allowedFunctionNames") {
                array(names, "allowedFunctionNames")?;
                let mut iter = names.array();
                if let (Some(name), None) = (iter.next(), iter.next()) {
                    return Ok(ToolChoice::Named(text(name, "allowedFunctionNames")?));
                }
            } else {
                return Ok(ToolChoice::Required);
            }
        }
    }
    Ok(ToolChoice::Opaque {
        source,
        value: v.raw_value(),
    })
}

fn parameters<'a, 't>(
    source: Format,
    root: V<'a, 't>,
    arena: &mut Arena<'_, 'a>,
) -> Result<Parameters<'a>, Error> {
    let p = match source.protocol {
        Protocol::GeminiGenerateContent => root.get("generationConfig"),
        Protocol::GeminiInteractions => root.get("generation_config"),
        _ => Some(root),
    };
    let mut params = Parameters::default();
    if let Some(v) = p {
        if v.is_null() {
            return Err(invalid(
                "generation config",
                "null is not a generation object",
            ));
        }
        object(v, "generation config")?;
        let gemini = source.protocol == Protocol::GeminiGenerateContent;
        params.temperature = raw_presence(v, "temperature");
        params.top_p = raw_presence(v, if gemini { "topP" } else { "top_p" });
        params.top_k = raw_presence(v, if gemini { "topK" } else { "top_k" });
        params.max_output_tokens = raw_presence(
            v,
            match source.protocol {
                Protocol::GeminiGenerateContent => "maxOutputTokens",
                Protocol::OpenAiResponses | Protocol::GeminiInteractions => "max_output_tokens",
                Protocol::OpenAiChat if v.get("max_completion_tokens").is_some() => {
                    "max_completion_tokens"
                }
                _ => "max_tokens",
            },
        );
        params.stop = raw_presence(
            v,
            if gemini {
                "stopSequences"
            } else if matches!(
                source.protocol,
                Protocol::AnthropicMessages | Protocol::GeminiInteractions
            ) {
                "stop_sequences"
            } else {
                "stop"
            },
        );
        params.seed = raw_presence(v, "seed");
        params.frequency_penalty = raw_presence(
            v,
            if gemini {
                "frequencyPenalty"
            } else {
                "frequency_penalty"
            },
        );
        params.presence_penalty = raw_presence(
            v,
            if gemini {
                "presencePenalty"
            } else {
                "presence_penalty"
            },
        );
        params.n = raw_presence(v, if gemini { "candidateCount" } else { "n" });
        if !matches!(
            source.protocol,
            Protocol::AnthropicMessages | Protocol::GeminiGenerateContent
        ) {
            params.top_k = Presence::Missing;
        }
        if !matches!(
            source.protocol,
            Protocol::OpenAiChat | Protocol::OpenAiCompletions | Protocol::GeminiGenerateContent
        ) {
            params.n = Presence::Missing;
            params.frequency_penalty = Presence::Missing;
            params.presence_penalty = Presence::Missing;
        }
        if matches!(
            source.protocol,
            Protocol::OpenAiResponses | Protocol::AnthropicMessages
        ) {
            params.seed = Presence::Missing;
        }
        if source.protocol == Protocol::OpenAiResponses {
            params.stop = Presence::Missing;
        }
        for (field, p) in [
            ("temperature", params.temperature),
            ("top_p", params.top_p),
            ("top_k", params.top_k),
            ("max_tokens", params.max_output_tokens),
            ("seed", params.seed),
            ("frequency_penalty", params.frequency_penalty),
            ("presence_penalty", params.presence_penalty),
            ("n", params.n),
        ] {
            if let Presence::Value(v) = p {
                if v.kind() != Kind::Number {
                    return Err(invalid(field, "expected number"));
                }
            }
        }
        for (field, p) in [
            ("top_k", params.top_k),
            ("max_tokens", params.max_output_tokens),
            ("n", params.n),
        ] {
            if let Presence::Value(v) = p {
                if !v.raw().iter().all(u8::is_ascii_digit) {
                    return Err(invalid(field, "expected nonnegative integer"));
                }
            }
        }
        if let Presence::Value(v) = params.seed {
            let bytes = v.raw().strip_prefix(b"-").unwrap_or(v.raw());
            if !bytes.iter().all(u8::is_ascii_digit) {
                return Err(invalid("seed", "expected integer"));
            }
        }
        if source.protocol == Protocol::AnthropicMessages
            && source.version == "2026-10-21"
            && (!params.temperature.is_missing()
                || !params.top_p.is_missing()
                || !params.top_k.is_missing())
        {
            return Err(invalid(
                "sampling",
                "temperature/top_p/top_k are disallowed in 2026-10-21",
            ));
        }
        if let Presence::Value(stop) = params.stop {
            if !matches!(stop.kind(), Kind::Array | Kind::String) {
                return Err(invalid("stop", "expected string or string array"));
            }
            let key = if gemini {
                "stopSequences"
            } else if matches!(
                source.protocol,
                Protocol::AnthropicMessages | Protocol::GeminiInteractions
            ) {
                "stop_sequences"
            } else {
                "stop"
            };
            if let Some(sv) = v.get(key) {
                if sv.kind() == Kind::Array {
                    for s in sv.array() {
                        text(s, "stop")?;
                    }
                }
            }
        }
        if matches!(
            source.protocol,
            Protocol::GeminiGenerateContent | Protocol::GeminiInteractions
        ) {
            params.extensions = arena.extras(
                v,
                &[
                    "temperature",
                    "topP",
                    "top_p",
                    "topK",
                    "top_k",
                    "maxOutputTokens",
                    "max_output_tokens",
                    "stopSequences",
                    "stop_sequences",
                    "seed",
                    "frequencyPenalty",
                    "frequency_penalty",
                    "presencePenalty",
                    "presence_penalty",
                    "candidateCount",
                    "n",
                    "tool_choice",
                    "responseMimeType",
                    "responseSchema",
                    "responseJsonSchema",
                ],
                "generation config",
                usize::MAX,
            )?;
        }
    }
    params.stream = if source.protocol == Protocol::GeminiGenerateContent {
        Presence::Missing
    } else {
        raw_presence(root, "stream")
    };
    params.parallel_tool_calls = if matches!(
        source.protocol,
        Protocol::OpenAiChat | Protocol::OpenAiResponses
    ) {
        raw_presence(root, "parallel_tool_calls")
    } else {
        Presence::Missing
    };
    for (field, p) in [
        ("stream", params.stream),
        ("parallel_tool_calls", params.parallel_tool_calls),
    ] {
        if let Presence::Value(v) = p {
            if v.as_bool().is_none() {
                return Err(invalid(field, "expected boolean"));
            }
        }
    }
    Ok(params)
}

fn validate_prompt(value: V<'_, '_>) -> Result<(), Error> {
    array(value, "prompt")?;
    let mut kind = None;
    for item in value.array() {
        if let Some(k) = kind {
            if item.kind() != k {
                return Err(invalid("prompt", "mixed prompt batch types"));
            }
        } else {
            kind = Some(item.kind());
        }
        match item.kind() {
            Kind::String => {}
            Kind::Number => {
                if !item.raw().iter().all(u8::is_ascii_digit) {
                    return Err(invalid("prompt", "token IDs must be unsigned integers"));
                }
            }
            Kind::Array => {
                for id in item.array() {
                    if id.kind() != Kind::Number || !id.raw().iter().all(u8::is_ascii_digit) {
                        return Err(invalid("prompt", "token IDs must be unsigned integers"));
                    }
                }
            }
            _ => return Err(invalid("prompt", "expected text or token-ID prompt")),
        }
    }
    Ok(())
}

pub(super) fn request_keys(source: Protocol) -> &'static [&'static str] {
    match source {
        Protocol::OpenAiCompletions => &[
            "model",
            "prompt",
            "max_tokens",
            "temperature",
            "top_p",
            "n",
            "stream",
            "stop",
            "seed",
            "frequency_penalty",
            "presence_penalty",
        ],
        Protocol::OpenAiChat => &[
            "model",
            "messages",
            "tools",
            "functions",
            "tool_choice",
            "function_call",
            "temperature",
            "top_p",
            "max_tokens",
            "max_completion_tokens",
            "n",
            "stream",
            "stop",
            "seed",
            "frequency_penalty",
            "presence_penalty",
            "parallel_tool_calls",
            "response_format",
        ],
        Protocol::OpenAiResponses => &[
            "model",
            "input",
            "instructions",
            "tools",
            "tool_choice",
            "temperature",
            "top_p",
            "max_output_tokens",
            "stream",
            "parallel_tool_calls",
            "previous_response_id",
            "text",
        ],
        Protocol::AnthropicMessages => &[
            "model",
            "messages",
            "system",
            "tools",
            "tool_choice",
            "max_tokens",
            "temperature",
            "top_p",
            "top_k",
            "stream",
            "stop_sequences",
        ],
        Protocol::GeminiGenerateContent => &[
            "model",
            "contents",
            "systemInstruction",
            "tools",
            "toolConfig",
            "generationConfig",
        ],
        Protocol::GeminiInteractions => &[
            "model",
            "input",
            "system_instruction",
            "tools",
            "generation_config",
            "response_format",
            "previous_interaction_id",
            "stream",
        ],
    }
}

/// Decode a validated request view into caller storage. Capacity errors are lower bounds.
pub fn decode_request<'a, 't, 's>(
    source: Format,
    root: V<'a, 't>,
    storage: &'s mut [Record<'a>],
) -> Result<Request<'a, 's>, Error> {
    let mut a = Arena {
        records: storage,
        used: 0,
        source,
        count_only: false,
    };
    let h = request_header(source, root, &mut a)?;
    let used = a.used;
    Ok(Request {
        source,
        model: h.0,
        tools: marker(root.get("tools")),
        functions: marker(root.get("functions")),
        instruction_presence: marker(root.get(match source.protocol {
            Protocol::OpenAiResponses => "instructions",
            Protocol::AnthropicMessages => "system",
            Protocol::GeminiGenerateContent => "systemInstruction",
            Protocol::GeminiInteractions => "system_instruction",
            _ => "",
        })),
        parameters: h.1,
        tool_choice: h.2,
        context: h.3,
        output_constraint: h.4,
        extensions: h.5,
        records: &mut a.records[..used],
    })
}

type RequestHeader<'a> = (
    Presence<Text<'a>>,
    Parameters<'a>,
    ToolChoice<'a>,
    Presence<ContextRef<'a>>,
    Presence<OutputConstraint<'a>>,
    Span,
);

fn request_header<'a, 't>(
    source: Format,
    root: V<'a, 't>,
    a: &mut Arena<'_, 'a>,
) -> Result<RequestHeader<'a>, Error> {
    validate_format(source)?;
    object(root, "request")?;
    let model = ptext(root, "model")?;
    if source.protocol != Protocol::GeminiGenerateContent && !matches!(model, Presence::Value(_)) {
        return Err(invalid("model", "required string missing"));
    }
    let params = parameters(source, root, a)?;
    if source.protocol == Protocol::AnthropicMessages
        && !matches!(params.max_output_tokens, Presence::Value(_))
    {
        return Err(invalid("max_tokens", "required number missing"));
    }
    match source.protocol {
        Protocol::OpenAiCompletions => {
            let prompt = required(root, "prompt")?;
            let start = a.message_start()?;
            if prompt.kind() != Kind::String {
                validate_prompt(prompt)?;
                if !a.count_only {
                    return Err(Error::ContextRequired {
                        field: "batch selection or tokenizer-decoded completion prompt",
                    });
                }
            } else {
                a.block(
                    Content::Text(text(prompt, "prompt")?),
                    None,
                    &[],
                    Presence::Missing,
                )?;
            }
            a.message_end(
                start,
                Role::User,
                Presence::Missing,
                ContentShape::String,
                None,
                &[],
                Presence::Missing,
            )?;
        }
        Protocol::OpenAiChat => {
            let messages = required(root, "messages")?;
            array(messages, "messages")?;
            for m in messages.array() {
                a.chat_message(m)?;
            }
        }
        Protocol::AnthropicMessages => {
            if let Some(system) = root.get("system") {
                let start = a.message_start()?;
                a.content(system, Role::System)?;
                a.message_end(
                    start,
                    Role::System,
                    Presence::Missing,
                    shape(Some(system)),
                    None,
                    &[],
                    Presence::Missing,
                )?;
            }
            let messages = required(root, "messages")?;
            array(messages, "messages")?;
            for m in messages.array() {
                a.simple_message(m, false)?;
            }
        }
        Protocol::GeminiGenerateContent => {
            if let Some(system) = root.get("systemInstruction") {
                let start = a.message_start()?;
                a.content(required(system, "parts")?, Role::System)?;
                a.message_end(
                    start,
                    Role::System,
                    Presence::Missing,
                    ContentShape::Array,
                    Some(system),
                    &["role", "parts"],
                    Presence::Missing,
                )?;
            }
            let contents = required(root, "contents")?;
            array(contents, "contents")?;
            for m in contents.array() {
                a.simple_message(m, true)?;
            }
        }
        Protocol::OpenAiResponses | Protocol::GeminiInteractions => {
            let interactions = source.protocol == Protocol::GeminiInteractions;
            let system_key = if interactions {
                "system_instruction"
            } else {
                "instructions"
            };
            if let Some(system) = root.get(system_key) {
                if !system.is_null() {
                    let start = a.message_start()?;
                    a.block(
                        Content::Text(text(system, system_key)?),
                        None,
                        &[],
                        Presence::Missing,
                    )?;
                    a.message_end(
                        start,
                        Role::System,
                        Presence::Missing,
                        ContentShape::String,
                        None,
                        &[],
                        Presence::Missing,
                    )?;
                    if let Some(Record::Message(m)) = a.records.get_mut(start) {
                        m.system_instruction = true;
                    }
                }
            }
            let input = required(root, "input")?;
            if input.kind() == Kind::String {
                let start = a.message_start()?;
                a.block(
                    Content::Text(text(input, "input")?),
                    None,
                    &[],
                    Presence::Missing,
                )?;
                a.message_end(
                    start,
                    Role::User,
                    Presence::Missing,
                    ContentShape::String,
                    None,
                    &[],
                    Presence::Missing,
                )?;
            } else {
                array(input, "input")?;
                for item in input.array() {
                    let is_content = interactions
                        && item
                            .get("type")
                            .and_then(|v| v.as_string())
                            .map(|s| {
                                ["text", "image", "audio", "video", "document"]
                                    .iter()
                                    .any(|t| s.eq_str(t))
                            })
                            .unwrap_or(false);
                    if is_content {
                        let start = a.message_start()?;
                        a.content_block(item, Role::User)?;
                        a.message_end(
                            start,
                            Role::User,
                            Presence::Missing,
                            ContentShape::Array,
                            None,
                            &[],
                            Presence::Missing,
                        )?;
                    } else {
                        a.response_item(item, interactions)?;
                    }
                }
            }
        }
    }
    a.tools(root)?;
    let choice_value = match source.protocol {
        Protocol::GeminiGenerateContent => root
            .get("toolConfig")
            .and_then(|v| v.get("functionCallingConfig")),
        Protocol::GeminiInteractions => root
            .get("generation_config")
            .and_then(|v| v.get("tool_choice")),
        _ => root
            .get("tool_choice")
            .or_else(|| root.get("function_call")),
    };
    let choice = tool_choice(source, choice_value)?;
    let ctx = match if matches!(
        source.protocol,
        Protocol::OpenAiResponses | Protocol::GeminiInteractions
    ) {
        ptext(
            root,
            if source.protocol == Protocol::GeminiInteractions {
                "previous_interaction_id"
            } else {
                "previous_response_id"
            },
        )?
    } else {
        Presence::Missing
    } {
        Presence::Missing => Presence::Missing,
        Presence::Null => Presence::Null,
        Presence::Value(id) => Presence::Value(ContextRef { source, id }),
    };
    let output = if source.protocol == Protocol::OpenAiResponses {
        if let Some(t) = root.get("text") {
            object(t, "text")?;
            reject_extra(t, &["format"], "Responses text configuration extension")?;
        }
        root.get("text").and_then(|v| v.get("format"))
    } else if source.protocol == Protocol::GeminiGenerateContent {
        root.get("generationConfig").and_then(|v| {
            v.get("responseJsonSchema")
                .or_else(|| v.get("responseSchema"))
                .or_else(|| v.get("responseMimeType"))
        })
    } else if matches!(
        source.protocol,
        Protocol::OpenAiChat | Protocol::GeminiInteractions
    ) {
        root.get("response_format")
    } else {
        None
    };
    let output = match output {
        None => Presence::Missing,
        Some(v) if v.is_null() => Presence::Null,
        Some(value) => {
            let mut constraint = OutputConstraint {
                source,
                value: value.raw_value(),
                mode: OutputMode::Opaque,
                schema: Presence::Missing,
                name: Presence::Missing,
                description: Presence::Missing,
                strict: Presence::Missing,
            };
            if source.protocol == Protocol::GeminiGenerateContent {
                let config = root
                    .get("generationConfig")
                    .ok_or_else(|| invalid("generationConfig", "missing config"))?;
                if let Some(schema) = config
                    .get("responseJsonSchema")
                    .or_else(|| config.get("responseSchema"))
                {
                    object(schema, "response schema")?;
                    constraint.mode = OutputMode::JsonSchema;
                    constraint.schema = Presence::Value(schema.raw_value());
                } else if value
                    .as_string()
                    .map(|s| s.eq_str("application/json"))
                    .unwrap_or(false)
                {
                    constraint.mode = OutputMode::JsonObject;
                } else if value
                    .as_string()
                    .map(|s| s.eq_str("text/plain"))
                    .unwrap_or(false)
                {
                    constraint.mode = OutputMode::Text;
                }
            } else if matches!(
                source.protocol,
                Protocol::OpenAiChat | Protocol::OpenAiResponses
            ) {
                object(value, "response format")?;
                let ty = req_text(value, "type")?;
                constraint.mode = if ty.eq_str("text") {
                    OutputMode::Text
                } else if ty.eq_str("json_object") {
                    OutputMode::JsonObject
                } else if ty.eq_str("json_schema") {
                    OutputMode::JsonSchema
                } else {
                    OutputMode::Opaque
                };
                if constraint.mode == OutputMode::JsonSchema {
                    let definition = if source.protocol == Protocol::OpenAiChat {
                        required(value, "json_schema")?
                    } else {
                        value
                    };
                    object(definition, "json_schema")?;
                    let schema = required(definition, "schema")?;
                    object(schema, "schema")?;
                    constraint.schema = Presence::Value(schema.raw_value());
                    constraint.name = ptext(definition, "name")?;
                    constraint.description = ptext(definition, "description")?;
                    constraint.strict = pbool(definition, "strict")?;
                    reject_extra(
                        definition,
                        if source.protocol == Protocol::OpenAiChat {
                            &["schema", "name", "description", "strict"]
                        } else {
                            &["type", "schema", "name", "description", "strict"]
                        },
                        "structured output extension",
                    )?;
                }
                reject_extra(
                    value,
                    if source.protocol == Protocol::OpenAiChat
                        && constraint.mode == OutputMode::JsonSchema
                    {
                        &["type", "json_schema"]
                    } else {
                        &["type", "schema", "name", "description", "strict"]
                    },
                    "structured output wrapper extension",
                )?;
            }
            Presence::Value(constraint)
        }
    };
    let extensions = a.extras(root, request_keys(source.protocol), "request", usize::MAX)?;
    Ok((model, params, choice, ctx, output, extensions))
}

fn finish<'a>(v: Option<V<'a, '_>>) -> Result<(Finish, Presence<Text<'a>>), Error> {
    let t = match v {
        None => return Ok((Finish::Missing, Presence::Missing)),
        Some(v) if v.is_null() => return Ok((Finish::Null, Presence::Null)),
        Some(v) => text(v, "finish reason")?,
    };
    let f = if ["stop", "end_turn", "stop_sequence", "STOP", "completed"]
        .iter()
        .any(|s| t.eq_str(s))
    {
        Finish::Stop
    } else if ["length", "max_tokens", "MAX_TOKENS", "incomplete"]
        .iter()
        .any(|s| t.eq_str(s))
    {
        Finish::Length
    } else if ["tool_calls", "function_call", "tool_use", "requires_action"]
        .iter()
        .any(|s| t.eq_str(s))
    {
        Finish::ToolCalls
    } else if [
        "content_filter",
        "SAFETY",
        "RECITATION",
        "BLOCKLIST",
        "PROHIBITED_CONTENT",
        "SPII",
        "IMAGE_SAFETY",
    ]
    .iter()
    .any(|s| t.eq_str(s))
    {
        Finish::Safety
    } else if t.eq_str("refusal") {
        Finish::Refusal
    } else if t.eq_str("error") || t.eq_str("failed") {
        Finish::Error
    } else if t.eq_str("cancelled") {
        Finish::Cancelled
    } else if t.eq_str("in_progress") || t.eq_str("queued") {
        Finish::InProgress
    } else {
        Finish::Other
    };
    Ok((f, Presence::Value(t)))
}

fn usage<'a, 't>(
    root: V<'a, 't>,
    source: Format,
    a: &mut Arena<'_, 'a>,
) -> Result<Usage<'a>, Error> {
    let v = match root.get(if source.protocol == Protocol::GeminiGenerateContent {
        "usageMetadata"
    } else {
        "usage"
    }) {
        None => return Ok(Usage::default()),
        Some(v) if v.is_null() => {
            return Ok(Usage {
                present: true,
                is_null: true,
                ..Usage::default()
            });
        }
        Some(v) => v,
    };
    object(v, "usage")?;
    let (input, output, total, cached, thought) = match source.protocol {
        Protocol::OpenAiChat | Protocol::OpenAiCompletions => (
            "prompt_tokens",
            "completion_tokens",
            "total_tokens",
            "cached_tokens",
            "reasoning_tokens",
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
        _ => (
            "input_tokens",
            "output_tokens",
            "total_tokens",
            "cache_read_input_tokens",
            "reasoning_tokens",
        ),
    };
    let mut u = Usage {
        input_tokens: pnumber(v, input)?,
        output_tokens: pnumber(v, output)?,
        total_tokens: pnumber(v, total)?,
        cached_tokens: pnumber(v, cached)?,
        cache_write_tokens: pnumber(v, "cache_creation_input_tokens")?,
        reasoning_tokens: pnumber(v, thought)?,
        present: true,
        ..Usage::default()
    };
    if let Some(details) = v.get(if source.protocol == Protocol::OpenAiResponses {
        "input_tokens_details"
    } else {
        "prompt_tokens_details"
    }) {
        object(details, "usage details")?;
        u.cached_tokens = pnumber(details, "cached_tokens")?;
        reject_extra(details, &["cached_tokens"], "input usage detail extension")?;
    }
    if let Some(details) = v.get(if source.protocol == Protocol::OpenAiResponses {
        "output_tokens_details"
    } else {
        "completion_tokens_details"
    }) {
        object(details, "usage details")?;
        u.reasoning_tokens = pnumber(details, "reasoning_tokens")?;
        reject_extra(
            details,
            &["reasoning_tokens"],
            "output usage detail extension",
        )?;
    }
    if source.protocol == Protocol::AnthropicMessages
        && (!u.cached_tokens.is_missing() || !u.cache_write_tokens.is_missing())
    {
        let input = counter_value(u.input_tokens, "input_tokens")?;
        let cached = counter_value(u.cached_tokens, "cache_read_input_tokens")?;
        let written = counter_value(u.cache_write_tokens, "cache_creation_input_tokens")?;
        let total = input
            .checked_add(cached)
            .and_then(|n| n.checked_add(written))
            .ok_or(Error::Unsupported {
                feature: "inclusive Anthropic usage exceeds u64 arithmetic",
            })?;
        u.input_tokens = Presence::Value(Number::Unsigned(total));
    }
    if matches!(
        source.protocol,
        Protocol::GeminiGenerateContent | Protocol::GeminiInteractions
    ) && !u.reasoning_tokens.is_missing()
    {
        if !matches!(u.output_tokens, Presence::Value(_)) {
            return Err(invalid(
                "output_tokens",
                "visible output count required alongside thought usage",
            ));
        }
        let visible = counter_value(u.output_tokens, "output_tokens")?;
        let thoughts = counter_value(u.reasoning_tokens, "thought tokens")?;
        u.output_tokens = Presence::Value(Number::Unsigned(visible.checked_add(thoughts).ok_or(
            Error::Unsupported {
                feature: "inclusive generated usage exceeds u64 arithmetic",
            },
        )?));
    }
    u.extensions = a.extras(
        v,
        &[
            input,
            output,
            total,
            cached,
            thought,
            "cache_creation_input_tokens",
            "input_tokens_details",
            "output_tokens_details",
            "prompt_tokens_details",
            "completion_tokens_details",
        ],
        "usage",
        usize::MAX,
    )?;
    Ok(u)
}

fn counter_value(value: Presence<Number<'_>>, field: &'static str) -> Result<u64, Error> {
    match value {
        Presence::Missing => Ok(0),
        Presence::Value(n) => n.as_u64().ok_or(Error::Unsupported { feature: field }),
        Presence::Null => Err(invalid(
            field,
            "null cannot participate in usage arithmetic",
        )),
    }
}

/// Decode a complete response, including every candidate in order.
pub fn decode_response<'a, 't, 's>(
    source: Format,
    root: V<'a, 't>,
    storage: &'s mut [Record<'a>],
) -> Result<Response<'a, 's>, Error> {
    let mut a = Arena {
        records: storage,
        used: 0,
        source,
        count_only: false,
    };
    let h = response_header(source, root, &mut a)?;
    let used = a.used;
    Ok(Response {
        source,
        id: h.0,
        model: h.1,
        created: h.2,
        created_iso: h.3,
        updated_iso: h.4,
        status: h.5,
        usage: h.6,
        error: h.7,
        stop_sequence: if source.protocol == Protocol::AnthropicMessages {
            ptext(root, "stop_sequence")?
        } else {
            Presence::Missing
        },
        incomplete_details: if source.protocol == Protocol::OpenAiResponses {
            raw_presence(root, "incomplete_details")
        } else {
            Presence::Missing
        },
        extensions: h.8,
        records: &mut a.records[..used],
    })
}

type ResponseHeader<'a> = (
    Presence<Text<'a>>,
    Presence<Text<'a>>,
    Presence<Number<'a>>,
    Presence<Text<'a>>,
    Presence<Text<'a>>,
    Finish,
    Usage<'a>,
    Presence<RawValue<'a>>,
    Span,
);

fn response_header<'a, 't>(
    source: Format,
    root: V<'a, 't>,
    a: &mut Arena<'_, 'a>,
) -> Result<ResponseHeader<'a>, Error> {
    validate_format(source)?;
    object(root, "response")?;
    let id = ptext(
        root,
        if source.protocol == Protocol::GeminiGenerateContent {
            "responseId"
        } else {
            "id"
        },
    )?;
    let model = ptext(
        root,
        if source.protocol == Protocol::GeminiGenerateContent {
            "modelVersion"
        } else {
            "model"
        },
    )?;
    let created = if !matches!(
        source.protocol,
        Protocol::OpenAiChat | Protocol::OpenAiCompletions | Protocol::OpenAiResponses
    ) {
        Presence::Missing
    } else {
        pnumber(
            root,
            if source.protocol == Protocol::OpenAiResponses {
                "created_at"
            } else {
                "created"
            },
        )?
    };
    let (created_iso, updated_iso) = if source.protocol == Protocol::GeminiInteractions {
        (ptext(root, "created")?, ptext(root, "updated")?)
    } else {
        (Presence::Missing, Presence::Missing)
    };
    let mut status = if matches!(
        source.protocol,
        Protocol::OpenAiResponses | Protocol::GeminiInteractions
    ) {
        finish(root.get("status"))?.0
    } else {
        Finish::Missing
    };
    if let Some(details) = root
        .get("incomplete_details")
        .filter(|_| source.protocol == Protocol::OpenAiResponses)
    {
        if !details.is_null() {
            object(details, "incomplete_details")?;
            let reason = req_text(details, "reason")?;
            if reason.eq_str("max_output_tokens") {
                status = Finish::Length;
            } else if reason.eq_str("content_filter") {
                status = Finish::Safety;
            } else {
                status = Finish::Other;
            }
            reject_extra(details, &["reason"], "incomplete_details extension")?;
        }
    }
    match source.protocol {
        Protocol::OpenAiChat | Protocol::OpenAiCompletions | Protocol::GeminiGenerateContent => {
            let google = source.protocol == Protocol::GeminiGenerateContent;
            let choices = root.get(if google { "candidates" } else { "choices" });
            if let Some(choices) = choices {
                array(choices, "choices")?;
                for (ordinal, c) in choices.array().enumerate() {
                    object(c, "choice")?;
                    let owner = a.push(Record::Empty)?;
                    let index = match c.get("index") {
                        Some(v) => v
                            .as_u64()
                            .ok_or_else(|| invalid("index", "expected unsigned integer"))?,
                        None if google => ordinal as u64,
                        None => return Err(invalid("index", "required field missing")),
                    };
                    if google {
                        if let Some(content) = c.get("content") {
                            a.simple_message(content, true)?;
                        }
                    } else if source.protocol == Protocol::OpenAiChat {
                        let message = required(c, "message")?;
                        if role(message)? != Role::Assistant {
                            return Err(invalid("message.role", "response must be assistant"));
                        }
                        a.chat_message(message)?;
                    } else {
                        let start = a.message_start()?;
                        a.block(
                            Content::Text(req_text(c, "text")?),
                            None,
                            &[],
                            Presence::Missing,
                        )?;
                        a.message_end(
                            start,
                            Role::Assistant,
                            Presence::Missing,
                            ContentShape::String,
                            None,
                            &[],
                            Presence::Missing,
                        )?;
                    }
                    let (mut reason, raw_finish) = finish(c.get(if google {
                        "finishReason"
                    } else {
                        "finish_reason"
                    }))?;
                    if google
                        && reason == Finish::Stop
                        && c.get("content")
                            .and_then(|v| v.get("parts"))
                            .map(|parts| {
                                parts.array().any(|part| part.get("functionCall").is_some())
                            })
                            .unwrap_or(false)
                    {
                        reason = Finish::ToolCalls;
                    }
                    let extensions = a.extras(
                        c,
                        if google {
                            &["index", "content", "finishReason"]
                        } else {
                            &["index", "message", "text", "finish_reason"]
                        },
                        "choice",
                        owner,
                    )?;
                    a.set(
                        owner,
                        Record::Choice(Choice {
                            index,
                            finish: reason,
                            raw_finish,
                            end: a.used,
                            extensions,
                        }),
                    );
                }
            } else if google {
                let feedback = required(root, "promptFeedback")?;
                object(feedback, "promptFeedback")?;
                req_text(feedback, "blockReason")?;
                status = Finish::Safety;
                let owner = a.push(Record::Empty)?;
                let message = a.message_start()?;
                a.message_end(
                    message,
                    Role::Assistant,
                    Presence::Missing,
                    ContentShape::Null,
                    None,
                    &[],
                    Presence::Missing,
                )?;
                a.set(
                    owner,
                    Record::Choice(Choice {
                        index: 0,
                        finish: Finish::Safety,
                        raw_finish: Presence::Value(Text::Utf8("SAFETY")),
                        end: a.used,
                        extensions: Span::EMPTY,
                    }),
                );
            } else {
                return Err(invalid("choices", "required field missing"));
            }
        }
        Protocol::AnthropicMessages => {
            if !req_text(root, "role")?.eq_str("assistant") {
                return Err(invalid("role", "response role must be assistant"));
            }
            let owner = a.push(Record::Empty)?;
            let start = a.message_start()?;
            a.content(required(root, "content")?, Role::Assistant)?;
            a.message_end(
                start,
                Role::Assistant,
                Presence::Missing,
                ContentShape::Array,
                None,
                &[],
                Presence::Missing,
            )?;
            let (finish, raw_finish) = finish(root.get("stop_reason"))?;
            if finish == Finish::Refusal && !a.count_only {
                for record in &mut a.records[start + 1..a.used] {
                    if let Record::Block(b) = record {
                        if let Content::Text(t) = b.content {
                            b.content = Content::Refusal(t);
                        }
                    }
                }
            }
            a.set(
                owner,
                Record::Choice(Choice {
                    index: 0,
                    finish,
                    raw_finish,
                    end: a.used,
                    extensions: Span::EMPTY,
                }),
            );
        }
        Protocol::OpenAiResponses | Protocol::GeminiInteractions => {
            let interactions = source.protocol == Protocol::GeminiInteractions;
            let output = required(root, if interactions { "steps" } else { "output" })?;
            array(output, "output")?;
            let owner = a.push(Record::Empty)?;
            let mut has_calls = false;
            for item in output.array() {
                has_calls |= item
                    .get("type")
                    .and_then(|v| v.as_string())
                    .map(|s| s.eq_str("function_call"))
                    .unwrap_or(false);
                a.response_item(item, interactions)?;
            }
            let reason = if status == Finish::Stop && has_calls {
                Finish::ToolCalls
            } else {
                status
            };
            a.set(
                owner,
                Record::Choice(Choice {
                    index: 0,
                    finish: reason,
                    raw_finish: ptext(root, "status")?,
                    end: a.used,
                    extensions: Span::EMPTY,
                }),
            );
        }
    }
    let usage = usage(root, source, a)?;
    let extensions = a.extras(root, response_keys(source.protocol), "response", usize::MAX)?;
    Ok((
        id,
        model,
        created,
        created_iso,
        updated_iso,
        status,
        usage,
        if matches!(
            source.protocol,
            Protocol::OpenAiResponses | Protocol::GeminiInteractions
        ) {
            raw_presence(root, "error")
        } else {
            Presence::Missing
        },
        extensions,
    ))
}

pub(super) fn response_keys(source: Protocol) -> &'static [&'static str] {
    match source {
        Protocol::OpenAiChat | Protocol::OpenAiCompletions => {
            &["id", "object", "model", "created", "choices", "usage"]
        }
        Protocol::OpenAiResponses => &[
            "id",
            "object",
            "model",
            "created_at",
            "status",
            "output",
            "usage",
            "error",
            "incomplete_details",
        ],
        Protocol::AnthropicMessages => &[
            "id",
            "type",
            "model",
            "role",
            "content",
            "stop_reason",
            "stop_sequence",
            "usage",
        ],
        Protocol::GeminiGenerateContent => {
            &["responseId", "modelVersion", "candidates", "usageMetadata"]
        }
        Protocol::GeminiInteractions => &[
            "id", "object", "model", "created", "updated", "status", "steps", "usage", "error",
        ],
    }
}

/// Validate syntax-independent protocol semantics without allocating IR storage.
///
/// This runs the schema walk in validation mode. No arena entries are retained.
pub fn validate(source: Format, operation: Operation, root: V<'_, '_>) -> Result<(), Error> {
    let mut records = [];
    let mut arena = Arena {
        records: &mut records,
        used: 0,
        source,
        count_only: true,
    };
    match operation {
        Operation::Request => {
            request_header(source, root, &mut arena)?;
        }
        Operation::Response => {
            response_header(source, root, &mut arena)?;
        }
    }
    Ok(())
}
