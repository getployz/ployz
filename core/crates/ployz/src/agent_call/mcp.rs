//! `ployz mcp`'s one wrapper: every tool takes `intent`, `send_feedback` is listed, and
//! each call is reported in the background.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rmcp::model::{
    CallToolResponse, CallToolResult, ContentBlock, Implementation, JsonObject, Tool,
    ToolAnnotations,
};
use rmcp::{ErrorData as McpError, RoleServer, service::RequestContext};
use serde_json::{Value, json};
use tokio::task::JoinSet;

use super::{CallError, CallReport, Client, ErrorType, Feedback, Kind, Started, Surface};
use crate::cloud::account::Credential;

pub(crate) const INTENT: &str = "intent";
pub(crate) const FEEDBACK_TOOL: &str = "send_feedback";
const INTENT_DESCRIPTION: &str = "Why are you calling this tool? Describe the goal in general terms, and refer to people by role (\"a customer\") rather than by name. Leave out personal and identifying information.";
const INTENT_REQUIRED: &str = "Say why you are calling this tool. Send `intent`, a short sentence on the goal this call serves.";
const REFUSED: &str = "The tool refused this call.";
const THANKS: &str = "Thanks. The Ployz team reads this.";
const FEEDBACK_SHAPE: &str = "send_feedback takes `type` (missing_capability, issue, praise or other), `message`, and an optional `sentiment` (positive, neutral, negative or mixed).";

/// A session ends after this long without a call.
const IDLE: Duration = Duration::from_secs(30 * 60);

/// `intent` in a tool's input schema, required.
pub(crate) fn with_intent(schema: &mut JsonObject) {
    let properties = schema.entry("properties").or_insert_with(|| json!({}));
    if let Value::Object(properties) = properties {
        properties.insert(
            INTENT.to_owned(),
            json!({ "type": "string", "description": INTENT_DESCRIPTION }),
        );
    }
    let required = schema.entry("required").or_insert_with(|| json!([]));
    if let Value::Array(required) = required
        && !required.iter().any(|name| name == INTENT)
    {
        required.push(json!(INTENT));
    }
}

/// The intent out of a call's arguments, refusing a call with none.
pub(crate) fn take_intent(
    arguments: Option<JsonObject>,
) -> Result<(String, Option<JsonObject>), McpError> {
    let mut input = arguments.unwrap_or_default();
    match input.remove(INTENT) {
        Some(Value::String(intent)) if !intent.trim().is_empty() => Ok((intent, Some(input))),
        _ => Err(McpError::invalid_params(INTENT_REQUIRED, None)),
    }
}

pub(crate) fn feedback_tool() -> Tool {
    let input = json!({
        "type": "object",
        "properties": {
            "type": {
                "type": "string",
                "enum": ["missing_capability", "issue", "praise", "other"],
                "description": "What this is about: a tool you needed and did not find, something that broke, something that worked well, or anything else.",
            },
            "message": {
                "type": "string",
                "description": "What you were trying to do and what happened. Refer to people by role rather than by name.",
            },
            "sentiment": {
                "type": "string",
                "enum": ["positive", "neutral", "negative", "mixed"],
                "description": "How it went, if you want to say.",
            },
        },
        "required": ["type", "message"],
    });
    let Value::Object(input) = input else {
        unreachable!("a JSON object literal")
    };
    Tool::new(
        FEEDBACK_TOOL,
        "Tell the Ployz team about a tool you needed and did not find, something that broke, or something that worked well. Needs no intent.",
        Arc::new(input),
    )
    .with_title("Send feedback")
    .annotate(
        ToolAnnotations::new()
            .read_only(false)
            .destructive(false)
            .idempotent(false)
            .open_world(false),
    )
}

/// The MCP client, from the `initialize` the host sent.
pub(crate) fn client_of(context: &RequestContext<RoleServer>) -> Client {
    context
        .peer
        .peer_info()
        .map(|info| client(&info.client_info))
        .unwrap_or_default()
}

fn client(implementation: &Implementation) -> Client {
    Client {
        name: Some(implementation.name.clone()),
        version: Some(implementation.version.clone()),
    }
}

pub(crate) type Resolve =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Option<Credential>> + Send>> + Send + Sync>;

/// Reports calls to Cloud in the background. Off, it reports nothing: tests and a server
/// nobody signed in to.
#[derive(Clone)]
pub(crate) struct Reporter(Option<Arc<Live>>);

struct Live {
    resolve: Resolve,
    session: Mutex<Session>,
    sends: Mutex<JoinSet<()>>,
}

struct Session {
    id: String,
    last: Instant,
}

fn new_session() -> String {
    format!("ses_{}", uuid::Uuid::now_v7())
}

impl Reporter {
    pub(crate) fn off() -> Self {
        Self(None)
    }

    /// Report as the credential `resolve` finds at each report.
    pub(crate) fn on(resolve: Resolve) -> Self {
        Self(Some(Arc::new(Live {
            resolve,
            session: Mutex::new(Session {
                id: new_session(),
                last: Instant::now(),
            }),
            sends: Mutex::new(JoinSet::new()),
        })))
    }

    pub(crate) fn initialized(&self, started: Started, implementation: &Implementation) {
        self.submit(started, Kind::Initialize, client(implementation), |_| {});
    }

    pub(crate) fn listed(&self, started: Started, tools: Vec<String>, client: Client) {
        self.submit(started, Kind::ToolsList, client, |report| {
            report.tools = Some(tools);
        });
    }

    /// A call that ended. A refusal for a missing intent carries no intent; an answer
    /// that only asks for an approval is not a call yet.
    pub(crate) fn called(
        &self,
        started: Started,
        tool: &str,
        intent: Option<String>,
        known: bool,
        result: &Result<CallToolResponse, McpError>,
        client: Client,
    ) {
        let error = match outcome(result) {
            Outcome::Asking => return,
            Outcome::Ended(error) => error,
        };
        let kind = if known { Kind::Call } else { Kind::UnknownTool };
        let tool = tool.to_owned();
        self.submit(started, kind, client, move |report| {
            report.tool = tool;
            report.intent = intent;
            report.error = error;
        });
    }

    /// Answer `send_feedback` and record it.
    pub(crate) fn feedback(
        &self,
        started: Started,
        arguments: Option<JsonObject>,
        client: Client,
    ) -> Result<CallToolResponse, McpError> {
        let mut input = arguments.unwrap_or_default();
        input.remove(INTENT);
        let feedback: Feedback = serde_json::from_value(Value::Object(input))
            .map_err(|_| McpError::invalid_params(FEEDBACK_SHAPE, None))?;
        self.submit(started, Kind::Feedback, client, move |report| {
            report.feedback = Some(feedback);
        });
        Ok(CallToolResult::success(vec![ContentBlock::text(THANKS)]).into())
    }

    fn submit(
        &self,
        started: Started,
        kind: Kind,
        client: Client,
        fill: impl FnOnce(&mut CallReport),
    ) {
        let Some(live) = &self.0 else {
            return;
        };
        let session = live.session();
        let mut report = CallReport::new(Surface::Local, kind, started, session);
        report.client = client;
        fill(&mut report);
        let resolve = live.resolve.clone();
        let Ok(mut sends) = live.sends.lock() else {
            return;
        };
        while sends.try_join_next().is_some() {}
        sends.spawn(async move {
            if let Some(credential) = resolve().await {
                super::send(&credential, &report).await;
            }
        });
    }

    /// Wait briefly for reports still on their way, at shutdown.
    pub(crate) async fn flush(&self) {
        let Some(live) = &self.0 else {
            return;
        };
        let mut sends = match live.sends.lock() {
            Ok(mut sends) => std::mem::take(&mut *sends),
            Err(_) => return,
        };
        let _ = tokio::time::timeout(crate::cloud::http::REPORT_TIMEOUT, async {
            while sends.join_next().await.is_some() {}
        })
        .await;
    }
}

impl Live {
    /// The current session, replaced once it sat idle past [`IDLE`].
    fn session(&self) -> String {
        let Ok(mut session) = self.session.lock() else {
            return new_session();
        };
        if session.last.elapsed() > IDLE {
            session.id = new_session();
        }
        session.last = Instant::now();
        session.id.clone()
    }
}

enum Outcome {
    /// The answer only asks a human for an approval; the retry that completes counts.
    Asking,
    Ended(Option<CallError>),
}

fn outcome(result: &Result<CallToolResponse, McpError>) -> Outcome {
    match result {
        Err(error) => Outcome::Ended(Some(CallError::new(
            if error.code == rmcp::model::ErrorCode::INVALID_PARAMS {
                ErrorType::Validation
            } else {
                ErrorType::Internal
            },
            error.message.to_string(),
        ))),
        // A host that fills in a form mid-call: not a call that ended.
        Ok(CallToolResponse::InputRequired(_)) => Outcome::Asking,
        Ok(CallToolResponse::Complete(answer)) if answer.is_error == Some(true) => {
            if answer
                .structured_content
                .as_ref()
                .is_some_and(|content| content.get("approval_required").is_some())
            {
                return Outcome::Asking;
            }
            Outcome::Ended(Some(CallError::new(
                ErrorType::Validation,
                refusal_message(answer),
            )))
        }
        // A completed answer, or a task the host polls for one.
        Ok(_) => Outcome::Ended(None),
    }
}

/// What a refusal says about itself. Its text also carries the call to retry, with every
/// argument the agent sent, so only its own `message` leaves; an answer that is not a
/// refusal with one gets a fixed sentence.
fn refusal_message(answer: &CallToolResult) -> String {
    let parsed = answer.content.iter().find_map(|block| {
        let refusal: Value = serde_json::from_str(&block.as_text()?.text).ok()?;
        refusal.get("message")?.as_str().map(str::to_owned)
    });
    parsed.unwrap_or_else(|| REFUSED.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    fn intent_is_required_on_the_schema_and_stripped_from_a_call() {
        let Value::Object(mut schema) = json!({ "type": "object", "properties": { "name": { "type": "string" } }, "required": ["name"] })
        else {
            unreachable!()
        };
        with_intent(&mut schema);
        assert_eq!(schema["properties"]["intent"]["type"], "string");
        assert_eq!(schema["required"], json!(["name", "intent"]));

        let Value::Object(arguments) = json!({ "name": "web", "intent": "see what runs" }) else {
            unreachable!()
        };
        let (intent, rest) = take_intent(Some(arguments)).unwrap();
        assert_eq!(intent, "see what runs");
        assert_eq!(Value::Object(rest.unwrap()), json!({ "name": "web" }));

        let Value::Object(blank) = json!({ "name": "web", "intent": "  " }) else {
            unreachable!()
        };
        let refused = take_intent(Some(blank)).unwrap_err();
        assert_eq!(refused.message, INTENT_REQUIRED);
        assert_eq!(take_intent(None).unwrap_err().message, INTENT_REQUIRED);
    }

    #[test]
    fn a_refused_call_reports_the_refusal_message_and_no_argument() {
        let refusal = json!({
            "code": "invalid_argument",
            "message": "This input does not fit: unknown field",
            "retry": { "tool": "service_add", "input": { "env": { "DB_PASSWORD": "hunter2-plain" } } },
        });
        for text in [refusal.to_string(), "hunter2-plain is not valid".to_owned()] {
            let answer = CallToolResult::error(vec![ContentBlock::text(text)]);
            let Outcome::Ended(Some(error)) = outcome(&Ok(answer.into())) else {
                panic!("a refusal ends the call with an error");
            };
            let sent = serde_json::to_string(&error).unwrap();
            assert!(!sent.contains("hunter2"), "{sent}");
            assert!(sent.contains("validation"), "{sent}");
        }
        let answer = CallToolResult::error(vec![ContentBlock::text(refusal.to_string())]);
        let Outcome::Ended(Some(error)) = outcome(&Ok(answer.into())) else {
            panic!("a refusal ends the call with an error");
        };
        assert_eq!(error.message, "This input does not fit: unknown field");
    }

    #[test]
    fn an_approval_ask_is_not_an_ended_call() {
        let mut asking = CallToolResult::error(vec![ContentBlock::text("approve it")]);
        asking.structured_content =
            Some(json!({ "ok": false, "approval_required": { "approval_id": "apr_1" } }));
        assert!(matches!(outcome(&Ok(asking.into())), Outcome::Asking));
        let refused = CallToolResult::error(vec![ContentBlock::text("no such Service")]);
        assert!(matches!(
            outcome(&Ok(refused.into())),
            Outcome::Ended(Some(_))
        ));
        assert!(matches!(
            outcome(&Ok(
                CallToolResult::success(vec![ContentBlock::text("ok")]).into()
            )),
            Outcome::Ended(None)
        ));
    }
}
