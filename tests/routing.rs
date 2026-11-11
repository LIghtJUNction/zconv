use zconv::ir::{Context, Policy, Record};
use zconv::json::Token;
use zconv::{Error, Format, Operation, Options, Path, Protocol, Strategy, Workspace, convert};

const CHAT: &[u8] = br#"{"model":"m","messages":[{"role":"user","content":"hello"}]}"#;

fn run(
    source: Format,
    target: Format,
    strategy: Strategy,
    input: &[u8],
    output: &mut [u8],
    ir_capacity: usize,
) -> Result<(Path, usize, usize), Error> {
    let mut tokens = [Token::EMPTY; 256];
    let mut nested = [Token::EMPTY; 128];
    let mut records = vec![Record::EMPTY; ir_capacity];
    let mut scratch = [0; 2048];
    let result = convert(
        source,
        target,
        Operation::Request,
        input,
        output,
        Options {
            strategy,
            context: Context {
                max_tokens: Some(32),
                ..Context::default()
            },
            ..Options::default()
        },
        Workspace {
            tokens: &mut tokens,
            records: &mut records,
            scratch: &mut scratch,
            nested_tokens: &mut nested,
        },
    )?;
    Ok((
        result.path,
        result.written(),
        result.resources.ir_records_used,
    ))
}

#[test]
fn auto_uses_direct_without_any_ir_storage() {
    let mut output = [0; 1024];
    for target in [
        Protocol::OpenAiResponses,
        Protocol::AnthropicMessages,
        Protocol::GeminiGenerateContent,
    ] {
        let (path, _, used) = run(
            Protocol::OpenAiChat.into(),
            target.into(),
            Strategy::Auto,
            CHAT,
            &mut output,
            0,
        )
        .unwrap();
        assert_eq!(path, Path::Direct);
        assert_eq!(used, 0);
    }
}

#[test]
fn identity_is_validated_and_ir_is_really_forced() {
    let mut output = [0; 1024];
    let format = Format::new(Protocol::OpenAiChat);
    let (path, length, used) = run(format, format, Strategy::Auto, CHAT, &mut output, 0).unwrap();
    assert_eq!(path, Path::Identity);
    assert_eq!(&output[..length], CHAT);
    assert_eq!(used, 0);
    assert!(matches!(
        run(format, format, Strategy::Ir, CHAT, &mut output, 0),
        Err(Error::IrTooSmall { .. })
    ));
    assert_eq!(
        run(format, format, Strategy::Ir, CHAT, &mut output, 16)
            .unwrap()
            .0,
        Path::Ir
    );
    assert!(matches!(
        run(format, format, Strategy::Auto, b"{", &mut output, 0),
        Err(Error::InvalidJson { .. })
    ));
    assert!(matches!(
        run(
            format,
            format,
            Strategy::Auto,
            br#"{"model":"m","messages":false}"#,
            &mut output,
            0
        ),
        Err(Error::InvalidInput { .. })
    ));
    assert!(matches!(
        run(
            format,
            format,
            Strategy::Direct,
            br#"{"model":"m","messages":[],"x":{"a":0,"\u0061":1}}"#,
            &mut output,
            0
        ),
        Err(Error::DuplicateKey { .. })
    ));
}

#[test]
fn only_capability_errors_trigger_fallback() {
    let source = Format::new(Protocol::OpenAiChat);
    let target = Format::new(Protocol::OpenAiResponses);
    let mut empty = [];
    // A fallback here would fail IrTooSmall, hiding the precise output error.
    let direct = run(source, target, Strategy::Direct, CHAT, &mut empty, 0).unwrap_err();
    assert!(matches!(direct, Error::OutputTooSmall { .. }));
    assert_eq!(
        run(source, target, Strategy::Auto, CHAT, &mut empty, 0).unwrap_err(),
        direct
    );
    let mut output = [0; 4096];
    // Tool requests now have a direct Responses adapter. Anthropic tools still
    // exercise the feature-specific fallback of an otherwise direct direction.
    let target = Format::new(Protocol::AnthropicMessages);
    let tools = br#"{"model":"m","messages":[{"role":"user","content":"call f"}],"tools":[{"type":"function","function":{"name":"f","parameters":{"type":"object"}}}]}"#;
    assert!(matches!(
        run(source, target, Strategy::Direct, tools, &mut output, 32),
        Err(Error::DirectUnavailable { .. })
    ));
    let (path, _, used) = run(source, target, Strategy::Auto, tools, &mut output, 32).unwrap();
    assert_eq!(path, Path::Ir);
    assert!(used > 0);
    let invalid = br#"{"model":"m","messages":false,"tools":[null]}"#;
    for strategy in [Strategy::Auto, Strategy::Direct, Strategy::Ir] {
        assert!(matches!(
            run(source, target, strategy, invalid, &mut output, 32),
            Err(Error::InvalidInput { .. })
        ));
    }
}

#[test]
fn unsupported_direction_is_distinct_from_malformed_input() {
    let mut output = [0; 1024];
    let source = Format::new(Protocol::OpenAiCompletions);
    let target = Format::new(Protocol::OpenAiChat);
    let input = br#"{"model":"m","prompt":"hello"}"#;
    assert!(matches!(
        run(source, target, Strategy::Direct, input, &mut output, 16),
        Err(Error::DirectUnavailable { .. })
    ));
    assert_eq!(
        run(source, target, Strategy::Auto, input, &mut output, 16)
            .unwrap()
            .0,
        Path::Ir
    );
    assert!(matches!(
        run(source, target, Strategy::Auto, b"not json", &mut output, 16),
        Err(Error::InvalidJson { .. })
    ));
}

#[test]
fn version_and_profile_are_part_of_identity() {
    let mut output = [0; 1024];
    let source = Format {
        protocol: Protocol::OpenAiChat,
        version: "v1",
        profile: zconv::Profile::OpenAiCompatible("vendor-a"),
    };
    let target = Format {
        profile: zconv::Profile::OpenAiCompatible("vendor-b"),
        ..source
    };
    let result = run(source, target, Strategy::Auto, CHAT, &mut output, 16).unwrap();
    assert_eq!(result.0, Path::Ir);
    let extension =
        br#"{"model":"m","messages":[{"role":"user","content":"x"}],"vendor_secret_mode":1}"#;
    assert!(run(source, target, Strategy::Auto, extension, &mut output, 16).is_err());
    let invalid_version = Format {
        version: "future-unverified",
        ..source
    };
    assert!(matches!(
        run(
            invalid_version,
            invalid_version,
            Strategy::Auto,
            CHAT,
            &mut output,
            16
        ),
        Err(Error::Unsupported { .. })
    ));
}

#[test]
fn borrowed_input_and_workspace_can_be_reused() {
    let mut tokens = [Token::EMPTY; 64];
    let mut nested = [Token::EMPTY; 64];
    let mut records = [Record::EMPTY; 16];
    let mut scratch = [0; 512];
    let mut output = [0; 1024];
    for _ in 0..20 {
        let result = convert(
            Protocol::OpenAiChat.into(),
            Protocol::OpenAiResponses.into(),
            Operation::Request,
            CHAT,
            &mut output,
            Options {
                strategy: Strategy::Ir,
                policy: Policy::default(),
                ..Options::default()
            },
            Workspace {
                tokens: &mut tokens,
                records: &mut records,
                scratch: &mut scratch,
                nested_tokens: &mut nested,
            },
        )
        .unwrap();
        assert_eq!(result.path, Path::Ir);
    }
}
