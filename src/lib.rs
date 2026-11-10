#![no_std]
#![forbid(unsafe_code)]
//! Allocation-free conversion between model request, response, and event protocols.
//!
//! Input bytes are borrowed. The caller supplies token, IR, output and stream storage.
//! See the repository's support matrix for representability and context requirements.

pub mod json;
mod types;
pub use types::*;
