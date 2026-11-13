//! What a gated call goes through before its tool runs, the same for a compiled tool and
//! a published command: the `approval` argument, the race against the host's cancel and
//! the deadline, and the refusals that name an approval or a write that may have landed.

use std::future::Future;

use ployz_store::tools::Writes;
use rmcp::model::{CallToolResult, JsonObject};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer};
use serde_json::{Value, json};
use tokio::time::Instant;

use super::refused;

pub(super) const APPROVAL: &str = "approval";

/// A call's arguments with `approval` taken out, when the tool may ask for one. A tool
/// that never asks keeps a field of that name as its own input.
pub(super) struct GatedInput {
    pub(super) input: JsonObject,
    pub(super) approval: Option<String>,
}

impl GatedInput {
    pub(super) fn take(arguments: Option<JsonObject>, asks: bool) -> Result<Self, McpError> {
        let mut input = arguments.unwrap_or_default();
        let approval = match (asks, input.remove(APPROVAL)) {
            (false, Some(given)) => {
                input.insert(APPROVAL.to_owned(), given);
                None
            }
            (_, None | Some(Value::Null)) => None,
            (_, Some(Value::String(id))) => Some(id),
            (_, Some(_)) => {
                return Err(McpError::invalid_params(
                    format!("`{APPROVAL}` takes one string"),
                    None,
                ));
            }
        };
        Ok(Self { input, approval })
    }
}

/// `approval` in a tool's input schema, when the tool may ask for one.
pub(super) fn with_approval(schema: &mut JsonObject, asks: bool) {
    if asks && let Some(Value::Object(properties)) = schema.get_mut("properties") {
        properties.insert(
            APPROVAL.to_owned(),
            json!({
                "type": "string",
                "description": "The approval id a refusal named, once a human approved it in Ployz Cloud.",
            }),
        );
    }
}

pub(super) enum Raced<T> {
    Ran(T),
    Overdue,
}

/// Run a call until the host cancels it or `until` passes. A call that finishes in the
/// same poll as the deadline settles first, so a write past the deadline answers with
/// its own refusal.
pub(super) async fn raced<T>(
    context: &RequestContext<RoleServer>,
    until: Instant,
    run: impl Future<Output = T>,
) -> Result<Raced<T>, McpError> {
    tokio::select! {
        biased;
        ran = run => Ok(Raced::Ran(ran)),
        () = context.ct.cancelled() => {
            Err(McpError::internal_error("the call was cancelled", None))
        }
        () = tokio::time::sleep_until(until) => Ok(Raced::Overdue),
    }
}

/// `details` for a refusal of a write that may have landed: whatever the details held,
/// plus `writes: unknown`, so the retry repeats the call's key.
pub(super) fn writes_unknown(details: Value) -> Value {
    let mut details = if details.is_object() {
        details
    } else {
        json!({})
    };
    if let Value::Object(details) = &mut details {
        details.insert("writes".to_owned(), json!(Writes::Unknown));
    }
    details
}

/// What a person does to approve `id` once a refusal has named it.
pub(super) fn approve_at(url: &str, id: &str) -> String {
    format!(
        "open {url} and approve {id} under Waiting on you. Once they approve, call this tool \
         again with `approval` set to `{id}`."
    )
}

/// A call refused until a person approves it, in the shape Cloud's `/mcp` gives a remote
/// agent: the approval's id, Cloud's message and where a person approves it.
pub(super) fn approval_required(
    text: String,
    id: &str,
    message: &str,
    url: &str,
) -> CallToolResult {
    let mut result = refused(text);
    result.structured_content = Some(json!({
        "ok": false,
        "approval_required": { "approval_id": id, "message": message, "url": url },
    }));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(value: Value) -> Option<JsonObject> {
        let Value::Object(object) = value else {
            panic!("arguments are an object");
        };
        Some(object)
    }

    #[test]
    fn a_tool_that_asks_takes_its_approval_out_of_the_input() {
        let taken =
            GatedInput::take(arguments(json!({ "a": 1, "approval": "apr_1" })), true).unwrap();
        assert_eq!(taken.approval.as_deref(), Some("apr_1"));
        assert_eq!(Value::Object(taken.input), json!({ "a": 1 }));
    }

    #[test]
    fn a_tool_that_never_asks_keeps_the_field_as_its_own() {
        let taken =
            GatedInput::take(arguments(json!({ "a": 1, "approval": "apr_1" })), false).unwrap();
        assert_eq!(taken.approval, None);
        assert_eq!(
            Value::Object(taken.input),
            json!({ "a": 1, "approval": "apr_1" })
        );
    }

    #[test]
    fn a_missing_or_null_approval_is_none_and_no_arguments_are_empty() {
        for given in [json!({ "a": 1 }), json!({ "a": 1, "approval": null })] {
            let taken = GatedInput::take(arguments(given), true).unwrap();
            assert_eq!(taken.approval, None);
            assert_eq!(Value::Object(taken.input), json!({ "a": 1 }));
        }
        let none = GatedInput::take(None, true).unwrap();
        assert!(none.approval.is_none() && none.input.is_empty());
    }

    #[test]
    fn an_approval_that_is_not_a_string_is_an_invalid_params_error() {
        let Err(error) = GatedInput::take(arguments(json!({ "approval": 5 })), true) else {
            panic!("a number is not an approval");
        };
        assert_eq!(error.message, "`approval` takes one string");
    }

    #[test]
    fn the_approval_property_is_added_only_when_the_tool_asks() {
        let schema = || arguments(json!({ "type": "object", "properties": {} })).unwrap();
        let (mut asks, mut never) = (schema(), schema());
        with_approval(&mut asks, true);
        with_approval(&mut never, false);
        assert_eq!(
            Value::Object(asks).pointer("/properties/approval"),
            Some(&json!({
                "type": "string",
                "description":
                    "The approval id a refusal named, once a human approved it in Ployz Cloud.",
            }))
        );
        assert_eq!(
            Value::Object(never).pointer("/properties"),
            Some(&json!({}))
        );
        let mut bare = arguments(json!({ "type": "object" })).unwrap();
        with_approval(&mut bare, true);
        assert_eq!(Value::Object(bare), json!({ "type": "object" }));
    }

    #[test]
    fn writes_unknown_keeps_what_the_details_held() {
        assert_eq!(writes_unknown(Value::Null), json!({ "writes": "unknown" }));
        assert_eq!(
            writes_unknown(json!("text")),
            json!({ "writes": "unknown" })
        );
        assert_eq!(
            writes_unknown(json!({ "approval_id": "apr_1" })),
            json!({ "approval_id": "apr_1", "writes": "unknown" })
        );
    }

    #[test]
    fn the_approval_steps_name_the_url_and_the_id() {
        assert_eq!(
            approve_at("https://cloud/x", "apr_1"),
            "open https://cloud/x and approve apr_1 under Waiting on you. Once they approve, \
             call this tool again with `approval` set to `apr_1`."
        );
    }
}
