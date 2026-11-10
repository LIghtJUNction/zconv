//! Incremental WHATWG server-sent event framing.
//!
//! Framing does one pass over incoming bytes and retains only the current event.
//! CR, LF and CRLF are accepted. One leading UTF-8 BOM is ignored. The payload
//! extractor implements UTF-8 replacement decoding, joins `data` fields with LF,
//! and discards unterminated events at EOF as required by the HTML standard.

use crate::Error;

/// Result of consuming part of a wire stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Progress {
    pub consumed: usize,
    pub status: Status,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    NeedInput,
    Event,
    /// The current event is retained; replace the storage and resume.
    NeedBuffer {
        needed: usize,
    },
    End,
}

/// A framed event. Its bytes borrow the decoder until `consume_event` is called.
#[derive(Clone, Copy, Debug)]
pub struct Event<'a> {
    lines: &'a [u8],
}

impl<'a> Event<'a> {
    /// Last event field as wire bytes, or `message` when absent/empty.
    /// Use `event_type_into` for WHATWG UTF-8 replacement decoding.
    pub fn event_type(self) -> &'a [u8] {
        let mut name: &[u8] = b"message";
        for line in self.lines.split(|&b| b == b'\n') {
            if let Some(value) = field(line, b"event") {
                name = if value.is_empty() { b"message" } else { value };
            }
        }
        name
    }

    pub fn event_type_into(self, output: &mut [u8]) -> Result<usize, Error> {
        replacement_into(self.event_type(), output)
    }

    /// Last valid `id` as wire bytes. The transport owns cross-event persistence.
    /// `id_into` returns its WHATWG replacement-decoded representation.
    pub fn id(self) -> Option<&'a [u8]> {
        let mut result = None;
        for line in self.lines.split(|&b| b == b'\n') {
            if let Some(value) = field(line, b"id") {
                if !value.contains(&0) {
                    result = Some(value);
                }
            }
        }
        result
    }

    pub fn id_into(self, output: &mut [u8]) -> Result<Option<usize>, Error> {
        self.id()
            .map(|bytes| replacement_into(bytes, output))
            .transpose()
    }

    /// Last syntactically valid retry delay representable in u64.
    pub fn retry(self) -> Option<u64> {
        let mut result = None;
        for line in self.lines.split(|&b| b == b'\n') {
            if let Some(value) = field(line, b"retry") {
                if value.is_empty() {
                    continue;
                }
                let mut number = Some(0u64);
                for &byte in value {
                    number = if byte.is_ascii_digit() {
                        number
                            .and_then(|n| n.checked_mul(10))
                            .and_then(|n| n.checked_add(u64::from(byte - b'0')))
                    } else {
                        None
                    };
                }
                if number.is_some() {
                    result = number;
                }
            }
        }
        result
    }

    /// Decode and concatenate all data lines. Capacity errors give the exact size.
    /// No data field is `None`; one empty data field is `Some(0)` and dispatches.
    pub fn data_into(self, output: &mut [u8]) -> Result<Option<usize>, Error> {
        let mut writer = ReplacementWriter { output, count: 0 };
        let mut present = false;
        for line in self.lines.split(|&b| b == b'\n') {
            if let Some(value) = field(line, b"data") {
                if present {
                    writer.bytes(b"\n");
                }
                present = true;
                writer.utf8(value);
            }
        }
        if writer.count > writer.output.len() {
            return Err(Error::OutputTooSmall {
                needed: writer.count,
            });
        }
        Ok(if present { Some(writer.count) } else { None })
    }
}

fn replacement_into(bytes: &[u8], output: &mut [u8]) -> Result<usize, Error> {
    let mut writer = ReplacementWriter { output, count: 0 };
    writer.utf8(bytes);
    if writer.count > writer.output.len() {
        return Err(Error::OutputTooSmall {
            needed: writer.count,
        });
    }
    Ok(writer.count)
}

fn field<'a>(line: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    if line == name {
        return Some(&line[line.len()..]);
    }
    if line.get(..name.len()) != Some(name) || line.get(name.len()) != Some(&b':') {
        return None;
    }
    let value = line.get(name.len() + 1..)?;
    Some(if value.first() == Some(&b' ') {
        &value[1..]
    } else {
        value
    })
}

struct ReplacementWriter<'a> {
    output: &'a mut [u8],
    count: usize,
}
impl ReplacementWriter<'_> {
    fn bytes(&mut self, bytes: &[u8]) {
        let start = self.count;
        self.count = self.count.saturating_add(bytes.len());
        if let Some(dst) = self.output.get_mut(start..self.count) {
            dst.copy_from_slice(bytes);
        }
    }
    fn utf8(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            match core::str::from_utf8(bytes) {
                Ok(_) => {
                    self.bytes(bytes);
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    self.bytes(&bytes[..valid]);
                    self.bytes(b"\xef\xbf\xbd");
                    let bad = error.error_len().unwrap_or(bytes.len() - valid);
                    bytes = &bytes[valid + bad..];
                }
            }
        }
    }
}

/// Caller-buffered, resumable SSE framer. No input is rescanned when fragmented.
pub struct Decoder<'a> {
    buffer: &'a mut [u8],
    used: usize,
    line_len: usize,
    skip_lf: bool,
    ready: bool,
    ended: bool,
    prefix: [u8; 3],
    prefix_len: u8,
    prefix_done: bool,
    pending_prefix: u8,
    peak: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(buffer: &'a mut [u8]) -> Self {
        Self {
            buffer,
            used: 0,
            line_len: 0,
            skip_lf: false,
            ready: false,
            ended: false,
            prefix: [0; 3],
            prefix_len: 0,
            prefix_done: false,
            pending_prefix: 0,
            peak: 0,
        }
    }
    pub fn capacity(&self) -> usize {
        self.buffer.len()
    }
    pub fn buffered(&self) -> usize {
        self.used
    }
    pub fn peak(&self) -> usize {
        self.peak
    }
    pub fn event(&self) -> Option<Event<'_>> {
        if self.ready {
            Some(Event {
                lines: &self.buffer[..self.used],
            })
        } else {
            None
        }
    }
    pub fn consume_event(&mut self) {
        if self.ready {
            self.used = 0;
            self.line_len = 0;
            self.ready = false;
        }
    }
    /// Swap storage without replaying bytes. The returned slice is the old buffer.
    pub fn replace_buffer(&mut self, buffer: &'a mut [u8]) -> Result<&'a mut [u8], Error> {
        if buffer.len() < self.used {
            return Err(Error::EventTooLarge { needed: self.used });
        }
        buffer[..self.used].copy_from_slice(&self.buffer[..self.used]);
        Ok(core::mem::replace(&mut self.buffer, buffer))
    }
    /// Discard an incomplete event at EOF. Returns whether bytes were discarded.
    pub fn finish(&mut self) -> bool {
        let discarded = self.used != 0 || (!self.prefix_done && self.prefix_len != 0);
        self.used = 0;
        self.line_len = 0;
        self.ready = false;
        self.ended = true;
        discarded
    }

    pub fn push(&mut self, input: &[u8]) -> Progress {
        if self.ready {
            return Progress {
                consumed: 0,
                status: Status::Event,
            };
        }
        if self.ended {
            return Progress {
                consumed: 0,
                status: Status::End,
            };
        }
        let mut consumed = 0;
        loop {
            if self.pending_prefix < self.prefix_len && self.prefix_done {
                let byte = self.prefix[usize::from(self.pending_prefix)];
                if !self.accept(byte) {
                    return Progress {
                        consumed,
                        status: Status::NeedBuffer {
                            needed: self.used.saturating_add(1),
                        },
                    };
                }
                self.pending_prefix += 1;
                if self.ready {
                    return Progress {
                        consumed,
                        status: Status::Event,
                    };
                }
                continue;
            }
            let Some(&byte) = input.get(consumed) else {
                return Progress {
                    consumed,
                    status: Status::NeedInput,
                };
            };
            if !self.prefix_done {
                self.prefix[usize::from(self.prefix_len)] = byte;
                self.prefix_len += 1;
                consumed += 1;
                let length = usize::from(self.prefix_len);
                if self.prefix[..length] != b"\xef\xbb\xbf"[..length] {
                    self.prefix_done = true;
                } else if length == 3 {
                    self.prefix_done = true;
                    self.pending_prefix = self.prefix_len;
                }
                continue;
            }
            if !self.accept(byte) {
                return Progress {
                    consumed,
                    status: Status::NeedBuffer {
                        needed: self.used.saturating_add(1),
                    },
                };
            }
            consumed += 1;
            if self.ready {
                return Progress {
                    consumed,
                    status: Status::Event,
                };
            }
        }
    }

    fn accept(&mut self, byte: u8) -> bool {
        if self.skip_lf && byte == b'\n' {
            self.skip_lf = false;
            return true;
        }
        let newline = byte == b'\r' || byte == b'\n';
        if newline && self.line_len == 0 {
            self.skip_lf = byte == b'\r';
            if self.used != 0 {
                self.ready = true;
            }
            return true;
        }
        let Some(slot) = self.buffer.get_mut(self.used) else {
            return false;
        };
        *slot = if newline { b'\n' } else { byte };
        self.used += 1;
        self.peak = self.peak.max(self.used);
        self.skip_lf = byte == b'\r';
        self.line_len = if newline {
            0
        } else {
            self.line_len.saturating_add(1)
        };
        true
    }
}
