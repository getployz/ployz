use std::time::Duration;

use bollard::auth::DockerCredentials;
use bollard::errors::Error as BollardError;
use ployz_core::deploy::{ImageReference, RegistryCredential};
use ployz_core::image::OciDigest;

use super::runner::DockerManagedContainerRunner;
use crate::roles::api::runner::MachineRegistryImageResolveError;

const REGISTRY_RETRY_DELAYS: [Duration; 2] = [Duration::from_millis(250), Duration::from_secs(1)];

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
    use crate::roles::api::execution::docker::test_support::{image, runner_with_responses};
    use crate::roles::api::runner::{MachineContainerRunner, MachineRegistryImageResolveError};

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
