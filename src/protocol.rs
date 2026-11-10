//! Request and complete-response adapters, with explicit source ownership.

mod decode;
mod encode;

pub use decode::{decode_request, decode_response, validate};
pub use encode::{encode_request, encode_response};

use crate::{Error, Format, Profile, Protocol};

/// Reject unknown wire versions rather than quietly applying a nearby schema.
pub fn validate_format(format: Format) -> Result<(), Error> {
    let version = match format.protocol {
        Protocol::AnthropicMessages => matches!(format.version, "2023-06-01" | "2026-10-21"),
        Protocol::GeminiGenerateContent => matches!(format.version, "v1" | "v1beta"),
        Protocol::GeminiInteractions => format.version == "v1",
        _ => format.version == "v1",
    };
    if !version { return Err(Error::Unsupported { feature: "protocol version" }); }
    if !matches!(format.profile, Profile::Standard) && !matches!(format.protocol, Protocol::OpenAiChat | Protocol::OpenAiResponses | Protocol::OpenAiCompletions) {
        return Err(Error::Unsupported { feature: "profile is only defined for OpenAI-compatible protocols" });
    }
    Ok(())
}

pub(crate) fn invalid(field: &'static str, reason: &'static str) -> Error {
    Error::InvalidInput { field, reason }
}
