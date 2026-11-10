use zconv::sse::{Decoder, Status};

fn decode(bytes: &[u8], split: usize) -> Vec<Vec<u8>> {
    let mut buffer = vec![0; 4096];
    let mut output = vec![0; 4096];
    let mut decoder = Decoder::new(&mut buffer);
    let mut events = Vec::new();
    let mut position = 0;
    while position < bytes.len() {
        let end = (position + split).min(bytes.len());
        loop {
            let progress = decoder.push(&bytes[position..end]);
            position += progress.consumed;
            match progress.status {
                Status::Event => {
                    if let Some(length) = decoder.event().unwrap().data_into(&mut output).unwrap() {
                        events.push(output[..length].to_vec());
                    }
                    decoder.consume_event();
                }
                Status::NeedInput => break,
                other => panic!("unexpected {other:?}"),
            }
        }
    }
    decoder.finish();
    events
}

#[test]
fn whatwg_line_endings_bom_comments_and_data_join() {
    let input =
        b"\xef\xbb\xbf: keepalive\r\ndata: one\rdata: two\n\rdata:\n\ndata\r\rdata: final\r\n\r\n";
    let expected = vec![b"one\ntwo".to_vec(), vec![], vec![], b"final".to_vec()];
    for size in 1..=input.len() {
        assert_eq!(decode(input, size), expected, "split {size}");
    }
}

#[test]
fn eof_does_not_dispatch_an_unterminated_event() {
    for input in [
        b"data: discarded".as_slice(),
        b"data: discarded\n",
        b"data: discarded\r",
    ] {
        assert!(decode(input, 1).is_empty());
    }
}

#[test]
fn decoding_replaces_invalid_utf8_and_joins_once() {
    let bytes = b"data: \xf0\x80\x80\x80\xc2x\n\ndata: \xe4\xb8\xad\n\n";
    assert_eq!(decode(bytes, 1), ["�����x".as_bytes(), "中".as_bytes()]);
}

#[test]
fn metadata_last_value_and_nul_id_rules() {
    let mut memory = [0; 512];
    let mut decoder = Decoder::new(&mut memory);
    let input = b"id: first\nid: bad\0id\nretry: 15\nretry: -1\nevent: foo\nevent:\ndata: x\n\n";
    assert_eq!(decoder.push(input).status, Status::Event);
    let event = decoder.event().unwrap();
    assert_eq!(event.id(), Some(b"first".as_slice()));
    assert_eq!(event.retry(), Some(15));
    assert_eq!(event.event_type(), b"message");
}

#[test]
fn buffer_replacement_preserves_partially_framed_event() {
    let input = b"data: hello\r\n\r\n";
    let mut tiny = [0; 4];
    let mut large = [0; 64];
    let mut output = [0; 20];
    let mut decoder = Decoder::new(&mut tiny);
    let progress = decoder.push(input);
    assert!(matches!(progress.status, Status::NeedBuffer { .. }));
    decoder.replace_buffer(&mut large).unwrap();
    let resumed = decoder.push(&input[progress.consumed..]);
    assert_eq!(resumed.status, Status::Event);
    assert_eq!(
        decoder.event().unwrap().data_into(&mut output).unwrap(),
        Some(5)
    );
    assert_eq!(&output[..5], b"hello");
}

#[test]
fn data_size_recovery_is_exact_with_replacement_characters() {
    let mut memory = [0; 32];
    let mut decoder = Decoder::new(&mut memory);
    decoder.push(b"data:\xff\n\n");
    assert_eq!(
        decoder.event().unwrap().data_into(&mut []),
        Err(zconv::Error::OutputTooSmall { needed: 3 })
    );
}
