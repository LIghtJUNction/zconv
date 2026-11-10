//! External JSON oracle. This workspace is excluded from the published crate.

#[cfg(test)]
mod tests {
    use zconv::json::{self, Token, Writer};

    #[test]
    fn differential_valid_documents() {
        let cases = [
            "null",
            "true",
            "false",
            "-0",
            "1.234e-42",
            "18446744073709551615",
            r#""Unicode: 中文 😀 \uD83D\uDE00""#,
            r#"{"a":[0,{},[],null,true],"b":"\"\\\b\f\n\r\t"}"#,
            r#" { "模型": "转义\u4e2d文", "空": "", "数": 0 } "#,
        ];
        for input in cases {
            let expected: serde_json::Value = serde_json::from_str(input).unwrap();
            let mut tokens = [Token::EMPTY; 128];
            let parsed =
                json::parse(input.as_bytes(), &mut tokens, json::Limits::default()).unwrap();
            let actual: serde_json::Value = serde_json::from_slice(parsed.root().raw()).unwrap();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn differential_utf8_string_writer() {
        let samples = [
            "",
            "Hello",
            "\0\x01\x1f\n\r\t",
            "\"\\/",
            "中文😀é𝄞",
            "a\u{2028}b",
        ];
        for sample in samples {
            let mut buffer = [0; 256];
            let mut writer = Writer::new(&mut buffer);
            writer.string(sample.as_bytes());
            let length = writer.finish().unwrap();
            let decoded: String = serde_json::from_slice(&buffer[..length]).unwrap();
            assert_eq!(decoded, sample);
        }
    }

    #[test]
    fn differential_deterministic_mutations() {
        let seed = br#"{"model":"m","messages":[{"role":"user","content":"hello \uD83D\uDE00"}],"temperature":0.25}"#;
        let mut tokens = [Token::EMPTY; 256];
        for offset in 0..seed.len() {
            for byte in [
                0, b'"', b'\\', b'{', b'}', b'[', b']', b',', b':', b'0', b' ', 255,
            ] {
                let mut input = seed.to_vec();
                input[offset] = byte;
                let ours = json::parse(&input, &mut tokens, json::Limits::default()).is_ok();
                let oracle = serde_json::from_slice::<serde_json::Value>(&input).is_ok();
                assert_eq!(ours, oracle, "offset={offset}, byte={byte}");
            }
        }
    }

    #[test]
    fn duplicate_policy_is_deliberately_stricter_than_oracle() {
        let input = br#"{"a":1,"\u0061":2}"#;
        assert!(serde_json::from_slice::<serde_json::Value>(input).is_ok());
        let mut tokens = [Token::EMPTY; 8];
        assert!(matches!(
            json::parse(input, &mut tokens, json::Limits::default()),
            Err(zconv::Error::DuplicateKey { .. })
        ));
    }
}
