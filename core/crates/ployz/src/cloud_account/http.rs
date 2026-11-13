//! Sending a request to Cloud and reading what comes back: the one client builder, the
//! request every call starts from, and the manifest revision Cloud names on a reply.

use std::time::Duration;

use ployz_core::cloud_contract::{self, AGENT_HEADER};
use reqwest::{Method, StatusCode};

use super::credential::Credential;
use crate::cloud_login::{self, LoginError};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a call Cloud answers only after reading the Servers waits for that answer.
/// Cloud may spend up to 50s reading them to plan a clean, so the client's 15s default
/// would give up on a run Cloud goes on to start.
const OPERATION_TIMEOUT: Duration = Duration::from_secs(60);

/// Outlasts the longest wait a tool may ask for and matches the default build limit. A
/// Deployment or a Cloud operation goes on after ployz mcp stops waiting for it.
pub(crate) const CALL_DEADLINE: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Via {
    Cli,
    Agent,
}

/// What a request changes in Cloud, which says how it failed in flight.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Effect {
    Read,
    Write,
}

impl Effect {
    /// Why a request that failed in flight failed: a read never reached Cloud's answer, so
    /// it is unreachable. A write that left this device may have applied though its answer
    /// never came: [`cloud_login::lost`].
    pub(crate) fn failed(self, cloud: &str, error: reqwest::Error) -> LoginError {
        match self {
            Self::Read => cloud_login::unreachable(cloud, error),
            Self::Write => cloud_login::lost(cloud, error),
        }
    }
}

/// A Cloud client with this CLI's User-Agent and connect timeout, for a request that
/// takes at most `request_timeout`.
pub(crate) fn builder(request_timeout: Duration) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .user_agent(concat!("ployz-cli/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(request_timeout)
}

/// Cloud's client; its User-Agent marks the sessions it starts as signed-in devices,
/// and `X-Ployz-Agent` names a detected coding agent, for telemetry only.
pub(crate) fn client() -> Result<reqwest::Client, LoginError> {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(agent) = agent(|name| std::env::var(name).ok())
        .and_then(|agent| reqwest::header::HeaderValue::from_str(&agent).ok())
    {
        headers.insert(AGENT_HEADER, agent);
    }
    builder(REQUEST_TIMEOUT)
        .default_headers(headers)
        .build()
        .map_err(|error| LoginError::Reply(error.to_string()))
}

/// The coding agent running this CLI, from the markers agent harnesses set. It
/// never changes output, prompts or consent.
fn agent(var: impl Fn(&str) -> Option<String>) -> Option<String> {
    if let Some(agent) = var("AI_AGENT").filter(|agent| !agent.is_empty()) {
        return Some(agent);
    }
    [
        ("CLAUDECODE", "claude-code"),
        ("CURSOR_AGENT", "cursor"),
        ("CODEX_THREAD_ID", "codex"),
        ("GEMINI_CLI", "gemini-cli"),
        ("OPENCODE", "opencode"),
        ("AMP_CURRENT_THREAD_ID", "amp"),
    ]
    .into_iter()
    .find(|(marker, _)| var(marker).is_some_and(|value| !value.is_empty()))
    .map(|(_, agent)| agent.to_owned())
}

pub(crate) fn request(
    credential: &Credential,
    method: Method,
    url: &str,
    body: Option<&serde_json::Value>,
) -> Result<reqwest::RequestBuilder, LoginError> {
    let mut request = client()?
        .request(method, url)
        .bearer_auth(credential.bearer());
    if let Some(body) = body {
        request = request.json(body);
    }
    Ok(request)
}

/// [`request`] to `/api/cli/<path>` that waits [`OPERATION_TIMEOUT`] rather than the
/// client's default.
pub(crate) fn operation_request(
    credential: &Credential,
    method: Method,
    path: &str,
    body: Option<&serde_json::Value>,
) -> Result<reqwest::RequestBuilder, LoginError> {
    let url = format!("{}/api/cli/{path}", credential.cloud());
    Ok(request(credential, method, &url, body)?.timeout(OPERATION_TIMEOUT))
}

/// [`request`] with a JSON body that waits [`OPERATION_TIMEOUT`], heard as `via`, and
/// carrying `approval` when Cloud asked for one and `request_key` so a retried command
/// runs once.
pub(crate) fn approved_request(
    credential: &Credential,
    via: Via,
    method: Method,
    url: &str,
    body: &serde_json::Value,
    approval: Option<&str>,
    request_key: Option<&str>,
) -> Result<reqwest::RequestBuilder, LoginError> {
    let mut request = request(credential, method, url, Some(body))?.timeout(OPERATION_TIMEOUT);
    if via == Via::Agent {
        request = request.header(cloud_contract::VIA_HEADER, "agent");
    }
    if let Some(approval) = approval {
        request = request.header(cloud_contract::APPROVAL_HEADER, approval);
    }
    if let Some(key) = request_key {
        request = request.header(cloud_contract::REQUEST_KEY_HEADER, key);
    }
    Ok(request)
}

/// What Cloud answered: its status and the whole body.
pub(crate) struct Reply {
    pub(crate) status: StatusCode,
    pub(crate) body: Vec<u8>,
}

/// Send `request` to `cloud`.
pub(crate) async fn dispatch(
    cloud: &str,
    request: reqwest::RequestBuilder,
    effect: Effect,
) -> Result<reqwest::Response, LoginError> {
    request
        .send()
        .await
        .map_err(|error| effect.failed(cloud, error))
}

/// Read `response`'s body, whose status is the caller's to have looked at.
pub(crate) async fn read_body(
    cloud: &str,
    response: reqwest::Response,
    effect: Effect,
) -> Result<Vec<u8>, LoginError> {
    response
        .bytes()
        .await
        .map(|body| body.to_vec())
        .map_err(|error| effect.failed(cloud, error))
}

/// Read `response` whole, keeping the manifest revision it names.
pub(crate) async fn read_reply(
    cloud: &str,
    response: reqwest::Response,
    effect: Effect,
) -> Result<Reply, LoginError> {
    let status = response.status();
    notice_revision(&response);
    let body = read_body(cloud, response, effect).await?;
    Ok(Reply { status, body })
}

/// Send `request` to `cloud` and read the reply whole.
pub(crate) async fn exchange(
    cloud: &str,
    request: reqwest::RequestBuilder,
    effect: Effect,
) -> Result<Reply, LoginError> {
    read_reply(cloud, dispatch(cloud, request, effect).await?, effect).await
}

/// The manifest revision each Cloud named on its latest `/api/cli` reply, by origin.
static REVISION: std::sync::Mutex<std::collections::BTreeMap<String, String>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Keep the revision a reply names, so a command can refresh the published
/// commands after it prints when Cloud moved on.
pub(crate) fn notice_revision(response: &reqwest::Response) {
    if let Some(revision) = revision_of(response.headers()) {
        REVISION
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(response.url().origin().ascii_serialization(), revision);
    }
}

pub(crate) fn revision_of(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get(cloud_contract::REVISION_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

/// The revision `cloud`'s latest `/api/cli` reply named, if one did.
pub(crate) fn revision_seen(cloud: &str) -> Option<String> {
    let origin = reqwest::Url::parse(cloud)
        .ok()?
        .origin()
        .ascii_serialization();
    REVISION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&origin)
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_account::testing::token;

    #[test]
    fn agent_names_the_harness_from_its_markers() {
        let only = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            }
        };
        assert_eq!(agent(only(&[])), None);
        assert_eq!(agent(only(&[("CLAUDECODE", "")])), None);
        assert_eq!(
            agent(only(&[("CLAUDECODE", "1")])).as_deref(),
            Some("claude-code")
        );
        assert_eq!(
            agent(only(&[("CODEX_THREAD_ID", "t1"), ("AI_AGENT", "pi")])).as_deref(),
            Some("pi")
        );
    }

    #[test]
    fn a_clean_preview_waits_past_the_cluster_time_cloud_may_spend_on_it() {
        // Cloud's clean plan may spend a connect (30s), a Data Loss read (10s) and a frame (10s).
        let cloud_bound = Duration::from_secs(50);
        let preview = operation_request(
            &token("https://cloud.test"),
            Method::GET,
            "namespaces/app/clean",
            None,
        )
        .unwrap()
        .build()
        .unwrap();
        assert!(
            preview
                .timeout()
                .is_some_and(|waits| *waits >= cloud_bound + Duration::from_secs(5)),
            "{:?}",
            preview.timeout()
        );
    }
}
