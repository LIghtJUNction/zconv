//! Resource wire contracts, deliberately independent of model-conversation IR.
//!
//! Fixtures exercise the public OpenAI resource shapes (including nullable File
//! bytes, JSON image references, HTTP failures in Batch output, and SSE images).
//! Provider/model limits beyond the documented wire contract are caller policy.

use zconv::{
    Error,
    api::{
        self, Document, Embedding, EmbeddingInput, EmbeddingVector, EmbeddingsRequest,
        EmbeddingsResponse, Encoding, Endpoint, FieldValue, FileUpload, JsonLines, ListQuery, Part,
        Schema,
    },
    json::{self, Kind, Limits, Token},
    sse,
};

const FILE: &str = r#"{"object":"file","id":"file-1","bytes":null,"created_at":1700000000,"filename":"batch.jsonl","purpose":"batch","status_details":null,"expires_at":null}"#;
const BATCH: &str = r#"{"object":"batch","id":"batch-1","completion_window":"24h","endpoint":"/v1/responses","input_file_id":"file-1","status":"completed","created_at":1700000000,"output_file_id":"file-2","error_file_id":null,"request_counts":{"total":2,"completed":1,"failed":1},"metadata":{"project":"z"}}"#;

fn accepts(schema: Schema, input: &str) {
    let mut tokens = [Token::EMPTY; 512];
    api::parse(schema, input.as_bytes(), &mut tokens, Limits::default())
        .unwrap_or_else(|error| panic!("{schema:?}: {error:?}; input={input}"));
}

fn rejects(schema: Schema, input: &str) {
    let mut tokens = [Token::EMPTY; 512];
    assert!(
        api::parse(schema, input.as_bytes(), &mut tokens, Limits::default()).is_err(),
        "{schema:?} incorrectly accepted {input}"
    );
}

/// Every completed writer must report the exact needed capacity, accept that
/// capacity, and leave the unused caller-owned tail untouched.
fn encoded(mut write: impl FnMut(&mut [u8]) -> Result<usize, Error>) -> Vec<u8> {
    let needed = match write(&mut []) {
        Err(Error::OutputTooSmall { needed }) => needed,
        Ok(0) => 0,
        other => panic!("unexpected sizing result: {other:?}"),
    };
    let mut exact = vec![0; needed];
    assert_eq!(write(&mut exact), Ok(needed));
    let mut roomy = vec![0xa5; needed + 7];
    assert_eq!(write(&mut roomy), Ok(needed));
    assert_eq!(&roomy[..needed], exact);
    assert_eq!(&roomy[needed..], &[0xa5; 7]);
    if needed != 0 {
        let mut short = vec![0; needed - 1];
        assert_eq!(write(&mut short), Err(Error::OutputTooSmall { needed }));
    }
    exact
}

#[test]
fn all_resource_schemas_validate_and_preserve_unknown_fields() {
    let batch_list = format!(
        r#"{{"object":"list","data":[{BATCH}],"first_id":"batch-1","last_id":"batch-1","has_more":false}}"#
    );
    let file_list = format!(
        r#"{{"object":"list","data":[{FILE}],"first_id":"file-1","last_id":"file-1","has_more":false}}"#
    );
    let fixtures = [
        (
            Schema::EmbeddingsRequest,
            r#"{"model":"text-embedding-3-small","input":["hello","world"],"encoding_format":"float","dimensions":2}"#,
        ),
        (
            Schema::EmbeddingsResponse,
            r#"{"object":"list","model":"text-embedding-3-small","data":[{"object":"embedding","index":0,"embedding":[-0.25,1e-3]}],"usage":{"prompt_tokens":2,"total_tokens":2}}"#,
        ),
        (
            Schema::ImageGenerationRequest,
            r#"{"model":"gpt-image-1","prompt":"an island","size":"1024x1024","n":1,"stream":false}"#,
        ),
        (
            Schema::ImageEditRequest,
            r#"{"model":"gpt-image-1","prompt":"add clouds","images":[{"image_url":"https://example.test/island.png"},{"file_id":"file-image"}],"mask":{"file_id":"file-mask"}}"#,
        ),
        (
            Schema::ImagesResponse,
            r#"{"created":1700000000,"data":[{"b64_json":"YWJj","revised_prompt":null}],"usage":{"input_tokens":1,"output_tokens":2,"total_tokens":3,"input_tokens_details":{"image_tokens":0,"text_tokens":1}}}"#,
        ),
        (
            Schema::ImageStreamEvent,
            r#"{"type":"image_generation.partial_image","b64_json":"YWJj","created_at":1700000000,"partial_image_index":0,"background":"opaque","output_format":"png","quality":"medium","size":"1024x1024"}"#,
        ),
        (
            Schema::BatchCreateRequest,
            r#"{"input_file_id":"file-1","endpoint":"/v1/responses","completion_window":"24h","metadata":{"owner":"test"},"output_expires_after":{"anchor":"created_at","seconds":3600}}"#,
        ),
        (Schema::Batch, BATCH),
        (Schema::BatchList, batch_list.as_str()),
        (
            Schema::BatchInput,
            r#"{"custom_id":"request-1","method":"POST","url":"/v1/responses","body":{"model":"example","input":"hello","stream":false}}"#,
        ),
        (
            Schema::BatchOutput,
            r#"{"id":"batch_req-1","custom_id":"request-1","response":{"status_code":200,"request_id":"req-1","body":{"id":"resp-1","output":[]}},"error":null}"#,
        ),
        (Schema::File, FILE),
        (Schema::FileList, file_list.as_str()),
        (
            Schema::FileDeleted,
            r#"{"object":"file","id":"file-1","deleted":true}"#,
        ),
        (
            Schema::Error,
            r#"{"error":{"message":"quota exceeded","type":"insufficient_quota","param":null,"code":"insufficient_quota"}}"#,
        ),
    ];
    for (schema, fixture) in fixtures {
        // Future fields are kept byte-for-byte, including their number spelling.
        let input = format!(
            "{},\"vendor_future\":{{\"number\":1.2300e+02,\"nested\":[null,true,\"a\\nb\"]}}}}",
            fixture.strip_suffix('}').unwrap()
        );
        let mut tokens = [Token::EMPTY; 512];
        let doc = api::parse(schema, input.as_bytes(), &mut tokens, Limits::default())
            .unwrap_or_else(|e| panic!("{schema:?}: {e:?}"));
        assert_eq!(doc.schema(), schema);
        assert_eq!(doc.value().kind(), Kind::Object);
        assert_eq!(doc.field("vendor_future").unwrap().kind(), Kind::Object);
        assert_eq!(encoded(|out| doc.encode(out)), input.as_bytes());
        assert_eq!(doc.raw().raw(), input.as_bytes());
        assert!(doc.field("missing").is_none());
        assert!(Document::decode(schema, doc.value()).is_ok());
    }
}

#[test]
fn nullable_and_optional_resource_fields_follow_the_wire_contract() {
    accepts(Schema::File, FILE); // A file's size may be unknown/null.
    accepts(Schema::ImagesResponse, r#"{"created":1}"#);
    accepts(
        Schema::ImagesResponse,
        r#"{"created":1,"data":null,"usage":null}"#,
    );
    accepts(
        Schema::EmbeddingsRequest,
        r#"{"model":"m","input":"a","dimensions":null,"encoding_format":null,"user":null}"#,
    );
    accepts(
        Schema::BatchList,
        r#"{"object":"list","data":[],"first_id":null,"last_id":null,"has_more":false}"#,
    );
    accepts(
        Schema::FileList,
        r#"{"object":"list","data":[],"first_id":null,"last_id":null,"has_more":false}"#,
    );
    accepts(
        Schema::Error,
        r#"{"error":{"message":"bad request","type":null,"param":null,"code":null}}"#,
    );
    accepts(
        Schema::ImageEditRequest,
        r#"{"prompt":"p","images":[{"image_url":null,"file_id":"f"}],"mask":null}"#,
    );
}

#[test]
fn malformed_known_fields_are_rejected_across_resource_families() {
    let cases = [
        (
            Schema::EmbeddingsRequest,
            r#"{"model":"m","input":["text",1]}"#,
        ),
        (Schema::EmbeddingsRequest, r#"{"model":"m","input":[1,-2]}"#),
        (
            Schema::EmbeddingsRequest,
            r#"{"model":"m","input":[[1],[]]}"#,
        ),
        (
            Schema::EmbeddingsRequest,
            r#"{"model":"m","input":"x","dimensions":0}"#,
        ),
        (
            Schema::EmbeddingsRequest,
            r#"{"model":"m","input":"x","encoding_format":"binary"}"#,
        ),
        (
            Schema::EmbeddingsResponse,
            r#"{"object":"list","model":"m","data":[{"object":"embedding","index":0,"embedding":[1,"two"]}],"usage":{"prompt_tokens":1,"total_tokens":1}}"#,
        ),
        (
            Schema::EmbeddingsResponse,
            r#"{"object":"list","model":"m","data":[],"usage":{"prompt_tokens":-1,"total_tokens":1}}"#,
        ),
        (Schema::ImageGenerationRequest, r#"{"prompt":"p","n":0}"#),
        (
            Schema::ImageGenerationRequest,
            r#"{"prompt":"p","output_compression":101}"#,
        ),
        (
            Schema::ImageGenerationRequest,
            r#"{"prompt":"p","partial_images":4}"#,
        ),
        (
            Schema::ImageGenerationRequest,
            r#"{"prompt":"p","stream":"true"}"#,
        ),
        (Schema::ImageEditRequest, r#"{"prompt":"p","images":[]}"#),
        (
            Schema::ImagesResponse,
            r#"{"created":1,"data":[{"url":3}]}"#,
        ),
        (
            Schema::ImagesResponse,
            r#"{"created":1,"usage":{"input_tokens_details":{"text_tokens":-1}}}"#,
        ),
        (
            Schema::ImageStreamEvent,
            r#"{"type":"image_generation.partial_image","b64_json":"x","created_at":1}"#,
        ),
        (
            Schema::ImageStreamEvent,
            r#"{"type":"image_generation.completed","b64_json":null,"created_at":1}"#,
        ),
        (
            Schema::BatchCreateRequest,
            r#"{"input_file_id":"f","endpoint":"/v1/responses","completion_window":"1h"}"#,
        ),
        (
            Schema::BatchCreateRequest,
            r#"{"input_file_id":"f","endpoint":"/v1/responses","completion_window":"24h","metadata":{"attempt":1}}"#,
        ),
        (
            Schema::Batch,
            r#"{"object":"batch","id":"b","completion_window":"24h","endpoint":"/v1/responses","input_file_id":"f","status":"completed","created_at":1,"request_counts":{"total":1,"completed":1}}"#,
        ),
        (
            Schema::BatchList,
            r#"{"object":"list","data":[{"object":"batch"}]}"#,
        ),
        (
            Schema::BatchInput,
            r#"{"custom_id":"c","method":"GET","url":"/v1/responses","body":{}}"#,
        ),
        (
            Schema::BatchOutput,
            r#"{"id":"b","custom_id":"c","response":null,"error":null}"#,
        ),
        (
            Schema::BatchOutput,
            r#"{"id":"b","custom_id":"c","response":{"status_code":600,"body":{}}}"#,
        ),
        (
            Schema::File,
            r#"{"object":"file","id":"f","filename":"f.txt","purpose":"user_data","created_at":1,"bytes":-1}"#,
        ),
        (
            Schema::FileList,
            r#"{"object":"list","data":{},"has_more":false}"#,
        ),
        (
            Schema::FileDeleted,
            r#"{"object":"file","id":"f","deleted":"true"}"#,
        ),
        (
            Schema::Error,
            r#"{"error":{"message":"bad","code":{"nested":"code"}}}"#,
        ),
        (Schema::Error, r#"{"error":{"message":"bad","code":false}}"#),
    ];
    for (schema, input) in cases {
        rejects(schema, input);
    }
    for schema in [
        Schema::File,
        Schema::ImagesResponse,
        Schema::BatchInput,
        Schema::Error,
    ] {
        for input in ["null", "[]", "1", "\"text\""] {
            rejects(schema, input);
        }
    }
}

#[test]
fn resource_discriminators_are_required_at_every_nested_level() {
    for (schema, fixture, tag) in [
        (Schema::File, FILE, "file"),
        (Schema::Batch, BATCH, "batch"),
        (Schema::FileList, r#"{"object":"list","data":[]}"#, "list"),
        (Schema::BatchList, r#"{"object":"list","data":[]}"#, "list"),
        (
            Schema::FileDeleted,
            r#"{"object":"file","id":"f","deleted":true}"#,
            "file",
        ),
        (
            Schema::EmbeddingsResponse,
            r#"{"object":"list","model":"m","data":[],"usage":{"prompt_tokens":0,"total_tokens":0}}"#,
            "list",
        ),
    ] {
        rejects(
            schema,
            &fixture.replacen(&format!("\"object\":\"{tag}\","), "", 1),
        );
        rejects(
            schema,
            &fixture.replacen(&format!("\"object\":\"{tag}\""), "\"object\":null", 1),
        );
        rejects(
            schema,
            &fixture.replacen(&format!("\"object\":\"{tag}\""), "\"object\":\"wrong\"", 1),
        );
    }
    rejects(
        Schema::EmbeddingsResponse,
        r#"{"object":"list","model":"m","data":[{"index":0,"embedding":[1]}],"usage":{"prompt_tokens":1,"total_tokens":1}}"#,
    );
    rejects(
        Schema::FileList,
        &format!(
            r#"{{"object":"list","data":[{}]}}"#,
            FILE.replacen("\"object\":\"file\",", "", 1)
        ),
    );
}

#[test]
fn image_references_reject_ambiguous_or_empty_known_inputs() {
    for input in [
        r#"{"prompt":"p","images":[{"file_id":"f"}],"image":false}"#,
        r#"{"prompt":"p","images":[{"file_id":"f"}],"image":null}"#,
        r#"{"prompt":"p","images":[{"image_url":""}]}"#,
        r#"{"prompt":"p","images":[{"file_id":""}]}"#,
        r#"{"prompt":"p","images":[{"image_url":"https://example.test/a","file_id":"f"}]}"#,
        r#"{"prompt":"p","images":[{"file_id":"f","url":false}]}"#,
        r#"{"prompt":"p","images":[{"future":true}]}"#,
        r#"{"prompt":"p","images":[{"file_id":"f"}],"mask":{"file_id":""}}"#,
    ] {
        rejects(Schema::ImageEditRequest, input);
    }
    accepts(
        Schema::ImageEditRequest,
        r#"{"prompt":"p","image":"https://example.test/a","mask":{"url":"https://example.test/mask"}}"#,
    );
}

#[test]
fn object_builder_preserves_extensions_and_validates_authored_known_fields() {
    let raw_input = br#"{"nested":[1.200e+2,"future",null]}"#;
    let mut raw_tokens = [Token::EMPTY; 32];
    let raw = json::parse(raw_input, &mut raw_tokens, Limits::default()).unwrap();
    let fields = [
        ("object", FieldValue::Text("file")),
        ("id", FieldValue::Text("file-\"1")),
        ("filename", FieldValue::Text("a\n文.jsonl")),
        ("purpose", FieldValue::Text("batch")),
        ("bytes", FieldValue::Unsigned(u64::MAX)),
        ("created_at", FieldValue::Unsigned(1)),
        ("expires_at", FieldValue::Null),
        ("future", FieldValue::Json(raw.root().raw_value())),
        ("future_flag", FieldValue::Bool(true)),
    ];
    let mut tokens = [Token::EMPTY; 64];
    let bytes = encoded(|out| api::encode_object(Schema::File, &fields, &mut tokens, out));
    let doc = api::parse(Schema::File, &bytes, &mut tokens, Limits::default()).unwrap();
    assert!(doc.id().unwrap().eq_str("file-\"1"));
    assert!(
        doc.field("filename")
            .unwrap()
            .as_string()
            .unwrap()
            .eq_str("a\n文.jsonl")
    );
    assert_eq!(doc.field("bytes").unwrap().as_u64(), Some(u64::MAX));
    assert_eq!(doc.field("future").unwrap().raw(), raw_input);
    assert_eq!(doc.field("future_flag").unwrap().as_bool(), Some(true));
    let mut output = [0; 1024];
    let mut invalid_fields = fields;
    invalid_fields[4].1 = FieldValue::Bool(false);
    assert!(api::encode_object(Schema::File, &invalid_fields, &mut tokens, &mut output).is_err());
    assert!(api::encode_object(Schema::File, &fields[1..], &mut tokens, &mut output).is_err());
    let duplicates = [
        ("prompt", FieldValue::Text("a")),
        ("prompt", FieldValue::Text("b")),
    ];
    assert!(matches!(
        api::encode_object(
            Schema::ImageGenerationRequest,
            &duplicates,
            &mut tokens,
            &mut output
        ),
        Err(Error::DuplicateKey { .. })
    ));
    assert!(matches!(
        api::encode_object(Schema::File, &fields, &mut [], &mut output),
        Err(Error::WorkspaceTooSmall { .. })
    ));
}

#[test]
fn endpoints_expose_correct_methods_paths_and_body_contracts() {
    use Endpoint::*;
    let fixtures = [
        (
            Embeddings,
            "POST",
            None,
            "/v1/embeddings",
            Some(Schema::EmbeddingsRequest),
            Some(Schema::EmbeddingsResponse),
        ),
        (
            ImageGenerations,
            "POST",
            None,
            "/v1/images/generations",
            Some(Schema::ImageGenerationRequest),
            Some(Schema::ImagesResponse),
        ),
        (
            ImageEdits,
            "POST",
            None,
            "/v1/images/edits",
            Some(Schema::ImageEditRequest),
            Some(Schema::ImagesResponse),
        ),
        (
            ImageVariations,
            "POST",
            None,
            "/v1/images/variations",
            None,
            Some(Schema::ImagesResponse),
        ),
        (
            BatchCreate,
            "POST",
            None,
            "/v1/batches",
            Some(Schema::BatchCreateRequest),
            Some(Schema::Batch),
        ),
        (
            BatchRetrieve,
            "GET",
            Some("batch-1"),
            "/v1/batches/batch-1",
            None,
            Some(Schema::Batch),
        ),
        (
            BatchList,
            "GET",
            None,
            "/v1/batches",
            None,
            Some(Schema::BatchList),
        ),
        (
            BatchCancel,
            "POST",
            Some("batch-1"),
            "/v1/batches/batch-1/cancel",
            None,
            Some(Schema::Batch),
        ),
        (
            FileUpload,
            "POST",
            None,
            "/v1/files",
            None,
            Some(Schema::File),
        ),
        (
            FileRetrieve,
            "GET",
            Some("file-1"),
            "/v1/files/file-1",
            None,
            Some(Schema::File),
        ),
        (
            FileList,
            "GET",
            None,
            "/v1/files",
            None,
            Some(Schema::FileList),
        ),
        (
            FileDelete,
            "DELETE",
            Some("file-1"),
            "/v1/files/file-1",
            None,
            Some(Schema::FileDeleted),
        ),
        (
            FileContent,
            "GET",
            Some("file-1"),
            "/v1/files/file-1/content",
            None,
            None,
        ),
    ];
    for (endpoint, method, id, path, request, response) in fixtures {
        assert_eq!(endpoint.method(), method);
        assert_eq!(endpoint.request_schema(), request);
        assert_eq!(endpoint.response_schema(), response);
        assert_eq!(encoded(|out| endpoint.write_path(id, out)), path.as_bytes());
        let wrong_id = if id.is_some() {
            None
        } else {
            Some("unexpected")
        };
        assert!(endpoint.write_path(wrong_id, &mut [0; 128]).is_err());
    }
}

#[test]
fn resource_ids_and_query_values_cannot_change_url_structure() {
    let id = "a/b ?#%é\\\r\n";
    assert_eq!(
        encoded(|out| Endpoint::FileContent.write_path(Some(id), out)),
        b"/v1/files/a%2Fb%20%3F%23%25%C3%A9%5C%0D%0A/content"
    );
    assert_eq!(
        encoded(|out| Endpoint::BatchCancel.write_path(Some("AZaz09-_.~"), out)),
        b"/v1/batches/AZaz09-_.~/cancel"
    );
    for bad in ["", ".", ".."] {
        assert!(
            Endpoint::FileContent
                .write_path(Some(bad), &mut [0; 128])
                .is_err(),
            "unsafe ID {bad:?}"
        );
    }
    let query = ListQuery {
        after: Some("file/?# &é"),
        purpose: Some("batch&limit=0"),
        descending: Some(false),
        limit: Some(u32::MAX),
    };
    assert_eq!(
        encoded(|out| query.encode(out)),
        b"?after=file%2F%3F%23%20%26%C3%A9&purpose=batch%26limit%3D0&order=asc&limit=4294967295"
    );
    assert_eq!(encoded(|out| ListQuery::default().encode(out)), b"");
    assert_eq!(
        encoded(|out| ListQuery {
            descending: Some(true),
            ..ListQuery::default()
        }
        .encode(out)),
        b"?order=desc"
    );
    assert!(
        ListQuery {
            limit: Some(0),
            ..ListQuery::default()
        }
        .encode(&mut [])
        .is_err()
    );
}

#[test]
fn embeddings_builders_cover_text_and_token_batch_variants() {
    let texts = ["hello\n\"world\"", "你好"];
    let tokens = [0, 42, u32::MAX];
    let second = [5, 6];
    let batches = [&tokens[..], &second[..]];
    for input in [
        EmbeddingInput::Text("a\tb"),
        EmbeddingInput::Texts(&texts),
        EmbeddingInput::Tokens(&tokens),
        EmbeddingInput::TokenBatches(&batches),
    ] {
        for format in [Encoding::Float, Encoding::Base64] {
            let mut request = EmbeddingsRequest::new("text-embedding-3-small", input);
            request.dimensions = Some(256);
            request.encoding_format = Some(format);
            request.user = Some("user\"\\\n");
            let bytes = encoded(|out| request.encode(out));
            let mut tape = [Token::EMPTY; 128];
            let doc = api::parse(
                Schema::EmbeddingsRequest,
                &bytes,
                &mut tape,
                Limits::default(),
            )
            .unwrap();
            assert!(doc.model().unwrap().eq_str(request.model));
            assert_eq!(doc.field("dimensions").unwrap().as_u64(), Some(256));
            assert!(
                doc.field("user")
                    .unwrap()
                    .as_string()
                    .unwrap()
                    .eq_str(request.user.unwrap())
            );
            let value = doc.field("input").unwrap();
            match input {
                EmbeddingInput::Text(text) => assert!(value.as_string().unwrap().eq_str(text)),
                EmbeddingInput::Texts(texts) => {
                    assert_eq!(value.array().count(), texts.len());
                    for (actual, expected) in value.array().zip(texts) {
                        assert!(actual.as_string().unwrap().eq_str(expected));
                    }
                }
                EmbeddingInput::Tokens(tokens) => assert_eq!(
                    value
                        .array()
                        .map(|v| v.as_u64().unwrap())
                        .collect::<Vec<_>>(),
                    tokens.iter().map(|&v| u64::from(v)).collect::<Vec<_>>()
                ),
                EmbeddingInput::TokenBatches(batches) => {
                    assert_eq!(value.array().count(), batches.len());
                    for (actual, expected) in value.array().zip(batches) {
                        assert_eq!(
                            actual
                                .array()
                                .map(|v| v.as_u64().unwrap())
                                .collect::<Vec<_>>(),
                            expected.iter().map(|&v| u64::from(v)).collect::<Vec<_>>()
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn embeddings_builders_reject_empty_inputs_and_invalid_dimensions() {
    let empty_batch: [&[u32]; 1] = [&[]];
    for input in [
        EmbeddingInput::Text(""),
        EmbeddingInput::Texts(&[]),
        EmbeddingInput::Texts(&["ok", ""]),
        EmbeddingInput::Tokens(&[]),
        EmbeddingInput::TokenBatches(&[]),
        EmbeddingInput::TokenBatches(&empty_batch),
    ] {
        assert!(
            EmbeddingsRequest::new("m", input)
                .encode(&mut [0; 512])
                .is_err()
        );
    }
    assert!(
        EmbeddingsRequest::new("", EmbeddingInput::Text("x"))
            .encode(&mut [0; 512])
            .is_err()
    );
    let mut request = EmbeddingsRequest::new("m", EmbeddingInput::Text("x"));
    request.dimensions = Some(0);
    assert!(request.encode(&mut [0; 512]).is_err());
}

#[test]
fn embedding_vectors_and_usage_preserve_numeric_values() {
    let values = [-0.0, f32::MIN_POSITIVE, -0.75, f32::MAX];
    let data = [
        Embedding {
            index: 7,
            vector: EmbeddingVector::Floats(&values),
        },
        Embedding {
            index: 2,
            vector: EmbeddingVector::Base64("AAAAAA=="),
        },
    ];
    let response = EmbeddingsResponse {
        model: "m",
        data: &data,
        prompt_tokens: 9_007_199_254_740_993,
        total_tokens: u64::MAX,
    };
    let bytes = encoded(|out| response.encode(out));
    let mut tokens = [Token::EMPTY; 128];
    let doc = api::parse(
        Schema::EmbeddingsResponse,
        &bytes,
        &mut tokens,
        Limits::default(),
    )
    .unwrap();
    assert_eq!(doc.data().unwrap().count(), 2);
    assert_eq!(
        doc.usage().unwrap().get("prompt_tokens").unwrap().as_u64(),
        Some(response.prompt_tokens)
    );
    assert_eq!(
        doc.usage().unwrap().get("total_tokens").unwrap().as_u64(),
        Some(u64::MAX)
    );
    let first = doc.embedding(0).unwrap();
    assert_eq!(first.index(), 7);
    assert!(!first.is_base64());
    for (actual, expected) in first.vector().array().zip(values) {
        let actual: f32 = std::str::from_utf8(actual.raw()).unwrap().parse().unwrap();
        assert_eq!(actual.to_bits(), expected.to_bits());
    }
    assert!(doc.embedding(1).unwrap().is_base64());
    assert_eq!(doc.embedding(1).unwrap().index(), 2);
    assert!(
        doc.embedding(1)
            .unwrap()
            .vector()
            .as_string()
            .unwrap()
            .eq_str("AAAAAA==")
    );
    assert!(doc.embedding(2).is_err());
    assert!(doc.batch_body().is_err());
    assert!(doc.status().is_none());
    for number in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let floats = [number];
        let bad = [Embedding {
            index: 0,
            vector: EmbeddingVector::Floats(&floats),
        }];
        assert!(
            EmbeddingsResponse {
                data: &bad,
                ..response
            }
            .encode(&mut [0; 512])
            .is_err()
        );
    }
    rejects(
        Schema::EmbeddingsResponse,
        r#"{"object":"list","model":"m","data":[{"object":"embedding","index":0,"embedding":[1e9999]}],"usage":{"prompt_tokens":1,"total_tokens":1}}"#,
    );
}

#[test]
fn document_resource_accessors_do_not_guess_wrong_schema() {
    let mut tokens = [Token::EMPTY; 128];
    let doc = api::parse(
        Schema::Batch,
        BATCH.as_bytes(),
        &mut tokens,
        Limits::default(),
    )
    .unwrap();
    assert!(doc.id().unwrap().eq_str("batch-1"));
    assert!(doc.status().unwrap().eq_str("completed"));
    assert!(doc.data().is_none());
    assert!(doc.embedding(0).is_err());
    assert!(doc.batch_body().is_err());
    assert!(doc.encode_sse(&mut [0; 1024]).is_err());
    let doc = api::parse(
        Schema::FileList,
        br#"{"object":"list","data":[],"has_more":false}"#,
        &mut tokens,
        Limits::default(),
    )
    .unwrap();
    assert_eq!(doc.has_more(), Some(false));
    assert_eq!(doc.data().unwrap().count(), 0);
}

#[test]
fn batch_jsonl_compacts_only_whitespace_outside_strings() {
    let body = "{\r\n  \"model\": \"m\",\n  \"input\": \"space  文 \\\"quote\\\" \\\\ slash \\n newline\",\r\n  \"extra\": { \"n\": 1.2300e+02 },\n  \"stream\": false\n}";
    let mut tokens = [Token::EMPTY; 64];
    let parsed = json::parse(body.as_bytes(), &mut tokens, Limits::default()).unwrap();
    let bytes =
        encoded(|out| api::encode_batch_input("custom\n\"id", "/v1/responses", parsed.root(), out));
    assert!(!bytes.contains(&b'\n'));
    assert!(!bytes.contains(&b'\r'));
    let expected_body = r#"{"model":"m","input":"space  文 \"quote\" \\ slash \n newline","extra":{"n":1.2300e+02},"stream":false}"#;
    let mut envelope_tokens = [Token::EMPTY; 128];
    let envelope = api::parse(
        Schema::BatchInput,
        &bytes,
        &mut envelope_tokens,
        Limits::default(),
    )
    .unwrap();
    assert_eq!(
        envelope.batch_body().unwrap().raw(),
        expected_body.as_bytes()
    );
    assert!(
        envelope
            .field("custom_id")
            .unwrap()
            .as_string()
            .unwrap()
            .eq_str("custom\n\"id")
    );
    assert!(
        envelope
            .field("body")
            .unwrap()
            .get("input")
            .unwrap()
            .as_string()
            .unwrap()
            .bytes()
            .eq(parsed
                .root()
                .get("input")
                .unwrap()
                .as_string()
                .unwrap()
                .bytes())
    );
    assert_eq!(JsonLines::new(&bytes).count(), 1);
}

#[test]
fn batch_envelopes_forbid_streaming_and_require_relative_api_paths() {
    for body in [r#"{}"#, r#"{"stream":false}"#, r#"{"stream":null}"#] {
        let mut tokens = [Token::EMPTY; 32];
        let doc = json::parse(body.as_bytes(), &mut tokens, Limits::default()).unwrap();
        let bytes =
            encoded(|out| api::encode_batch_input("c", "/v1/chat/completions", doc.root(), out));
        accepts(Schema::BatchInput, std::str::from_utf8(&bytes).unwrap());
    }
    for body in [
        r#"{"stream":true}"#,
        r#"{"stream":"false"}"#,
        r#"{"stream":0}"#,
        "[]",
        "null",
    ] {
        let mut tokens = [Token::EMPTY; 32];
        let doc = json::parse(body.as_bytes(), &mut tokens, Limits::default()).unwrap();
        assert!(api::encode_batch_input("c", "/v1/responses", doc.root(), &mut [0; 512]).is_err());
        rejects(
            Schema::BatchInput,
            &format!(r#"{{"custom_id":"c","method":"POST","url":"/v1/responses","body":{body}}}"#),
        );
    }
    let mut tokens = [Token::EMPTY; 8];
    let body = json::parse(b"{}", &mut tokens, Limits::default()).unwrap();
    for url in [
        "",
        "https://example.test/v1/responses",
        "responses",
        "/v2/responses",
        "/v1/responses\r\nInjected: yes",
    ] {
        assert!(api::encode_batch_input("c", url, body.root(), &mut [0; 512]).is_err());
    }
    assert!(api::encode_batch_input("", "/v1/responses", body.root(), &mut [0; 512]).is_err());
    rejects(
        Schema::BatchInput,
        r#"{"custom_id":"c","method":"POST","url":"/v1/responses\n","body":{}}"#,
    );
    rejects(
        Schema::BatchCreateRequest,
        r#"{"input_file_id":"f","endpoint":"https://example.test/v1/responses","completion_window":"24h"}"#,
    );
    rejects(
        Schema::BatchCreateRequest,
        r#"{"input_file_id":"f","endpoint":"/v1/responses\u0000","completion_window":"24h"}"#,
    );
}

#[test]
fn jsonl_records_accept_crlf_and_terminal_newline_but_report_blank_records() {
    let input = b"{\"a\":1}\r\n{\"b\":2}\n{\"c\":3}";
    let mut records = JsonLines::new(input);
    assert_eq!(records.line_number(), 0);
    for (index, expected) in [br#"{"a":1}"#.as_slice(), br#"{"b":2}"#, br#"{"c":3}"#]
        .into_iter()
        .enumerate()
    {
        assert_eq!(records.next().unwrap().unwrap(), expected);
        assert_eq!(records.line_number(), index + 1);
    }
    assert!(records.next().is_none());
    assert!(records.next().is_none());
    assert_eq!(JsonLines::new(b"{}\n").count(), 1);
    assert_eq!(JsonLines::new(b"{}\r\n").count(), 1);
    assert_eq!(JsonLines::new(b"").count(), 0);
    let mut records = JsonLines::new(b"{}\n \t\r\n{}\n\n");
    assert!(records.next().unwrap().is_ok());
    assert!(records.next().unwrap().is_err());
    assert_eq!(records.line_number(), 2);
    assert!(records.next().unwrap().is_ok());
    assert!(records.next().unwrap().is_err());
    assert_eq!(records.line_number(), 4);
    assert!(records.next().is_none());
}

#[test]
fn batch_output_accepts_http_errors_and_per_request_batch_errors() {
    accepts(
        Schema::BatchOutput,
        r#"{"id":"batch_req-1","custom_id":"c","response":{"status_code":429,"request_id":"req-1","body":{"error":{"message":"rate limited","code":"rate_limit_exceeded"}}},"error":null}"#,
    );
    accepts(
        Schema::BatchOutput,
        r#"{"id":"batch_req-2","custom_id":"c","response":null,"error":{"code":"batch_expired","message":"expired before execution"}}"#,
    );
    rejects(
        Schema::BatchOutput,
        r#"{"id":"batch_req-1","custom_id":"c","response":{"status_code":99,"body":{}}}"#,
    );
    rejects(
        Schema::BatchOutput,
        r#"{"id":"batch_req-1","custom_id":"c","response":{"status_code":200,"body":[]}}"#,
    );
    rejects(
        Schema::BatchOutput,
        r#"{"id":"batch_req-1","custom_id":"c","error":{"code":123}}"#,
    );
    for seconds in [3599, 2_592_001] {
        rejects(
            Schema::BatchCreateRequest,
            &format!(
                r#"{{"input_file_id":"f","endpoint":"/v1/responses","completion_window":"24h","output_expires_after":{{"anchor":"created_at","seconds":{seconds}}}}}"#
            ),
        );
    }
}

#[test]
fn multipart_keeps_binary_files_and_repeated_image_arrays_intact() {
    let binary = [0, 0xff, 0x80, b'\r', b'\n', b'A'];
    let parts = [
        Part::file("file", "batch.jsonl", "application/octet-stream", &binary),
        Part::text("purpose", "batch"),
    ];
    let bytes = encoded(|out| api::encode_multipart(Endpoint::FileUpload, "boundary", &parts, out));
    let mut expected = b"--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"batch.jsonl\"\r\nContent-Type: application/octet-stream\r\n\r\n".to_vec();
    expected.extend_from_slice(&binary);
    expected.extend_from_slice(b"\r\n--boundary\r\nContent-Disposition: form-data; name=\"purpose\"\r\n\r\nbatch\r\n--boundary--\r\n");
    assert_eq!(bytes, expected);
    let images = [
        Part::file("image[]", "a.png", "image/png", &binary),
        Part::file("image[]", "b.png", "image/png", b"second"),
        Part::text("prompt", "add a cloud\nwith rain"),
        Part::text("model", "gpt-image-1"),
    ];
    let bytes = encoded(|out| api::encode_multipart(Endpoint::ImageEdits, "img", &images, out));
    assert_eq!(
        bytes
            .windows(b"name=\"image[]\"".len())
            .filter(|w| *w == b"name=\"image[]\"")
            .count(),
        2
    );
    assert!(bytes.windows(binary.len()).any(|w| w == binary));
    assert!(bytes.ends_with(b"--img--\r\n"));
    let variations = [Part::file("image", "a.png", "image/png", b"png")];
    assert!(
        !encoded(|out| api::encode_multipart(Endpoint::ImageVariations, "v", &variations, out))
            .is_empty()
    );
}

#[test]
fn multipart_rejects_missing_required_fields_duplicates_and_wrong_endpoints() {
    let file = Part::file("file", "f.bin", "application/octet-stream", b"bytes");
    let image = Part::file("image", "i.png", "image/png", b"image");
    let purpose = Part::text("purpose", "batch");
    let prompt = Part::text("prompt", "edit");
    for (endpoint, parts) in [
        (Endpoint::FileUpload, vec![file]),
        (Endpoint::FileUpload, vec![purpose]),
        (Endpoint::FileUpload, vec![file, Part::text("purpose", "")]),
        (Endpoint::FileUpload, vec![file, purpose, purpose]),
        (Endpoint::FileUpload, vec![file, file, purpose]),
        (
            Endpoint::FileUpload,
            vec![Part::text("file", "text"), purpose],
        ),
        (Endpoint::ImageEdits, vec![image]),
        (Endpoint::ImageEdits, vec![prompt]),
        (Endpoint::ImageEdits, vec![image, prompt, image]),
        (Endpoint::ImageEdits, vec![image, Part::text("prompt", "")]),
        (Endpoint::ImageVariations, vec![Part::text("image", "url")]),
        (Endpoint::Embeddings, vec![file, purpose]),
        (Endpoint::ImageGenerations, vec![image, prompt]),
    ] {
        let mut output = [0xa5; 512];
        assert!(api::encode_multipart(endpoint, "boundary", &parts, &mut output).is_err());
        assert_eq!(output, [0xa5; 512]);
    }
}

#[test]
fn multipart_rejects_header_injection_and_non_utf8_text_before_writing() {
    let file = Part::file("file", "f.bin", "application/octet-stream", b"binary");
    let purpose = Part::text("purpose", "batch");
    for part in [
        Part::text("evil\r\nX-Injected: yes", "data"),
        Part::text("evil\"; filename=\"x", "data"),
        Part::text("evil\\name", "data"),
        Part::text("", "data"),
        Part::file("other", "evil\r\n.txt", "application/octet-stream", b"data"),
        Part::file("other", "evil\".txt", "application/octet-stream", b"data"),
        Part::file("other", "evil\\.txt", "application/octet-stream", b"data"),
        Part::file("other", "", "application/octet-stream", b"data"),
        Part::file("other", "f", "text/plain\r\nX-Injected: yes", b"data"),
        Part::file("other", "f", "", b"data"),
        Part {
            name: "text",
            filename: None,
            content_type: None,
            data: &[0xff, 0x80],
        },
    ] {
        let mut output = [0xa5; 512];
        assert!(
            api::encode_multipart(
                Endpoint::FileUpload,
                "boundary",
                &[file, purpose, part],
                &mut output
            )
            .is_err()
        );
        assert_eq!(output, [0xa5; 512]);
    }
}

#[test]
fn multipart_rejects_actual_boundary_delimiters_but_allows_body_substrings() {
    for body in [
        b"--boundary\r\n".as_slice(),
        b"prefix\r\n--boundary--\r\nsuffix",
    ] {
        let parts = [
            Part::file("file", "f", "application/octet-stream", body),
            Part::text("purpose", "batch"),
        ];
        let mut output = [0xa5; 512];
        assert!(
            api::encode_multipart(Endpoint::FileUpload, "boundary", &parts, &mut output).is_err()
        );
        assert_eq!(output, [0xa5; 512]);
    }
    let parts = [
        Part::file(
            "file",
            "f",
            "application/octet-stream",
            b"text boundary inside and --boundary is not a line",
        ),
        Part::text("purpose", "batch"),
    ];
    assert!(
        !encoded(|out| api::encode_multipart(Endpoint::FileUpload, "boundary", &parts, out))
            .is_empty()
    );
    for boundary in ["", "bad boundary", "bad\r\n", "bad\"boundary", "é"] {
        assert!(
            api::encode_multipart(Endpoint::FileUpload, boundary, &parts, &mut [0; 1024]).is_err()
        );
    }
    assert!(
        api::encode_multipart(
            Endpoint::FileUpload,
            &"a".repeat(71),
            &parts,
            &mut [0; 1024]
        )
        .is_err()
    );
    assert!(
        !encoded(|out| api::encode_multipart(Endpoint::FileUpload, &"a".repeat(70), &parts, out))
            .is_empty()
    );
}

#[test]
fn file_upload_expiry_uses_native_form_fields_and_validates_bounds() {
    let upload = FileUpload {
        filename: "input.jsonl",
        content_type: "application/jsonl",
        contents: b"{}\n",
        purpose: "batch",
        expires_after: None,
    };
    let bytes = encoded(|out| upload.encode("bound", out));
    assert!(
        !bytes
            .windows(b"expires_after".len())
            .any(|w| w == b"expires_after")
    );
    for seconds in [3600, 2_592_000] {
        let bytes = encoded(|out| {
            FileUpload {
                expires_after: Some(seconds),
                ..upload
            }
            .encode("bound", out)
        });
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.contains("name=\"expires_after[anchor]\"\r\n\r\ncreated_at\r\n"));
        assert!(text.contains(&format!(
            "name=\"expires_after[seconds]\"\r\n\r\n{seconds}\r\n"
        )));
    }
    for seconds in [0, 3599, 2_592_001, u32::MAX] {
        let mut output = [0xa5; 1024];
        assert!(
            FileUpload {
                expires_after: Some(seconds),
                ..upload
            }
            .encode("bound", &mut output)
            .is_err()
        );
        assert_eq!(output, [0xa5; 1024]);
    }
}

#[test]
fn image_stream_events_round_trip_sse_under_arbitrary_input_chunks() {
    for kind in [
        "image_generation.partial_image",
        "image_generation.completed",
        "image_edit.partial_image",
        "image_edit.completed",
    ] {
        let input = format!(
            r#"{{"type":"{kind}","b64_json":"YWJj","created_at":1,"partial_image_index":0,"background":"opaque","output_format":"png","quality":"medium","size":"1024x1024","usage":{{"total_tokens":3}}}}"#
        );
        let mut tokens = [Token::EMPTY; 128];
        let doc = api::parse(
            Schema::ImageStreamEvent,
            input.as_bytes(),
            &mut tokens,
            Limits::default(),
        )
        .unwrap();
        let bytes = encoded(|out| doc.encode_sse(out));
        let mut frame = [0; 2048];
        let mut decoder = sse::Decoder::new(&mut frame);
        for (at, byte) in bytes.iter().enumerate() {
            let progress = decoder.push(std::slice::from_ref(byte));
            assert_eq!(progress.consumed, 1);
            assert_eq!(
                progress.status,
                if at + 1 == bytes.len() {
                    sse::Status::Event
                } else {
                    sse::Status::NeedInput
                }
            );
        }
        let event = decoder.event().unwrap();
        assert_eq!(event.event_type(), kind.as_bytes());
        let mut payload = [0; 1024];
        let length = event.data_into(&mut payload).unwrap().unwrap();
        assert_eq!(&payload[..length], input.as_bytes());
        accepts(
            Schema::ImageStreamEvent,
            std::str::from_utf8(&payload[..length]).unwrap(),
        );
        decoder.consume_event();
        assert!(decoder.event().is_none());
    }
    rejects(
        Schema::ImageStreamEvent,
        r#"{"type":"response.completed","b64_json":"x","created_at":1}"#,
    );
}

#[test]
fn image_sse_normalizes_cr_lf_and_crlf_without_changing_string_content() {
    let canonical = "{\n  \"type\": \"image_generation.completed\",\n  \"created_at\": 1,\n  \"b64_json\": \"abc\\nxyz\",\n  \"future\": \"空 格\"\n}";
    let expected = format!(
        "event: image_generation.completed\n{}\n",
        canonical
            .lines()
            .map(|line| format!("data: {line}\n"))
            .collect::<String>()
    );
    for newline in ["\n", "\r", "\r\n"] {
        let input = canonical.replace('\n', newline);
        let mut tokens = [Token::EMPTY; 128];
        let doc = api::parse(
            Schema::ImageStreamEvent,
            input.as_bytes(),
            &mut tokens,
            Limits::default(),
        )
        .unwrap();
        let frame = encoded(|out| doc.encode_sse(out));
        assert_eq!(frame, expected.as_bytes(), "line ending {newline:?}");
        let mut storage = [0; 2048];
        let mut decoder = sse::Decoder::new(&mut storage);
        assert_eq!(decoder.push(&frame).status, sse::Status::Event);
        let mut payload = [0; 1024];
        let length = decoder
            .event()
            .unwrap()
            .data_into(&mut payload)
            .unwrap()
            .unwrap();
        assert_eq!(&payload[..length], canonical.as_bytes());
        accepts(
            Schema::ImageStreamEvent,
            std::str::from_utf8(&payload[..length]).unwrap(),
        );
    }
}
