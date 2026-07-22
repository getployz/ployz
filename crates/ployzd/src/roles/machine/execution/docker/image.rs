use std::time::Duration;

use bollard::auth::DockerCredentials;
use bollard::errors::Error as BollardError;
use bollard::query_parameters::CreateImageOptionsBuilder;
use futures_util::StreamExt;
use ployz_core::deploy::{ImageReference, RegistryCredential};
use ployz_core::image::OciDigest;

use super::network::is_docker_object_missing;
use super::runner::DockerManagedContainerRunner;
use crate::roles::machine::protocol::MachineImagePull;
use crate::roles::machine::runner::MachineRegistryImageResolveError;

const REGISTRY_RETRY_DELAYS: [Duration; 2] = [Duration::from_millis(250), Duration::from_secs(1)];
const MESH_SEED_PULL_ATTEMPTS: u8 = 10;
const MESH_SEED_PULL_RETRY_DELAY: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq, Eq)]
enum DockerImagePullError {
    Retryable { message: String },
    Terminal { message: String },
}

impl DockerImagePullError {
    fn from_bollard(
        image: &str,
        credential: Option<&RegistryCredential>,
        error: BollardError,
    ) -> Self {
        let message =
            redact_registry_credential(format!("pull Docker image {image}: {error}"), credential);
        if retryable_registry_error(&error) {
            Self::Retryable { message }
        } else {
            Self::Terminal { message }
        }
    }

    fn into_message(self) -> String {
        match self {
            Self::Retryable { message } | Self::Terminal { message } => message,
        }
    }
}

impl DockerManagedContainerRunner {
    pub(super) async fn resolve_registry_reference(
        &self,
        reference: &ImageReference,
        credential: Option<&RegistryCredential>,
    ) -> Result<OciDigest, MachineRegistryImageResolveError> {
        let failure = |error: BollardError| MachineRegistryImageResolveError::ImagePull {
            message: redact_registry_credential(
                format!("resolve Docker image {}: {error}", reference.as_str()),
                credential,
            ),
        };
        let mut retry_delays = REGISTRY_RETRY_DELAYS.into_iter();
        let inspected = loop {
            let docker = match self.docker().await {
                Ok(docker) => docker,
                Err(error) => {
                    if let Some(delay) = retry_delays.next() {
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Err(MachineRegistryImageResolveError::ImagePull {
                        message: error.to_string(),
                    });
                }
            };
            match docker
                .inspect_registry_image(reference.as_str(), docker_credentials(credential))
                .await
            {
                Ok(inspected) => break inspected,
                Err(error) => {
                    if retryable_registry_error(&error)
                        && let Some(delay) = retry_delays.next()
                    {
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Err(failure(error));
                }
            }
        };
        let Some(digest) = inspected.descriptor.digest else {
            return Err(MachineRegistryImageResolveError::ImagePull {
                message: format!("registry returned no digest for {}", reference.as_str()),
            });
        };
        OciDigest::try_new(digest).map_err(|error| MachineRegistryImageResolveError::ImagePull {
            message: error.to_string(),
        })
    }

    async fn pull_registry_image(
        &self,
        image: &ImageReference,
        credential: Option<&RegistryCredential>,
    ) -> Result<(), DockerImagePullError> {
        if image.pinned_digest().is_some() {
            let mut retry_delays = REGISTRY_RETRY_DELAYS.into_iter();
            loop {
                let docker = match self.docker().await {
                    Ok(docker) => docker,
                    Err(error) => {
                        let message = error.to_string();
                        let Some(delay) = retry_delays.next() else {
                            return Err(DockerImagePullError::Retryable { message });
                        };
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                };
                match docker.inspect_image(image.as_str()).await {
                    Ok(_) => return Ok(()),
                    Err(error) if is_docker_object_missing(&error) => break,
                    Err(error) => {
                        return Err(DockerImagePullError::Terminal {
                            message: format!(
                                "inspect local Docker image {}: {error}",
                                image.as_str()
                            ),
                        });
                    }
                }
            }
        }

        for delay in REGISTRY_RETRY_DELAYS {
            match self.pull_image(image.as_str(), credential).await {
                Ok(()) => return Ok(()),
                Err(DockerImagePullError::Retryable { .. }) => tokio::time::sleep(delay).await,
                Err(error) => return Err(error),
            }
        }
        self.pull_image(image.as_str(), credential).await
    }

    async fn pull_image(
        &self,
        image: &str,
        credential: Option<&RegistryCredential>,
    ) -> Result<(), DockerImagePullError> {
        let docker = self
            .docker()
            .await
            .map_err(|error| DockerImagePullError::Retryable {
                message: error.to_string(),
            })?;
        let options = CreateImageOptionsBuilder::new().from_image(image).build();
        let mut stream = docker.create_image(Some(options), None, docker_credentials(credential));

        while let Some(result) = stream.next().await {
            result.map_err(|error| DockerImagePullError::from_bollard(image, credential, error))?;
        }

        Ok(())
    }

    pub(super) async fn pull_machine_image(&self, pull: &MachineImagePull) -> Result<(), String> {
        match pull {
            MachineImagePull::Registry {
                reference,
                credential,
            } => self
                .pull_registry_image(reference, credential.as_ref())
                .await
                .map_err(DockerImagePullError::into_message),
            MachineImagePull::MeshSeed {
                seed_host: _,
                repository: _,
                manifest_digest: _,
            } => self
                .pull_image(&pull.reference(), None)
                .await
                .map_err(DockerImagePullError::into_message),
        }
    }

    pub(crate) async fn pull_mesh_seed_image(&self, reference: &str) -> Result<(), String> {
        for attempt in 1..=MESH_SEED_PULL_ATTEMPTS {
            match self.pull_image(reference, None).await {
                Ok(()) => return Ok(()),
                Err(error) if attempt == MESH_SEED_PULL_ATTEMPTS => {
                    return Err(error.into_message());
                }
                Err(
                    DockerImagePullError::Retryable { message: _ }
                    | DockerImagePullError::Terminal { message: _ },
                ) => {
                    tokio::time::sleep(MESH_SEED_PULL_RETRY_DELAY).await;
                }
            }
        }
        unreachable!("the mesh-seed pull loop has at least one attempt")
    }
}

fn retryable_registry_error(error: &BollardError) -> bool {
    if let BollardError::DockerResponseServerError { status_code, .. } = error {
        return *status_code == 429 || *status_code >= 500;
    }
    if let BollardError::DockerStreamError { error } = error {
        return retryable_registry_stream_error(error);
    }
    matches!(
        error,
        BollardError::RequestTimeoutError
            | BollardError::HyperResponseError { .. }
            | BollardError::HyperLegacyError { .. }
            | BollardError::IOError { .. }
    )
}

fn retryable_registry_stream_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    if retryable_http_status(&message) {
        return true;
    }
    [
        "toomanyrequests",
        "too many requests",
        "client.timeout exceeded",
        "i/o timeout",
        "context deadline exceeded",
        "net/http: request canceled",
        "temporarily unavailable",
        "temporary failure in name resolution",
        "connection reset by peer",
        "connect: connection refused",
        "use of closed network connection",
        "broken pipe",
        "network is unreachable",
        "no route to host",
        "unexpected eof",
    ]
    .iter()
    .any(|signal| message.contains(signal))
}

fn retryable_http_status(message: &str) -> bool {
    ["status code ", "http status: ", "http status "]
        .iter()
        .filter_map(|prefix| message.find(prefix).map(|offset| (prefix, offset)))
        .filter_map(|(prefix, offset)| message.get(offset + prefix.len()..))
        .filter_map(|suffix| suffix.get(..3))
        .filter_map(|status| status.parse::<u16>().ok())
        .any(|status| status == 429 || (500..600).contains(&status))
}

fn docker_credentials(credential: Option<&RegistryCredential>) -> Option<DockerCredentials> {
    credential.map(|credential| match credential {
        RegistryCredential::Basic { username, password } => DockerCredentials {
            username: Some(username.as_str().to_owned()),
            password: Some(password.secret().to_owned()),
            ..DockerCredentials::default()
        },
        RegistryCredential::IdentityToken { token } => DockerCredentials {
            identitytoken: Some(token.secret().to_owned()),
            ..DockerCredentials::default()
        },
    })
}

fn redact_registry_credential(message: String, credential: Option<&RegistryCredential>) -> String {
    match credential {
        Some(credential) => credential.redact_secret_in(message),
        None => message,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use ployz_core::deploy::RegistryCredential;
    use ployz_core::image::OciDigest;

    use super::*;
    use crate::roles::machine::execution::docker::test_support::{image, runner_with_responses};
    use crate::roles::machine::runner::{MachineContainerRunner, MachineRegistryImageResolveError};

    #[tokio::test]
    async fn registry_resolution_retries_transient_server_failures() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let success = format!(r#"{{"Descriptor":{{"digest":"{digest}"}},"Platforms":[]}}"#);
        let (runner, attempts, _socket_dir) = runner_with_responses(vec![
            (429, r#"{"message":"rate limited"}"#.to_owned()),
            (500, r#"{"message":"registry unavailable"}"#.to_owned()),
            (200, success),
        ])
        .await;

        let resolved = runner
            .resolve_registry_image(&image("nginx:1.27-alpine"), None)
            .await
            .expect("third registry inspection succeeds");

        assert_eq!(resolved, OciDigest::try_new(digest).expect("valid digest"));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn registry_resolution_does_not_retry_terminal_client_failures() {
        let digest = format!("sha256:{}", "b".repeat(64));
        let (runner, attempts, _socket_dir) = runner_with_responses(vec![
            (404, r#"{"message":"manifest unknown"}"#.to_owned()),
            (
                200,
                format!(r#"{{"Descriptor":{{"digest":"{digest}"}},"Platforms":[]}}"#),
            ),
        ])
        .await;

        let error = runner
            .resolve_registry_image(&image("nginx:missing"), None)
            .await
            .expect_err("missing manifest is terminal");

        let MachineRegistryImageResolveError::ImagePull { message } = error;
        assert!(message.contains("status code 404"), "{message}");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn registry_resolution_stops_after_three_transient_server_failures() {
        let digest = format!("sha256:{}", "c".repeat(64));
        let (runner, attempts, _socket_dir) = runner_with_responses(vec![
            (500, r#"{"message":"first failure"}"#.to_owned()),
            (500, r#"{"message":"second failure"}"#.to_owned()),
            (500, r#"{"message":"final failure"}"#.to_owned()),
            (
                200,
                format!(r#"{{"Descriptor":{{"digest":"{digest}"}},"Platforms":[]}}"#),
            ),
        ])
        .await;

        let error = runner
            .resolve_registry_image(&image("nginx:1.27-alpine"), None)
            .await
            .expect_err("three transient failures exhaust retries");

        let MachineRegistryImageResolveError::ImagePull { message } = error;
        assert!(message.contains("final failure"), "{message}");
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn registry_pull_retries_transient_failure_then_reuses_local_digest() {
        let reference = image(&format!("nginx@sha256:{}", "d".repeat(64)));
        let (runner, attempts, _socket_dir) = runner_with_responses(vec![
            (404, r#"{"message":"not found"}"#.to_owned()),
            (
                200,
                r#"{"errorDetail":{"message":"received unexpected HTTP status: 503 Service Unavailable"},"error":"registry unavailable"}
"#
                .to_owned(),
            ),
            (200, "{}\n".to_owned()),
            (200, "{}".to_owned()),
        ])
        .await;

        runner
            .pull_registry_image(&reference, None)
            .await
            .expect("transient pull succeeds on retry");
        runner
            .pull_registry_image(&reference, None)
            .await
            .expect("local digest skips another registry pull");

        assert_eq!(attempts.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn registry_pull_does_not_retry_terminal_stream_failure() {
        let (runner, attempts, _socket_dir) = runner_with_responses(vec![
            (
                200,
                r#"{"errorDetail":{"message":"manifest request failed: 404 Not Found"},"error":"manifest unknown"}
"#
                .to_owned(),
            ),
            (200, "{}\n".to_owned()),
        ])
        .await;

        let error = runner
            .pull_registry_image(&image("nginx:missing"), None)
            .await
            .expect_err("terminal registry failure is not retried");

        assert!(error.into_message().contains("404 Not Found"));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn registry_pull_retries_any_streamed_server_error() {
        let (runner, attempts, _socket_dir) = runner_with_responses(vec![
            (
                200,
                r#"{"errorDetail":{"message":"received unexpected HTTP status: 501 Not Implemented"},"error":"registry unavailable"}
"#
                .to_owned(),
            ),
            (200, "{}\n".to_owned()),
        ])
        .await;

        runner
            .pull_registry_image(&image("nginx:1.27-alpine"), None)
            .await
            .expect("another 5xx status remains retryable");

        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn registry_pull_does_not_treat_local_inspection_failure_as_a_cache_miss() {
        let reference = image(&format!("nginx@sha256:{}", "e".repeat(64)));
        let (runner, attempts, _socket_dir) = runner_with_responses(vec![
            (
                500,
                r#"{"message":"Docker storage unavailable"}"#.to_owned(),
            ),
            (200, "{}\n".to_owned()),
        ])
        .await;

        let error = runner
            .pull_registry_image(&reference, None)
            .await
            .expect_err("local inspection failure does not trigger a registry pull");

        assert!(error.into_message().contains("inspect local Docker image"));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn registry_stream_retry_classification_requires_a_transient_phrase() {
        assert!(retryable_registry_stream_error(
            "received unexpected HTTP status: 503 Service Unavailable"
        ));
        assert!(retryable_registry_stream_error(
            "dial tcp [2001:db8::1]:443: network is unreachable"
        ));
        assert!(retryable_registry_stream_error(
            "request failed: context deadline exceeded"
        ));
        assert!(retryable_registry_stream_error(
            "received unexpected HTTP status: 599 Unknown"
        ));
        assert!(!retryable_registry_stream_error(
            "manifest identifier 500 was denied"
        ));
        assert!(!retryable_registry_stream_error(
            "manifest request failed: 404 Not Found"
        ));
        assert!(!retryable_registry_stream_error(
            "manifest metadata says timeout is disabled"
        ));
    }

    #[test]
    fn docker_credentials_keep_basic_and_identity_token_modes_distinct() {
        let basic = RegistryCredential::try_basic("alice", "password").expect("valid basic auth");
        let token = RegistryCredential::try_identity_token("token").expect("valid token auth");

        let basic = docker_credentials(Some(&basic)).expect("basic credentials");
        assert_eq!(basic.username.as_deref(), Some("alice"));
        assert_eq!(basic.password.as_deref(), Some("password"));
        assert_eq!(basic.identitytoken, None);

        let token = docker_credentials(Some(&token)).expect("token credentials");
        assert_eq!(token.username, None);
        assert_eq!(token.password, None);
        assert_eq!(token.identitytoken.as_deref(), Some("token"));
    }

    #[test]
    fn registry_errors_redact_the_deploy_scoped_secret() {
        let basic = RegistryCredential::try_basic("alice", "password").expect("valid basic auth");
        let token = RegistryCredential::try_identity_token("token").expect("valid token auth");

        assert_eq!(
            redact_registry_credential(
                "registry reflected password in its response".to_owned(),
                Some(&basic),
            ),
            "registry reflected [redacted] in its response"
        );
        assert_eq!(
            redact_registry_credential(
                "registry reflected token in its response".to_owned(),
                Some(&token),
            ),
            "registry reflected [redacted] in its response"
        );
    }
}
