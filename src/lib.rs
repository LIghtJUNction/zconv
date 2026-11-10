#![no_std]
#![forbid(unsafe_code)]
//! Allocation-free conversion between model request, response, and event protocols.
//!
//! Input bytes are borrowed. The caller supplies token, IR, output and stream storage.
//! See the repository's support matrix for representability and context requirements.
//!
//! # Complete request conversion
//!
//! ```
//! use zconv::{convert, Operation, Options, Path, Protocol, Workspace};
//! use zconv::json::Token;
//!
//! # fn main() -> Result<(), zconv::Error> {
//! let input = br#"{"model":"example","messages":[{"role":"user","content":"Hello"}]}"#;
//! let mut tokens = [Token::EMPTY; 32];
//! let mut output = [0; 256];
//! let result = convert(
//!     Protocol::OpenAiChat.into(), Protocol::OpenAiResponses.into(),
//!     Operation::Request, input, &mut output, Options::default(),
//!     Workspace { tokens: &mut tokens, records: &mut [],
//!                 scratch: &mut [], nested_tokens: &mut [] },
//! )?;
//! assert_eq!(result.path, Path::Direct);
//! assert_eq!(result.resources.ir_records_used, 0);
//! assert!(result.written() > 0);
//! # Ok(())
//! # }
//! ```
//!
//! Empty IR storage is sufficient for this direct shape. Auto fallback for other
//! features needs caller-provided [`ir::Record`] storage. Use [`protocol`] to
//! decode, inspect and edit IR explicitly, or [`stream::StreamConverter`] for
//! resumable SSE conversion with output backpressure.

mod convert;
pub mod direct;
pub mod ir;
pub mod json;
pub mod protocol;
pub mod sse;
pub mod stream;
mod types;
pub use convert::{convert, Conversion, Options, Resources, Workspace};
pub use types::*;
