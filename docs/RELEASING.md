# Releases

## Credentials and scope

The release workflow reads the repository or environment secret named
`CARGO_REGISTRY_TOKEN` and passes it as the environment variable of the same name
to `cargo publish`. The token is never written into configuration or command-line
arguments. The publication job uses the `crates-io` GitHub Environment. Repository
owners can configure that environment's protection rules and permitted branches.
The repository workflow does not claim to configure those server-side rules.

All jobs have read-only repository permission by default. Only the publication
job receives `contents: write` to record the version, tag and GitHub release.
Third-party GitHub Actions are pinned to reviewed commit SHAs. Pull-request CI
never receives the publishing token. Release jobs are serialized and are not
cancelled midway by a newer release run.

## Manual release

1. Open GitHub Actions → Release → Run workflow on `main`.
2. Choose `bump=none` for the version already prepared in the manifest, or supply an explicit version
   with `bump=none`.
3. Leave `publish=false` to verify code, package contents and registry state and
   build the package. The final Git-reference preflight runs in the publish job.
4. Set `publish=true` for publication once the registry token is configured.

Later releases support `patch`, `minor`, or `major`. An explicit stable SemVer
also works with `bump=none`; conflicting inputs, decreasing versions, leading
zeroes, invalid tags and shell-shaped values are rejected by the Python gate.
Version text is received through environment variables, never interpolated into
shell source. Rust 1.99.1 writes the lockfile so it remains usable by the MSRV.
The local zconv entries in the independent comparison and fuzz lockfiles are
updated at the same time, without changing their registry dependencies.

A pushed `vMAJOR.MINOR.PATCH` tag is another release entry point. Its version must
exactly match Cargo.toml. The workflow validates MSRV, stable tests, formatting,
Clippy, zero-dependency packaging and the external JSON oracle before publishing.
Immediately before publication it verifies the checked-out commit, the current
remote main for manual runs, and any existing version tag. A stale branch or a
tag pointing at different source stops publication.

## Registry name and ownership checks

The initial crates.io API lookup returned an explicit 404 for zconv before
initialization. The registry gate repeats the lookup immediately before a first
publication. For subsequent publication it verifies the crate's repository and
owner, and rejects an already-published version. If another project has acquired
the name, publication stops; the package, repository references and documentation
must be renamed together before using the fallback name `zaiwire`.

## Recovering a partial external failure

Registry publication is immutable. If crates.io accepted a version but the later
Git push or GitHub release creation failed, do not bump and republish the same
change merely to finish those operations. Verify the existing published version
and complete its Git version commit/tag/release from the verified workflow SHA.
The registry gate deliberately rejects duplicate publication attempts.

## Local release verification

```sh
cargo +1.99.1 test --all-targets --locked
cargo test --release
cargo test --doc
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
python3 scripts/check_package.py
python3 -m unittest discover -s scripts -p 'test_*.py'
cargo test --manifest-path comparison/Cargo.toml --locked
cargo package --locked
```

Cross-target checks and Miri run in CI. Benchmarks report measurements; they do
not use a noisy shared-runner timing threshold as a release correctness gate.
