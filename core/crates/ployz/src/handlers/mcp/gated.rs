//! What a gated call goes through before its tool runs, the same for a compiled tool and
//! a published command: the race against the host's cancel and the deadline, and the
//! refusal that names a write that may have landed.

use std::future::Future;

use ployz_agent_tools::Writes;
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer};
use serde_json::{Value, json};
use tokio::time::Instant;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
