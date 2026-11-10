#![no_main]
use libfuzzer_sys::fuzz_target;
use zconv::json::{self, Token};

fuzz_target!(|data: &[u8]| {
    let mut tokens = [Token::EMPTY; 512];
    let limits = json::Limits {
        max_depth: 32,
        max_input: 65_536,
        max_tokens: 512,
        max_object_fields: 256,
    };
    if let Ok(document) = json::parse(data, &mut tokens, limits) {
        let root = document.root();
        for (key, value) in root.object() {
            assert!(key.decoded_len() <= key.raw().len());
            assert!(value.raw().len() <= data.len());
        }
    }
});
