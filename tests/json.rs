use zconv::json::{parse, JsonError, JsonString, Kind, Limits, Token, Writer};
use zconv::Error;

#[test]
fn borrowed_structure_and_arbitrary_field_order() {
    let input = br#" { "z": [null, false, true, -12.50e+2, {"s":"hi"}], "a":0 } "#;
    let mut tokens = [Token::EMPTY; 64];
    let document = parse(input, &mut tokens, Limits::default()).unwrap();
    let root = document.root();
    assert_eq!(root.kind(), Kind::Object);
    assert_eq!(root.get("a").unwrap().as_u64(), Some(0));
    let values: Vec<_> = root.get("z").unwrap().array().collect();
    assert_eq!(values.len(), 5);
    assert!(values[0].is_null());
    assert_eq!(values[1].as_bool(), Some(false));
    assert_eq!(values[2].as_bool(), Some(true));
    assert_eq!(values[3].raw(), b"-12.50e+2");
    assert_eq!(values[3].as_u64(), None);
    assert!(values[4]
        .get("s")
        .unwrap()
        .as_string()
        .unwrap()
        .eq_str("hi"));
    assert_eq!(
        root.object()
            .map(|(key, _)| key.as_str().unwrap())
            .collect::<Vec<_>>(),
        ["z", "a"]
    );
    assert_eq!(root.get("missing").map(|v| v.raw()), None);
    assert_eq!(root.array().count(), 0);
    assert_eq!(values[0].object().count(), 0);
}

#[test]
fn escaped_names_and_unicode_are_semantic() {
    let input = br#"{"\u006b\u0065y":"a\n\t\b\f\r\/\\\"\u0000\u00e9\u4e2d\ud83d\ude00"}"#;
    let mut tokens = [Token::EMPTY; 8];
    let document = parse(input, &mut tokens, Limits::default()).unwrap();
    let value = document.root().get("key").unwrap().as_string().unwrap();
    let expected = "a\n\t\u{8}\u{c}\r/\\\"\0é中😀";
    assert_eq!(value.as_str(), None);
    assert!(value.eq_str(expected));
    assert!(!value.eq_str("wrong"));
    assert_eq!(value.bytes().collect::<Vec<_>>(), expected.as_bytes());
    assert_eq!(value.decoded_len(), expected.len());
    let mut empty = [];
    assert_eq!(
        value.decode_into(&mut empty),
        Err(Error::OutputTooSmall {
            needed: expected.len()
        })
    );
    let mut output = [0; 64];
    let length = value.decode_into(&mut output).unwrap();
    assert_eq!(&output[..length], expected.as_bytes());
}

#[test]
fn duplicate_keys_are_rejected_after_unescaping() {
    for input in [
        br#"{"a":0,"a":1}"#.as_slice(),
        br#"{"a":0,"\u0061":1}"#,
        "{\"é\":0,\"\\u00e9\":1}".as_bytes(),
        "{\"😀\":0,\"\\ud83d\\ude00\":1}".as_bytes(),
        br#"{"nested":{"x":0,"y":0,"\u0078":2}}"#,
    ] {
        let mut tokens = [Token::EMPTY; 64];
        assert!(
            matches!(
                parse(input, &mut tokens, Limits::default()),
                Err(Error::DuplicateKey { .. })
            ),
            "{input:?}"
        );
    }
    let mut tokens = [Token::EMPTY; 64];
    assert!(parse(
        br#"{"a":0,"b":{"a":1},"A":2}"#,
        &mut tokens,
        Limits::default()
    )
    .is_ok());
}

#[test]
fn key_sort_does_not_reorder_objects_or_confuse_nested_links() {
    for count in [1, 2, 3, 7, 31, 64, 513] {
        let mut input = String::from("{");
        for index in (0..count).rev() {
            if input.len() != 1 {
                input.push(',');
            }
            input.push_str(&format!("\"key{index:05}\":{{\"nested\":{index}}}"));
        }
        input.push('}');
        let mut tokens = vec![Token::EMPTY; 6 * count + 1];
        let document = parse(input.as_bytes(), &mut tokens, Limits::default()).unwrap();
        let actual: Vec<_> = document
            .root()
            .object()
            .map(|(_, value)| value.get("nested").unwrap().as_u64().unwrap())
            .collect();
        assert_eq!(actual, (0..count as u64).rev().collect::<Vec<_>>());
        input.pop();
        input.push_str(",\"key00000\":0}");
        assert!(matches!(
            parse(input.as_bytes(), &mut tokens, Limits::default()),
            Err(Error::DuplicateKey { .. })
        ));
    }
}

#[test]
fn validates_every_ignored_subtree() {
    let invalid: &[&[u8]] = &[
        b"",
        b" ",
        b"nul",
        b"null null",
        b"nulL",
        b"[true,]",
        b"[,0]",
        b"{a:0}",
        b"{\"a\":}",
        b"{\"a\":0,}",
        b"{\"a\" 0}",
        b"[",
        b"{",
        b"[0",
        b"{\"a\":0",
        b"true\x0c",
        b"\xef\xbb\xbfnull",
        b"+0",
        b"01",
        b"-01",
        b"-",
        b".0",
        b"0.",
        b"0.e1",
        b"1e",
        b"1e+",
        b"1e-",
        b"NaN",
        b"Infinity",
        b"0x1",
        b"--1",
        br#""\a""#,
        br#""\u0""#,
        br#""\u12gg""#,
        br#""\ud800""#,
        br#""\udc00""#,
        br#""\ud800\ud800""#,
        br#""\udbff\u0000""#,
        b"\"\x00\"",
        b"\"\n\"",
        b"\"\xff\"",
        b"\"\xc0\x80\"",
        b"\"\xed\xa0\x80\"",
        b"\"\xf4\x90\x80\x80\"",
        br#"{"ignored":{"also_ignored":[0,01]}}"#,
    ];
    for input in invalid {
        let mut tokens = [Token::EMPTY; 64];
        assert!(
            parse(input, &mut tokens, Limits::default()).is_err(),
            "accepted {input:?}"
        );
    }
}

#[test]
fn valid_scalars_and_preserved_number_spelling() {
    for input in [
        "null",
        "true",
        "false",
        "0",
        "-0",
        "1",
        "-123",
        "0.01",
        "1e-99",
        "1E+123",
        "18446744073709551616",
        "\"\"",
        "[]",
        "{}",
        "\"é😀\"",
    ] {
        let mut tokens = [Token::EMPTY; 8];
        let document = parse(input.as_bytes(), &mut tokens, Limits::default()).unwrap();
        assert_eq!(document.root().raw(), input.as_bytes());
    }
    let mut tokens = [Token::EMPTY; 8];
    assert_eq!(
        parse(b"18446744073709551615", &mut tokens, Limits::default())
            .unwrap()
            .root()
            .as_u64(),
        Some(u64::MAX)
    );
    assert_eq!(
        parse(b"18446744073709551616", &mut tokens, Limits::default())
            .unwrap()
            .root()
            .as_u64(),
        None
    );
}

#[test]
fn explicit_resource_limits_and_retry() {
    let input = br#"{"a":[1,2]}"#;
    let mut empty = [];
    assert_eq!(
        parse(input, &mut empty, Limits::default()).unwrap_err(),
        Error::WorkspaceTooSmall { needed: 1 }
    );
    let mut tokens = [Token::EMPTY; 8];
    assert_eq!(
        parse(
            input,
            &mut tokens,
            Limits {
                max_tokens: 2,
                ..Limits::default()
            }
        )
        .unwrap_err(),
        Error::LimitExceeded {
            kind: "JSON tokens",
            limit: 2
        }
    );
    assert_eq!(
        parse(
            input,
            &mut tokens,
            Limits {
                max_input: 2,
                ..Limits::default()
            }
        )
        .unwrap_err(),
        Error::InputTooLarge { limit: 2 }
    );
    assert_eq!(
        parse(
            input,
            &mut tokens,
            Limits {
                max_depth: 1,
                ..Limits::default()
            }
        )
        .unwrap_err(),
        Error::DepthLimitExceeded { limit: 1 }
    );
    assert_eq!(
        parse(
            input,
            &mut tokens,
            Limits {
                max_object_fields: 0,
                ..Limits::default()
            }
        )
        .unwrap_err(),
        Error::LimitExceeded {
            kind: "JSON object fields",
            limit: 0
        }
    );
    assert_eq!(
        parse(input, &mut tokens, Limits::default())
            .unwrap()
            .tokens_used(),
        5
    );
    let deep = format!("{}0{}", "[".repeat(65), "]".repeat(65));
    let mut large = [Token::EMPTY; 128];
    assert_eq!(
        parse(
            deep.as_bytes(),
            &mut large,
            Limits {
                max_depth: usize::MAX,
                ..Limits::default()
            }
        )
        .unwrap_err(),
        Error::DepthLimitExceeded { limit: 64 }
    );
    let deep = format!("{}0{}", "[".repeat(64), "]".repeat(64));
    assert!(parse(deep.as_bytes(), &mut large, Limits::default()).is_ok());
}

#[test]
fn writer_roundtrip_and_exact_capacity() {
    let text = "hello\0\u{1}\n\r\t\u{8}\u{c}\\\"é中😀";
    let mut empty = [];
    let mut count = Writer::new(&mut empty);
    count.string(text.as_bytes());
    let needed = count.written();
    assert_eq!(count.finish(), Err(Error::OutputTooSmall { needed }));
    let mut output = vec![0; needed];
    let mut writer = Writer::new(&mut output);
    writer.string(text.as_bytes());
    assert_eq!(writer.finish(), Ok(needed));
    let mut tokens = [Token::EMPTY; 8];
    let document = parse(&output, &mut tokens, Limits::default()).unwrap();
    assert!(document.root().as_string().unwrap().eq_str(text));
    let mut other = vec![0; needed];
    let mut writer = Writer::new(&mut other);
    writer.json_string(document.root().as_string().unwrap());
    writer.finish().unwrap();
    assert_eq!(other, output);
    let mut output = [0; 16];
    let mut writer = Writer::new(&mut output);
    writer.string(b"\xff");
    assert_eq!(
        writer.finish(),
        Err(Error::InvalidJson {
            offset: 0,
            kind: JsonError::InvalidUtf8
        })
    );
}

#[test]
fn detached_spans_and_public_string_validation() {
    let input = br#"{"payload":{"x":[1,null]},"text":"a\u0062"}"#;
    let mut tokens = [Token::EMPTY; 32];
    let document = parse(input, &mut tokens, Limits::default()).unwrap();
    let detached = document.root().get("payload").unwrap().raw_value();
    let original = document.root().get("text").unwrap().as_string().unwrap();
    let reparsed = JsonString::parse(original.raw()).unwrap();
    assert!(reparsed.eq_str("ab"));
    // A detached raw value borrows input bytes, so token storage is reusable.
    let other = parse(b"false", &mut tokens, Limits::default()).unwrap();
    assert_eq!(other.root().as_bool(), Some(false));
    assert_eq!(detached.kind(), Kind::Object);
    assert_eq!(detached.raw(), br#"{"x":[1,null]}"#);
    for invalid in [
        b"".as_slice(),
        b"0",
        b" \"a\"",
        b"\"a\" ",
        br#""\ud800""#,
        b"\"\xff\"",
    ] {
        assert!(JsonString::parse(invalid).is_err(), "{invalid:?}");
    }
}

#[test]
fn arbitrary_bytes_and_every_truncation_do_not_panic() {
    let valid = br#"{"key":[{"x":"hi\ud83d\ude00\n"},1.25e-30,true,null]}"#;
    for end in 0..valid.len() {
        let mut tokens = [Token::EMPTY; 128];
        assert!(parse(&valid[..end], &mut tokens, Limits::default()).is_err());
    }
    let mut random = 0x123456789abcdefu64;
    let mut input = [0u8; 128];
    let mut tokens = [Token::EMPTY; 128];
    for length in 0..input.len() {
        for _ in 0..128 {
            for byte in &mut input[..length] {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                *byte = random as u8;
            }
            let _ = parse(&input[..length], &mut tokens, Limits::default());
        }
    }
}
