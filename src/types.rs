//! Protocol identities and structured errors shared by every conversion path.

/// A wire protocol, independent of API version and vendor profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Protocol {
    OpenAiCompletions,
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
    GeminiGenerateContent,
    GeminiInteractions,
}

/// Selects conversion implementation. Only capability errors permit Auto fallback.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Strategy {
    #[default]
    Auto,
    Direct,
    Ir,
}

/// The implementation that actually produced the output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Path {
    Identity,
    Direct,
    Ir,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Request,
    Response,
}

/// Provider identity for extension isolation. Standard means the documented API.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Profile {
    #[default]
    Standard,
    OpenAiCompatible(&'static str),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Format {
    pub protocol: Protocol,
    pub version: &'static str,
    pub profile: Profile,
}

impl Format {
    /// Official protocol snapshot; documentation was retrieved on 2026-11-10.
    pub const fn new(protocol: Protocol) -> Self {
        let version = match protocol {
            Protocol::AnthropicMessages => "2023-06-01",
            Protocol::GeminiGenerateContent => "v1beta",
            Protocol::GeminiInteractions => "v1",
            _ => "v1",
        };
        Self {
            protocol,
            version,
            profile: Profile::Standard,
        }
    }
}

impl From<Protocol> for Format {
    fn from(protocol: Protocol) -> Self {
        Self::new(protocol)
    }
}

/// Capacity `needed` is exact for completed output counts, a lower bound for
/// workspaces and incomplete streaming input. No core error performs I/O.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Error {
    InvalidJson {
        offset: usize,
        kind: crate::json::JsonError,
    },
    DuplicateKey {
        offset: usize,
    },
    DepthLimitExceeded {
        limit: usize,
    },
    InputTooLarge {
        limit: usize,
    },
    WorkspaceTooSmall {
        needed: usize,
    },
    /// Exact decoded byte capacity required for embedded JSON validation.
    ScratchTooSmall {
        needed: usize,
    },
    OutputTooSmall {
        needed: usize,
    },
    IrTooSmall {
        needed: usize,
    },
    LimitExceeded {
        kind: &'static str,
        limit: usize,
    },
    InvalidInput {
        field: &'static str,
        reason: &'static str,
    },
    Unsupported {
        feature: &'static str,
    },
    ContextRequired {
        field: &'static str,
    },
    DirectUnavailable {
        feature: &'static str,
    },
    PolicyViolation {
        feature: &'static str,
    },
    EventTooLarge {
        needed: usize,
    },
    TooManyActiveItems {
        needed: usize,
    },
    TruncatedStream,
    AlreadyFinished,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}
