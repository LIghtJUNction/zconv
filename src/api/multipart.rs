use super::{Endpoint, invalid, write_u64};
use crate::{Error, json::Writer};

/// A borrowed multipart field. Binary payloads are copied without UTF-8 decoding.
#[derive(Clone, Copy, Debug)]
pub struct Part<'a> {
    pub name: &'a str,
    pub filename: Option<&'a str>,
    pub content_type: Option<&'a str>,
    pub data: &'a [u8],
}

impl<'a> Part<'a> {
    pub const fn text(name: &'a str, value: &'a str) -> Self {
        Self {
            name,
            filename: None,
            content_type: None,
            data: value.as_bytes(),
        }
    }
    pub const fn file(
        name: &'a str,
        filename: &'a str,
        content_type: &'a str,
        data: &'a [u8],
    ) -> Self {
        Self {
            name,
            filename: Some(filename),
            content_type: Some(content_type),
            data,
        }
    }
}

/// Construct a Files upload, image edit or legacy variation body.
/// The caller supplies an RFC 2046 boundary and sets Content-Type with that boundary.
/// Boundary collisions and header injection are rejected before writing output.
pub fn encode_multipart(
    endpoint: Endpoint,
    boundary: &str,
    parts: &[Part<'_>],
    output: &mut [u8],
) -> Result<usize, Error> {
    if boundary.is_empty()
        || boundary.len() > 70
        || !boundary
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"'()+_,-./:=?".contains(&b))
    {
        return Err(invalid("boundary", "invalid multipart boundary"));
    }
    let mut file = false;
    let mut purpose = false;
    let mut image = false;
    let mut prompt = false;
    for (index, part) in parts.iter().enumerate() {
        header(part.name)?;
        if part.name.is_empty() {
            return Err(invalid("name", "empty multipart name"));
        }
        if !part.name.ends_with("[]") && parts[..index].iter().any(|p| p.name == part.name) {
            return Err(invalid("name", "duplicate multipart field"));
        }
        if let Some(filename) = part.filename {
            header(filename)?;
            if filename.is_empty() {
                return Err(invalid("filename", "must not be empty"));
            }
        }
        if let Some(mime) = part.content_type {
            header(mime)?;
            if mime.is_empty() {
                return Err(invalid("content_type", "must not be empty"));
            }
        }
        if part.filename.is_none() && core::str::from_utf8(part.data).is_err() {
            return Err(invalid("field", "text multipart fields require UTF-8"));
        }
        if collision(part.data, boundary.as_bytes()) {
            return Err(invalid("boundary", "boundary occurs in part body"));
        }
        file |= part.name == "file" && part.filename.is_some();
        image |= matches!(part.name, "image" | "image[]") && part.filename.is_some();
        if part.name == "purpose" {
            purpose = !part.data.is_empty() && part.filename.is_none();
            if core::str::from_utf8(part.data).is_err() {
                return Err(invalid("purpose", "expected UTF-8"));
            }
        }
        prompt |= part.name == "prompt" && !part.data.is_empty() && part.filename.is_none();
    }
    match endpoint {
        Endpoint::FileUpload if !file || !purpose => {
            return Err(invalid("file", "upload requires file and purpose"));
        }
        Endpoint::ImageEdits if !image || !prompt => {
            return Err(invalid("image", "edit requires image and prompt"));
        }
        Endpoint::ImageVariations if !image => {
            return Err(invalid("image", "variation requires image"));
        }
        Endpoint::FileUpload | Endpoint::ImageEdits | Endpoint::ImageVariations => {}
        _ => return Err(invalid("endpoint", "endpoint does not use multipart")),
    }
    let mut w = Writer::new(output);
    for part in parts {
        w.raw(b"--");
        w.raw(boundary.as_bytes());
        w.raw(b"\r\nContent-Disposition: form-data; name=\"");
        w.raw(part.name.as_bytes());
        w.byte(b'"');
        if let Some(filename) = part.filename {
            w.raw(b"; filename=\"");
            w.raw(filename.as_bytes());
            w.byte(b'"');
        }
        if let Some(mime) = part.content_type {
            w.raw(b"\r\nContent-Type: ");
            w.raw(mime.as_bytes());
        }
        w.raw(b"\r\n\r\n");
        w.raw(part.data);
        w.raw(b"\r\n");
    }
    w.raw(b"--");
    w.raw(boundary.as_bytes());
    w.raw(b"--\r\n");
    w.finish()
}

fn header(value: &str) -> Result<(), Error> {
    if value
        .bytes()
        .any(|b| b.is_ascii_control() || matches!(b, b'"' | b'\\'))
    {
        Err(invalid(
            "multipart header",
            "control or quoting characters are forbidden",
        ))
    } else {
        Ok(())
    }
}
fn collision(data: &[u8], boundary: &[u8]) -> bool {
    if data.starts_with(b"--") && data[2..].starts_with(boundary) {
        return true;
    }
    data.windows(boundary.len() + 4)
        .any(|w| w.starts_with(b"\r\n--") && &w[4..] == boundary)
}

#[derive(Clone, Copy, Debug)]
pub struct FileUpload<'a> {
    pub filename: &'a str,
    pub content_type: &'a str,
    pub contents: &'a [u8],
    pub purpose: &'a str,
    pub expires_after: Option<u32>,
}

impl FileUpload<'_> {
    pub fn encode(self, boundary: &str, output: &mut [u8]) -> Result<usize, Error> {
        if self
            .expires_after
            .is_some_and(|s| !(3600..=2_592_000).contains(&s))
        {
            return Err(invalid(
                "expires_after",
                "seconds must be between 3600 and 2592000",
            ));
        }
        let mut digits = [0u8; 20];
        let mut w = Writer::new(&mut digits);
        write_u64(&mut w, u64::from(self.expires_after.unwrap_or(0)));
        let n = w.finish()?;
        let parts = [
            Part::file("file", self.filename, self.content_type, self.contents),
            Part::text("purpose", self.purpose),
            Part::text("expires_after[anchor]", "created_at"),
            Part {
                name: "expires_after[seconds]",
                filename: None,
                content_type: None,
                data: &digits[..n],
            },
        ];
        encode_multipart(
            Endpoint::FileUpload,
            boundary,
            &parts[..if self.expires_after.is_some() { 4 } else { 2 }],
            output,
        )
    }
}
