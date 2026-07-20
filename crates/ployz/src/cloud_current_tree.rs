use std::path::{Path, PathBuf};
use std::time::Duration;

use ployz_core::build::LocalSnapshotDigest;
use ployz_core::ids::{BuildExecutorId, BuildPoolId};
use ployz_core::image::OciPlatform;
use ployz_core::install::{MachineJoinRuntimeNatsUrl, MachineJoinTrustedNats};
use ployz_core::nats_config::NatsUserPublicKey;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const SESSION_FILE_NAME: &str = "current-tree-session.json";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudSession {
    pub cloud_url: String,
    pub session_secret: String,
    pub expires_at: String,
}

impl std::fmt::Display for CloudSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Cloud current-tree session [redacted]")
    }
}

impl std::fmt::Debug for CloudSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CloudSession { secret: [redacted] }")
    }
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BeginSession {
    pub browser_url: String,
    pub user_code: String,
    pub session_secret: String,
    pub poll_after_seconds: u64,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContextSummary {
    pub organization: NamedContext,
    pub project: NamedContext,
    pub environment: EnvironmentContext,
    pub service: NamedContext,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NamedContext {
    pub id: String,
    pub slug: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EnvironmentContext {
    pub id: String,
    pub name: String,
    pub namespace: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrozenBuild {
    pub assignment_id: String,
    pub build_record_id: String,
    pub deployment_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivatedExecutor {
    pub runtime_nats_url: MachineJoinRuntimeNatsUrl,
    pub trusted_nats: MachineJoinTrustedNats,
    pub pool_id: BuildPoolId,
    pub executor_id: BuildExecutorId,
    pub platform: OciPlatform,
    pub expires_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultEnvelope<T> {
    result: T,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorEnvelope {
    error: String,
}

#[derive(Clone)]
pub(crate) struct CloudCurrentTreeClient {
    base_url: String,
    http: reqwest::Client,
}

impl CloudCurrentTreeClient {
    pub(crate) fn new(base_url: &str) -> Result<Self, CloudCurrentTreeError> {
        let base_url = base_url.trim_end_matches('/');
        let url =
            reqwest::Url::parse(base_url).map_err(|_| CloudCurrentTreeError::InvalidCloudUrl)?;
        if !matches!(url.scheme(), "http" | "https") || url.cannot_be_a_base() {
            return Err(CloudCurrentTreeError::InvalidCloudUrl);
        }
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| CloudCurrentTreeError::Transport(error.to_string()))?;
        Ok(Self {
            base_url: base_url.to_owned(),
            http,
        })
    }

    pub(crate) async fn begin(&self) -> Result<BeginSession, CloudCurrentTreeError> {
        self.request(None, serde_json::json!({ "action": "begin" }))
            .await
    }

    pub(crate) async fn poll(
        &self,
        secret: &str,
    ) -> Result<serde_json::Value, CloudCurrentTreeError> {
        self.request_result(secret, serde_json::json!({ "action": "poll" }))
            .await
    }

    pub(crate) async fn contexts(
        &self,
        secret: &str,
    ) -> Result<Vec<ContextSummary>, CloudCurrentTreeError> {
        self.request_result(secret, serde_json::json!({ "action": "list_contexts" }))
            .await
    }

    pub(crate) async fn select(
        &self,
        secret: &str,
        context: &ContextSummary,
    ) -> Result<ContextSummary, CloudCurrentTreeError> {
        self.request_result(
            secret,
            serde_json::json!({
                "action": "select_context",
                "organization_id": context.organization.id,
                "environment_id": context.environment.id,
                "service_id": context.service.id,
            }),
        )
        .await
    }

    pub(crate) async fn freeze(
        &self,
        secret: &str,
        digest: &LocalSnapshotDigest,
        platform: &OciPlatform,
    ) -> Result<FrozenBuild, CloudCurrentTreeError> {
        self.request_result(
            secret,
            serde_json::json!({
                "action": "freeze",
                "digest": digest.as_str(),
                "architecture": platform.architecture(),
            }),
        )
        .await
    }

    pub(crate) async fn activate(
        &self,
        secret: &str,
        frozen: &FrozenBuild,
        digest: &LocalSnapshotDigest,
        public_key: &NatsUserPublicKey,
    ) -> Result<ActivatedExecutor, CloudCurrentTreeError> {
        self.request_result(
            secret,
            serde_json::json!({
                "action": "activate",
                "assignment_id": frozen.assignment_id,
                "digest": digest.as_str(),
                "public_key": public_key.as_str(),
            }),
        )
        .await
    }

    pub(crate) async fn observe(
        &self,
        secret: &str,
    ) -> Result<serde_json::Value, CloudCurrentTreeError> {
        self.request_result(secret, serde_json::json!({ "action": "observe" }))
            .await
    }

    pub(crate) async fn cancel(&self, secret: &str) -> Result<(), CloudCurrentTreeError> {
        self.request_result::<serde_json::Value>(secret, serde_json::json!({ "action": "cancel" }))
            .await
            .map(|_| ())
    }

    async fn request_result<T: DeserializeOwned>(
        &self,
        secret: &str,
        body: serde_json::Value,
    ) -> Result<T, CloudCurrentTreeError> {
        self.request::<ResultEnvelope<T>>(Some(secret), body)
            .await
            .map(|envelope| envelope.result)
    }

    async fn request<T: DeserializeOwned>(
        &self,
        secret: Option<&str>,
        body: serde_json::Value,
    ) -> Result<T, CloudCurrentTreeError> {
        let mut request = self
            .http
            .post(format!("{}/api/builds/current-tree", self.base_url))
            .json(&body);
        if let Some(secret) = secret {
            request = request.bearer_auth(secret);
        }
        let response = request
            .send()
            .await
            .map_err(|error| CloudCurrentTreeError::Transport(error.to_string()))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| CloudCurrentTreeError::Transport(error.to_string()))?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(CloudCurrentTreeError::ResponseTooLarge);
        }
        if !status.is_success() {
            let message = serde_json::from_slice::<ErrorEnvelope>(&bytes)
                .map(|error| error.error)
                .unwrap_or_else(|_| "Cloud current-tree request failed".to_owned());
            return Err(match status.as_u16() {
                401 | 403 => CloudCurrentTreeError::ApprovalExpired,
                409 => CloudCurrentTreeError::Conflict(message),
                _ => CloudCurrentTreeError::Rejected(message),
            });
        }
        serde_json::from_slice(&bytes)
            .map_err(|error| CloudCurrentTreeError::InvalidResponse(error.to_string()))
    }
}

pub(crate) fn select_context<'a>(
    contexts: &'a [ContextSummary],
    organization: Option<&str>,
    environment: Option<&str>,
    service: Option<&str>,
) -> Result<&'a ContextSummary, CloudCurrentTreeError> {
    let matching = contexts
        .iter()
        .filter(|context| {
            selector_matches(organization, &context.organization)
                && environment.is_none_or(|selector| {
                    selector == context.environment.id
                        || selector == context.environment.name
                        || selector == context.environment.namespace
                })
                && selector_matches(service, &context.service)
        })
        .collect::<Vec<_>>();
    match matching.as_slice() {
        [selected] => Ok(*selected),
        [] => Err(CloudCurrentTreeError::ContextNotFound),
        _ => Err(CloudCurrentTreeError::ContextAmbiguous {
            matches: matching.len(),
        }),
    }
}

fn selector_matches(selector: Option<&str>, context: &NamedContext) -> bool {
    selector.is_none_or(|selector| {
        selector == context.id || selector == context.slug || selector == context.name
    })
}

pub(crate) fn session_path() -> Option<PathBuf> {
    crate::machine::operator_context::default_cluster_context_path()
        .and_then(|path| path.parent().map(|parent| parent.join(SESSION_FILE_NAME)))
}

pub(crate) fn persist_session(
    path: &Path,
    session: &CloudSession,
) -> Result<(), CloudCurrentTreeError> {
    let Some(parent) = path.parent() else {
        return Err(CloudCurrentTreeError::SessionStore(
            "session path has no parent".to_owned(),
        ));
    };
    std::fs::create_dir_all(parent)
        .map_err(|error| CloudCurrentTreeError::SessionStore(error.to_string()))?;
    restrict_directory(parent)?;
    let bytes = serde_json::to_vec(session)
        .map_err(|error| CloudCurrentTreeError::SessionStore(error.to_string()))?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    let mut file = options
        .open(path)
        .map_err(|error| CloudCurrentTreeError::SessionStore(error.to_string()))?;
    restrict_file(path)?;
    file.set_len(0)
        .map_err(|error| CloudCurrentTreeError::SessionStore(error.to_string()))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| CloudCurrentTreeError::SessionStore(error.to_string()))?;
    restrict_file(path)
}

pub(crate) fn remove_session(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(unix)]
fn restrict_directory(path: &Path) -> Result<(), CloudCurrentTreeError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| CloudCurrentTreeError::SessionStore(error.to_string()))
}

#[cfg(not(unix))]
fn restrict_directory(_path: &Path) -> Result<(), CloudCurrentTreeError> {
    Ok(())
}

#[cfg(unix)]
fn restrict_file(path: &Path) -> Result<(), CloudCurrentTreeError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| CloudCurrentTreeError::SessionStore(error.to_string()))
}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) -> Result<(), CloudCurrentTreeError> {
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum CloudCurrentTreeError {
    #[error("Ployz Cloud URL is invalid")]
    InvalidCloudUrl,
    #[error("Ployz Cloud approval expired")]
    ApprovalExpired,
    #[error("no matching Cloud build context was found")]
    ContextNotFound,
    #[error("Cloud build context is ambiguous ({matches} matches); pass explicit selectors")]
    ContextAmbiguous { matches: usize },
    #[error("Cloud current-tree request conflicted: {0}")]
    Conflict(String),
    #[error("Cloud current-tree request was rejected: {0}")]
    Rejected(String),
    #[error("Cloud current-tree transport failed: {0}")]
    Transport(String),
    #[error("Cloud current-tree response exceeded its size limit")]
    ResponseTooLarge,
    #[error("Cloud current-tree response was invalid: {0}")]
    InvalidResponse(String),
    #[error("Cloud current-tree session storage failed: {0}")]
    SessionStore(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn receive_one_request(listener: tokio::net::TcpListener, body: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut request = Vec::new();
        let header_end = loop {
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).await.expect("read request");
            assert_ne!(read, 0, "request ended before headers");
            request.extend_from_slice(chunk.get(..read).expect("read fits buffer"));
            if let Some(position) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                break position + 4;
            }
        };
        let headers = String::from_utf8_lossy(
            request
                .get(..header_end)
                .expect("header boundary is within request"),
        );
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .expect("content length");
        while request.len() < header_end + content_length {
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).await.expect("read body");
            assert_ne!(read, 0, "request ended before body");
            request.extend_from_slice(chunk.get(..read).expect("read fits buffer"));
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .await
            .expect("respond");
        String::from_utf8(request).expect("utf8 request")
    }

    fn context(id: &str) -> ContextSummary {
        ContextSummary {
            organization: NamedContext {
                id: format!("org-{id}"),
                slug: "acme".to_owned(),
                name: "Acme".to_owned(),
            },
            project: NamedContext {
                id: format!("project-{id}"),
                slug: "web".to_owned(),
                name: "Web".to_owned(),
            },
            environment: EnvironmentContext {
                id: format!("env-{id}"),
                name: "Production".to_owned(),
                namespace: "production".to_owned(),
            },
            service: NamedContext {
                id: format!("service-{id}"),
                slug: id.to_owned(),
                name: id.to_owned(),
            },
        }
    }

    #[test]
    fn context_selection_requires_one_unique_match() {
        let contexts = [context("api"), context("worker")];
        assert!(matches!(
            select_context(&contexts, None, None, None),
            Err(CloudCurrentTreeError::ContextAmbiguous { matches: 2 })
        ));
        assert_eq!(
            select_context(&contexts, Some("acme"), Some("production"), Some("api"))
                .expect("unique")
                .service
                .slug,
            "api"
        );
        assert!(matches!(
            select_context(&contexts, None, None, Some("missing")),
            Err(CloudCurrentTreeError::ContextNotFound)
        ));
    }

    #[test]
    fn session_debug_and_display_redact_secret() {
        let session = CloudSession {
            cloud_url: "https://cloud.example".to_owned(),
            session_secret: "pct_secret".to_owned(),
            expires_at: "later".to_owned(),
        };
        assert!(!session.to_string().contains("pct_secret"));
        assert!(!format!("{session:?}").contains("pct_secret"));
    }

    #[tokio::test]
    async fn cancel_is_authenticated_and_contains_no_source_material() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listen");
        let address = listener.local_addr().expect("address");
        let request = tokio::spawn(receive_one_request(
            listener,
            "{\"result\":{\"status\":\"cancelled\"}}",
        ));
        CloudCurrentTreeClient::new(&format!("http://{address}"))
            .expect("client")
            .cancel("pct_secret")
            .await
            .expect("cancel");
        let request = request.await.expect("server");
        assert!(request.contains("authorization: Bearer pct_secret\r\n"));
        assert!(request.ends_with("{\"action\":\"cancel\"}"));
        assert!(!request.contains("digest"));
        assert!(!request.contains("source"));
    }

    #[tokio::test]
    async fn freeze_sends_only_digest_and_platform_metadata() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listen");
        let address = listener.local_addr().expect("address");
        let request = tokio::spawn(receive_one_request(
            listener,
            "{\"result\":{\"assignment_id\":\"assignment-1\",\"build_record_id\":\"build-1\",\"deployment_id\":\"deployment-1\"}}",
        ));
        let digest =
            LocalSnapshotDigest::try_new(format!("sha256:{}", "a".repeat(64))).expect("digest");
        let platform = OciPlatform::try_new("linux", "amd64").expect("platform");
        CloudCurrentTreeClient::new(&format!("http://{address}"))
            .expect("client")
            .freeze("pct_secret", &digest, &platform)
            .await
            .expect("freeze");
        let request = request.await.expect("server");
        assert!(request.contains(&format!("\"digest\":\"{}\"", digest.as_str())));
        assert!(request.contains("\"architecture\":\"amd64\""));
        assert!(!request.contains("source_bytes"));
        assert!(!request.contains("source_path"));
    }

    #[cfg(unix)]
    #[test]
    fn persisted_session_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config/ployz/current-tree-session.json");
        let session = CloudSession {
            cloud_url: "https://cloud.example".to_owned(),
            session_secret: "pct_secret".to_owned(),
            expires_at: "later".to_owned(),
        };
        persist_session(&path, &session).expect("persist");
        assert_eq!(
            std::fs::metadata(path.parent().expect("parent"))
                .expect("dir")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&path).expect("file").permissions().mode() & 0o777,
            0o600
        );
    }
}
