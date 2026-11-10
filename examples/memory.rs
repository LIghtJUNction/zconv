//! Inspect the caller storage required by one fixed request fixture.
//!
//! The outer demonstration uses `std` to discover capacities and allocate the
//! final buffers. Each conversion receives those buffers fully allocated; the
//! conversion core performs no allocation. `tests/allocation.rs` separately
//! checks the allocation contract with a counting allocator.
//!
//! Counts below describe the provided slices. Stack frames, allocator bookkeeping,
//! the probing process, and process RSS are outside this accounting. Capacities
//! are specific to this fixture; different requests can require different sizes.

use std::mem::size_of;
use zconv::ir::Record;
use zconv::json::{self, Token};
use zconv::{convert, Error, Operation, Options, Protocol, Strategy, Workspace};

const INPUT: &[u8] = br#"{"model":"example-model","messages":[{"role":"user","content":"Hello"}]}"#;

#[derive(Clone, Copy)]
struct Capacities {
    tokens: usize,
    records: usize,
    output: usize,
}

fn token_count() -> Result<usize, Error> {
    let mut tokens = Vec::new();
    loop {
        match json::parse(INPUT, &mut tokens, json::Limits::default()) {
            Ok(document) => return Ok(document.tokens_used()),
            Err(Error::WorkspaceTooSmall { needed }) => tokens.resize(needed, Token::EMPTY),
            Err(error) => return Err(error),
        }
    }
}

fn discover(strategy: Strategy, tokens_needed: usize) -> Result<Capacities, Error> {
    let mut tokens = vec![Token::EMPTY; tokens_needed];
    let mut records = Vec::new();
    let mut output = Vec::new();
    loop {
        let result = convert(
            Protocol::OpenAiChat.into(),
            Protocol::OpenAiResponses.into(),
            Operation::Request,
            INPUT,
            &mut output,
            Options {
                strategy,
                ..Options::default()
            },
            Workspace {
                tokens: &mut tokens,
                records: &mut records,
                scratch: &mut [],
                nested_tokens: &mut [],
            },
        );
        match result {
            Ok(converted) => {
                return Ok(Capacities {
                    tokens: converted.resources.tokens_used,
                    records: converted.resources.ir_records_used,
                    output: converted.written(),
                });
            }
            // IR capacity errors report a lower bound; retry until it suffices.
            Err(Error::IrTooSmall { needed }) => records.resize(needed, Record::EMPTY),
            // The writer has counted the complete output, so this length is exact.
            Err(Error::OutputTooSmall { needed }) => output.resize(needed, 0),
            Err(error) => return Err(error),
        }
    }
}

fn show(strategy: Strategy, capacity: Capacities) -> Result<(), Error> {
    // Allocate only the capacities discovered for this fixture. Probing buffers
    // have already been dropped before this function is called.
    let mut tokens = vec![Token::EMPTY; capacity.tokens].into_boxed_slice();
    let mut records = vec![Record::EMPTY; capacity.records].into_boxed_slice();
    let mut output = vec![0; capacity.output].into_boxed_slice();

    let converted = convert(
        Protocol::OpenAiChat.into(),
        Protocol::OpenAiResponses.into(),
        Operation::Request,
        INPUT,
        &mut output,
        Options {
            strategy,
            ..Options::default()
        },
        Workspace {
            tokens: &mut tokens,
            records: &mut records,
            scratch: &mut [],
            nested_tokens: &mut [],
        },
    )?;

    let resources = converted.resources;
    assert_eq!(resources.tokens_used, capacity.tokens);
    assert_eq!(resources.ir_records_used, capacity.records);
    assert_eq!(resources.output_bytes, capacity.output);
    let provided = resources.output_capacity
        + resources.token_storage_bytes
        + resources.ir_storage_bytes
        + resources.scratch_capacity
        + resources.nested_token_storage_bytes;
    println!(
        "{:?},{},{},{},{},{},{},{},{},{},{}",
        converted.path,
        resources.input_bytes,
        resources.output_capacity,
        resources.tokens_used,
        resources.token_storage_bytes,
        resources.ir_records_used,
        resources.ir_storage_bytes,
        resources.scratch_capacity,
        resources.nested_token_storage_bytes,
        provided,
        resources.input_bytes + provided,
    );
    Ok(())
}

fn main() -> Result<(), Error> {
    let tokens_needed = token_count()?;
    let direct = discover(Strategy::Direct, tokens_needed)?;
    let ir = discover(Strategy::Ir, tokens_needed)?;
    println!("Fixture-specific caller buffer capacities for Chat -> Responses:");
    println!("input={}", String::from_utf8_lossy(INPUT));
    println!(
        "sizeof(Token)={}, sizeof(Record)={} bytes",
        size_of::<Token>(),
        size_of::<Record<'_>>()
    );
    println!("path,input_bytes,output_bytes,token_slots,token_bytes,ir_slots,ir_bytes,scratch_bytes,nested_token_bytes,provided_bytes,input_plus_provided_bytes");
    show(Strategy::Direct, direct)?;
    show(Strategy::Ir, ir)?;
    Ok(())
}
