//! Direct conversion from validated borrowed wire views to a JSON writer.
//!
//! These paths do not construct request, response, or content IR records. Their
//! applicability scan covers every field they consume; features outside this
//! finite subset return `DirectUnavailable` before writing output. Rich content,
//! provider extensions remain available through the semantic IR path. OpenAI
//! Chat/Responses additionally have direct function-tool request conversion.

use crate::ir::{
    Context, EncodeReport, METADATA_CREATED, METADATA_ITEM_ID, METADATA_ITEM_STATUS, Policy,
    Presence, Text,
};
use crate::json::{JsonString, Kind, Token, Value, Writer};
use crate::{Error, Format, Operation, Profile, Protocol};

type V<'a, 't> = Value<'a, 't>;

fn unavailable(feature: &'static str) -> Error {
    Error::DirectUnavailable { feature }
}

fn invalid(field: &'static str, reason: &'static str) -> Error {
    Error::InvalidInput { field, reason }
}

fn keys(value: V<'_, '_>, allowed: &[&str]) -> Result<(), Error> {
    if value.kind() != Kind::Object {
        return Err(invalid("object", "expected an object"));
    }
    for (key, _) in value.object() {
        if !allowed.iter().any(|name| key.eq_str(name)) {
            return Err(unavailable("field outside direct subset"));
        }
    }
    Ok(())
}

fn required<'a, 't>(value: V<'a, 't>, field: &'static str) -> Result<V<'a, 't>, Error> {
    value
        .get(field)
        .ok_or_else(|| invalid(field, "missing field"))
}

fn string<'a>(value: V<'a, '_>, field: &'static str) -> Result<JsonString<'a>, Error> {
    value
        .as_string()
        .ok_or_else(|| invalid(field, "expected a string"))
}

fn array(value: V<'_, '_>, field: &'static str) -> Result<(), Error> {
    if value.kind() == Kind::Array {
        Ok(())
    } else {
        Err(invalid(field, "expected an array"))
    }
}

fn integer(value: V<'_, '_>) -> bool {
    value.kind() == Kind::Number && value.raw().iter().all(u8::is_ascii_digit)
}

fn request_scalars(root: V<'_, '_>, source: Protocol, target: Protocol) -> Result<(), Error> {
    if root.kind() != Kind::Object {
        return Err(invalid("request", "expected an object"));
    }
    if source != Protocol::GeminiGenerateContent {
        string(required(root, "model")?, "model")?;
    }
    let parameters = if source == Protocol::GeminiGenerateContent {
        root.get("generationConfig")
    } else {
        Some(root)
    };
    if let Some(parameters) = parameters {
        if parameters.kind() != Kind::Object {
            return Err(invalid("generation config", "expected an object"));
        }
        for field in [
            "temperature",
            if source == Protocol::GeminiGenerateContent {
                "topP"
            } else {
                "top_p"
            },
        ] {
            if let Some(value) = parameters.get(field) {
                if !value.is_null() && value.kind() != Kind::Number {
                    return Err(invalid(field, "expected number"));
                }
                if value.is_null()
                    && matches!(
                        target,
                        Protocol::AnthropicMessages | Protocol::GeminiGenerateContent
                    )
                {
                    return Err(unavailable("null sampling parameter"));
                }
            }
        }
        let maximum_fields: &[&str] = match source {
            Protocol::OpenAiChat => &["max_tokens", "max_completion_tokens"],
            Protocol::OpenAiResponses => &["max_output_tokens"],
            Protocol::GeminiGenerateContent => &["maxOutputTokens"],
            _ => &["max_tokens"],
        };
        for &field in maximum_fields {
            if let Some(value) = parameters.get(field) {
                if !value.is_null() && !integer(value) {
                    return Err(invalid(field, "expected unsigned integer"));
                }
                if value.is_null()
                    && matches!(
                        target,
                        Protocol::AnthropicMessages | Protocol::GeminiGenerateContent
                    )
                {
                    return Err(unavailable("null token limit"));
                }
            }
        }
        for field in [match source {
            Protocol::AnthropicMessages => "stop_sequences",
            Protocol::GeminiGenerateContent => "stopSequences",
            _ => "stop",
        }] {
            if let Some(value) = parameters.get(field) {
                match value.kind() {
                    Kind::String if field == "stop" => {}
                    Kind::Array => {
                        for entry in value.array() {
                            string(entry, field)?;
                        }
                    }
                    Kind::Null => {
                        if matches!(
                            target,
                            Protocol::AnthropicMessages | Protocol::GeminiGenerateContent
                        ) {
                            return Err(unavailable("null stop sequences"));
                        }
                    }
                    _ => return Err(invalid(field, "expected string array")),
                }
            }
        }
    }
    for field in ["stream", "parallel_tool_calls"] {
        if source == Protocol::GeminiGenerateContent
            || (field == "parallel_tool_calls"
                && !matches!(source, Protocol::OpenAiChat | Protocol::OpenAiResponses))
        {
            continue;
        }
        if let Some(value) = root.get(field) {
            if !value.is_null() && value.as_bool().is_none() {
                return Err(invalid(field, "expected boolean"));
            }
            if value.is_null() && target == Protocol::AnthropicMessages {
                return Err(unavailable("null stream setting"));
            }
        }
    }
    if source == Protocol::AnthropicMessages && !root.get("max_tokens").is_some_and(integer) {
        return Err(invalid("max_tokens", "required unsigned integer missing"));
    }
    Ok(())
}

fn is(value: Option<V<'_, '_>>, expected: &str) -> bool {
    value
        .and_then(Value::as_string)
        .is_some_and(|text| text.eq_str(expected))
}

fn property(writer: &mut Writer<'_>, name: &[u8]) {
    writer.byte(b',');
    writer.string(name);
    writer.byte(b':');
}

fn copy(writer: &mut Writer<'_>, root: V<'_, '_>, source: &str, target: &[u8]) {
    if let Some(value) = root.get(source) {
        property(writer, target);
        writer.raw(value.raw());
    }
}

fn unsigned(writer: &mut Writer<'_>, mut number: u64) {
    let mut buffer = [0u8; 20];
    let mut position = buffer.len();
    loop {
        position -= 1;
        buffer[position] = b'0' + (number % 10) as u8;
        number /= 10;
        if number == 0 {
            break;
        }
    }
    writer.raw(&buffer[position..]);
}

fn field_or_context<'a>(
    root: V<'a, '_>,
    field: &'static str,
    context: Option<&'a str>,
) -> Result<Text<'a>, Error> {
    if let Some(value) = root.get(field) {
        Ok(Text::Json(string(value, field)?))
    } else {
        context
            .map(Text::Utf8)
            .ok_or(Error::ContextRequired { field })
    }
}

fn text(writer: &mut Writer<'_>, value: Text<'_>) {
    match value {
        Text::Json(value) => writer.json_string(value),
        Text::Utf8(value) => writer.string(value.as_bytes()),
    }
}

/// Convert a validated JSON document without constructing semantic IR.
///
/// The supported wire subset is validated without constructing semantic IR.
/// `DirectUnavailable` authorizes an Auto caller to validate the full source and
/// try IR; use the crate-level `convert` function for that orchestration. Output is provisional
/// until success; an `OutputTooSmall` result reports the exact required length.
#[allow(clippy::too_many_arguments)]
pub fn convert<'a>(
    source: Format,
    target: Format,
    operation: Operation,
    root: V<'a, '_>,
    context: &Context<'a>,
    policy: Policy,
    output: &mut [u8],
    _scratch: &mut [u8],
    _scratch_tokens: &mut [Token],
) -> Result<EncodeReport<'a>, Error> {
    crate::protocol::validate_format(source)?;
    crate::protocol::validate_format(target)?;
    if context.tool_calls.len() > 64 {
        return Err(Error::LimitExceeded {
            kind: "external tool bindings",
            limit: 64,
        });
    }
    if source.profile != Profile::Standard
        || target.profile != Profile::Standard
        || source.version != Format::new(source.protocol).version
        || target.version != Format::new(target.protocol).version
    {
        return Err(unavailable("version or vendor profile"));
    }
    use Protocol::*;
    if !matches!(
        (source.protocol, target.protocol),
        (
            OpenAiChat,
            OpenAiResponses | AnthropicMessages | GeminiGenerateContent
        ) | (
            OpenAiResponses | AnthropicMessages | GeminiGenerateContent,
            OpenAiChat
        )
    ) {
        return Err(unavailable("conversion direction"));
    }
    match operation {
        Operation::Request => request(
            source.protocol,
            target.protocol,
            root,
            context,
            policy,
            output,
        ),
        Operation::Response => response(
            source.protocol,
            target.protocol,
            root,
            context,
            policy,
            output,
        ),
    }
}

fn check_function_definition(
    definition: V<'_, '_>,
    source: Protocol,
    legacy: bool,
) -> Result<(), Error> {
    let named = if source == Protocol::OpenAiChat && !legacy {
        keys(definition, &["type", "function"])?;
        if !string(required(definition, "type")?, "type")?.eq_str("function") {
            return Err(unavailable("non-function tool definition"));
        }
        required(definition, "function")?
    } else {
        definition
    };
    if source == Protocol::OpenAiResponses {
        keys(
            named,
            &["type", "name", "description", "parameters", "strict"],
        )?;
        if !string(required(named, "type")?, "type")?.eq_str("function") {
            return Err(unavailable("non-function tool definition"));
        }
    } else {
        keys(named, &["name", "description", "parameters", "strict"])?;
    }
    string(required(named, "name")?, "name")?;
    if let Some(value) = named.get("description") {
        if !value.is_null() {
            string(value, "description")?;
        }
    }
    if let Some(value) = named.get("parameters") {
        if !value.is_null() && value.kind() != Kind::Object {
            return Err(invalid("parameters", "expected object"));
        }
    }
    if let Some(value) = named.get("strict") {
        if !value.is_null() && value.as_bool().is_none() {
            return Err(invalid("strict", "expected boolean"));
        }
    }
    if source == Protocol::OpenAiResponses && named.get("strict").map_or(true, Value::is_null) {
        // Responses can normalize schemas when strictness is implicit; Chat
        // cannot reproduce that provider operation by merely omitting a flag.
        return Err(Error::ContextRequired {
            field: "resolved tool strict mode",
        });
    }
    Ok(())
}

fn check_tools(root: V<'_, '_>, source: Protocol) -> Result<(), Error> {
    if root.get("tools").is_some() && root.get("functions").is_some() {
        return Err(unavailable("simultaneous tool definition spellings"));
    }
    for field in ["tools", "functions"] {
        if let Some(list) = root.get(field) {
            if list.is_null() && field == "tools" {
                continue;
            }
            array(list, field)?;
            for definition in list.array() {
                check_function_definition(definition, source, field == "functions")?;
            }
        }
    }
    if root.get("tool_choice").is_some() && root.get("function_call").is_some() {
        return Err(unavailable("simultaneous tool choice spellings"));
    }
    if let Some(value) = root
        .get("tool_choice")
        .or_else(|| root.get("function_call"))
    {
        if value.is_null() {
            return Ok(());
        }
        if let Some(choice) = value.as_string() {
            if ["auto", "none", "required"]
                .iter()
                .any(|name| choice.eq_str(name))
            {
                return Ok(());
            }
            return Err(unavailable("tool choice mode"));
        }
        if source == Protocol::OpenAiResponses {
            keys(value, &["type", "name"])?;
            if !string(required(value, "type")?, "type")?.eq_str("function") {
                return Err(unavailable("tool choice type"));
            }
            string(required(value, "name")?, "name")?;
        } else if root.get("function_call").is_some() {
            keys(value, &["name"])?;
            string(required(value, "name")?, "name")?;
        } else {
            keys(value, &["type", "function"])?;
            if !string(required(value, "type")?, "type")?.eq_str("function") {
                return Err(unavailable("tool choice type"));
            }
            let function = required(value, "function")?;
            keys(function, &["name"])?;
            string(required(function, "name")?, "name")?;
        }
    }
    Ok(())
}

fn write_function_definition(
    writer: &mut Writer<'_>,
    definition: V<'_, '_>,
    source: Protocol,
    target: Protocol,
    legacy: bool,
) {
    let definition = if source == Protocol::OpenAiChat && !legacy {
        definition.get("function").unwrap_or(definition)
    } else {
        definition
    };
    writer.raw(if target == Protocol::OpenAiChat {
        b"{\"type\":\"function\",\"function\":{\"name\":"
    } else {
        b"{\"type\":\"function\",\"name\":"
    });
    if let Some(name) = definition.get("name") {
        writer.raw(name.raw());
    }
    for field in ["description", "parameters"] {
        copy(writer, definition, field, field.as_bytes());
    }
    if source == Protocol::OpenAiChat
        && target == Protocol::OpenAiResponses
        && definition.get("strict").map_or(true, Value::is_null)
    {
        // Preserve Chat's non-strict default across Responses' different default.
        writer.raw(b",\"strict\":false");
    } else {
        copy(writer, definition, "strict", b"strict");
    }
    if target == Protocol::OpenAiChat {
        writer.byte(b'}');
    }
    writer.byte(b'}');
}

fn write_tools(writer: &mut Writer<'_>, root: V<'_, '_>, source: Protocol, target: Protocol) {
    let modern = root.get("tools");
    let legacy = root.get("functions");
    if modern.is_some() || legacy.is_some() {
        property(writer, b"tools");
        if modern.is_some_and(Value::is_null) && legacy.is_none() {
            writer.raw(b"null");
        } else {
            writer.byte(b'[');
            let mut first = true;
            for (list, is_legacy) in [(modern, false), (legacy, true)] {
                for definition in list.into_iter().flat_map(Value::array) {
                    if !first {
                        writer.byte(b',');
                    }
                    first = false;
                    write_function_definition(writer, definition, source, target, is_legacy);
                }
            }
            writer.byte(b']');
        }
    }
    if let Some(choice) = root
        .get("tool_choice")
        .or_else(|| root.get("function_call"))
    {
        property(writer, b"tool_choice");
        if choice.is_null() || choice.kind() == Kind::String {
            writer.raw(choice.raw());
        } else {
            let name = choice.get("name").or_else(|| {
                choice
                    .get("function")
                    .and_then(|function| function.get("name"))
            });
            writer.raw(if target == Protocol::OpenAiChat {
                b"{\"type\":\"function\",\"function\":{\"name\":"
            } else {
                b"{\"type\":\"function\",\"name\":"
            });
            if let Some(name) = name {
                writer.raw(name.raw());
            }
            if target == Protocol::OpenAiChat {
                writer.byte(b'}');
            }
            writer.byte(b'}');
        }
    }
    copy(writer, root, "parallel_tool_calls", b"parallel_tool_calls");
}

fn call_id<'a>(
    id: Option<V<'a, '_>>,
    name: Option<JsonString<'a>>,
    context: &Context<'a>,
) -> Result<Text<'a>, Error> {
    if let Some(value) = id {
        if !value.is_null() {
            return Ok(Text::Json(string(value, "tool call ID")?));
        }
    }
    let name = name.ok_or(Error::ContextRequired {
        field: "tool call ID",
    })?;
    let mut found = None;
    for binding in context.tool_calls {
        if name.eq_str(binding.name) {
            if found.is_some() {
                return Err(Error::ContextRequired {
                    field: "unambiguous tool call ID",
                });
            }
            found = Some(Text::Utf8(binding.id));
        }
    }
    found.ok_or(Error::ContextRequired {
        field: "tool call ID",
    })
}

fn check_call(function: V<'_, '_>) -> Result<(), Error> {
    keys(function, &["name", "arguments"])?;
    string(required(function, "name")?, "name")?;
    string(required(function, "arguments")?, "arguments")?;
    Ok(())
}

fn check_chat_history(
    message: V<'_, '_>,
    role: JsonString<'_>,
    context: &Context<'_>,
) -> Result<(), Error> {
    if role.eq_str("tool") || role.eq_str("function") {
        keys(
            message,
            if role.eq_str("tool") {
                &["role", "tool_call_id", "content"]
            } else {
                &["role", "name", "content"]
            },
        )?;
        let name = if role.eq_str("function") {
            Some(string(required(message, "name")?, "name")?)
        } else {
            None
        };
        let id = if role.eq_str("tool") {
            Some(required(message, "tool_call_id")?)
        } else {
            None
        };
        call_id(id, name, context)?;
        let result = required(message, "content")?;
        if result.kind() != Kind::String {
            return Err(unavailable("non-string tool result"));
        }
        return Ok(());
    }
    keys(message, &["role", "content", "tool_calls", "function_call"])?;
    let modern = message.get("tool_calls");
    let legacy = message.get("function_call");
    if modern.is_some() && legacy.is_some() {
        return Err(unavailable("simultaneous tool call spellings"));
    }
    if modern.is_some() || legacy.is_some() {
        if !role.eq_str("assistant") {
            return Err(invalid("tool_calls", "must belong to assistant"));
        }
        if let Some(calls) = modern {
            if calls.is_null() {
                return Err(unavailable("null tool call list"));
            }
            array(calls, "tool_calls")?;
            if calls.array().next().is_none() {
                return Err(unavailable("empty tool call list"));
            }
            for call in calls.array() {
                keys(call, &["id", "type", "function"])?;
                if !string(required(call, "type")?, "type")?.eq_str("function") {
                    return Err(unavailable("non-function tool call"));
                }
                let function = required(call, "function")?;
                check_call(function)?;
                call_id(
                    call.get("id"),
                    function.get("name").and_then(Value::as_string),
                    context,
                )?;
            }
        }
        if let Some(function) = legacy {
            if function.is_null() {
                return Err(unavailable("null legacy function call"));
            }
            check_call(function)?;
            call_id(
                None,
                function.get("name").and_then(Value::as_string),
                context,
            )?;
        }
        if let Some(content) = message.get("content") {
            if !content.is_null() {
                check_text(content, Protocol::OpenAiChat)?;
            }
        }
    } else {
        check_text(required(message, "content")?, Protocol::OpenAiChat)?;
    }
    Ok(())
}

fn request_item_metadata(
    item: V<'_, '_>,
    policy: Policy,
    count: &mut usize,
    fields: &mut u32,
) -> Result<(), Error> {
    for field in ["id", "status"] {
        if let Some(value) = item.get(field) {
            if !value.is_null() {
                string(value, field)?;
            }
            if field == "status" && !value.is_null() && !is(Some(value), "completed") {
                return Err(unavailable("non-completed request item"));
            }
            if !policy.drop_metadata {
                return Err(Error::PolicyViolation {
                    feature: "response item metadata",
                });
            }
            *count = count.checked_add(1).ok_or(Error::LimitExceeded {
                kind: "metadata losses",
                limit: usize::MAX,
            })?;
            *fields |= if field == "id" {
                METADATA_ITEM_ID
            } else {
                METADATA_ITEM_STATUS
            };
        }
    }
    Ok(())
}

fn check_response_history(
    item: V<'_, '_>,
    context: &Context<'_>,
    policy: Policy,
    count: &mut usize,
    fields: &mut u32,
) -> Result<(), Error> {
    if is(item.get("type"), "function_call") {
        keys(
            item,
            &["type", "call_id", "name", "arguments", "id", "status"],
        )?;
        let name = string(required(item, "name")?, "name")?;
        string(required(item, "arguments")?, "arguments")?;
        call_id(item.get("call_id"), Some(name), context)?;
    } else if is(item.get("type"), "function_call_output") {
        keys(item, &["type", "call_id", "output", "id", "status"])?;
        string(required(item, "call_id")?, "call_id")?;
        if required(item, "output")?.kind() != Kind::String {
            return Err(unavailable("non-string tool result"));
        }
    } else {
        keys(item, &["type", "role", "content", "id", "status"])?;
        if item.get("type").is_some() && !is(item.get("type"), "message") {
            return Err(unavailable("Responses input item"));
        }
        let role = string(required(item, "role")?, "role")?;
        if !["user", "assistant", "system", "developer"]
            .iter()
            .any(|name| role.eq_str(name))
        {
            return Err(invalid("role", "unknown Responses message role"));
        }
        check_text(required(item, "content")?, Protocol::OpenAiResponses)?;
    }
    request_item_metadata(item, policy, count, fields)
}

fn separate(writer: &mut Writer<'_>, first: &mut bool) {
    if !*first {
        writer.byte(b',');
    }
    *first = false;
}

fn write_call_item(writer: &mut Writer<'_>, function: V<'_, '_>, id: Text<'_>) {
    writer.raw(b"{\"type\":\"function_call\",\"call_id\":");
    text(writer, id);
    copy(writer, function, "name", b"name");
    copy(writer, function, "arguments", b"arguments");
    writer.byte(b'}');
}

fn write_chat_history(
    writer: &mut Writer<'_>,
    message: V<'_, '_>,
    context: &Context<'_>,
    first: &mut bool,
) -> Result<(), Error> {
    let role = string(required(message, "role")?, "role")?;
    if role.eq_str("tool") || role.eq_str("function") {
        let id = call_id(
            message.get("tool_call_id"),
            message.get("name").and_then(Value::as_string),
            context,
        )?;
        separate(writer, first);
        writer.raw(b"{\"type\":\"function_call_output\",\"call_id\":");
        text(writer, id);
        copy(writer, message, "content", b"output");
        writer.byte(b'}');
        return Ok(());
    }
    let modern = message.get("tool_calls");
    let legacy = message.get("function_call");
    if let Some(content) = message.get("content") {
        if content.kind() == Kind::String
            || (content.kind() == Kind::Array
                && (content.array().next().is_some() || (modern.is_none() && legacy.is_none())))
        {
            separate(writer, first);
            write_message(
                writer,
                role,
                content,
                Protocol::OpenAiChat,
                Protocol::OpenAiResponses,
            );
        }
    }
    for call in modern.into_iter().flat_map(Value::array) {
        let function = required(call, "function")?;
        let id = call_id(
            call.get("id"),
            function.get("name").and_then(Value::as_string),
            context,
        )?;
        separate(writer, first);
        write_call_item(writer, function, id);
    }
    if let Some(function) = legacy {
        let id = call_id(
            None,
            function.get("name").and_then(Value::as_string),
            context,
        )?;
        separate(writer, first);
        write_call_item(writer, function, id);
    }
    Ok(())
}

fn write_response_history(
    writer: &mut Writer<'_>,
    item: V<'_, '_>,
    context: &Context<'_>,
    first: &mut bool,
) -> Result<(), Error> {
    separate(writer, first);
    if is(item.get("type"), "function_call") {
        let id = call_id(
            item.get("call_id"),
            item.get("name").and_then(Value::as_string),
            context,
        )?;
        writer.raw(b"{\"role\":\"assistant\",\"tool_calls\":[{\"id\":");
        text(writer, id);
        writer.raw(b",\"type\":\"function\",\"function\":{\"name\":");
        writer.raw(required(item, "name")?.raw());
        copy(writer, item, "arguments", b"arguments");
        writer.raw(b"}}]}");
    } else if is(item.get("type"), "function_call_output") {
        writer.raw(b"{\"role\":\"tool\",\"tool_call_id\":");
        writer.raw(required(item, "call_id")?.raw());
        copy(writer, item, "output", b"content");
        writer.byte(b'}');
    } else {
        write_message(
            writer,
            string(required(item, "role")?, "role")?,
            required(item, "content")?,
            Protocol::OpenAiResponses,
            Protocol::OpenAiChat,
        );
    }
    Ok(())
}

fn check_text(value: V<'_, '_>, source: Protocol) -> Result<(), Error> {
    if value.kind() == Kind::String && source != Protocol::GeminiGenerateContent {
        return Ok(());
    }
    if value.kind() != Kind::Array {
        return Err(if value.is_null() {
            unavailable("null content shape")
        } else {
            invalid("content", "expected string or array")
        });
    }
    for block in value.array() {
        match source {
            Protocol::GeminiGenerateContent => keys(block, &["text"])?,
            Protocol::OpenAiResponses => {
                keys(block, &["type", "text", "annotations"])?;
                string(required(block, "type")?, "type")?;
                if !is(block.get("type"), "input_text") && !is(block.get("type"), "output_text") {
                    return Err(unavailable("non-text Responses block"));
                }
                if let Some(annotations) = block.get("annotations") {
                    if annotations.kind() != Kind::Array || annotations.array().next().is_some() {
                        return Err(unavailable("response annotations"));
                    }
                }
            }
            _ => {
                keys(block, &["type", "text"])?;
                string(required(block, "type")?, "type")?;
                if !is(block.get("type"), "text") {
                    return Err(unavailable("non-text content block"));
                }
            }
        }
        string(required(block, "text")?, "text")?;
    }
    Ok(())
}

fn write_blocks(
    writer: &mut Writer<'_>,
    content: V<'_, '_>,
    source: Protocol,
    target: Protocol,
    assistant: bool,
) {
    let mut first = true;
    writer.byte(b'[');
    let mut one = |value: JsonString<'_>| {
        if !first {
            writer.byte(b',');
        }
        first = false;
        match target {
            Protocol::GeminiGenerateContent => writer.raw(b"{\"text\":"),
            Protocol::OpenAiResponses => writer.raw(if assistant {
                b"{\"type\":\"output_text\",\"text\":"
            } else {
                b"{\"type\":\"input_text\",\"text\":"
            }),
            _ => writer.raw(b"{\"type\":\"text\",\"text\":"),
        }
        writer.json_string(value);
        writer.byte(b'}');
    };
    if let Some(value) = content.as_string() {
        one(value);
    } else {
        for block in content.array() {
            if let Some(value) = block.get("text").and_then(Value::as_string) {
                one(value);
            }
        }
    }
    let _ = source; // the preflight determines the source-specific block shape
    writer.byte(b']');
}

fn write_message(
    writer: &mut Writer<'_>,
    role: JsonString<'_>,
    content: V<'_, '_>,
    source: Protocol,
    target: Protocol,
) {
    let assistant = role.eq_str("assistant") || role.eq_str("model");
    writer.raw(b"{\"role\":");
    if target == Protocol::GeminiGenerateContent && assistant {
        writer.raw(b"\"model\"");
    } else if source == Protocol::GeminiGenerateContent && assistant {
        writer.raw(b"\"assistant\"");
    } else {
        writer.json_string(role);
    }
    writer.raw(if target == Protocol::GeminiGenerateContent {
        b",\"parts\":"
    } else {
        b",\"content\":"
    });
    if matches!(target, Protocol::OpenAiChat | Protocol::AnthropicMessages)
        && content.kind() == Kind::String
    {
        writer.raw(content.raw());
    } else {
        write_blocks(writer, content, source, target, assistant);
    }
    writer.byte(b'}');
}

fn request<'a>(
    source: Protocol,
    target: Protocol,
    root: V<'a, '_>,
    context: &Context<'a>,
    policy: Policy,
    output: &mut [u8],
) -> Result<EncodeReport<'a>, Error> {
    use Protocol::*;
    request_scalars(root, source, target)?;
    let model = match root.get("model") {
        Some(value) => Presence::Value(Text::Json(string(value, "model")?)),
        None => context.model.map_or(Presence::Missing, |value| {
            Presence::Value(Text::Utf8(value))
        }),
    };
    let mut system = None;
    let mut instructions = None;
    let mut dropped_metadata = 0;
    let mut metadata_loss_fields = 0;
    let messages;
    match source {
        OpenAiChat => {
            keys(
                root,
                if target == OpenAiResponses {
                    &[
                        "model",
                        "messages",
                        "stream",
                        "temperature",
                        "top_p",
                        "max_tokens",
                        "max_completion_tokens",
                        "stop",
                        "tools",
                        "functions",
                        "tool_choice",
                        "function_call",
                        "parallel_tool_calls",
                    ]
                } else {
                    &[
                        "model",
                        "messages",
                        "stream",
                        "temperature",
                        "top_p",
                        "max_tokens",
                        "max_completion_tokens",
                        "stop",
                    ]
                },
            )?;
            if target == OpenAiResponses {
                check_tools(root, source)?;
            }
            if root.get("max_tokens").is_some() && root.get("max_completion_tokens").is_some() {
                return Err(unavailable("two token limit fields"));
            }
            if target == OpenAiResponses && root.get("stop").is_some() {
                return Err(unavailable("stop sequences in Responses"));
            }
            messages = required(root, "messages")?;
            array(messages, "messages")?;
            for (position, message) in messages.array().enumerate() {
                let role = string(required(message, "role")?, "role")?;
                if ![
                    "system",
                    "developer",
                    "user",
                    "assistant",
                    "tool",
                    "function",
                ]
                .iter()
                .any(|value| role.eq_str(value))
                {
                    return Err(invalid("role", "unknown role"));
                }
                if target == OpenAiResponses {
                    check_chat_history(message, role, context)?;
                    continue;
                }
                keys(message, &["role", "content"])?;
                let content = required(message, "content")?;
                check_text(content, source)?;
                if role.eq_str("system") && target != OpenAiResponses {
                    if position != 0 || system.is_some() {
                        return Err(unavailable("system message ordering"));
                    }
                    system = Some(content);
                } else if !role.eq_str("user")
                    && !role.eq_str("assistant")
                    && !(target == OpenAiResponses
                        && (role.eq_str("system") || role.eq_str("developer")))
                {
                    return Err(unavailable("message role"));
                }
            }
        }
        OpenAiResponses => {
            keys(
                root,
                &[
                    "model",
                    "input",
                    "instructions",
                    "stream",
                    "temperature",
                    "top_p",
                    "max_output_tokens",
                    "tools",
                    "tool_choice",
                    "parallel_tool_calls",
                ],
            )?;
            check_tools(root, source)?;
            messages = required(root, "input")?;
            if let Some(value) = root.get("instructions") {
                if value.is_null() {
                    return Err(unavailable("null instructions"));
                }
                string(value, "instructions")?;
                instructions = Some(value);
            }
            if messages.kind() == Kind::String { /* input string denotes one user message */
            } else {
                array(messages, "input")?;
                for message in messages.array() {
                    check_response_history(
                        message,
                        context,
                        policy,
                        &mut dropped_metadata,
                        &mut metadata_loss_fields,
                    )?;
                }
            }
        }
        AnthropicMessages => {
            keys(
                root,
                &[
                    "model",
                    "messages",
                    "system",
                    "stream",
                    "temperature",
                    "top_p",
                    "max_tokens",
                    "stop_sequences",
                ],
            )?;
            messages = required(root, "messages")?;
            array(messages, "messages")?;
            if let Some(value) = root.get("system") {
                check_text(value, source)?;
                system = Some(value);
            }
            for message in messages.array() {
                keys(message, &["role", "content"])?;
                if !is(message.get("role"), "user") && !is(message.get("role"), "assistant") {
                    return Err(invalid(
                        "role",
                        "Anthropic messages must be user or assistant",
                    ));
                }
                check_text(required(message, "content")?, source)?;
            }
        }
        GeminiGenerateContent => {
            keys(root, &["contents", "systemInstruction", "generationConfig"])?;
            messages = required(root, "contents")?;
            array(messages, "contents")?;
            if let Some(value) = root.get("systemInstruction") {
                keys(value, &["parts", "role"])?;
                if value.get("role").is_some() {
                    return Err(unavailable("system instruction role"));
                }
                let parts = required(value, "parts")?;
                check_text(parts, source)?;
                system = Some(parts);
            }
            for message in messages.array() {
                keys(message, &["role", "parts"])?;
                if message.get("role").is_none() {
                    return Err(unavailable("implicit Gemini user role"));
                }
                if !is(message.get("role"), "user") && !is(message.get("role"), "model") {
                    return Err(invalid("role", "Gemini content must be user or model"));
                }
                check_text(required(message, "parts")?, source)?;
            }
            if let Some(config) = root.get("generationConfig") {
                keys(
                    config,
                    &["temperature", "topP", "maxOutputTokens", "stopSequences"],
                )?;
            }
        }
        _ => return Err(unavailable("request source")),
    }
    if target != GeminiGenerateContent && model.is_missing() {
        return Err(Error::ContextRequired { field: "model" });
    }
    if target == AnthropicMessages
        && root
            .get("max_tokens")
            .or_else(|| root.get("max_completion_tokens"))
            .is_none()
        && context.max_tokens.is_none()
    {
        return Err(Error::ContextRequired {
            field: "max_tokens",
        });
    }

    let mut writer = Writer::new(output);
    writer.byte(b'{');
    if target == GeminiGenerateContent {
        writer.raw(b"\"contents\":");
    } else {
        writer.raw(b"\"model\":");
        if let Presence::Value(value) = model {
            text(&mut writer, value);
        }
        writer.raw(if target == OpenAiResponses {
            b",\"input\":"
        } else {
            b",\"messages\":"
        });
    }
    writer.byte(b'[');
    let mut first = true;
    if target == OpenAiChat {
        if let Some(content) = instructions.or(system) {
            writer.raw(b"{\"role\":\"system\",\"content\":");
            if content.kind() == Kind::String {
                writer.raw(content.raw());
            } else {
                write_blocks(&mut writer, content, source, target, false);
            }
            writer.byte(b'}');
            first = false;
        }
    }
    if source == OpenAiResponses && messages.kind() == Kind::String {
        if !first {
            writer.byte(b',');
        }
        writer.raw(b"{\"role\":\"user\",\"content\":");
        writer.raw(messages.raw());
        writer.byte(b'}');
    } else {
        for message in messages.array() {
            if source == OpenAiChat && target == OpenAiResponses {
                write_chat_history(&mut writer, message, context, &mut first)?;
                continue;
            }
            if source == OpenAiResponses && target == OpenAiChat {
                write_response_history(&mut writer, message, context, &mut first)?;
                continue;
            }
            let role = string(required(message, "role")?, "role")?;
            if source == OpenAiChat && target != OpenAiResponses && role.eq_str("system") {
                continue;
            }
            let content = required(
                message,
                if source == GeminiGenerateContent {
                    "parts"
                } else {
                    "content"
                },
            )?;
            if !first {
                writer.byte(b',');
            }
            first = false;
            write_message(&mut writer, role, content, source, target);
        }
    }
    writer.byte(b']');
    if source == OpenAiChat {
        if let Some(content) = system {
            if target == AnthropicMessages {
                property(&mut writer, b"system");
                if content.kind() == Kind::String {
                    writer.raw(content.raw());
                } else {
                    write_blocks(&mut writer, content, source, target, false);
                }
            } else if target == GeminiGenerateContent {
                writer.raw(b",\"systemInstruction\":{\"parts\":");
                write_blocks(&mut writer, content, source, target, false);
                writer.byte(b'}');
            }
        }
    }
    let parameters = if source == GeminiGenerateContent {
        root.get("generationConfig")
    } else {
        Some(root)
    };
    if target == GeminiGenerateContent {
        writer.raw(b",\"generationConfig\":{");
        let mut separator = false;
        for (input, output) in [
            ("temperature", "temperature"),
            ("top_p", "topP"),
            ("max_tokens", "maxOutputTokens"),
            ("max_completion_tokens", "maxOutputTokens"),
            ("stop", "stopSequences"),
        ] {
            if let Some(value) = root.get(input) {
                if separator {
                    writer.byte(b',');
                }
                separator = true;
                writer.string(output.as_bytes());
                writer.byte(b':');
                if input == "stop" && value.kind() == Kind::String {
                    writer.byte(b'[');
                    writer.raw(value.raw());
                    writer.byte(b']');
                } else {
                    writer.raw(value.raw());
                }
            }
        }
        writer.byte(b'}');
    } else if let Some(parameters) = parameters {
        for (input, output) in match source {
            GeminiGenerateContent => [
                ("temperature", "temperature"),
                ("topP", "top_p"),
                ("maxOutputTokens", "max_completion_tokens"),
                ("stopSequences", "stop"),
            ],
            OpenAiResponses => [
                ("temperature", "temperature"),
                ("top_p", "top_p"),
                ("max_output_tokens", "max_completion_tokens"),
                ("stream", "stream"),
            ],
            AnthropicMessages => [
                ("temperature", "temperature"),
                ("top_p", "top_p"),
                ("max_tokens", "max_completion_tokens"),
                ("stop_sequences", "stop"),
            ],
            _ => [
                ("temperature", "temperature"),
                ("top_p", "top_p"),
                (
                    "max_tokens",
                    if target == OpenAiResponses {
                        "max_output_tokens"
                    } else {
                        "max_tokens"
                    },
                ),
                (
                    "max_completion_tokens",
                    if target == OpenAiResponses {
                        "max_output_tokens"
                    } else {
                        "max_tokens"
                    },
                ),
            ],
        } {
            copy(&mut writer, parameters, input, output.as_bytes());
        }
        if source != OpenAiResponses {
            copy(&mut writer, root, "stream", b"stream");
        }
        if source == OpenAiChat && target == AnthropicMessages {
            if let Some(stop) = root.get("stop") {
                property(&mut writer, b"stop_sequences");
                if stop.kind() == Kind::String {
                    writer.byte(b'[');
                    writer.raw(stop.raw());
                    writer.byte(b']');
                } else {
                    writer.raw(stop.raw());
                }
            }
            if root
                .get("max_tokens")
                .or_else(|| root.get("max_completion_tokens"))
                .is_none()
            {
                property(&mut writer, b"max_tokens");
                if let Some(value) = context.max_tokens {
                    unsigned(&mut writer, value);
                }
            }
        }
    }
    if matches!(
        (source, target),
        (OpenAiChat, OpenAiResponses) | (OpenAiResponses, OpenAiChat)
    ) {
        write_tools(&mut writer, root, source, target);
    }
    writer.byte(b'}');
    let mut report = EncodeReport {
        written: writer.finish()?,
        dropped_metadata,
        metadata_loss_fields,
        ..EncodeReport::default()
    };
    if target == GeminiGenerateContent {
        report.transport_model = model;
        report.transport_stream = match root.get("stream") {
            None => Presence::Missing,
            Some(value) if value.is_null() => Presence::Null,
            Some(value) => value.as_bool().map_or(Presence::Missing, Presence::Value),
        };
    }
    Ok(report)
}

fn only_text<'a>(content: V<'a, '_>, source: Protocol) -> Result<JsonString<'a>, Error> {
    check_text(content, source)?;
    if let Some(value) = content.as_string() {
        return Ok(value);
    }
    let mut blocks = content.array();
    let block = blocks
        .next()
        .ok_or_else(|| unavailable("empty response content"))?;
    if blocks.next().is_some() {
        return Err(unavailable("multiple response text blocks"));
    }
    string(required(block, "text")?, "text")
}

fn finish<'a>(choice: V<'a, '_>, source: Protocol) -> Result<JsonString<'a>, Error> {
    let field = match source {
        Protocol::AnthropicMessages => "stop_reason",
        Protocol::GeminiGenerateContent => "finishReason",
        Protocol::OpenAiResponses => "status",
        _ => "finish_reason",
    };
    let value = choice
        .get(field)
        .ok_or_else(|| unavailable("missing finish reason"))?;
    if value.is_null() {
        return Err(unavailable("null finish reason"));
    }
    string(value, field)
}

fn check_finish(reason: JsonString<'_>, source: Protocol) -> Result<(), Error> {
    let allowed: &[&str] = match source {
        Protocol::AnthropicMessages => &["end_turn", "max_tokens"],
        Protocol::GeminiGenerateContent => &["STOP", "MAX_TOKENS", "SAFETY"],
        Protocol::OpenAiResponses => &["completed", "incomplete"],
        _ => &["stop", "length", "content_filter"],
    };
    if allowed.iter().any(|value| reason.eq_str(value)) {
        Ok(())
    } else {
        Err(unavailable("response finish reason"))
    }
}

fn write_finish(
    writer: &mut Writer<'_>,
    reason: JsonString<'_>,
    source: Protocol,
    target: Protocol,
    incomplete: Option<V<'_, '_>>,
) {
    let length = reason.eq_str("length")
        || reason.eq_str("max_tokens")
        || reason.eq_str("MAX_TOKENS")
        || (source == Protocol::OpenAiResponses
            && reason.eq_str("incomplete")
            && is(
                incomplete.and_then(|v| v.get("reason")),
                "max_output_tokens",
            ));
    let safety = reason.eq_str("content_filter")
        || reason.eq_str("SAFETY")
        || (source == Protocol::OpenAiResponses
            && reason.eq_str("incomplete")
            && is(incomplete.and_then(|v| v.get("reason")), "content_filter"));
    writer.raw(match target {
        Protocol::AnthropicMessages => {
            if length {
                b"\"max_tokens\""
            } else {
                b"\"end_turn\""
            }
        }
        Protocol::GeminiGenerateContent => {
            if length {
                b"\"MAX_TOKENS\""
            } else if safety {
                b"\"SAFETY\""
            } else {
                b"\"STOP\""
            }
        }
        Protocol::OpenAiResponses => {
            if length || safety {
                b"\"incomplete\""
            } else {
                b"\"completed\""
            }
        }
        _ => {
            if length {
                b"\"length\""
            } else if safety {
                b"\"content_filter\""
            } else {
                b"\"stop\""
            }
        }
    });
}

fn usage_keys(source: Protocol) -> (&'static str, &'static str, &'static str, &'static str) {
    match source {
        Protocol::AnthropicMessages => ("usage", "input_tokens", "output_tokens", "total_tokens"),
        Protocol::OpenAiResponses => ("usage", "input_tokens", "output_tokens", "total_tokens"),
        Protocol::GeminiGenerateContent => (
            "usageMetadata",
            "promptTokenCount",
            "candidatesTokenCount",
            "totalTokenCount",
        ),
        _ => (
            "usage",
            "prompt_tokens",
            "completion_tokens",
            "total_tokens",
        ),
    }
}

fn check_usage(root: V<'_, '_>, source: Protocol, target: Protocol) -> Result<(), Error> {
    let (field, input, output, total) = usage_keys(source);
    if let Some(usage) = root.get(field) {
        if usage.is_null() {
            return Err(unavailable("null response usage"));
        }
        if source == Protocol::AnthropicMessages {
            keys(usage, &[input, output])?;
        } else {
            keys(usage, &[input, output, total])?;
        }
        for (_, value) in usage.object() {
            if value.is_null() {
                return Err(unavailable("null usage counter"));
            }
            if !integer(value) {
                return Err(invalid("usage", "expected unsigned integer"));
            }
        }
        if target == Protocol::AnthropicMessages {
            if let Some(total) = usage.get(total) {
                let calculated = usage
                    .get(input)
                    .and_then(Value::as_u64)
                    .and_then(|v| v.checked_add(usage.get(output)?.as_u64()?));
                if total.as_u64() != calculated {
                    return Err(unavailable("non-additive total token count"));
                }
            }
        }
    }
    Ok(())
}

fn write_usage(writer: &mut Writer<'_>, root: V<'_, '_>, source: Protocol, target: Protocol) {
    let (field, input, output, total) = usage_keys(source);
    let (target_field, target_input, target_output, target_total) = usage_keys(target);
    if let Some(usage) = root.get(field) {
        property(writer, target_field.as_bytes());
        writer.byte(b'{');
        let mut separator = false;
        for (from, to) in [
            (input, target_input),
            (output, target_output),
            (total, target_total),
        ] {
            if target == Protocol::AnthropicMessages && from == total {
                continue;
            }
            if let Some(value) = usage.get(from) {
                if separator {
                    writer.byte(b',');
                }
                separator = true;
                writer.string(to.as_bytes());
                writer.byte(b':');
                writer.raw(value.raw());
            }
        }
        if target != Protocol::AnthropicMessages && usage.get(total).is_none() {
            if let Some(value) = usage
                .get(input)
                .and_then(Value::as_u64)
                .and_then(|a| a.checked_add(usage.get(output)?.as_u64()?))
            {
                if separator {
                    writer.byte(b',');
                }
                writer.string(target_total.as_bytes());
                writer.byte(b':');
                unsigned(writer, value);
            }
        }
        writer.byte(b'}');
    }
}

fn response<'a>(
    source: Protocol,
    target: Protocol,
    root: V<'a, '_>,
    context: &Context<'a>,
    policy: Policy,
    output: &mut [u8],
) -> Result<EncodeReport<'a>, Error> {
    use Protocol::*;
    if root.kind() != Kind::Object {
        return Err(invalid("response", "expected an object"));
    }
    let mut dropped_metadata = 0;
    let mut metadata_loss_fields = 0;
    let id = field_or_context(
        root,
        if source == GeminiGenerateContent {
            "responseId"
        } else {
            "id"
        },
        context.response_id,
    )?;
    let model = field_or_context(
        root,
        if source == GeminiGenerateContent {
            "modelVersion"
        } else {
            "model"
        },
        context.model,
    )?;
    let created = root.get(if source == OpenAiResponses {
        "created_at"
    } else {
        "created"
    });
    if created.is_some_and(|value| !integer(value)) {
        return Err(invalid("created", "expected unsigned integer"));
    }
    if matches!(target, OpenAiChat | OpenAiResponses)
        && created.is_none()
        && context.created.is_none()
    {
        return Err(Error::ContextRequired { field: "created" });
    }
    if matches!(target, AnthropicMessages | GeminiGenerateContent) && created.is_some() {
        if !policy.drop_metadata {
            return Err(Error::PolicyViolation {
                feature: "response timestamp",
            });
        }
        dropped_metadata += 1;
        metadata_loss_fields |= METADATA_CREATED;
    }
    let choices;
    let mut singleton_content = None;
    let mut singleton_finish = None;
    match source {
        OpenAiChat => {
            keys(
                root,
                &["id", "object", "created", "model", "choices", "usage"],
            )?;
            choices = Some(required(root, "choices")?);
            if let Some(value) = choices {
                array(value, "choices")?;
            }
            if root.get("object").is_some() && !is(root.get("object"), "chat.completion") {
                return Err(invalid("object", "expected chat.completion"));
            }
            if target != GeminiGenerateContent && choices.is_some_and(|v| v.array().count() != 1) {
                return Err(unavailable("multiple response choices"));
            }
            for choice in choices.into_iter().flat_map(Value::array) {
                keys(choice, &["index", "message", "finish_reason"])?;
                if required(choice, "index")?.as_u64().is_none() {
                    return Err(invalid("index", "expected unsigned integer"));
                }
                if target != GeminiGenerateContent
                    && choice.get("index").and_then(Value::as_u64) != Some(0)
                {
                    return Err(unavailable("nonzero single choice index"));
                }
                let message = required(choice, "message")?;
                keys(message, &["role", "content"])?;
                if !is(message.get("role"), "assistant") {
                    return Err(unavailable("response role"));
                }
                let content = required(message, "content")?;
                if content.is_null() || content.kind() == Kind::Array {
                    return Err(unavailable("non-string response content"));
                }
                string(content, "content")?;
                let reason = finish(choice, source)?;
                check_finish(reason, source)?;
                if target == AnthropicMessages && reason.eq_str("content_filter") {
                    return Err(unavailable("safety finish in Anthropic"));
                }
            }
        }
        AnthropicMessages => {
            if root.get("type").is_some() && !is(root.get("type"), "message") {
                return Err(invalid("type", "expected message"));
            }
            keys(
                root,
                &[
                    "id",
                    "type",
                    "role",
                    "model",
                    "content",
                    "stop_reason",
                    "stop_sequence",
                    "usage",
                ],
            )?;
            if !is(root.get("role"), "assistant") {
                return Err(unavailable("response role"));
            }
            if root.get("stop_sequence").is_some_and(|v| !v.is_null()) {
                return Err(unavailable("matched stop sequence"));
            }
            singleton_content = Some(only_text(required(root, "content")?, source)?);
            let reason = finish(root, source)?;
            check_finish(reason, source)?;
            singleton_finish = Some(reason);
            choices = None;
        }
        OpenAiResponses => {
            if root.get("object").is_some() && !is(root.get("object"), "response") {
                return Err(invalid("object", "expected response"));
            }
            keys(
                root,
                &[
                    "id",
                    "object",
                    "created_at",
                    "model",
                    "output",
                    "status",
                    "usage",
                    "incomplete_details",
                    "error",
                ],
            )?;
            if root.get("error").is_some_and(|v| !v.is_null()) {
                return Err(unavailable("failed response"));
            }
            let reason = finish(root, source)?;
            check_finish(reason, source)?;
            if reason.eq_str("incomplete") {
                let details = required(root, "incomplete_details")?;
                keys(details, &["reason"])?;
                if !is(details.get("reason"), "max_output_tokens")
                    && !is(details.get("reason"), "content_filter")
                {
                    return Err(unavailable("incomplete response reason"));
                }
            } else if root.get("incomplete_details").is_some_and(|v| !v.is_null()) {
                return Err(unavailable("completed response incomplete details"));
            }
            let output = required(root, "output")?;
            array(output, "output")?;
            let mut items = output.array();
            let item = items
                .next()
                .ok_or_else(|| unavailable("empty response output"))?;
            if items.next().is_some() {
                return Err(unavailable("multiple response output items"));
            }
            keys(item, &["type", "id", "status", "role", "content"])?;
            for field in ["id", "status", "role", "type"] {
                if let Some(value) = item.get(field) {
                    if !value.is_null() {
                        string(value, field)?;
                    }
                }
            }
            if !is(item.get("type"), "message") || !is(item.get("role"), "assistant") {
                return Err(unavailable("response output item"));
            }
            if item.get("status").is_some() && !is(item.get("status"), "completed") {
                return Err(unavailable("non-completed response item"));
            }
            for field in ["id", "status"] {
                if item.get(field).is_some() {
                    if !policy.drop_metadata {
                        return Err(Error::PolicyViolation {
                            feature: "response item metadata",
                        });
                    }
                    dropped_metadata += 1;
                    metadata_loss_fields |= if field == "id" {
                        METADATA_ITEM_ID
                    } else {
                        METADATA_ITEM_STATUS
                    };
                }
            }
            singleton_content = Some(only_text(required(item, "content")?, source)?);
            singleton_finish = Some(reason);
            choices = None;
        }
        GeminiGenerateContent => {
            keys(
                root,
                &["responseId", "modelVersion", "candidates", "usageMetadata"],
            )?;
            choices = Some(required(root, "candidates")?);
            if let Some(value) = choices {
                array(value, "candidates")?;
            }
            for choice in choices.into_iter().flat_map(Value::array) {
                keys(choice, &["index", "content", "finishReason"])?;
                if choice.get("index").is_some_and(|v| v.as_u64().is_none()) {
                    return Err(invalid("index", "expected unsigned integer"));
                }
                let content = required(choice, "content")?;
                keys(content, &["role", "parts"])?;
                if content.get("role").is_some() && !is(content.get("role"), "model") {
                    return Err(unavailable("response role"));
                }
                only_text(required(content, "parts")?, source)?;
                check_finish(finish(choice, source)?, source)?;
            }
        }
        _ => return Err(unavailable("response source")),
    }
    check_usage(root, source, target)?;
    let output_item_id = if target == OpenAiResponses {
        Some(*context.item_ids.first().ok_or(Error::ContextRequired {
            field: "output item id",
        })?)
    } else {
        None
    };
    let mut writer = Writer::new(output);
    writer.raw(if target == GeminiGenerateContent {
        b"{\"responseId\":"
    } else {
        b"{\"id\":"
    });
    text(&mut writer, id);
    property(
        &mut writer,
        if target == GeminiGenerateContent {
            b"modelVersion"
        } else {
            b"model"
        },
    );
    text(&mut writer, model);
    if matches!(target, OpenAiChat | OpenAiResponses) {
        property(
            &mut writer,
            if target == OpenAiResponses {
                b"created_at"
            } else {
                b"created"
            },
        );
        if let Some(value) = created {
            writer.raw(value.raw());
        } else if let Some(value) = context.created {
            unsigned(&mut writer, value);
        }
        writer.raw(if target == OpenAiResponses {
            b",\"object\":\"response\""
        } else {
            b",\"object\":\"chat.completion\""
        });
    }
    if target == OpenAiChat || target == GeminiGenerateContent {
        writer.raw(if target == OpenAiChat {
            b",\"choices\":["
        } else {
            b",\"candidates\":["
        });
        if let Some(choices) = choices {
            for (ordinal, choice) in choices.array().enumerate() {
                if ordinal != 0 {
                    writer.byte(b',');
                }
                let index = choice
                    .get("index")
                    .and_then(Value::as_u64)
                    .unwrap_or(ordinal as u64);
                let content = if source == OpenAiChat {
                    string(
                        required(required(choice, "message")?, "content")?,
                        "content",
                    )?
                } else {
                    only_text(required(required(choice, "content")?, "parts")?, source)?
                };
                write_choice(
                    &mut writer,
                    index,
                    content,
                    finish(choice, source)?,
                    source,
                    target,
                    root.get("incomplete_details"),
                );
            }
        } else if let (Some(content), Some(reason)) = (singleton_content, singleton_finish) {
            write_choice(
                &mut writer,
                0,
                content,
                reason,
                source,
                target,
                root.get("incomplete_details"),
            );
        }
        writer.byte(b']');
    } else {
        let choice = choices
            .and_then(|v| v.array().next())
            .ok_or_else(|| unavailable("response choice"))?;
        let content = string(
            required(required(choice, "message")?, "content")?,
            "content",
        )?;
        let reason = finish(choice, source)?;
        if target == AnthropicMessages {
            writer.raw(b",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":");
            writer.json_string(content);
            writer.raw(b"}],\"stop_reason\":");
            write_finish(&mut writer, reason, source, target, None);
        } else {
            writer.raw(b",\"output\":[{\"type\":\"message\",\"role\":\"assistant\",\"id\":");
            if let Some(id) = output_item_id {
                writer.string(id.as_bytes());
            }
            writer
                .raw(b",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":");
            writer.json_string(content);
            writer.raw(b"}]}],\"status\":");
            write_finish(&mut writer, reason, source, target, None);
            if reason.eq_str("length") || reason.eq_str("content_filter") {
                writer.raw(b",\"incomplete_details\":{\"reason\":");
                writer.raw(if reason.eq_str("length") {
                    b"\"max_output_tokens\""
                } else {
                    b"\"content_filter\""
                });
                writer.byte(b'}');
            }
        }
    }
    write_usage(&mut writer, root, source, target);
    writer.byte(b'}');
    Ok(EncodeReport {
        written: writer.finish()?,
        dropped_metadata,
        metadata_loss_fields,
        ..EncodeReport::default()
    })
}

fn write_choice(
    writer: &mut Writer<'_>,
    index: u64,
    content: JsonString<'_>,
    reason: JsonString<'_>,
    source: Protocol,
    target: Protocol,
    incomplete: Option<V<'_, '_>>,
) {
    writer.raw(b"{\"index\":");
    unsigned(writer, index);
    if target == Protocol::OpenAiChat {
        writer.raw(b",\"message\":{\"role\":\"assistant\",\"content\":");
        writer.json_string(content);
        writer.raw(b"},\"finish_reason\":");
    } else {
        writer.raw(b",\"content\":{\"role\":\"model\",\"parts\":[{\"text\":");
        writer.json_string(content);
        writer.raw(b"}]},\"finishReason\":");
    }
    write_finish(writer, reason, source, target, incomplete);
    writer.byte(b'}');
}
