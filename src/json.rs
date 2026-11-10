//! Bounded JSON validation, compact token indexing, borrowed views and writing.
//!
//! The token tape stores source spans and subtree end indices, not owned values.
//! Every byte is validated, including fields an adapter never examines. Object
//! names are compared after escape decoding and duplicate names are rejected.

use crate::Error;
use core::cmp::Ordering;

const NONE: u32 = u32::MAX;
/// Recursion is bounded independently of caller-provided limits.
pub const MAX_DEPTH: usize = 64;

/// The reason a JSON document was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JsonError {
    Syntax,
    UnexpectedEof,
    InvalidNumber,
    InvalidEscape,
    InvalidUnicode,
    InvalidUtf8,
    TrailingData,
}

/// JSON value categories. Object names use the `String` category in the tape.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Kind {
    Null,
    Bool,
    Number,
    String,
    Array,
    Object,
}

/// A compact index entry. Fields are private: views can only come from validation.
#[derive(Clone, Copy, Debug)]
pub struct Token {
    start: u32,
    end: u32,
    next: u32,
    link: u32,
    kind: Kind,
}

impl Token {
    pub const EMPTY: Self = Self {
        start: 0,
        end: 0,
        next: 0,
        link: NONE,
        kind: Kind::Null,
    };
}

impl Default for Token {
    fn default() -> Self {
        Self::EMPTY
    }
}

/// Resource limits apply even when the caller supplies larger buffers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub max_depth: usize,
    pub max_input: usize,
    pub max_tokens: usize,
    pub max_object_fields: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_depth: MAX_DEPTH,
            max_input: 16 * 1024 * 1024,
            max_tokens: 262_144,
            max_object_fields: 16_384,
        }
    }
}

/// A validated input and the initialized portion of the caller's token tape.
#[derive(Debug)]
pub struct Document<'a, 't> {
    input: &'a [u8],
    tokens: &'t [Token],
}

impl<'a, 't> Document<'a, 't> {
    pub fn root(&self) -> Value<'a, 't> {
        Value {
            input: self.input,
            tokens: self.tokens,
            index: 0,
        }
    }

    pub fn tokens_used(&self) -> usize {
        self.tokens.len()
    }
}

/// Validate exactly one complete JSON value and build its borrowed span index.
///
/// `WorkspaceTooSmall.needed` is a lower bound on the number of tokens required:
/// enlarge the tape and retry the unchanged input. Output is not produced here.
pub fn parse<'a, 't>(
    input: &'a [u8],
    tokens: &'t mut [Token],
    limits: Limits,
) -> Result<Document<'a, 't>, Error> {
    let input_limit = limits.max_input.min(u32::MAX as usize);
    if input.len() > input_limit {
        return Err(Error::InputTooLarge { limit: input_limit });
    }
    if let Err(error) = core::str::from_utf8(input) {
        return Err(Error::InvalidJson {
            offset: error.valid_up_to(),
            kind: JsonError::InvalidUtf8,
        });
    }
    let mut parser = Parser {
        input,
        tokens,
        pos: 0,
        used: 0,
        limits,
    };
    parser.value(0)?;
    parser.whitespace();
    if parser.pos != input.len() {
        return Err(parser.error(JsonError::TrailingData));
    }
    let used = parser.used;
    Ok(Document {
        input,
        tokens: &tokens[..used],
    })
}

struct Parser<'a, 't> {
    input: &'a [u8],
    tokens: &'t mut [Token],
    pos: usize,
    used: usize,
    limits: Limits,
}

impl Parser<'_, '_> {
    fn error(&self, kind: JsonError) -> Error {
        Error::InvalidJson {
            offset: self.pos,
            kind,
        }
    }

    fn whitespace(&mut self) {
        while matches!(self.input.get(self.pos), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.pos += 1;
        }
    }

    fn reserve(&mut self, kind: Kind, start: usize) -> Result<usize, Error> {
        if self.used >= self.limits.max_tokens.min((u32::MAX - 1) as usize) {
            return Err(Error::LimitExceeded {
                kind: "JSON tokens",
                limit: self.limits.max_tokens.min((u32::MAX - 1) as usize),
            });
        }
        if self.used >= self.tokens.len() {
            return Err(Error::WorkspaceTooSmall {
                needed: self.used + 1,
            });
        }
        let index = self.used;
        self.used += 1;
        self.tokens[index] = Token {
            start: start as u32,
            end: self.pos as u32,
            next: self.used as u32,
            link: NONE,
            kind,
        };
        Ok(index)
    }

    fn value(&mut self, depth: usize) -> Result<usize, Error> {
        self.whitespace();
        let start = self.pos;
        let byte = *self
            .input
            .get(self.pos)
            .ok_or_else(|| self.error(JsonError::UnexpectedEof))?;
        match byte {
            b'{' | b'[' => {
                let limit = self.limits.max_depth.min(MAX_DEPTH);
                if depth >= limit {
                    return Err(Error::DepthLimitExceeded { limit });
                }
                let object = byte == b'{';
                self.pos += 1;
                let index = self.reserve(if object { Kind::Object } else { Kind::Array }, start)?;
                if object {
                    self.object(depth + 1)?;
                } else {
                    self.array(depth + 1)?;
                }
                self.tokens[index].end = self.pos as u32;
                self.tokens[index].next = self.used as u32;
                Ok(index)
            }
            b'"' => {
                self.string()?;
                self.reserve(Kind::String, start)
            }
            b'-' | b'0'..=b'9' => {
                self.number()?;
                self.reserve(Kind::Number, start)
            }
            b'n' => {
                self.literal(b"null")?;
                self.reserve(Kind::Null, start)
            }
            b't' => {
                self.literal(b"true")?;
                self.reserve(Kind::Bool, start)
            }
            b'f' => {
                self.literal(b"false")?;
                self.reserve(Kind::Bool, start)
            }
            _ => Err(self.error(JsonError::Syntax)),
        }
    }

    fn literal(&mut self, expected: &[u8]) -> Result<(), Error> {
        let remaining = &self.input[self.pos..];
        if remaining.starts_with(expected) {
            self.pos += expected.len();
            Ok(())
        } else if expected.starts_with(remaining) {
            Err(self.error(JsonError::UnexpectedEof))
        } else {
            Err(self.error(JsonError::Syntax))
        }
    }

    fn array(&mut self, depth: usize) -> Result<(), Error> {
        self.whitespace();
        if self.input.get(self.pos) == Some(&b']') {
            self.pos += 1;
            return Ok(());
        }
        loop {
            self.value(depth)?;
            self.whitespace();
            match self.input.get(self.pos) {
                Some(b']') => {
                    self.pos += 1;
                    return Ok(());
                }
                Some(b',') => self.pos += 1,
                None => return Err(self.error(JsonError::UnexpectedEof)),
                _ => return Err(self.error(JsonError::Syntax)),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<(), Error> {
        self.whitespace();
        if self.input.get(self.pos) == Some(&b'}') {
            self.pos += 1;
            return Ok(());
        }
        let mut head = NONE;
        let mut tail = NONE;
        let mut fields = 0usize;
        loop {
            self.whitespace();
            if fields >= self.limits.max_object_fields {
                return Err(Error::LimitExceeded {
                    kind: "JSON object fields",
                    limit: self.limits.max_object_fields,
                });
            }
            if self.input.get(self.pos) != Some(&b'"') {
                return Err(self.error(if self.pos == self.input.len() {
                    JsonError::UnexpectedEof
                } else {
                    JsonError::Syntax
                }));
            }
            let start = self.pos;
            self.string()?;
            let key = self.reserve(Kind::String, start)? as u32;
            if tail == NONE {
                head = key;
            } else {
                self.tokens[tail as usize].link = key;
            }
            tail = key;
            fields += 1;
            self.whitespace();
            if self.input.get(self.pos) != Some(&b':') {
                return Err(self.error(if self.pos == self.input.len() {
                    JsonError::UnexpectedEof
                } else {
                    JsonError::Syntax
                }));
            }
            self.pos += 1;
            self.value(depth)?;
            self.whitespace();
            match self.input.get(self.pos) {
                Some(b'}') => {
                    self.pos += 1;
                    return self.unique_keys(head, fields);
                }
                Some(b',') => self.pos += 1,
                None => return Err(self.error(JsonError::UnexpectedEof)),
                _ => return Err(self.error(JsonError::Syntax)),
            }
        }
    }

    fn key(&self, index: u32) -> JsonString<'_> {
        let token = self.tokens[index as usize];
        JsonString {
            raw: &self.input[token.start as usize..token.end as usize],
        }
    }

    // Bottom-up linked mergesort uses only otherwise-unused key link fields.
    // O(k log k) decoded comparisons, O(1) extra workspace; source order is intact.
    fn unique_keys(&mut self, mut head: u32, fields: usize) -> Result<(), Error> {
        let mut width = 1usize;
        while width < fields {
            let mut cursor = head;
            let mut new_head = NONE;
            let mut tail = NONE;
            while cursor != NONE {
                let mut left = cursor;
                let mut right = cursor;
                let mut left_count = 0usize;
                while left_count < width && right != NONE {
                    right = self.tokens[right as usize].link;
                    left_count += 1;
                }
                let mut right_count = width;
                while left_count != 0 || (right_count != 0 && right != NONE) {
                    let take_left = if left_count == 0 {
                        false
                    } else if right_count == 0 || right == NONE {
                        true
                    } else {
                        match self.key(left).bytes().cmp(self.key(right).bytes()) {
                            Ordering::Equal => {
                                return Err(Error::DuplicateKey {
                                    offset: self.tokens[right as usize].start as usize,
                                });
                            }
                            Ordering::Less => true,
                            Ordering::Greater => false,
                        }
                    };
                    let next = if take_left {
                        let selected = left;
                        left = self.tokens[left as usize].link;
                        left_count -= 1;
                        selected
                    } else {
                        let selected = right;
                        right = self.tokens[right as usize].link;
                        right_count -= 1;
                        selected
                    };
                    if tail == NONE {
                        new_head = next;
                    } else {
                        self.tokens[tail as usize].link = next;
                    }
                    tail = next;
                }
                cursor = right;
            }
            if tail != NONE {
                self.tokens[tail as usize].link = NONE;
            }
            head = new_head;
            width = width.saturating_mul(2);
        }
        Ok(())
    }

    fn string(&mut self) -> Result<(), Error> {
        self.pos += 1; // opening quotation mark was checked by the caller
        loop {
            let byte = *self
                .input
                .get(self.pos)
                .ok_or_else(|| self.error(JsonError::UnexpectedEof))?;
            match byte {
                b'"' => {
                    self.pos += 1;
                    return Ok(());
                }
                b'\\' => {
                    self.pos += 1;
                    let escaped = *self
                        .input
                        .get(self.pos)
                        .ok_or_else(|| self.error(JsonError::UnexpectedEof))?;
                    self.pos += 1;
                    match escaped {
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {}
                        b'u' => {
                            let first = self.hex4()?;
                            if (0xd800..=0xdbff).contains(&first) {
                                if self.input.get(self.pos..self.pos.saturating_add(2))
                                    != Some(b"\\u")
                                {
                                    return Err(self.error(JsonError::InvalidUnicode));
                                }
                                self.pos += 2;
                                let second = self.hex4()?;
                                if !(0xdc00..=0xdfff).contains(&second) {
                                    return Err(self.error(JsonError::InvalidUnicode));
                                }
                            } else if (0xdc00..=0xdfff).contains(&first) {
                                return Err(self.error(JsonError::InvalidUnicode));
                            }
                        }
                        _ => return Err(self.error(JsonError::InvalidEscape)),
                    }
                }
                0..=0x1f => return Err(self.error(JsonError::InvalidEscape)),
                _ => self.pos += 1,
            }
        }
    }

    fn hex4(&mut self) -> Result<u16, Error> {
        let mut value = 0u16;
        for _ in 0..4 {
            let byte = *self
                .input
                .get(self.pos)
                .ok_or_else(|| self.error(JsonError::UnexpectedEof))?;
            let digit = hex(byte).ok_or_else(|| self.error(JsonError::InvalidUnicode))?;
            value = value * 16 + u16::from(digit);
            self.pos += 1;
        }
        Ok(value)
    }

    fn number(&mut self) -> Result<(), Error> {
        if self.input.get(self.pos) == Some(&b'-') {
            self.pos += 1;
        }
        match self.input.get(self.pos) {
            Some(b'0') => {
                self.pos += 1;
                if matches!(self.input.get(self.pos), Some(b'0'..=b'9')) {
                    return Err(self.error(JsonError::InvalidNumber));
                }
            }
            Some(b'1'..=b'9') => {
                self.pos += 1;
                self.digits();
            }
            _ => return Err(self.error(JsonError::InvalidNumber)),
        }
        if self.input.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            let start = self.pos;
            self.digits();
            if start == self.pos {
                return Err(self.error(JsonError::InvalidNumber));
            }
        }
        if matches!(self.input.get(self.pos), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.input.get(self.pos), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            let start = self.pos;
            self.digits();
            if start == self.pos {
                return Err(self.error(JsonError::InvalidNumber));
            }
        }
        Ok(())
    }

    fn digits(&mut self) {
        while matches!(self.input.get(self.pos), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
    }
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// A borrowed validated JSON value. Cloning a view never clones input bytes.
#[derive(Clone, Copy, Debug)]
pub struct Value<'a, 't> {
    input: &'a [u8],
    tokens: &'t [Token],
    index: usize,
}

impl<'a, 't> Value<'a, 't> {
    pub fn raw(self) -> &'a [u8] {
        let token = self.tokens[self.index];
        &self.input[token.start as usize..token.end as usize]
    }

    pub fn kind(self) -> Kind {
        self.tokens[self.index].kind
    }

    pub fn is_null(self) -> bool {
        self.kind() == Kind::Null
    }

    pub fn as_string(self) -> Option<JsonString<'a>> {
        (self.kind() == Kind::String).then(|| JsonString { raw: self.raw() })
    }

    pub fn as_bool(self) -> Option<bool> {
        if self.kind() == Kind::Bool {
            Some(self.raw() == b"true")
        } else {
            None
        }
    }

    /// Integers remain decimal bytes unless explicitly requested as `u64`.
    pub fn as_u64(self) -> Option<u64> {
        if self.kind() != Kind::Number {
            return None;
        }
        let mut result = 0u64;
        for &byte in self.raw() {
            if !byte.is_ascii_digit() {
                return None;
            }
            result = result
                .checked_mul(10)?
                .checked_add(u64::from(byte - b'0'))?;
        }
        Some(result)
    }

    pub fn get(self, name: &str) -> Option<Self> {
        self.object()
            .find_map(|(key, value)| key.eq_str(name).then_some(value))
    }

    /// Non-array values produce an empty iterator; use `kind` for validation.
    pub fn array(self) -> Array<'a, 't> {
        Array {
            value: self,
            next: self.index + 1,
            end: if self.kind() == Kind::Array {
                self.tokens[self.index].next as usize
            } else {
                self.index + 1
            },
        }
    }

    /// Non-object values produce an empty iterator; use `kind` for validation.
    pub fn object(self) -> Object<'a, 't> {
        Object {
            value: self,
            next: self.index + 1,
            end: if self.kind() == Kind::Object {
                self.tokens[self.index].next as usize
            } else {
                self.index + 1
            },
        }
    }
}

#[derive(Clone, Debug)]
pub struct Array<'a, 't> {
    value: Value<'a, 't>,
    next: usize,
    end: usize,
}

impl<'a, 't> Iterator for Array<'a, 't> {
    type Item = Value<'a, 't>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        let value = Value {
            index: self.next,
            ..self.value
        };
        self.next = self.value.tokens[self.next].next as usize;
        Some(value)
    }
}

#[derive(Clone, Debug)]
pub struct Object<'a, 't> {
    value: Value<'a, 't>,
    next: usize,
    end: usize,
}

impl<'a, 't> Iterator for Object<'a, 't> {
    type Item = (JsonString<'a>, Value<'a, 't>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        let key = Value {
            index: self.next,
            ..self.value
        };
        let value = Value {
            index: self.next + 1,
            ..self.value
        };
        self.next = self.value.tokens[self.next + 1].next as usize;
        Some((JsonString { raw: key.raw() }, value))
    }
}

/// A JSON string including its original quoting and escapes.
#[derive(Clone, Copy, Debug)]
pub struct JsonString<'a> {
    raw: &'a [u8],
}

impl<'a> JsonString<'a> {
    pub fn raw(self) -> &'a [u8] {
        self.raw
    }

    /// Return a borrowed decoded string when no unescaping is necessary.
    pub fn as_str(self) -> Option<&'a str> {
        let bytes = &self.raw[1..self.raw.len() - 1];
        if bytes.contains(&b'\\') {
            None
        } else {
            core::str::from_utf8(bytes).ok()
        }
    }

    pub fn eq_str(self, expected: &str) -> bool {
        if let Some(plain) = self.as_str() {
            plain == expected
        } else {
            self.bytes().eq(expected.bytes())
        }
    }

    pub fn decoded_len(self) -> usize {
        if let Some(plain) = self.as_str() {
            plain.len()
        } else {
            self.bytes().count()
        }
    }

    /// On insufficient capacity, returns the exact decoded byte count.
    pub fn decode_into(self, output: &mut [u8]) -> Result<usize, Error> {
        let needed = self.decoded_len();
        if output.len() < needed {
            return Err(Error::OutputTooSmall { needed });
        }
        if let Some(plain) = self.as_str() {
            output[..needed].copy_from_slice(plain.as_bytes());
        } else {
            for (destination, byte) in output.iter_mut().zip(self.bytes()) {
                *destination = byte;
            }
        }
        Ok(needed)
    }

    pub fn bytes(self) -> DecodedBytes<'a> {
        DecodedBytes {
            raw: &self.raw[1..self.raw.len() - 1],
            pos: 0,
            pending: [0; 4],
            pending_pos: 0,
            pending_len: 0,
        }
    }
}

/// Decodes validated escapes lazily, using a four-byte UTF-8 staging array.
#[derive(Clone, Debug)]
pub struct DecodedBytes<'a> {
    raw: &'a [u8],
    pos: usize,
    pending: [u8; 4],
    pending_pos: usize,
    pending_len: usize,
}

impl DecodedBytes<'_> {
    fn code_unit(&mut self) -> u32 {
        let mut value = 0u32;
        for _ in 0..4 {
            value = (value << 4) | u32::from(hex(self.raw[self.pos]).unwrap_or(0));
            self.pos += 1;
        }
        value
    }
}

impl Iterator for DecodedBytes<'_> {
    type Item = u8;

    fn next(&mut self) -> Option<u8> {
        if self.pending_pos < self.pending_len {
            let byte = self.pending[self.pending_pos];
            self.pending_pos += 1;
            return Some(byte);
        }
        let byte = *self.raw.get(self.pos)?;
        self.pos += 1;
        if byte != b'\\' {
            return Some(byte);
        }
        let escaped = self.raw[self.pos];
        self.pos += 1;
        Some(match escaped {
            b'"' | b'\\' | b'/' => escaped,
            b'b' => 8,
            b'f' => 12,
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'u' => {
                let mut code = self.code_unit();
                if (0xd800..=0xdbff).contains(&code) {
                    self.pos += 2;
                    let low = self.code_unit();
                    code = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
                }
                // Validation guarantees a Unicode scalar; the fallback is unreachable.
                let scalar = char::from_u32(code).unwrap_or('\u{fffd}');
                self.pending_len = scalar.encode_utf8(&mut self.pending).len();
                self.pending_pos = 1;
                self.pending[0]
            }
            _ => escaped, // unreachable for validated strings
        })
    }
}

/// A bounded writer that continues counting after output capacity is exhausted.
///
/// `finish` returns the exact required length. A failed output is provisional and
/// must not be transmitted: retry with a larger slice. `raw` accepts trusted JSON
/// fragments; use `string` for data and `json_string` for validated source strings.
#[derive(Debug)]
pub struct Writer<'o> {
    output: &'o mut [u8],
    used: usize,
    error: Option<Error>,
}

impl<'o> Writer<'o> {
    pub fn new(output: &'o mut [u8]) -> Self {
        Self {
            output,
            used: 0,
            error: None,
        }
    }

    pub fn raw(&mut self, bytes: &[u8]) {
        let Some(end) = self.used.checked_add(bytes.len()) else {
            self.error = Some(Error::LimitExceeded {
                kind: "JSON output bytes",
                limit: usize::MAX,
            });
            return;
        };
        if self.used < self.output.len() {
            let count = bytes.len().min(self.output.len() - self.used);
            self.output[self.used..self.used + count].copy_from_slice(&bytes[..count]);
        }
        self.used = end;
    }

    pub fn byte(&mut self, byte: u8) {
        self.raw(&[byte]);
    }

    pub fn json_string(&mut self, value: JsonString<'_>) {
        self.raw(value.raw());
    }

    pub fn string(&mut self, bytes: &[u8]) {
        if let Err(error) = core::str::from_utf8(bytes) {
            self.error = Some(Error::InvalidJson {
                offset: error.valid_up_to(),
                kind: JsonError::InvalidUtf8,
            });
            return;
        }
        self.byte(b'"');
        let mut start = 0usize;
        for (index, &byte) in bytes.iter().enumerate() {
            if byte == b'"' || byte == b'\\' || byte < 0x20 {
                self.raw(&bytes[start..index]);
                match byte {
                    b'"' => self.raw(b"\\\""),
                    b'\\' => self.raw(b"\\\\"),
                    b'\n' => self.raw(b"\\n"),
                    b'\r' => self.raw(b"\\r"),
                    b'\t' => self.raw(b"\\t"),
                    8 => self.raw(b"\\b"),
                    12 => self.raw(b"\\f"),
                    _ => {
                        const HEX: &[u8; 16] = b"0123456789abcdef";
                        self.raw(&[
                            b'\\',
                            b'u',
                            b'0',
                            b'0',
                            HEX[(byte >> 4) as usize],
                            HEX[(byte & 15) as usize],
                        ]);
                    }
                }
                start = index + 1;
            }
        }
        self.raw(&bytes[start..]);
        self.byte(b'"');
    }

    pub fn written(&self) -> usize {
        self.used
    }

    pub fn finish(self) -> Result<usize, Error> {
        if let Some(error) = self.error {
            return Err(error);
        }
        if self.used > self.output.len() {
            Err(Error::OutputTooSmall { needed: self.used })
        } else {
            Ok(self.used)
        }
    }
}
