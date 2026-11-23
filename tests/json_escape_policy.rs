use zconv::{
    Error,
    json::{ControlEscapes, JsonError, Writer},
};

// Independent oracle retained from the pre-migration zuse platform and
// zcalld compatibility-client producers. It is test-only, never a serializer
// used by production callers.
fn previous_quote(text: &str) -> String {
    let mut output = String::from("\"");
    for character in text.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            c if c < ' ' => output.push_str(&format!("\\u{:04x}", c as u32)),
            c => output.push(c),
        }
    }
    output.push('"');
    output
}

fn encoded(text: &str, style: ControlEscapes) -> Vec<u8> {
    let mut sizing = Writer::new(&mut []);
    sizing.string_with_control_escapes(text.as_bytes(), style);
    let needed = match sizing.finish() {
        Err(Error::OutputTooSmall { needed }) => needed,
        result => panic!("empty output must report the complete string size: {result:?}"),
    };
    let mut output = vec![0; needed];
    let mut writer = Writer::new(&mut output);
    writer.string_with_control_escapes(text.as_bytes(), style);
    assert_eq!(writer.finish().unwrap(), needed);
    output
}

#[test]
fn unicode_control_policy_matches_existing_producer_bytes() {
    let controls: String = (0u8..32).map(char::from).collect();
    let mut cases = vec![
        String::new(),
        controls,
        "\"quoted\" \\ slash /".into(),
        "模型 🚀 \u{2028}\u{2029} e\u{301}".into(),
        r"literal \b and \f and \u0008 must stay literal".into(),
        "\u{8}\u{c}\n\r\t".into(),
    ];
    cases.extend((0u8..32).map(|c| format!("before{}after", char::from(c))));
    for text in cases {
        let expected = previous_quote(&text);
        assert_eq!(
            encoded(&text, ControlEscapes::Unicode),
            expected.as_bytes(),
            "input {text:?}"
        );

        // Check a too-small slice with sentinels on either side. The provider
        // must report the full size and never write beyond the caller's slice.
        let capacity = expected.len() - 1;
        let mut guarded = vec![0xa5; capacity + 2];
        let mut writer = Writer::new(&mut guarded[1..capacity + 1]);
        writer.string_with_control_escapes(text.as_bytes(), ControlEscapes::Unicode);
        assert!(matches!(
            writer.finish(),
            Err(Error::OutputTooSmall { needed }) if needed == expected.len()
        ));
        assert_eq!(guarded[0], 0xa5);
        assert_eq!(guarded[capacity + 1], 0xa5);
        assert_eq!(&guarded[1..capacity + 1], &expected.as_bytes()[..capacity]);
    }
}

#[test]
fn default_short_spelling_stays_byte_identical() {
    let controls: String = (0u8..32).map(char::from).collect();
    let expected = br#""\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\b\t\n\u000b\f\r\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f""#;
    assert_eq!(encoded(&controls, ControlEscapes::Short), expected);

    let mut output = vec![0; expected.len()];
    let mut writer = Writer::new(&mut output);
    writer.string(controls.as_bytes());
    assert_eq!(writer.finish().unwrap(), expected.len());
    assert_eq!(output, expected);
    assert_eq!(ControlEscapes::default(), ControlEscapes::Short);
}

#[test]
fn both_policies_keep_invalid_utf8_failure_and_output_unchanged() {
    for style in [ControlEscapes::Short, ControlEscapes::Unicode] {
        let mut output = [0xa5; 16];
        let mut writer = Writer::new(&mut output);
        writer.string_with_control_escapes(b"\xff", style);
        assert!(matches!(
            writer.finish(),
            Err(Error::InvalidJson {
                offset: 0,
                kind: JsonError::InvalidUtf8
            })
        ));
        assert_eq!(output, [0xa5; 16]);
    }
}
