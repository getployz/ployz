use std::time::Duration;

use ployz_core::build::LocalSnapshotDigest;
use ployz_core::ids::{BuildExecutorId, BuildPoolId};
use ployz_core::image::OciPlatform;
use ployz_core::install::{MachineJoinRuntimeNatsUrl, MachineJoinTrustedNats};
use ployz_core::nats_config::NatsUserPublicKey;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

pub(crate) fn validated_cloud_base_url(
    base_url: &str,
    allow_loopback_http: bool,
) -> Result<reqwest::Url, CloudCurrentTreeError> {
    let base_url = base_url.trim_end_matches('/');
    let url = reqwest::Url::parse(base_url).map_err(|_| CloudCurrentTreeError::InvalidCloudUrl)?;
    let loopback_http = allow_loopback_http
        && url.scheme() == "http"
        && url.host_str().is_some_and(|host| {
            host.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
        });
    if (url.scheme() != "https" && !loopback_http) || url.cannot_be_a_base() {
        return Err(CloudCurrentTreeError::InvalidCloudUrl);
    }
    Ok(url)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ApprovalState {
    PendingApproval,
    Approved,
    ContextSelected,
    Expired,
    Cancelled,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CloudDeploymentStatus {
    Queued,
    Planning,
    Building,
    Deploying,
    Applied,
    Failed,
    Cancelled,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudDeploymentObservation {
    pub status: CloudDeploymentStatus,
    #[serde(rename = "failure_code")]
    _failure_code: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CloudBuildStatus {
    Queued,
    WaitingCi,
    Submitting,
    Building,
    Succeeded,
    Reused,
    Failed,
    Cancelled,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudBuildObservation {
    #[serde(rename = "status")]
    _status: CloudBuildStatus,
    #[serde(rename = "failure_code")]
    _failure_code: Option<String>,
    #[serde(rename = "receipt")]
    _receipt: Option<serde::de::IgnoredAny>,
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ObserveState {
    Dispatched {
        #[serde(rename = "build")]
        _build: CloudBuildObservation,
        #[serde(rename = "deployment")]
        _deployment: CloudDeploymentObservation,
    },
    Terminal {
        #[serde(rename = "build")]
        _build: CloudBuildObservation,
        deployment: CloudDeploymentObservation,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum CancelState {
    Cancelled {
        #[serde(rename = "targetAttemptId")]
        _target_attempt_id: Option<String>,
    },
    Terminal {
        #[serde(rename = "targetAttemptId")]
        _target_attempt_id: Option<String>,
    },
    Expired {
        #[serde(rename = "targetAttemptId")]
        _target_attempt_id: Option<String>,
    },
}

#[derive(Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum CurrentTreeRequest<'a> {
    Begin,
    Poll,
    ListContexts,
    SelectContext {
        organization_id: &'a str,
        environment_id: &'a str,
        service_id: &'a str,
    },
    Freeze {
        digest: &'a str,
        architecture: &'a str,
    },
    Activate {
        assignment_id: &'a str,
        digest: &'a str,
        public_key: &'a str,
    },
    Dispatch {
        assignment_id: &'a str,
        digest: &'a str,
        public_key: &'a str,
    },
    Observe,
    Cancel,
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
    #[serde(default)]
    code: Option<String>,
}

#[derive(Clone)]
pub(crate) struct CloudCurrentTreeClient {
    base_url: String,
    http: reqwest::Client,
}

impl CloudCurrentTreeClient {
    pub(crate) fn new(base_url: &str) -> Result<Self, CloudCurrentTreeError> {
        let url = validated_cloud_base_url(base_url, true)?;
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| CloudCurrentTreeError::Transport(error.to_string()))?;
        Ok(Self {
            base_url: url.as_str().trim_end_matches('/').to_owned(),
            http,
        })
    }

    pub(crate) async fn begin(&self) -> Result<BeginSession, CloudCurrentTreeError> {
        self.request(None, &CurrentTreeRequest::Begin).await
    }

    pub(crate) async fn poll(&self, secret: &str) -> Result<ApprovalState, CloudCurrentTreeError> {
        self.request_result(secret, &CurrentTreeRequest::Poll).await
    }

    pub(crate) async fn contexts(
        &self,
        secret: &str,
    ) -> Result<Vec<ContextSummary>, CloudCurrentTreeError> {
        self.request_result(secret, &CurrentTreeRequest::ListContexts)
            .await
    }

    pub(crate) async fn select(
        &self,
        secret: &str,
        context: &ContextSummary,
    ) -> Result<ContextSummary, CloudCurrentTreeError> {
        self.request_result(
            secret,
            &CurrentTreeRequest::SelectContext {
                organization_id: &context.organization.id,
                environment_id: &context.environment.id,
                service_id: &context.service.id,
            },
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
            &CurrentTreeRequest::Freeze {
                digest: digest.as_str(),
                architecture: platform.architecture(),
            },
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
            &CurrentTreeRequest::Activate {
                assignment_id: &frozen.assignment_id,
                digest: digest.as_str(),
                public_key: public_key.as_str(),
            },
        )
        .await
    }

    pub(crate) async fn dispatch(
        &self,
        secret: &str,
        frozen: &FrozenBuild,
        digest: &LocalSnapshotDigest,
        public_key: &NatsUserPublicKey,
    ) -> Result<(), CloudCurrentTreeError> {
        self.request_result::<serde::de::IgnoredAny>(
            secret,
            &CurrentTreeRequest::Dispatch {
                assignment_id: &frozen.assignment_id,
                digest: digest.as_str(),
                public_key: public_key.as_str(),
            },
        )
        .await
        .map(|_| ())
    }

    pub(crate) async fn observe(
        &self,
        secret: &str,
    ) -> Result<ObserveState, CloudCurrentTreeError> {
        self.request_result(secret, &CurrentTreeRequest::Observe)
            .await
    }

    pub(crate) async fn cancel(&self, secret: &str) -> Result<(), CloudCurrentTreeError> {
        self.request_result::<CancelState>(secret, &CurrentTreeRequest::Cancel)
            .await
            .map(|_| ())
    }

    async fn request_result<T: DeserializeOwned>(
        &self,
        secret: &str,
        body: &CurrentTreeRequest<'_>,
    ) -> Result<T, CloudCurrentTreeError> {
        self.request::<ResultEnvelope<T>>(Some(secret), body)
            .await
            .map(|envelope| envelope.result)
    }

    async fn request<T: DeserializeOwned>(
        &self,
        secret: Option<&str>,
        body: &CurrentTreeRequest<'_>,
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
            let envelope = serde_json::from_slice::<ErrorEnvelope>(&bytes).ok();
            if envelope.as_ref().and_then(|error| error.code.as_deref())
                == Some("no_reachable_image_seed")
            {
                return Err(CloudCurrentTreeError::NoReachableImageSeed);
            }
            let message = envelope
                .map(|error| error.error)
                .unwrap_or_else(|| "Cloud current-tree request failed".to_owned());
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
    #[error("no reachable image seed is available for the current-tree build")]
    NoReachableImageSeed,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn receive_one_request(
        listener: tokio::net::TcpListener,
        status: &'static str,
        body: &'static str,
    ) -> String {
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
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
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
    fn cloud_url_requires_https_except_literal_loopback_http() {
        assert!(CloudCurrentTreeClient::new("https://cloud.example").is_ok());
        assert!(CloudCurrentTreeClient::new("http://127.0.0.1:3000").is_ok());
        assert!(CloudCurrentTreeClient::new("http://[::1]:3000").is_ok());
        assert!(CloudCurrentTreeClient::new("http://localhost:3000").is_err());
        assert!(CloudCurrentTreeClient::new("http://cloud.example").is_err());
    }

    #[tokio::test]
    async fn cancel_is_authenticated_and_contains_no_source_material() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listen");
        let address = listener.local_addr().expect("address");
        let request = tokio::spawn(receive_one_request(
            listener,
            "200 OK",
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
            "200 OK",
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

    #[tokio::test]
    async fn dispatch_is_authenticated_and_names_only_the_frozen_records() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listen");
        let address = listener.local_addr().expect("address");
        let request = tokio::spawn(receive_one_request(listener, "200 OK", "{\"result\":null}"));
        let frozen = FrozenBuild {
            assignment_id: "assignment-1".to_owned(),
            build_record_id: "build-1".to_owned(),
            deployment_id: "deployment-1".to_owned(),
        };
        let digest =
            LocalSnapshotDigest::try_new(format!("sha256:{}", "a".repeat(64))).expect("digest");
        let public_key = ployz_core::nats_config::MintedNatsUser::generate()
            .expect("NKey")
            .public;
        CloudCurrentTreeClient::new(&format!("http://{address}"))
            .expect("client")
            .dispatch("pct_secret", &frozen, &digest, &public_key)
            .await
            .expect("dispatch");
        let request = request.await.expect("server");
        assert!(request.contains("authorization: Bearer pct_secret\r\n"));
        assert!(request.ends_with(
            &format!(
                "{{\"action\":\"dispatch\",\"assignment_id\":\"assignment-1\",\"digest\":\"{}\",\"public_key\":\"{}\"}}",
                digest.as_str(),
                public_key.as_str(),
            )
        ));
        assert!(!request.contains("source"));
        assert!(!request.contains("build_record_id"));
        assert!(!request.contains("deployment_id"));
    }

    #[tokio::test]
    async fn stable_no_seed_code_decodes_to_typed_error() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listen");
        let address = listener.local_addr().expect("address");
        let request = tokio::spawn(receive_one_request(
            listener,
            "409 Conflict",
            "{\"error\":\"no seed can receive the image\",\"code\":\"no_reachable_image_seed\"}",
        ));
        let frozen = FrozenBuild {
            assignment_id: "assignment-1".to_owned(),
            build_record_id: "build-1".to_owned(),
            deployment_id: "deployment-1".to_owned(),
        };
        let digest =
            LocalSnapshotDigest::try_new(format!("sha256:{}", "a".repeat(64))).expect("digest");
        let public_key = ployz_core::nats_config::MintedNatsUser::generate()
            .expect("NKey")
            .public;
        let error = CloudCurrentTreeClient::new(&format!("http://{address}"))
            .expect("client")
            .activate("pct_secret", &frozen, &digest, &public_key)
            .await
            .expect_err("typed rejection");
        assert_eq!(error, CloudCurrentTreeError::NoReachableImageSeed);
        let request = request.await.expect("server");
        assert!(request.contains("\"action\":\"activate\""));
        assert!(!request.contains("\"action\":\"dispatch\""));
    }

    #[test]
    fn observe_contract_accepts_dispatched_and_terminal_payloads() {
        let dispatched: ResultEnvelope<ObserveState> = serde_json::from_str(
            r#"{"result":{"status":"dispatched","build":{"status":"building","failure_code":null,"receipt":null},"deployment":{"status":"building","failure_code":null}}}"#,
        )
        .expect("dispatched observation");
        assert!(matches!(dispatched.result, ObserveState::Dispatched { .. }));

        let terminal: ResultEnvelope<ObserveState> = serde_json::from_str(
            r#"{"result":{"status":"terminal","build":{"status":"succeeded","failure_code":null,"receipt":{}},"deployment":{"status":"applied","failure_code":null}}}"#,
        )
        .expect("terminal observation");
        assert!(matches!(terminal.result, ObserveState::Terminal { .. }));
    }

    #[test]
    fn cancel_contract_accepts_every_terminal_session_state() {
        for status in ["cancelled", "terminal", "expired"] {
            let body = format!(
                r#"{{"result":{{"status":"{status}","targetAttemptId":"assignment-1"}}}}"#,
            );
            serde_json::from_str::<ResultEnvelope<CancelState>>(&body)
                .expect("closed cancellation state");
        }
    }
}
