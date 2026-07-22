use std::time::Duration;

use bollard::auth::DockerCredentials;
use bollard::errors::Error as BollardError;
use bollard::query_parameters::CreateImageOptionsBuilder;
use futures_util::StreamExt;
use ployz_core::deploy::{ImageReference, RegistryCredential};
use ployz_core::image::{ImageEnsureSource, OciDigest, OciPlatform};

use super::network::is_docker_object_missing;
use super::runner::DockerManagedContainerRunner;
use crate::roles::machine::image_ensure::ImagePullProgress;
use crate::roles::machine::runner::MachineRegistryImageResolveError;

const REGISTRY_RETRY_DELAYS: [Duration; 2] = [Duration::from_millis(250), Duration::from_secs(1)];
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

    async fn pull_image_with_progress(
        &self,
        image: &str,
        credential: Option<&RegistryCredential>,
        progress: &tokio::sync::mpsc::UnboundedSender<ImagePullProgress>,
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
            let frame = result
                .map_err(|error| DockerImagePullError::from_bollard(image, credential, error))?;
            let detail = frame.progress_detail.unwrap_or_default();
            let _ = progress.send(ImagePullProgress {
                layer: frame.id.unwrap_or_else(|| "manifest".to_owned()),
                state: frame.status.unwrap_or_else(|| "progress".to_owned()),
                current_bytes: detail.current.unwrap_or_default().max(0) as u64,
            });
        }
        Ok(())
    }

    pub(crate) async fn ensure_machine_image(
        &self,
        source: &ImageEnsureSource,
        progress: tokio::sync::mpsc::UnboundedSender<ImagePullProgress>,
    ) -> Result<(), String> {
        let exact_reference = source.reference();
        let expected = ExpectedLocalImage::from_source(source)?;
        if self.verify_local_image(&exact_reference, &expected).await? {
            return Ok(());
        }
        let (reference, credential) = match source {
            ImageEnsureSource::Registry {
                reference,
                credential,
                platform: _,
            } => (reference.as_str().to_owned(), credential.as_ref()),
            ImageEnsureSource::MeshSeed { .. } | ImageEnsureSource::LocalSeed { .. } => {
                (source.reference(), None)
            }
        };
        loop {
            match self
                .pull_image_with_progress(&reference, credential, &progress)
                .await
            {
                Ok(()) => {
                    if self.verify_local_image(&exact_reference, &expected).await? {
                        return Ok(());
                    }
                    return Err(format!(
                        "Docker pull completed without installing exact image {exact_reference}"
                    ));
                }
                Err(DockerImagePullError::Retryable { .. }) => {
                    tokio::time::sleep(MESH_SEED_PULL_RETRY_DELAY).await;
                }
                Err(error) => return Err(error.into_message()),
            }
        }
    }

    async fn verify_local_image(
        &self,
        exact_reference: &str,
        expected: &ExpectedLocalImage,
    ) -> Result<bool, String> {
        let docker = self.docker().await.map_err(|error| error.to_string())?;
        let inspected = match docker.inspect_image(exact_reference).await {
            Ok(inspected) => inspected,
            Err(error) if is_docker_object_missing(&error) => return Ok(false),
            Err(error) => {
                return Err(format!(
                    "inspect local Docker image {exact_reference}: {error}"
                ));
            }
        };
        let repo_digests = inspected.repo_digests.unwrap_or_default();
        if !repo_digests
            .iter()
            .any(|reference| reference == exact_reference)
        {
            return Err(format!(
                "local Docker image {exact_reference} does not have expected manifest digest {}",
                expected.manifest_digest
            ));
        }
        if let Some(expected_image_id) = &expected.image_id {
            let actual = inspected.id.unwrap_or_default();
            if actual != expected_image_id.as_str() {
                return Err(format!(
                    "local Docker image {exact_reference} has config identity {actual:?}, expected {expected_image_id}"
                ));
            }
        }
        let actual_platform = OciPlatform::try_new(
            inspected.os.unwrap_or_default(),
            inspected.architecture.unwrap_or_default(),
        )
        .map_err(|error| {
            format!("local Docker image {exact_reference} has invalid platform: {error}")
        })?;
        if actual_platform != expected.platform {
            return Err(format!(
                "local Docker image {exact_reference} has platform {}/{}, expected {}/{}",
                actual_platform.os(),
                actual_platform.architecture(),
                expected.platform.os(),
                expected.platform.architecture()
            ));
        }
        Ok(true)
    }
}

struct ExpectedLocalImage {
    manifest_digest: OciDigest,
    image_id: Option<OciDigest>,
    platform: OciPlatform,
}

impl ExpectedLocalImage {
    fn from_source(source: &ImageEnsureSource) -> Result<Self, String> {
        match source {
            ImageEnsureSource::Registry {
                reference,
                platform,
                credential: _,
            } => Ok(Self {
                manifest_digest: reference.pinned_digest().ok_or_else(|| {
                    "registry image ensure requires a digest-pinned reference".to_owned()
                })?,
                image_id: None,
                platform: platform.clone(),
            }),
            ImageEnsureSource::MeshSeed {
                manifest_digest,
                image_id,
                platform,
                ..
            }
            | ImageEnsureSource::LocalSeed {
                manifest_digest,
                image_id,
                platform,
                ..
            } => Ok(Self {
                manifest_digest: manifest_digest.clone(),
                image_id: Some(image_id.clone()),
                platform: platform.clone(),
            }),
        }
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
    use ployz_core::image::{ImageRepository, OciDigest, OciPlatform};

    use super::*;
    use crate::roles::machine::execution::docker::test_support::{image, runner_with_responses};
    use crate::roles::machine::runner::{MachineContainerRunner, MachineRegistryImageResolveError};

    fn registry_source(reference: ImageReference) -> ImageEnsureSource {
        ImageEnsureSource::Registry {
            reference,
            platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
            credential: None,
        }
    }

    fn local_image_json(reference: &str, image_id: &OciDigest, architecture: &str) -> String {
        format!(
            r#"{{"Id":"{image_id}","RepoDigests":["{reference}"],"Os":"linux","Architecture":"{architecture}"}}"#
        )
    }

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
        let image_id = OciDigest::try_new(format!("sha256:{}", "c".repeat(64))).expect("image id");
        let (runner, attempts, _socket_dir) = runner_with_responses(vec![
            (404, r#"{"message":"not found"}"#.to_owned()),
            (
                200,
                r#"{"errorDetail":{"message":"received unexpected HTTP status: 503 Service Unavailable"},"error":"registry unavailable"}
"#
                .to_owned(),
            ),
            (200, "{}\n".to_owned()),
            (200, local_image_json(reference.as_str(), &image_id, "amd64")),
            (200, local_image_json(reference.as_str(), &image_id, "amd64")),
        ])
        .await;

        let source = registry_source(reference);
        let (progress, _rx) = tokio::sync::mpsc::unbounded_channel();
        runner
            .ensure_machine_image(&source, progress)
            .await
            .expect("transient pull succeeds on retry");
        let (progress, _rx) = tokio::sync::mpsc::unbounded_channel();
        runner
            .ensure_machine_image(&source, progress)
            .await
            .expect("local digest skips another registry pull");

        assert_eq!(attempts.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn exact_mesh_source_reuses_local_digest_without_create_image() {
        let digest = OciDigest::try_new(format!("sha256:{}", "f".repeat(64))).expect("digest");
        let image_id = OciDigest::try_new(format!("sha256:{}", "e".repeat(64))).expect("image id");
        let source = ImageEnsureSource::MeshSeed {
            seed_host: "10.42.0.1".parse().expect("host"),
            repository: ImageRepository::try_new("ployz/default/api".to_owned())
                .expect("repository"),
            manifest_digest: digest.clone(),
            image_id: image_id.clone(),
            platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
        };
        let (runner, attempts, _socket_dir) = runner_with_responses(vec![
            (
                200,
                local_image_json(&source.reference(), &image_id, "amd64"),
            ),
            (
                500,
                r#"{"message":"create_image must not be called"}"#.to_owned(),
            ),
        ])
        .await;
        let (progress, _rx) = tokio::sync::mpsc::unbounded_channel();
        runner
            .ensure_machine_image(&source, progress)
            .await
            .expect("local exact image is reused");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unpinned_registry_source_is_rejected_before_docker_access() {
        let (runner, attempts, _socket_dir) = runner_with_responses(Vec::new()).await;
        let (progress, _rx) = tokio::sync::mpsc::unbounded_channel();
        let error = runner
            .ensure_machine_image(&registry_source(image("nginx:latest")), progress)
            .await
            .expect_err("mutable registry reference is rejected");
        assert!(error.contains("digest-pinned"), "{error}");
        assert_eq!(attempts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn successful_pull_stream_without_a_local_exact_image_fails() {
        let reference = image(&format!("nginx@sha256:{}", "1".repeat(64)));
        let (runner, attempts, _socket_dir) = runner_with_responses(vec![
            (404, r#"{"message":"not found"}"#.to_owned()),
            (200, "{}\n".to_owned()),
            (404, r#"{"message":"still absent"}"#.to_owned()),
        ])
        .await;
        let (progress, _rx) = tokio::sync::mpsc::unbounded_channel();
        let error = runner
            .ensure_machine_image(&registry_source(reference), progress)
            .await
            .expect_err("empty pull stream cannot manufacture completion");
        assert!(error.contains("without installing exact image"), "{error}");
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn local_mesh_image_requires_exact_manifest_config_and_platform() {
        let manifest = OciDigest::try_new(format!("sha256:{}", "2".repeat(64))).expect("manifest");
        let image_id = OciDigest::try_new(format!("sha256:{}", "3".repeat(64))).expect("image id");
        let source = ImageEnsureSource::MeshSeed {
            seed_host: "10.42.0.1".parse().expect("host"),
            repository: ImageRepository::try_new("ployz/default/api".to_owned())
                .expect("repository"),
            manifest_digest: manifest.clone(),
            image_id: image_id.clone(),
            platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
        };
        let reference = source.reference();

        let wrong_manifest = format!("10.42.0.1:5000/ployz/default/api@sha256:{}", "4".repeat(64));
        let (runner, _, _socket_dir) = runner_with_responses(vec![(
            200,
            local_image_json(&wrong_manifest, &image_id, "amd64"),
        )])
        .await;
        let (progress, _rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(
            runner
                .ensure_machine_image(&source, progress)
                .await
                .expect_err("manifest mismatch")
                .contains("manifest digest")
        );

        let wrong_image_id =
            OciDigest::try_new(format!("sha256:{}", "5".repeat(64))).expect("image id");
        let (runner, _, _socket_dir) = runner_with_responses(vec![(
            200,
            local_image_json(&reference, &wrong_image_id, "amd64"),
        )])
        .await;
        let (progress, _rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(
            runner
                .ensure_machine_image(&source, progress)
                .await
                .expect_err("config mismatch")
                .contains("config identity")
        );

        let (runner, _, _socket_dir) = runner_with_responses(vec![(
            200,
            local_image_json(&reference, &image_id, "arm64"),
        )])
        .await;
        let (progress, _rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(
            runner
                .ensure_machine_image(&source, progress)
                .await
                .expect_err("platform mismatch")
                .contains("platform")
        );
    }

    #[tokio::test]
    async fn registry_pull_does_not_retry_terminal_stream_failure() {
        let reference = image(&format!("nginx@sha256:{}", "a".repeat(64)));
        let (runner, attempts, _socket_dir) = runner_with_responses(vec![
            (404, r#"{"message":"not found"}"#.to_owned()),
            (
                200,
                r#"{"errorDetail":{"message":"manifest request failed: 404 Not Found"},"error":"manifest unknown"}
"#
                .to_owned(),
            ),
            (200, "{}\n".to_owned()),
        ])
        .await;

        let (progress, _rx) = tokio::sync::mpsc::unbounded_channel();
        let error = runner
            .ensure_machine_image(&registry_source(reference), progress)
            .await
            .expect_err("terminal registry failure is not retried");

        assert!(error.contains("404 Not Found"));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn registry_pull_retries_any_streamed_server_error() {
        let reference = image(&format!("nginx@sha256:{}", "b".repeat(64)));
        let image_id = OciDigest::try_new(format!("sha256:{}", "c".repeat(64))).expect("image id");
        let (runner, attempts, _socket_dir) = runner_with_responses(vec![
            (404, r#"{"message":"not found"}"#.to_owned()),
            (
                200,
                r#"{"errorDetail":{"message":"received unexpected HTTP status: 501 Not Implemented"},"error":"registry unavailable"}
"#
                .to_owned(),
            ),
            (200, "{}\n".to_owned()),
            (200, local_image_json(reference.as_str(), &image_id, "amd64")),
        ])
        .await;

        let (progress, _rx) = tokio::sync::mpsc::unbounded_channel();
        runner
            .ensure_machine_image(&registry_source(reference), progress)
            .await
            .expect("another 5xx status remains retryable");

        assert_eq!(attempts.load(Ordering::SeqCst), 4);
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

        let (progress, _rx) = tokio::sync::mpsc::unbounded_channel();
        let error = runner
            .ensure_machine_image(&registry_source(reference), progress)
            .await
            .expect_err("local inspection failure does not trigger a registry pull");

        assert!(error.contains("inspect local Docker image"));
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
