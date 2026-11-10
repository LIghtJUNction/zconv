#![no_std]
#![forbid(unsafe_code)]
//! Allocation-free conversion between model request, response, and event protocols.
//!
//! Input bytes are borrowed. The caller supplies token, IR, output and stream storage.
//! See the repository's support matrix for representability and context requirements.

pub mod json;
pub mod ir;
pub mod protocol;
pub mod direct;
pub mod sse;
pub mod stream;
mod convert;
mod types;
pub use convert::{convert, Conversion, Options, Resources, Workspace};
pub use types::*;
