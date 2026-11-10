//! Routing, resource accounting, and caller-owned conversion buffers.

use crate::ir::{Context, EncodeReport, Policy, Presence, Record, Text};
use crate::{direct, json, protocol, Error, Format, Operation, Path, Strategy};

/// Controls selection and permitted semantic losses. The default is strict.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options<'a> {
    pub strategy: Strategy,
    pub policy: Policy,
    pub context: Context<'a>,
    pub limits: json::Limits,
}

/// All transient and semantic storage is provided by the caller.
///
/// Token storage can be reused after conversion: IR JSON fragments borrow input
/// bytes, not the token tape. IR records continue borrowing the input until the
/// caller releases/reinitializes their arena. Output and scratch never alias input.
pub struct Workspace<'w, 'a> {
    pub tokens: &'w mut [json::Token],
    pub records: &'w mut [Record<'a>],
    pub scratch: &'w mut [u8],
    pub nested_tokens: &'w mut [json::Token],
}

/// Explicit memory accounting. Reserved capacities are not claimed as peak usage.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Resources {
    pub input_bytes: usize,
    pub output_bytes: usize,
    pub output_capacity: usize,
    pub tokens_used: usize,
    pub token_storage_bytes: usize,
    pub ir_records_used: usize,
    pub ir_storage_bytes: usize,
    pub scratch_capacity: usize,
    pub nested_token_storage_bytes: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct Conversion<'a> {
    pub path: Path,
    pub report: EncodeReport<'a>,
    pub resources: Resources,
}

impl Conversion<'_> {
    pub const fn written(&self) -> usize {
        self.report.written
    }
}

/// Convert a complete request or response.
///
/// On failure no output is committed: discard the output slice's contents.
/// `OutputTooSmall` gives the exact required capacity; retry with the original
/// input and a larger output. Workspace errors give a lower bound. Only
/// `DirectUnavailable` permits automatic fallback. JSON and source semantics are
/// validated even for identity conversion.
pub fn convert<'a>(
    source: Format,
    target: Format,
    operation: Operation,
    input: &'a [u8],
    output: &mut [u8],
    options: Options<'a>,
    workspace: Workspace<'_, 'a>,
) -> Result<Conversion<'a>, Error> {
    let mut resources = Resources {
        input_bytes: input.len(),
        output_capacity: output.len(),
        token_storage_bytes: core::mem::size_of_val(workspace.tokens),
        ir_storage_bytes: core::mem::size_of_val(workspace.records),
        scratch_capacity: workspace.scratch.len(),
        nested_token_storage_bytes: core::mem::size_of_val(workspace.nested_tokens),
        ..Resources::default()
    };
    let doc = json::parse(input, workspace.tokens, options.limits)?;
    resources.tokens_used = doc.tokens_used();
    let root = doc.root();
    if options.strategy != Strategy::Ir && source == target {
        protocol::validate(source, operation, root)?;
        let mut writer = json::Writer::new(output);
        writer.raw(input);
        let written = writer.finish()?;
        resources.output_bytes = written;
        return Ok(Conversion {
            path: Path::Identity,
            report: EncodeReport {
                written,
                transport_model: if operation == Operation::Request {
                    root.get("model")
                        .and_then(|v| v.as_string())
                        .map(Text::Json)
                        .or(options.context.model.map(Text::Utf8))
                        .map_or(Presence::Missing, Presence::Value)
                } else {
                    Presence::Missing
                },
                transport_stream: root
                    .get("stream")
                    .and_then(|v| v.as_bool())
                    .map_or(Presence::Missing, Presence::Value),
                ..EncodeReport::default()
            },
            resources,
        });
    }
    if options.strategy != Strategy::Ir {
        match direct::convert(
            source,
            target,
            operation,
            root,
            &options.context,
            options.policy,
            output,
            workspace.scratch,
            workspace.nested_tokens,
        ) {
            Ok(report) => {
                resources.output_bytes = report.written;
                return Ok(Conversion {
                    path: Path::Direct,
                    report,
                    resources,
                });
            }
            Err(error @ Error::DirectUnavailable { .. }) => {
                // Unsupported features do not conceal malformed source semantics.
                // IR decoding performs that validation itself on Auto fallback.
                if options.strategy == Strategy::Direct {
                    protocol::validate(source, operation, root)?;
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    }
    let report = match operation {
        Operation::Request => {
            let request = protocol::decode_request(source, root, workspace.records)?;
            resources.ir_records_used = request.records.len();
            protocol::encode_request(
                &request,
                target,
                &options.context,
                options.policy,
                output,
                workspace.scratch,
                workspace.nested_tokens,
            )?
        }
        Operation::Response => {
            let response = protocol::decode_response(source, root, workspace.records)?;
            resources.ir_records_used = response.records.len();
            protocol::encode_response(
                &response,
                target,
                &options.context,
                options.policy,
                output,
                workspace.scratch,
                workspace.nested_tokens,
            )?
        }
    };
    resources.output_bytes = report.written;
    Ok(Conversion {
        path: Path::Ir,
        report,
        resources,
    })
}
