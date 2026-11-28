//! Native Anthropic Messages transport and request controls.

use super::invalid;
use crate::{Error, json::Value};

pub const VERSION: &str = "2023-06-01";
const CLI: &str = "claude-code-20250219";
const INTERLEAVED: &str = "interleaved-thinking-2025-05-14";
const CONTEXT: &str = "context-management-2025-06-27";
pub const EFFORT_BETA: &str = "effort-2025-11-24";
const DISPLAY_UPDATES: &str = "thinking-display-updates-2026-08-18";
const TOKEN_PROGRESS: &str = "thinking-token-count-2026-05-13";

/// Validate the standard Messages version and known SDK negotiation markers.
pub fn validate_messages_envelope(
    version: Option<&str>,
    beta: Option<&str>,
    query: Option<&str>,
) -> Result<(), Error> {
    if version.is_some_and(|v| v != VERSION)
        || query.is_some_and(|q| q != "beta=true")
        || beta.is_some_and(|value| {
            !value.split(',').all(|marker| {
                matches!(
                    marker.trim(),
                    CLI | INTERLEAVED | CONTEXT | TOKEN_PROGRESS | EFFORT_BETA | DISPLAY_UPDATES
                )
            })
        })
    {
        return Err(Error::Unsupported {
            feature: "unsupported native Anthropic version, beta or query",
        });
    }
    Ok(())
}

/// Validate a Messages output configuration carrying a named thinking effort.
pub fn messages_effort(controls: Value<'_, '_>) -> Result<Option<&'static str>, Error> {
    let Some(config) = controls.get("output_config") else {
        return Ok(None);
    };
    let value = config.get("effort").and_then(Value::as_string);
    if config.object().any(|(key, _)| !key.eq_str("effort")) {
        return Err(invalid(
            "output_config",
            "unsupported native output control",
        ));
    }
    ["low", "medium", "high", "xhigh", "max"]
        .into_iter()
        .find(|effort| value.is_some_and(|v| v.eq_str(effort)))
        .map(Some)
        .ok_or_else(|| invalid("output_config", "invalid thinking effort"))
}

/// Project a Messages effort to Responses when the native-only context
/// policy retains all thinking and no fixed thinking budget is requested.
pub fn responses_effort(controls: Value<'_, '_>) -> Result<Option<&'static str>, Error> {
    let unsupported = || Error::Unsupported {
        feature: "native Anthropic thinking or context control in Responses",
    };
    if controls.get("thinking").is_some_and(|v| {
        !v.get("type")
            .and_then(Value::as_string)
            .is_some_and(|v| v.eq_str("adaptive"))
            || v.get("display")
                .is_some_and(|d| !d.as_string().is_some_and(|d| d.eq_str("omitted")))
    }) || controls.get("context_management").is_some_and(|ctx| {
        !ctx.get("edits")
            .and_then(|edits| edits.array().next())
            .and_then(|edit| edit.get("keep"))
            .and_then(Value::as_string)
            .is_some_and(|keep| keep.eq_str("all"))
    }) {
        return Err(unsupported());
    }
    messages_effort(controls)
}

/// Project inclusive input usage into the disjoint Messages input count.
pub fn uncached_input(
    input: u64,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
) -> Result<u64, Error> {
    input
        .checked_sub(cache_read.unwrap_or(0))
        .and_then(|left| left.checked_sub(cache_write.unwrap_or(0)))
        .ok_or_else(|| invalid("usage", "cache counts exceed inclusive input"))
}

/// Validate the native fields used by Claude Code and identify the supplier
/// betas needed by their actual values. Client CLI markers never select betas.
pub fn messages_control_betas(
    controls: Value<'_, '_>,
    max_tokens: u64,
) -> Result<Option<&'static str>, Error> {
    let thinking = controls.get("thinking");
    let context = controls.get("context_management");
    let mut manual = false;
    let mut enabled = false;
    let mut updates = false;
    if let Some(thinking) = thinking {
        let field = "thinking";
        let kind = thinking.get("type").and_then(Value::as_string);
        let is = |name| kind.is_some_and(|kind| kind.eq_str(name));
        manual = is("enabled");
        enabled = manual || is("adaptive");
        let budget = thinking.get("budget_tokens");
        let display = thinking.get("display");
        updates = display
            .and_then(Value::as_string)
            .is_some_and(|d| d.eq_str("updates"));
        if !(enabled || is("disabled"))
            || thinking.object().any(|(k, _)| {
                !["type", "budget_tokens", "display"]
                    .iter()
                    .any(|v| k.eq_str(v))
            })
            || (manual
                && budget
                    .and_then(Value::as_u64)
                    .is_none_or(|n| n < 1024 || n >= max_tokens))
            || (!manual && budget.is_some())
            || display.is_some_and(|d| {
                !enabled
                    || d.as_string().is_none_or(|d| {
                        !d.eq_str("omitted") && !d.eq_str("summarized") && !d.eq_str("updates")
                    })
            })
        {
            return Err(invalid(field, "invalid native thinking configuration"));
        }
    }
    if let Some(context) = context {
        let field = "context_management";
        let invalid_context = || invalid(field, "unsupported native context edit");
        let mut edits = context.get("edits").ok_or_else(invalid_context)?.array();
        let edit = edits.next().ok_or_else(invalid_context)?;
        let keep = edit.get("keep").ok_or_else(invalid_context)?;
        if !enabled
            || edits.next().is_some()
            || context.object().any(|(key, _)| !key.eq_str("edits"))
            || edit
                .object()
                .any(|(key, _)| !key.eq_str("type") && !key.eq_str("keep"))
            || !edit
                .get("type")
                .and_then(Value::as_string)
                .is_some_and(|v| v.eq_str("clear_thinking_20251015"))
            || !(keep.as_string().is_some_and(|v| v.eq_str("all"))
                || (keep
                    .get("type")
                    .and_then(Value::as_string)
                    .is_some_and(|v| v.eq_str("thinking_turns"))
                    && keep
                        .get("value")
                        .and_then(Value::as_u64)
                        .is_some_and(|v| v > 0)
                    && keep
                        .object()
                        .all(|(key, _)| key.eq_str("type") || key.eq_str("value"))))
        {
            return Err(invalid_context());
        }
    }
    Ok(match (manual, context.is_some(), updates) {
        (true, true, true) => Some(
            "interleaved-thinking-2025-05-14,context-management-2025-06-27,thinking-display-updates-2026-08-18",
        ),
        (true, false, true) => {
            Some("interleaved-thinking-2025-05-14,thinking-display-updates-2026-08-18")
        }
        (false, true, true) => {
            Some("context-management-2025-06-27,thinking-display-updates-2026-08-18")
        }
        (false, false, true) => Some(DISPLAY_UPDATES),
        (true, true, false) => {
            Some("interleaved-thinking-2025-05-14,context-management-2025-06-27")
        }
        (true, false, false) => Some(INTERLEAVED),
        (false, true, false) => Some(CONTEXT),
        (false, false, false) => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::{Limits, Token, parse};

    #[test]
    fn claude_code_negotiation_accepts_known_combinations_only() {
        let beta = "interleaved-thinking-2025-05-14, context-management-2025-06-27,claude-code-20250219,thinking-token-count-2026-05-13";
        assert!(validate_messages_envelope(Some(VERSION), Some(beta), Some("beta=true")).is_ok());
        for (version, markers, query) in [
            (Some("future"), Some(beta), None),
            (Some(VERSION), Some("claude-code-20250219,unknown"), None),
            (Some(VERSION), Some("claude-code-20250219,"), None),
            (Some(VERSION), Some(beta), Some("beta=true&beta=true")),
        ] {
            assert!(validate_messages_envelope(version, markers, query).is_err());
        }
    }

    #[test]
    fn claude_code_effort_projects_without_fabricating_native_reasoning() {
        let mut tokens = [Token::EMPTY; 64];
        let request = parse(
            br#"{"thinking":{"type":"adaptive","display":"omitted"},"output_config":{"effort":"high"},"context_management":{"edits":[{"type":"clear_thinking_20251015","keep":"all"}]}}"#,
            &mut tokens,
            Limits::default(),
        ).unwrap();
        assert_eq!(responses_effort(request.root()), Ok(Some("high")));
        let request = parse(
            br#"{"thinking":{"type":"enabled","budget_tokens":1024},"output_config":{"effort":"high"}}"#,
            &mut tokens,
            Limits::default(),
        ).unwrap();
        assert!(responses_effort(request.root()).is_err());
    }

    #[test]
    fn thinking_display_updates_requires_native_enabled_thinking() {
        for (body, expected) in [
            (br#"{"thinking":{"type":"adaptive","display":"updates"}}"#.as_slice(),
             Some(DISPLAY_UPDATES)),
            (br#"{"thinking":{"type":"adaptive","display":"updates"},"context_management":{"edits":[{"type":"clear_thinking_20251015","keep":"all"}]}}"#.as_slice(),
             Some("context-management-2025-06-27,thinking-display-updates-2026-08-18")),
        ] {
            let mut tokens = [Token::EMPTY; 64];
            let doc = parse(body, &mut tokens, Limits::default()).unwrap();
            let betas = messages_control_betas(doc.root(), 128_000).unwrap();
            assert_eq!(betas, expected);
            assert!(validate_messages_envelope(Some(VERSION), betas, None).is_ok());
            assert!(responses_effort(doc.root()).is_err());
        }
        let mut tokens = [Token::EMPTY; 64];
        let doc = parse(
            br#"{"thinking":{"type":"disabled","display":"updates"}}"#,
            &mut tokens,
            Limits::default(),
        )
        .unwrap();
        assert!(messages_control_betas(doc.root(), 128_000).is_err());
    }

    #[test]
    fn claude_code_native_options_preserve_output_bound_and_validate_context() {
        for (body, expected) in [
            (br#"{"thinking":{"type":"enabled","budget_tokens":63999,"display":"omitted"},"context_management":{"edits":[{"type":"clear_thinking_20251015","keep":"all"}]}}"#.as_slice(),
             Ok(Some("interleaved-thinking-2025-05-14,context-management-2025-06-27"))),
            (br#"{"thinking":{"type":"adaptive"}}"#.as_slice(), Ok(None)),
            (br#"{"thinking":{"type":"disabled"}}"#.as_slice(), Ok(None)),
        ] {
            let mut tokens = [Token::EMPTY; 64];
            let doc = parse(body, &mut tokens, Limits::default()).unwrap();
            assert_eq!(messages_control_betas(doc.root(), 64000), expected);
        }
        for body in [
            br#"{"thinking":{"type":"enabled","budget_tokens":64000}}"#.as_slice(),
            br#"{"thinking":{"type":"enabled","budget_tokens":2000,"display":"other"}}"#.as_slice(),
            br#"{"context_management":{"edits":[{"type":"clear_thinking_20251015","keep":"all"}]}}"#.as_slice(),
            br#"{"thinking":{"type":"adaptive"},"context_management":{"edits":[{"type":"unknown","keep":"all"}]}}"#.as_slice(),
        ] {
            let mut tokens = [Token::EMPTY; 64];
            let doc = parse(body, &mut tokens, Limits::default()).unwrap();
            assert!(messages_control_betas(doc.root(), 64000).is_err());
        }
    }
}
