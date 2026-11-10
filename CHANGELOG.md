# Changelog

## 0.1.0

- Six independent AI wire protocol adapters with borrowed request/response IR.
- Auto, Direct and Ir routing with validated identity and narrow fallback.
- Caller-owned token, output, semantic, streaming and aggregation buffers.
- Stable Rust 1.74 MSRV; always no_std; no third-party dependencies or unsafe core.
- Full JSON validation, decoded duplicate-key rejection and bounded resource use.
- Incremental SSE conversion with resumable backpressure and explicit EOF.
- Strict provenance, field-specific downgrade reports and integer usage mapping.
- Native tests, semantic and external JSON differential tests, allocation oracle,
  Miri corpus, fuzz workspaces, reproducible benchmarks and guarded releases.
