//! Why an agent runs this binary, and the report of each agent call to Ployz Cloud.
//!
//! Three things live here and nowhere else: which coding agent is driving this process
//! ([`agent`]), the session its calls group under, and the body posted to Cloud's
//! `/api/cli/agent/calls`. The CLI wraps its dispatch once with [`cli`]; `ployz mcp` wraps
//! its `call_tool` once with [`mcp`]. A report is best effort: it never changes a result,
//! an exit code or a tool answer, and it prints nothing.

use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, SecondsFormat, Utc};
use ployz_core::RpcErrorCode;
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::cloud_account::{self, Credential};

mod agent;
pub(crate) mod cli;
pub(crate) mod mcp;

pub(crate) use agent::name as agent_name;

/// How long a report may take. Cloud answers 204 at once; anything slower is dropped.
const SEND_TIMEOUT: Duration = Duration::from_secs(2);

/// One agent call as Cloud records it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CallReport {
    surface: Surface,
    kind: Kind,
    /// The MCP tool name, or the CLI command path joined by spaces. Never an argument.
    tool: String,
    intent: Option<String>,
    session: String,
    started_at: String,
    duration_ms: u64,
    error: Option<CallError>,
    client: Client,
    server_version: &'static str,
    tools: Option<Vec<String>>,
    feedback: Option<Feedback>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Surface {
    Local,
    Cli,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Kind {
    Call,
    UnknownTool,
    Initialize,
    ToolsList,
    Feedback,
}

/// The low-cardinality failure types PostHog groups by.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ErrorType {
    Validation,
    Permission,
    #[serde(rename = "api_4xx")]
    Api4xx,
    #[serde(rename = "api_5xx")]
    Api5xx,
    Cancelled,
    Internal,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CallError {
    #[serde(rename = "type")]
    kind: ErrorType,
    message: String,
}

impl CallError {
    pub(crate) fn new(kind: ErrorType, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// A CLI failure by its `--json` error code.
    pub(crate) fn of_failure(failure: &crate::failure::Failure) -> Option<Self> {
        if failure.printed_exit() == Some(0) {
            return None;
        }
        let kind = if failure.is_interrupted() {
            ErrorType::Cancelled
        } else {
            match failure.report().code {
                RpcErrorCode::InvalidArgument
                | RpcErrorCode::ConfirmationRequired
                | RpcErrorCode::Ambiguous => ErrorType::Validation,
                RpcErrorCode::Unauthenticated => ErrorType::Permission,
                RpcErrorCode::NotFound | RpcErrorCode::Conflict | RpcErrorCode::Unsupported => {
                    ErrorType::Api4xx
                }
                RpcErrorCode::Unavailable => ErrorType::Api5xx,
                RpcErrorCode::Internal | RpcErrorCode::Unknown(_) => ErrorType::Internal,
            }
        };
        Some(Self::new(kind, kind.sentence()))
    }
}

impl ErrorType {
    /// What a failed command says about itself. Its own message can name the arguments
    /// it was given, so the type is all that leaves.
    fn sentence(self) -> &'static str {
        match self {
            Self::Validation => "The command was refused.",
            Self::Permission => "The command was not allowed.",
            Self::Api4xx => "The command found nothing to act on, or it conflicted.",
            Self::Api5xx => "The command could not reach what it needed.",
            Self::Cancelled => "The command was interrupted.",
            Self::Internal => "The command failed.",
        }
    }
}

/// Who is calling: the MCP client, or the agent behind the CLI.
#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct Client {
    name: Option<String>,
    version: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Feedback {
    #[serde(rename = "type")]
    kind: FeedbackType,
    #[serde(default)]
    sentiment: Option<Sentiment>,
    message: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FeedbackType {
    MissingCapability,
    Issue,
    Praise,
    Other,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Sentiment {
    Positive,
    Neutral,
    Negative,
    Mixed,
}

/// When a call began: the wall clock for the report, the monotonic clock for its length.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Started {
    at: SystemTime,
    clock: Instant,
}

impl Started {
    pub(crate) fn now() -> Self {
        Self {
            at: SystemTime::now(),
            clock: Instant::now(),
        }
    }
}

impl CallReport {
    fn new(surface: Surface, kind: Kind, started: Started, session: String) -> Self {
        Self {
            surface,
            kind,
            tool: String::new(),
            intent: None,
            session,
            started_at: DateTime::<Utc>::from(started.at)
                .to_rfc3339_opts(SecondsFormat::Millis, true),
            duration_ms: u64::try_from(started.clock.elapsed().as_millis()).unwrap_or(u64::MAX),
            error: None,
            client: Client::default(),
            server_version: env!("CARGO_PKG_VERSION"),
            tools: None,
            feedback: None,
        }
    }
}

/// Post `report` as `credential`. Every failure is dropped: a Cloud without the route,
/// a slow network or no network at all cost nothing visible.
async fn send(credential: &Credential, report: &CallReport) {
    let url = format!("{}/api/cli/agent/calls", credential.cloud());
    let Ok(body) = serde_json::to_value(report) else {
        return;
    };
    let Ok(request) = cloud_account::http::request(credential, Method::POST, &url, Some(&body))
    else {
        return;
    };
    let _ = request.timeout(SEND_TIMEOUT).send().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    fn a_report_serializes_to_the_body_cloud_decodes() {
        let started = Started {
            at: SystemTime::UNIX_EPOCH + Duration::from_millis(1_800_000_000_123),
            clock: Instant::now(),
        };
        let mut report = CallReport::new(Surface::Cli, Kind::Call, started, "ses_x".to_owned());
        report.tool = "service deploy".to_owned();
        report.intent = Some("ship the fix".to_owned());
        report.duration_ms = 412;
        report.error = Some(CallError::new(ErrorType::Api4xx, "not found"));
        report.client = Client {
            name: Some("claude-code".to_owned()),
            version: None,
        };
        assert_eq!(
            serde_json::to_value(&report).unwrap(),
            json!({
                "surface": "cli",
                "kind": "call",
                "tool": "service deploy",
                "intent": "ship the fix",
                "session": "ses_x",
                "startedAt": "2027-01-15T08:00:00.123Z",
                "durationMs": 412,
                "error": { "type": "api_4xx", "message": "not found" },
                "client": { "name": "claude-code", "version": null },
                "serverVersion": env!("CARGO_PKG_VERSION"),
                "tools": null,
                "feedback": null,
            })
        );
        let listed = CallReport {
            kind: Kind::ToolsList,
            tools: Some(vec!["service_deploy".to_owned()]),
            ..CallReport::new(Surface::Local, Kind::ToolsList, started, "ses_y".to_owned())
        };
        let body = serde_json::to_value(&listed).unwrap();
        assert_eq!(body["kind"], "toolsList");
        assert_eq!(body["tools"], json!(["service_deploy"]));
        assert_eq!(body["tool"], "");
        let feedback: Feedback = serde_json::from_value(json!({
            "type": "missing_capability",
            "message": "no way to tail logs"
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&feedback).unwrap(),
            json!({ "type": "missing_capability", "sentiment": null, "message": "no way to tail logs" })
        );
    }
}
