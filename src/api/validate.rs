use super::{Schema, invalid, required};
use crate::{
    Error,
    json::{Kind, Value},
};

type V<'a, 't> = Value<'a, 't>;

fn kind(v: V<'_, '_>, expected: Kind, field: &'static str) -> Result<(), Error> {
    if v.kind() == expected {
        Ok(())
    } else {
        Err(invalid(field, "unexpected JSON type"))
    }
}
fn string(v: V<'_, '_>, field: &'static str) -> Result<(), Error> {
    kind(v, Kind::String, field)
}
fn nonempty(v: V<'_, '_>, field: &'static str) -> Result<(), Error> {
    let text = v.as_string().ok_or(invalid(field, "expected string"))?;
    if text.decoded_len() == 0 {
        Err(invalid(field, "must not be empty"))
    } else {
        Ok(())
    }
}
fn unsigned(v: V<'_, '_>, field: &'static str) -> Result<u64, Error> {
    v.as_u64()
        .ok_or(invalid(field, "expected unsigned integer"))
}
fn nullable(
    v: V<'_, '_>,
    field: &'static str,
    check: fn(V<'_, '_>, &'static str) -> Result<(), Error>,
) -> Result<(), Error> {
    if let Some(value) = v.get(field).filter(|value| !value.is_null()) {
        check(value, field)?;
    }
    Ok(())
}
fn uint(v: V<'_, '_>, field: &'static str) -> Result<(), Error> {
    unsigned(v, field).map(|_| ())
}
fn boolean(v: V<'_, '_>, field: &'static str) -> Result<(), Error> {
    kind(v, Kind::Bool, field)
}
fn object(v: V<'_, '_>, field: &'static str) -> Result<(), Error> {
    kind(v, Kind::Object, field)
}
fn object_tag(v: V<'_, '_>, tag: &str) -> Result<(), Error> {
    if !required(v, "object")?
        .as_string()
        .is_some_and(|s| s.eq_str(tag))
    {
        return Err(invalid("object", "incorrect resource discriminator"));
    }
    Ok(())
}
fn options(
    v: V<'_, '_>,
    strings: &[&'static str],
    integers: &[&'static str],
    booleans: &[&'static str],
) -> Result<(), Error> {
    for &field in strings {
        nullable(v, field, string)?;
    }
    for &field in integers {
        nullable(v, field, uint)?;
    }
    for &field in booleans {
        nullable(v, field, boolean)?;
    }
    Ok(())
}
fn between(v: V<'_, '_>, field: &'static str, min: u64, max: u64) -> Result<(), Error> {
    if let Some(value) = v.get(field).filter(|v| !v.is_null()) {
        let n = unsigned(value, field)?;
        if n < min || n > max {
            return Err(invalid(field, "outside supported range"));
        }
    }
    Ok(())
}
fn metadata(v: V<'_, '_>) -> Result<(), Error> {
    if let Some(value) = v.get("metadata").filter(|v| !v.is_null()) {
        object(value, "metadata")?;
        for (_, value) in value.object() {
            string(value, "metadata value")?;
        }
    }
    Ok(())
}
fn expires(v: V<'_, '_>, field: &'static str) -> Result<(), Error> {
    if let Some(value) = v.get(field).filter(|v| !v.is_null()) {
        object(value, field)?;
        if !required(value, "anchor")?
            .as_string()
            .is_some_and(|s| s.eq_str("created_at"))
        {
            return Err(invalid("anchor", "expected created_at"));
        }
        unsigned(required(value, "seconds")?, "seconds")?;
        between(value, "seconds", 3600, 2_592_000)?;
    }
    Ok(())
}
pub(super) fn embeddings_input(v: V<'_, '_>) -> Result<(), Error> {
    if v.kind() == Kind::String {
        return nonempty(v, "input");
    }
    kind(v, Kind::Array, "input")?;
    let first = v
        .array()
        .next()
        .ok_or(invalid("input", "must not be empty"))?;
    let expected = first.kind();
    if !matches!(expected, Kind::String | Kind::Number | Kind::Array) {
        return Err(invalid(
            "input",
            "expected text, token IDs or homogeneous batches",
        ));
    }
    for value in v.array() {
        kind(value, expected, "input")?;
        match expected {
            Kind::String => nonempty(value, "input")?,
            Kind::Number => {
                unsigned(value, "token ID")?;
            }
            Kind::Array => {
                if value.array().next().is_none() {
                    return Err(invalid("input", "empty token batch"));
                }
                for token in value.array() {
                    unsigned(token, "token ID")?;
                }
            }
            _ => unreachable!(),
        }
    }
    Ok(())
}
fn usage(v: V<'_, '_>) -> Result<(), Error> {
    object(v, "usage")?;
    options(
        v,
        &[],
        &[
            "prompt_tokens",
            "completion_tokens",
            "input_tokens",
            "output_tokens",
            "total_tokens",
            "reasoning_tokens",
        ],
        &[],
    )?;
    for field in [
        "input_tokens_details",
        "output_tokens_details",
        "prompt_tokens_details",
        "completion_tokens_details",
    ] {
        if let Some(details) = v.get(field).filter(|v| !v.is_null()) {
            object(details, field)?;
            options(
                details,
                &[],
                &[
                    "image_tokens",
                    "text_tokens",
                    "cached_tokens",
                    "reasoning_tokens",
                    "audio_tokens",
                ],
                &[],
            )?;
        }
    }
    Ok(())
}
fn image_options(v: V<'_, '_>) -> Result<(), Error> {
    options(
        v,
        &[
            "model",
            "background",
            "quality",
            "size",
            "response_format",
            "output_format",
            "moderation",
            "user",
            "safety_identifier",
            "input_fidelity",
            "complexity",
        ],
        &[
            "n",
            "output_compression",
            "partial_images",
            "frame_count",
            "frame_duration_ms",
        ],
        &["stream"],
    )?;
    between(v, "n", 1, 10)?;
    between(v, "output_compression", 0, 100)?;
    between(v, "partial_images", 0, 3)?;
    Ok(())
}
fn image_ref(v: V<'_, '_>) -> Result<(), Error> {
    if v.kind() == Kind::String {
        return nonempty(v, "image");
    }
    object(v, "image")?;
    let mut references = 0;
    for key in ["image_url", "file_id", "url"] {
        if let Some(value) = v.get(key).filter(|v| !v.is_null()) {
            nonempty(value, key)?;
            references += 1;
        }
    }
    if references != 1 {
        return Err(invalid(
            "image",
            "exactly one image URL or file ID required",
        ));
    }
    Ok(())
}
fn image_data(v: V<'_, '_>) -> Result<(), Error> {
    object(v, "image")?;
    options(v, &["b64_json", "url", "revised_prompt"], &[], &[])
}
fn batch(v: V<'_, '_>) -> Result<(), Error> {
    object_tag(v, "batch")?;
    for field in [
        "id",
        "completion_window",
        "endpoint",
        "input_file_id",
        "status",
    ] {
        nonempty(required(v, field)?, field)?;
    }
    unsigned(required(v, "created_at")?, "created_at")?;
    options(
        v,
        &["output_file_id", "error_file_id", "model"],
        &[
            "cancelled_at",
            "cancelling_at",
            "completed_at",
            "expired_at",
            "expires_at",
            "failed_at",
            "finalizing_at",
            "in_progress_at",
        ],
        &[],
    )?;
    metadata(v)?;
    if let Some(counts) = v.get("request_counts").filter(|v| !v.is_null()) {
        object(counts, "request_counts")?;
        for key in ["total", "completed", "failed"] {
            unsigned(required(counts, key)?, key)?;
        }
    }
    if let Some(value) = v.get("usage").filter(|v| !v.is_null()) {
        usage(value)?;
    }
    if let Some(errors) = v.get("errors").filter(|v| !v.is_null()) {
        object(errors, "errors")?;
        if let Some(data) = errors.get("data").filter(|v| !v.is_null()) {
            kind(data, Kind::Array, "errors.data")?;
            for error in data.array() {
                object(error, "error")?;
                options(error, &["code", "message", "param"], &["line"], &[])?;
            }
        }
    }
    Ok(())
}
fn file(v: V<'_, '_>) -> Result<(), Error> {
    object_tag(v, "file")?;
    for key in ["id", "filename", "purpose"] {
        nonempty(required(v, key)?, key)?;
    }
    let bytes = required(v, "bytes")?;
    if !bytes.is_null() {
        unsigned(bytes, "bytes")?;
    }
    unsigned(required(v, "created_at")?, "created_at")?;
    options(v, &["status", "status_details"], &["expires_at"], &[])
}
fn list(v: V<'_, '_>, schema: Schema) -> Result<(), Error> {
    object_tag(v, "list")?;
    let data = required(v, "data")?;
    kind(data, Kind::Array, "data")?;
    for item in data.array() {
        validate(schema, item)?;
    }
    options(v, &["first_id", "last_id"], &[], &["has_more"])
}

pub(super) fn batch_body(body: V<'_, '_>) -> Result<(), Error> {
    object(body, "body")?;
    if let Some(stream) = body.get("stream").filter(|v| !v.is_null()) {
        boolean(stream, "stream")?;
        if stream.as_bool() == Some(true) {
            return Err(invalid("stream", "batch requests cannot stream"));
        }
    }
    Ok(())
}

fn api_path(v: V<'_, '_>, field: &'static str) -> Result<(), Error> {
    let text = v
        .as_string()
        .ok_or(invalid(field, "expected API path string"))?;
    let mut bytes = text.bytes();
    if !bytes.by_ref().take(4).eq(b"/v1/".iter().copied()) || bytes.any(|b| b.is_ascii_control()) {
        return Err(invalid(field, "expected an API path"));
    }
    Ok(())
}

fn question_type(question: V<'_, '_>, variants: &[&str]) -> Result<(), Error> {
    object(question, "question")?;
    let ty = required(question, "type")?
        .as_string()
        .ok_or(invalid("type", "expected a question type"))?;
    if variants.iter().any(|variant| ty.eq_str(variant)) {
        Ok(())
    } else {
        Err(invalid("type", "unsupported question type"))
    }
}

fn system_one_request(value: V<'_, '_>) -> Result<(), Error> {
    if let Some(model) = value.get("model") {
        nonempty(model, "model")?;
    }
    if !matches!(
        required(value, "state")?.kind(),
        Kind::String | Kind::Object | Kind::Array | Kind::Null
    ) {
        return Err(invalid("state", "expected text, object or array"));
    }
    match (value.get("questions"), value.get("fields")) {
        (Some(questions), None) if questions.kind() == Kind::Object => {
            let count = questions.object().count();
            if !(1..=32).contains(&count) {
                return Err(invalid("questions", "expected 1 to 32 named questions"));
            }
            for (_, question) in questions.object() {
                question_type(question, &["noul", "choice", "score"])?;
            }
        }
        (None, Some(fields))
            if fields.kind() == Kind::Object && fields.object().next().is_some() => {}
        _ => {
            return Err(invalid(
                "questions",
                "expected exactly one questions or fields object",
            ));
        }
    }
    Ok(())
}

fn openai_decisions_request(value: V<'_, '_>) -> Result<(), Error> {
    nonempty(required(value, "model")?, "model")?;
    if !matches!(required(value, "input")?.kind(), Kind::String | Kind::Array) {
        return Err(invalid("input", "expected text or input items"));
    }
    let questions = required(value, "questions")?;
    kind(questions, Kind::Array, "questions")?;
    if !(1..=400).contains(&questions.array().count()) {
        return Err(invalid("questions", "expected 1 to 400 questions"));
    }
    for question in questions.array() {
        question_type(question, &["predicate", "choice", "score"])?;
    }
    Ok(())
}

pub(super) fn validate(schema: Schema, v: V<'_, '_>) -> Result<(), Error> {
    object(v, "root")?;
    match schema {
        Schema::SystemOneRequest => system_one_request(v)?,
        Schema::OpenAiDecisionsRequest => openai_decisions_request(v)?,
        Schema::SystemOneResponse => {
            nonempty(required(v, "model")?, "model")?;
            // The named answers can hold decision results or typed extraction values.
            object(required(v, "answers")?, "answers")?;
        }
        Schema::OpenAiDecisionsResponse => {
            nonempty(required(v, "model")?, "model")?;
            kind(required(v, "answers")?, Kind::Array, "answers")?;
        }
        Schema::EmbeddingsRequest => {
            nonempty(required(v, "model")?, "model")?;
            embeddings_input(required(v, "input")?)?;
            options(v, &["user", "encoding_format"], &["dimensions"], &[])?;
            between(v, "dimensions", 1, u64::MAX)?;
            if let Some(format) = v.get("encoding_format").filter(|v| !v.is_null()) {
                let s = format
                    .as_string()
                    .ok_or(invalid("encoding_format", "expected string"))?;
                if !s.eq_str("float") && !s.eq_str("base64") {
                    return Err(invalid("encoding_format", "expected float or base64"));
                }
            }
        }
        Schema::EmbeddingsResponse => {
            object_tag(v, "list")?;
            nonempty(required(v, "model")?, "model")?;
            let data = required(v, "data")?;
            kind(data, Kind::Array, "data")?;
            for item in data.array() {
                object(item, "embedding")?;
                object_tag(item, "embedding")?;
                unsigned(required(item, "index")?, "index")?;
                let vector = required(item, "embedding")?;
                if vector.kind() == Kind::String {
                    continue;
                }
                kind(vector, Kind::Array, "embedding")?;
                for number in vector.array() {
                    kind(number, Kind::Number, "embedding element")?;
                    let n = core::str::from_utf8(number.raw())
                        .ok()
                        .and_then(|s| s.parse::<f64>().ok());
                    if !n.is_some_and(f64::is_finite) {
                        return Err(invalid("embedding", "expected finite values"));
                    }
                }
            }
            let counters = required(v, "usage")?;
            usage(counters)?;
            for field in ["prompt_tokens", "total_tokens"] {
                unsigned(required(counters, field)?, field)?;
            }
        }
        Schema::ImageGenerationRequest => {
            nonempty(required(v, "prompt")?, "prompt")?;
            image_options(v)?;
        }
        Schema::ImageEditRequest => {
            nonempty(required(v, "prompt")?, "prompt")?;
            image_options(v)?;
            if v.get("images").is_some() && v.get("image").is_some() {
                return Err(invalid("images", "image and images are mutually exclusive"));
            }
            let images = v
                .get("images")
                .or_else(|| v.get("image"))
                .ok_or(invalid("images", "image input required"))?;
            if images.kind() == Kind::Array {
                if images.array().next().is_none() {
                    return Err(invalid("images", "must not be empty"));
                }
                for image in images.array() {
                    image_ref(image)?;
                }
            } else {
                image_ref(images)?;
            }
            if let Some(mask) = v.get("mask").filter(|v| !v.is_null()) {
                image_ref(mask)?;
            }
        }
        Schema::ImagesResponse => {
            unsigned(required(v, "created")?, "created")?;
            if let Some(data) = v.get("data").filter(|v| !v.is_null()) {
                kind(data, Kind::Array, "data")?;
                for image in data.array() {
                    image_data(image)?;
                }
            }
            options(
                v,
                &[
                    "background",
                    "output_format",
                    "quality",
                    "size",
                    "safety_identifier",
                ],
                &[],
                &[],
            )?;
            for field in ["usage", "svg_usage"] {
                if let Some(value) = v.get(field).filter(|v| !v.is_null()) {
                    usage(value)?;
                }
            }
        }
        Schema::ImageStreamEvent => {
            let ty = required(v, "type")?
                .as_string()
                .ok_or(invalid("type", "expected string"))?;
            let partial = ty.eq_str("image_generation.partial_image")
                || ty.eq_str("image_edit.partial_image");
            let complete =
                ty.eq_str("image_generation.completed") || ty.eq_str("image_edit.completed");
            if !partial && !complete {
                return Err(Error::Unsupported {
                    feature: "image stream event type",
                });
            }
            string(required(v, "b64_json")?, "b64_json")?;
            unsigned(required(v, "created_at")?, "created_at")?;
            if partial {
                unsigned(required(v, "partial_image_index")?, "partial_image_index")?;
            }
            options(
                v,
                &["background", "output_format", "quality", "size"],
                &[],
                &[],
            )?;
            if let Some(value) = v.get("usage").filter(|v| !v.is_null()) {
                usage(value)?;
            }
        }
        Schema::BatchCreateRequest => {
            nonempty(required(v, "input_file_id")?, "input_file_id")?;
            api_path(required(v, "endpoint")?, "endpoint")?;
            if !required(v, "completion_window")?
                .as_string()
                .is_some_and(|s| s.eq_str("24h"))
            {
                return Err(invalid("completion_window", "expected 24h"));
            }
            metadata(v)?;
            expires(v, "output_expires_after")?;
        }
        Schema::Batch => batch(v)?,
        Schema::BatchList => list(v, Schema::Batch)?,
        Schema::File => file(v)?,
        Schema::FileList => list(v, Schema::File)?,
        Schema::FileDeleted => {
            object_tag(v, "file")?;
            nonempty(required(v, "id")?, "id")?;
            boolean(required(v, "deleted")?, "deleted")?;
        }
        Schema::BatchInput => {
            nonempty(required(v, "custom_id")?, "custom_id")?;
            if !required(v, "method")?
                .as_string()
                .is_some_and(|s| s.eq_str("POST"))
            {
                return Err(invalid("method", "batch requests require POST"));
            }
            api_path(required(v, "url")?, "url")?;
            batch_body(required(v, "body")?)?;
        }
        Schema::BatchOutput => {
            nonempty(required(v, "id")?, "id")?;
            nonempty(required(v, "custom_id")?, "custom_id")?;
            let response = v.get("response").filter(|v| !v.is_null());
            let error = v.get("error").filter(|v| !v.is_null());
            if response.is_none() && error.is_none() {
                return Err(invalid("response", "response or error required"));
            }
            if let Some(response) = response {
                object(response, "response")?;
                unsigned(required(response, "status_code")?, "status_code")?;
                between(response, "status_code", 100, 599)?;
                nullable(response, "request_id", string)?;
                object(required(response, "body")?, "body")?;
            }
            if let Some(error) = error {
                object(error, "error")?;
                options(error, &["code", "message"], &[], &[])?;
            }
        }
        Schema::Error => {
            let error = required(v, "error")?;
            object(error, "error")?;
            string(required(error, "message")?, "message")?;
            options(error, &["type", "param", "code"], &[], &[])?;
        }
    }
    Ok(())
}
