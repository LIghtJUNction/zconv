use super::{invalid, write_u64};
use crate::{Error, json::Writer};
use core::fmt::Write as _;

/// Inputs remain borrowed; tokenization belongs to the caller/model.
#[derive(Clone, Copy, Debug)]
pub enum EmbeddingInput<'a> {
    Text(&'a str),
    Texts(&'a [&'a str]),
    Tokens(&'a [u32]),
    TokenBatches(&'a [&'a [u32]]),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Encoding {
    Float,
    Base64,
}

#[derive(Clone, Copy, Debug)]
pub struct EmbeddingsRequest<'a> {
    pub model: &'a str,
    pub input: EmbeddingInput<'a>,
    pub dimensions: Option<u32>,
    pub encoding_format: Option<Encoding>,
    pub user: Option<&'a str>,
}

impl<'a> EmbeddingsRequest<'a> {
    pub const fn new(model: &'a str, input: EmbeddingInput<'a>) -> Self {
        Self {
            model,
            input,
            dimensions: None,
            encoding_format: None,
            user: None,
        }
    }
    pub fn encode(self, output: &mut [u8]) -> Result<usize, Error> {
        if self.model.is_empty() {
            return Err(invalid("model", "must not be empty"));
        }
        if self.dimensions == Some(0) {
            return Err(invalid("dimensions", "must be positive"));
        }
        let mut w = Writer::new(output);
        w.raw(b"{\"model\":");
        w.string(self.model.as_bytes());
        w.raw(b",\"input\":");
        match self.input {
            EmbeddingInput::Text(s) => {
                nonempty(s)?;
                w.string(s.as_bytes());
            }
            EmbeddingInput::Texts(texts) => {
                if texts.is_empty() {
                    return Err(invalid("input", "empty batch"));
                }
                w.byte(b'[');
                for (i, text) in texts.iter().enumerate() {
                    nonempty(text)?;
                    if i != 0 {
                        w.byte(b',');
                    }
                    w.string(text.as_bytes());
                }
                w.byte(b']');
            }
            EmbeddingInput::Tokens(tokens) => tokens_json(&mut w, tokens)?,
            EmbeddingInput::TokenBatches(batches) => {
                if batches.is_empty() {
                    return Err(invalid("input", "empty batch"));
                }
                w.byte(b'[');
                for (i, tokens) in batches.iter().enumerate() {
                    if i != 0 {
                        w.byte(b',');
                    }
                    tokens_json(&mut w, tokens)?;
                }
                w.byte(b']');
            }
        }
        if let Some(d) = self.dimensions {
            w.raw(b",\"dimensions\":");
            write_u64(&mut w, u64::from(d));
        }
        if let Some(format) = self.encoding_format {
            w.raw(b",\"encoding_format\":");
            w.string(match format {
                Encoding::Float => b"float",
                Encoding::Base64 => b"base64",
            });
        }
        if let Some(user) = self.user {
            w.raw(b",\"user\":");
            w.string(user.as_bytes());
        }
        w.byte(b'}');
        w.finish()
    }
}

fn nonempty(s: &str) -> Result<(), Error> {
    if s.is_empty() {
        Err(invalid("input", "must not be empty"))
    } else {
        Ok(())
    }
}
fn tokens_json(w: &mut Writer<'_>, tokens: &[u32]) -> Result<(), Error> {
    if tokens.is_empty() {
        return Err(invalid("input", "empty token batch"));
    }
    w.byte(b'[');
    for (i, token) in tokens.iter().enumerate() {
        if i != 0 {
            w.byte(b',');
        }
        write_u64(w, u64::from(*token));
    }
    w.byte(b']');
    Ok(())
}

#[derive(Clone, Copy, Debug)]
pub enum EmbeddingVector<'a> {
    Floats(&'a [f32]),
    Base64(&'a str),
}

#[derive(Clone, Copy, Debug)]
pub struct Embedding<'a> {
    pub index: u64,
    pub vector: EmbeddingVector<'a>,
}

#[derive(Clone, Copy, Debug)]
pub struct EmbeddingsResponse<'a> {
    pub model: &'a str,
    pub data: &'a [Embedding<'a>],
    pub prompt_tokens: u64,
    pub total_tokens: u64,
}

struct Fmt<'w, 'o>(&'w mut Writer<'o>);
impl core::fmt::Write for Fmt<'_, '_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0.raw(s.as_bytes());
        Ok(())
    }
}

impl EmbeddingsResponse<'_> {
    pub fn encode(self, output: &mut [u8]) -> Result<usize, Error> {
        if self.model.is_empty() {
            return Err(invalid("model", "must not be empty"));
        }
        let mut w = Writer::new(output);
        w.raw(b"{\"object\":\"list\",\"model\":");
        w.string(self.model.as_bytes());
        w.raw(b",\"data\":[");
        for (i, embedding) in self.data.iter().enumerate() {
            if i != 0 {
                w.byte(b',');
            }
            w.raw(b"{\"object\":\"embedding\",\"index\":");
            write_u64(&mut w, embedding.index);
            w.raw(b",\"embedding\":");
            match embedding.vector {
                EmbeddingVector::Base64(s) => w.string(s.as_bytes()),
                EmbeddingVector::Floats(values) => {
                    w.byte(b'[');
                    for (n, value) in values.iter().enumerate() {
                        if !value.is_finite() {
                            return Err(invalid("embedding", "expected finite values"));
                        }
                        if n != 0 {
                            w.byte(b',');
                        }
                        write!(Fmt(&mut w), "{value}")
                            .map_err(|_| invalid("embedding", "number formatting failed"))?;
                    }
                    w.byte(b']');
                }
            }
            w.byte(b'}');
        }
        w.raw(b"],\"usage\":{\"prompt_tokens\":");
        write_u64(&mut w, self.prompt_tokens);
        w.raw(b",\"total_tokens\":");
        write_u64(&mut w, self.total_tokens);
        w.raw(b"}}");
        w.finish()
    }
}
