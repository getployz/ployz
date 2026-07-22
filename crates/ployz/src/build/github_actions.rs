//! GitHub Actions one-shot Build Executor wrapper.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Args;
use ployz_core::ids::{BuildExecutorId, BuildPoolId, OperationId};
use ployz_core::image::OciPlatform;
use ployz_core::install::{MachineJoinRuntimeNatsUrl, MachineJoinTrustedNats};
use ployz_core::nats_config::BuildExecutorCredentialExpiresAt;
use ployz_core::nats_config::{MintedNatsUser, NatsUserPublicKey};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::build::command::{BuildExecutorAdmission, BuildExecutorCommand, BuildExecutorRunMode};
use crate::build::external_runtime::WorkspaceStartup;
use crate::build::runtime::BuildExecutionError;
use crate::cloud_current_tree::{CloudTransportPolicy, validated_cloud_base_url};
use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_error::PloyzctlExecutionError;
use crate::execution_support::PloyzctlExecutionOutput;

const ACTIONS_ID_TOKEN_REQUEST_URL: &str = "ACTIONS_ID_TOKEN_REQUEST_URL";
const ACTIONS_ID_TOKEN_REQUEST_TOKEN: &str = "ACTIONS_ID_TOKEN_REQUEST_TOKEN";
const GITHUB_ACTIONS: &str = "GITHUB_ACTIONS";
const PLOYZ_BUILD_ASSIGNMENT_ID: &str = "PLOYZ_BUILD_ASSIGNMENT_ID";
const PLOYZ_CLOUD_URL: &str = "PLOYZ_CLOUD_URL";
const RUNNER_ARCH: &str = "RUNNER_ARCH";
const RUNNER_OS: &str = "RUNNER_OS";
const RUNNER_TEMP: &str = "RUNNER_TEMP";
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const EXECUTOR_WAIT_TIMEOUT: Duration = Duration::from_secs(40 * 60);
const MAX_OIDC_RESPONSE_BYTES: usize = 32 * 1024;
const MAX_EXCHANGE_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Debug, Args, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GithubActionsBuildCli {}

#[derive(Debug)]
struct RunnerInputs {
    assignment_id: String,
    cloud_url: reqwest::Url,
    oidc_request_url: reqwest::Url,
    oidc_request_token: String,
    platform: OciPlatform,
    runner_temp: PathBuf,
}

impl RunnerInputs {
    fn from_environment() -> Result<Self, GithubActionsBuildError> {
        Self::from_pairs(std::env::vars_os())
    }

    fn from_pairs(
        pairs: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Result<Self, GithubActionsBuildError> {
        let mut values = BTreeMap::new();
        for (key, value) in pairs {
            let Some(key) = key.to_str() else {
                continue;
            };
            if !REQUIRED_ENV.contains(&key) {
                continue;
            }
            let Some(value) = value.to_str() else {
                return Err(GithubActionsBuildError::InvalidRunnerEnvironment);
            };
            if values.insert(key.to_owned(), value.to_owned()).is_some() {
                return Err(GithubActionsBuildError::DuplicateRunnerVariable);
            }
        }
        let value = |name: &str| {
            values
                .get(name)
                .filter(|value| !value.is_empty())
                .cloned()
                .ok_or(GithubActionsBuildError::MissingRunnerVariable)
        };
        if value(GITHUB_ACTIONS)? != "true" || value(RUNNER_OS)? != "Linux" {
            return Err(GithubActionsBuildError::InvalidRunnerEnvironment);
        }
        let architecture = match value(RUNNER_ARCH)?.as_str() {
            "X64" => "amd64",
            "ARM64" => "arm64",
            _ => return Err(GithubActionsBuildError::UnsupportedRunnerPlatform),
        };
        let platform = OciPlatform::try_new("linux", architecture)
            .map_err(|_| GithubActionsBuildError::UnsupportedRunnerPlatform)?;
        let native = ployz_build_executor::native_oci_platform()
            .map_err(|_| GithubActionsBuildError::UnsupportedRunnerPlatform)?;
        if native != platform {
            return Err(GithubActionsBuildError::NonNativeRunnerPlatform);
        }
        let assignment_id = value(PLOYZ_BUILD_ASSIGNMENT_ID)?;
        if !is_uuid(&assignment_id) {
            return Err(GithubActionsBuildError::InvalidAssignment);
        }
        let cloud_url =
            validated_cloud_base_url(&value(PLOYZ_CLOUD_URL)?, CloudTransportPolicy::HttpsOnly)
                .map_err(|_| GithubActionsBuildError::InvalidCloudUrl)?;
        let oidc_request_url = validate_oidc_request_url(&value(ACTIONS_ID_TOKEN_REQUEST_URL)?)?;
        let runner_temp = PathBuf::from(value(RUNNER_TEMP)?);
        if !runner_temp.is_absolute() || !runner_temp.is_dir() {
            return Err(GithubActionsBuildError::InvalidRunnerTemporaryDirectory);
        }
        Ok(Self {
            assignment_id,
            cloud_url,
            oidc_request_url,
            oidc_request_token: value(ACTIONS_ID_TOKEN_REQUEST_TOKEN)?,
            platform,
            runner_temp,
        })
    }

    fn audience(&self) -> Result<String, GithubActionsBuildError> {
        cloud_endpoint(&self.cloud_url, "github-actions-build-authority").map(|url| url.to_string())
    }
}

fn cloud_endpoint(
    base: &reqwest::Url,
    relative_path: &str,
) -> Result<reqwest::Url, GithubActionsBuildError> {
    let mut base = base.clone();
    if !base.path().ends_with('/') {
        let mut path = base.path().to_owned();
        path.push('/');
        base.set_path(&path);
    }
    base.join(relative_path)
        .map_err(|_| GithubActionsBuildError::InvalidCloudUrl)
}

const REQUIRED_ENV: [&str; 8] = [
    ACTIONS_ID_TOKEN_REQUEST_URL,
    ACTIONS_ID_TOKEN_REQUEST_TOKEN,
    GITHUB_ACTIONS,
    PLOYZ_BUILD_ASSIGNMENT_ID,
    PLOYZ_CLOUD_URL,
    RUNNER_ARCH,
    RUNNER_OS,
    RUNNER_TEMP,
];

fn validate_oidc_request_url(value: &str) -> Result<reqwest::Url, GithubActionsBuildError> {
    let Some(remainder) = value.strip_prefix("https://") else {
        return Err(GithubActionsBuildError::InvalidOidcRequestUrl);
    };
    let raw_authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    if raw_authority.is_empty() {
        return Err(GithubActionsBuildError::InvalidOidcRequestUrl);
    }
    let url =
        reqwest::Url::parse(value).map_err(|_| GithubActionsBuildError::InvalidOidcRequestUrl)?;
    let Some(host) = url.host_str() else {
        return Err(GithubActionsBuildError::InvalidOidcRequestUrl);
    };
    let github_job_server = raw_authority == host
        && (matches!(
            host,
            "pipelines.actions.githubusercontent.com" | "token.actions.githubusercontent.com"
        ) || host
            .strip_suffix(".actions.githubusercontent.com")
            .and_then(|label| label.strip_prefix("run-actions-"))
            .is_some_and(|remainder| !remainder.is_empty() && !remainder.contains('.')));
    if url.scheme() != "https"
        || !github_job_server
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.cannot_be_a_base()
        || url.query_pairs().any(|(name, _)| name == "audience")
    {
        return Err(GithubActionsBuildError::InvalidOidcRequestUrl);
    }
    Ok(url)
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExchangeRequest<'a> {
    assignment_id: &'a str,
    platform: &'a OciPlatform,
    public_key: &'a str,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OidcResponse {
    value: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExchangeResponse {
    runtime_nats_url: MachineJoinRuntimeNatsUrl,
    trusted_nats: MachineJoinTrustedNats,
    pool_id: BuildPoolId,
    executor_id: BuildExecutorId,
    operation_id: OperationId,
    platform: OciPlatform,
    #[serde(deserialize_with = "super::deserialize_credential_expiry")]
    expires_at: BuildExecutorCredentialExpiresAt,
}

struct TemporaryNatsMaterial {
    _directory: tempfile::TempDir,
    ca_path: PathBuf,
    seed_path: PathBuf,
}

impl TemporaryNatsMaterial {
    fn new(root: &Path, ca_pem: &str, seed: &str) -> Result<Self, GithubActionsBuildError> {
        let directory = tempfile::Builder::new()
            .prefix("ployz-github-actions-")
            .tempdir_in(root)
            .map_err(|_| GithubActionsBuildError::CredentialMaterialUnavailable)?;
        let ca_path = directory.path().join("nats-ca.pem");
        let seed_path = directory.path().join("executor.nk");
        write_private(&ca_path, ca_pem)
            .and_then(|()| write_private(&seed_path, seed))
            .map_err(|_| GithubActionsBuildError::CredentialMaterialUnavailable)?;
        Ok(Self {
            _directory: directory,
            ca_path,
            seed_path,
        })
    }
}

struct GithubActionsClient {
    http: reqwest::Client,
    cloud_url: reqwest::Url,
}

impl GithubActionsClient {
    fn new(cloud_url: reqwest::Url) -> Result<Self, GithubActionsBuildError> {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| GithubActionsBuildError::HttpUnavailable)?;
        Ok(Self { http, cloud_url })
    }

    async fn oidc_token(
        &self,
        request_url: &reqwest::Url,
        request_token: &str,
        audience: &str,
    ) -> Result<String, GithubActionsBuildError> {
        let mut url = request_url.clone();
        url.query_pairs_mut().append_pair("audience", audience);
        let response = self
            .http
            .get(url)
            .bearer_auth(request_token)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|_| GithubActionsBuildError::OidcRequestFailed)?;
        let oidc: OidcResponse = decode_json_response(
            response,
            MAX_OIDC_RESPONSE_BYTES,
            GithubActionsBuildError::OidcRequestFailed,
        )
        .await?;
        if oidc.value.is_empty() || oidc.value.len() > 16 * 1024 {
            return Err(GithubActionsBuildError::OidcRequestFailed);
        }
        Ok(oidc.value)
    }

    async fn exchange(
        &self,
        assignment_id: &str,
        platform: &OciPlatform,
        public_key: &NatsUserPublicKey,
        oidc_token: &str,
    ) -> Result<ExchangeResponse, GithubActionsBuildError> {
        let url = cloud_endpoint(
            &self.cloud_url,
            "api/builds/github-actions-authority/exchange",
        )?;
        let response = self
            .http
            .post(url)
            .bearer_auth(oidc_token)
            .json(&ExchangeRequest {
                assignment_id,
                platform,
                public_key: public_key.as_str(),
            })
            .send()
            .await
            .map_err(|_| GithubActionsBuildError::ExchangeFailed)?;
        if !response
            .headers()
            .get(reqwest::header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.split(',').any(|part| part.trim() == "no-store"))
        {
            return Err(GithubActionsBuildError::ExchangeFailed);
        }
        decode_json_response(
            response,
            MAX_EXCHANGE_RESPONSE_BYTES,
            GithubActionsBuildError::ExchangeFailed,
        )
        .await
    }
}

async fn decode_json_response<T: DeserializeOwned>(
    mut response: reqwest::Response,
    maximum_bytes: usize,
    failure: GithubActionsBuildError,
) -> Result<T, GithubActionsBuildError> {
    if !response.status().is_success()
        || response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_none_or(|value| {
                value
                    .split(';')
                    .next()
                    .is_none_or(|media_type| media_type.trim() != "application/json")
            })
    {
        return Err(failure);
    }
    if response
        .content_length()
        .is_some_and(|length| usize::try_from(length).map_or(true, |length| length > maximum_bytes))
    {
        return Err(failure);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| failure.clone())? {
        if bytes.len().saturating_add(chunk.len()) > maximum_bytes {
            return Err(failure);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| failure)
}

pub(crate) async fn execute(
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let inputs = RunnerInputs::from_environment().map_err(execution_error)?;
    let client = GithubActionsClient::new(inputs.cloud_url.clone()).map_err(execution_error)?;
    let minted = MintedNatsUser::generate()
        .map_err(|_| execution_error(GithubActionsBuildError::CredentialMaterialUnavailable))?;
    let oidc_token = client
        .oidc_token(
            &inputs.oidc_request_url,
            &inputs.oidc_request_token,
            &inputs.audience().map_err(execution_error)?,
        )
        .await
        .map_err(execution_error)?;
    let authority = client
        .exchange(
            &inputs.assignment_id,
            &inputs.platform,
            &minted.public,
            &oidc_token,
        )
        .await
        .map_err(execution_error)?;
    if authority.platform != inputs.platform {
        return Err(execution_error(
            GithubActionsBuildError::AuthorityPlatformMismatch,
        ));
    }

    let material = TemporaryNatsMaterial::new(
        &inputs.runner_temp,
        authority.trusted_nats.ca_pem.as_str(),
        minted.seed.secret(),
    )
    .map_err(execution_error)?;

    let runtime_config = PloyzctlRuntimeConfig {
        nats_url: Some(authority.runtime_nats_url.as_str().to_owned()),
        nats_ca_file: Some(material.ca_path.clone()),
        nats_seed_file: Some(material.seed_path.clone()),
        nats_connect_timeout: config.nats_connect_timeout,
        ..PloyzctlRuntimeConfig::default()
    };
    let workspace_root = inputs.runner_temp.join("ployz-build-executor");
    let output = crate::build::external_runtime::run_controlled(
        one_shot_command(
            authority.pool_id,
            authority.executor_id,
            authority.operation_id,
            workspace_root,
        ),
        runtime_config,
        WorkspaceStartup::Recover,
        authority.expires_at,
        None,
        None,
    )
    .await?;
    Ok(output)
}

fn one_shot_command(
    pool_id: BuildPoolId,
    executor_id: BuildExecutorId,
    operation_id: OperationId,
    workspace_root: PathBuf,
) -> BuildExecutorCommand {
    BuildExecutorCommand {
        pool_id,
        executor_id,
        workspace_root: Some(workspace_root),
        admission: BuildExecutorAdmission::ExactOperation(operation_id),
        mode: BuildExecutorRunMode::Once {
            wait_timeout: EXECUTOR_WAIT_TIMEOUT,
        },
    }
}

fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    let mut file = options.open(path)?;
    file.write_all(contents.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn execution_error(error: GithubActionsBuildError) -> PloyzctlExecutionError {
    BuildExecutionError::ExecutorContext {
        message: error.to_string(),
    }
    .into()
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
enum GithubActionsBuildError {
    #[error("required GitHub Actions runner input is missing")]
    MissingRunnerVariable,
    #[error("a GitHub Actions runner input was provided more than once")]
    DuplicateRunnerVariable,
    #[error("the command is not running in a supported GitHub Actions job")]
    InvalidRunnerEnvironment,
    #[error("the GitHub Actions runner platform is unsupported")]
    UnsupportedRunnerPlatform,
    #[error("the declared GitHub Actions runner platform does not match this process")]
    NonNativeRunnerPlatform,
    #[error("the build assignment is invalid")]
    InvalidAssignment,
    #[error("the Ployz Cloud URL must use HTTPS")]
    InvalidCloudUrl,
    #[error("the GitHub Actions identity endpoint is invalid")]
    InvalidOidcRequestUrl,
    #[error("the GitHub Actions temporary directory is invalid")]
    InvalidRunnerTemporaryDirectory,
    #[error("the HTTP client is unavailable")]
    HttpUnavailable,
    #[error("GitHub Actions identity could not be obtained")]
    OidcRequestFailed,
    #[error("Build Executor authority could not be exchanged")]
    ExchangeFailed,
    #[error("Build Executor credential material could not be prepared")]
    CredentialMaterialUnavailable,
    #[error("Cloud returned authority for a different platform")]
    AuthorityPlatformMismatch,
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    async fn receive_request(
        listener: tokio::net::TcpListener,
        status: &'static str,
        headers: impl Into<String> + Send + 'static,
        body: String,
    ) -> String {
        let headers = headers.into();
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
        let request_headers = String::from_utf8_lossy(
            request
                .get(..header_end)
                .expect("header boundary is within request"),
        );
        let content_length = request_headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .unwrap_or(0);
        while request.len() < header_end + content_length {
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).await.expect("read body");
            assert_ne!(read, 0, "request ended before body");
            request.extend_from_slice(chunk.get(..read).expect("read fits buffer"));
        }
        let response = format!(
            "HTTP/1.1 {status}\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .await
            .expect("respond");
        String::from_utf8(request).expect("utf8 request")
    }

    fn valid_environment(temp: &Path) -> Vec<(OsString, OsString)> {
        let architecture = match std::env::consts::ARCH {
            "x86_64" => "X64",
            "aarch64" => "ARM64",
            other => panic!("unsupported test architecture {other}"),
        };
        [
            (GITHUB_ACTIONS, "true".to_owned()),
            (RUNNER_OS, "Linux".to_owned()),
            (RUNNER_ARCH, architecture.to_owned()),
            (PLOYZ_CLOUD_URL, "https://cloud.example".to_owned()),
            (
                PLOYZ_BUILD_ASSIGNMENT_ID,
                "00000000-0000-4000-8000-000000000501".to_owned(),
            ),
            (
                ACTIONS_ID_TOKEN_REQUEST_URL,
                "https://pipelines.actions.githubusercontent.com/abc123/_apis/distributedtask/hubs/build/plans/plan/jobs/job/idtoken?api-version=2.0"
                    .to_owned(),
            ),
            (ACTIONS_ID_TOKEN_REQUEST_TOKEN, "request-secret".to_owned()),
            (RUNNER_TEMP, temp.display().to_string()),
        ]
        .into_iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
        .collect()
    }

    #[test]
    fn runner_inputs_are_closed_and_native() {
        let temp = tempfile::tempdir().expect("tempdir");
        let inputs = RunnerInputs::from_pairs(valid_environment(temp.path())).expect("inputs");
        assert_eq!(inputs.assignment_id, "00000000-0000-4000-8000-000000000501");
        assert_eq!(
            inputs.audience().expect("audience"),
            "https://cloud.example/github-actions-build-authority"
        );
        assert_eq!(inputs.platform.os(), "linux");
    }

    #[test]
    fn cloud_authority_paths_preserve_the_validated_base_prefix() {
        let base = validated_cloud_base_url(
            "https://cloud.example/ployz/proxy",
            CloudTransportPolicy::HttpsOnly,
        )
        .expect("cloud base");

        assert_eq!(
            cloud_endpoint(&base, "github-actions-build-authority")
                .expect("audience URL")
                .as_str(),
            "https://cloud.example/ployz/proxy/github-actions-build-authority"
        );
        assert_eq!(
            cloud_endpoint(&base, "api/builds/github-actions-authority/exchange")
                .expect("exchange URL")
                .as_str(),
            "https://cloud.example/ployz/proxy/api/builds/github-actions-authority/exchange"
        );
    }

    #[test]
    fn runner_requires_the_exact_assignment_environment_name() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut environment = valid_environment(temp.path());
        environment.retain(|(key, _)| key != PLOYZ_BUILD_ASSIGNMENT_ID);
        environment.push((
            OsString::from("PLOYZ_BUILD_ATTEMPT_ID"),
            OsString::from("00000000-0000-4000-8000-000000000501"),
        ));

        assert_eq!(
            RunnerInputs::from_pairs(environment).expect_err("wrong variable is ignored"),
            GithubActionsBuildError::MissingRunnerVariable
        );
    }

    #[test]
    fn missing_and_duplicate_runner_inputs_are_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut missing = valid_environment(temp.path());
        missing.retain(|(key, _)| key != RUNNER_OS);
        assert_eq!(
            RunnerInputs::from_pairs(missing).expect_err("missing"),
            GithubActionsBuildError::MissingRunnerVariable
        );

        let mut duplicate = valid_environment(temp.path());
        duplicate.push((OsString::from(RUNNER_OS), OsString::from("Linux")));
        assert_eq!(
            RunnerInputs::from_pairs(duplicate).expect_err("duplicate"),
            GithubActionsBuildError::DuplicateRunnerVariable
        );
    }

    #[test]
    fn urls_and_assignment_are_validated_before_secrets_are_used() {
        let temp = tempfile::tempdir().expect("tempdir");
        for (name, value, expected) in [
            (
                PLOYZ_CLOUD_URL,
                "http://cloud.example",
                GithubActionsBuildError::InvalidCloudUrl,
            ),
            (
                ACTIONS_ID_TOKEN_REQUEST_URL,
                "https://evil.example/token",
                GithubActionsBuildError::InvalidOidcRequestUrl,
            ),
            (
                ACTIONS_ID_TOKEN_REQUEST_URL,
                "https://token.actions.githubusercontent.com/token?audience=wrong",
                GithubActionsBuildError::InvalidOidcRequestUrl,
            ),
            (
                PLOYZ_BUILD_ASSIGNMENT_ID,
                "not-a-uuid",
                GithubActionsBuildError::InvalidAssignment,
            ),
        ] {
            let mut environment = valid_environment(temp.path());
            environment
                .iter_mut()
                .find(|(key, _)| key == name)
                .expect("variable")
                .1 = OsString::from(value);
            assert_eq!(
                RunnerInputs::from_pairs(environment).expect_err("invalid"),
                expected
            );
        }
    }

    #[test]
    fn oidc_request_url_accepts_only_explicit_github_job_server_hosts() {
        assert!(validate_oidc_request_url(
            "https://pipelines.actions.githubusercontent.com/abc123/_apis/distributedtask/hubs/build/plans/plan/jobs/job/idtoken?api-version=2.0"
        ).is_ok());
        assert!(
            validate_oidc_request_url("https://token.actions.githubusercontent.com/token").is_ok()
        );
        assert!(
            validate_oidc_request_url(
                "https://run-actions-3-azure-eastus.actions.githubusercontent.com/token"
            )
            .is_ok()
        );
        for malicious in [
            "https://pipelines.actions.githubusercontent.com.evil.example/token",
            "https://evil-pipelines.actions.githubusercontent.com/token",
            "https://attacker@pipelines.actions.githubusercontent.com/token",
            "https://pipelines.actions.githubusercontent.com:444/token",
            "http://pipelines.actions.githubusercontent.com/token",
            "https://run-actions-.actions.githubusercontent.com/token",
            "https://nested.run-actions-3-azure-eastus.actions.githubusercontent.com/token",
            "https://run-actions-3-azure-eastus.nested.actions.githubusercontent.com/token",
            "https://run-actions-3-azure-eastus.actions.githubusercontent.com.evil.example/token",
            "https://attacker@run-actions-3-azure-eastus.actions.githubusercontent.com/token",
            "https://run-actions-3-azure-eastus.actions.githubusercontent.com:444/token",
            "https://run-actions-3-azure-eastus.actions.githubusercontent.com:443/token",
            "https://@run-actions-3-azure-eastus.actions.githubusercontent.com/token",
            "https:\\\\run-actions-3-azure-eastus.actions.githubusercontent.com:443/token",
            "https:///run-actions-3-azure-eastus.actions.githubusercontent.com:443/token",
            "http://run-actions-3-azure-eastus.actions.githubusercontent.com/token",
            "https://run-actions-3-azure-eastus.actions.githubusercontent.com/token?audience=wrong",
        ] {
            assert_eq!(
                validate_oidc_request_url(malicious).expect_err("malicious URL rejected"),
                GithubActionsBuildError::InvalidOidcRequestUrl
            );
        }
    }

    #[test]
    fn unsupported_and_non_native_runner_platforms_are_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut unsupported = valid_environment(temp.path());
        unsupported
            .iter_mut()
            .find(|(key, _)| key == RUNNER_ARCH)
            .expect("runner architecture")
            .1 = OsString::from("RISCV64");
        assert_eq!(
            RunnerInputs::from_pairs(unsupported).expect_err("unsupported"),
            GithubActionsBuildError::UnsupportedRunnerPlatform
        );

        let mut non_native = valid_environment(temp.path());
        non_native
            .iter_mut()
            .find(|(key, _)| key == RUNNER_ARCH)
            .expect("runner architecture")
            .1 = OsString::from(match std::env::consts::ARCH {
            "x86_64" => "ARM64",
            "aarch64" => "X64",
            other => panic!("unsupported test architecture {other}"),
        });
        assert_eq!(
            RunnerInputs::from_pairs(non_native).expect_err("non-native"),
            GithubActionsBuildError::NonNativeRunnerPlatform
        );
    }

    #[cfg(unix)]
    #[test]
    fn credential_material_is_always_private() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("executor.nk");
        write_private(&path, "seed-secret").expect("write");
        assert_eq!(
            std::fs::metadata(path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn credential_material_is_removed_on_every_scope_exit() {
        let root = tempfile::tempdir().expect("tempdir");
        let directory = {
            let material = TemporaryNatsMaterial::new(root.path(), "trusted-ca", "seed-secret")
                .expect("material");
            let directory = material
                .ca_path
                .parent()
                .expect("material directory")
                .to_path_buf();
            assert!(directory.exists());
            directory
        };
        assert!(!directory.exists());
    }

    #[test]
    fn wrapper_invokes_the_existing_bounded_once_runtime() {
        let workspace = PathBuf::from("/tmp/ployz-build-executor-test");
        let operation_id = OperationId::try_new("op_expected").expect("operation id");
        let command = one_shot_command(
            BuildPoolId::try_new("hosted-builders").expect("pool id"),
            BuildExecutorId::try_new("gha-executor1").expect("executor id"),
            operation_id.clone(),
            workspace.clone(),
        );
        assert_eq!(command.workspace_root, Some(workspace));
        assert_eq!(
            command.admission,
            BuildExecutorAdmission::ExactOperation(operation_id)
        );
        assert_eq!(
            command.mode,
            BuildExecutorRunMode::Once {
                wait_timeout: EXECUTOR_WAIT_TIMEOUT
            }
        );
    }

    #[tokio::test]
    async fn exchange_sends_only_public_identity_and_accepts_bounded_no_store_json() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listen");
        let address = listener.local_addr().expect("address");
        let minted = MintedNatsUser::generate().expect("NKey");
        let public_key = minted.public.clone();
        let response = format!(
            "{{\"runtime_nats_url\":\"tls://core.example:4222\",\"trusted_nats\":{{\"ca_pem\":\"-----BEGIN CERTIFICATE-----\\nTUlJQg==\\n-----END CERTIFICATE-----\\n\"}},\"pool_id\":\"hosted-builders\",\"executor_id\":\"gha-executor1\",\"operation_id\":\"op_exact_assignment\",\"platform\":{{\"os\":\"linux\",\"architecture\":\"{}\"}},\"expires_at\":\"2026-07-20T12:00:00Z\"}}",
            match std::env::consts::ARCH {
                "x86_64" => "amd64",
                "aarch64" => "arm64",
                other => panic!("unsupported test architecture {other}"),
            }
        );
        let request = tokio::spawn(receive_request(
            listener,
            "200 OK",
            "content-type: application/json\r\ncache-control: no-store\r\n",
            response,
        ));
        let platform = ployz_build_executor::native_oci_platform().expect("native platform");
        let client = GithubActionsClient::new(
            reqwest::Url::parse(&format!("http://{address}/proxy")).expect("test URL"),
        )
        .expect("client");

        let authority = client
            .exchange(
                "00000000-0000-4000-8000-000000000501",
                &platform,
                &public_key,
                "oidc-secret",
            )
            .await
            .expect("exchange");
        assert_eq!(authority.platform, platform);
        assert_eq!(authority.operation_id.as_str(), "op_exact_assignment");
        assert_eq!(authority.expires_at.unix_seconds(), 1_784_548_800);
        let request = request.await.expect("server");
        assert!(request.starts_with("POST /proxy/api/builds/github-actions-authority/exchange "));
        assert!(request.contains("authorization: Bearer oidc-secret\r\n"));
        assert!(request.contains(public_key.as_str()));
        assert!(!request.contains(minted.seed.secret()));
        assert!(!request.contains("seed"));
        let body = request
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .expect("HTTP request body");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(body).expect("exchange request JSON"),
            serde_json::json!({
                "assignmentId": "00000000-0000-4000-8000-000000000501",
                "platform": {
                    "os": "linux",
                    "architecture": match std::env::consts::ARCH {
                        "x86_64" => "amd64",
                        "aarch64" => "arm64",
                        other => panic!("unsupported test architecture {other}"),
                    },
                },
                "publicKey": public_key.as_str(),
            })
        );
    }

    #[tokio::test]
    async fn oidc_request_uses_the_exact_audience_and_a_bounded_json_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listen");
        let address = listener.local_addr().expect("address");
        let request = tokio::spawn(receive_request(
            listener,
            "200 OK",
            "content-type: application/json\r\n",
            "{\"value\":\"header.payload.signature\"}".to_owned(),
        ));
        let client = GithubActionsClient::new(
            reqwest::Url::parse("https://cloud.example").expect("cloud URL"),
        )
        .expect("client");
        let request_url =
            reqwest::Url::parse(&format!("http://{address}/oidc?api-version=1")).expect("test URL");

        assert_eq!(
            client
                .oidc_token(
                    &request_url,
                    "request-secret",
                    "https://cloud.example/github-actions-build-authority",
                )
                .await
                .expect("OIDC token"),
            "header.payload.signature"
        );
        let request = request.await.expect("server");
        assert!(request.contains("authorization: Bearer request-secret\r\n"));
        assert!(
            request
                .contains("audience=https%3A%2F%2Fcloud.example%2Fgithub-actions-build-authority")
        );
        assert_eq!(request.matches("audience=").count(), 1);
    }

    #[tokio::test]
    async fn oidc_rejects_status_content_type_and_oversized_bodies() {
        for (status, headers, body) in [
            (
                "403 Forbidden",
                "content-type: application/json\r\n",
                "{}".to_owned(),
            ),
            ("200 OK", "content-type: text/plain\r\n", "{}".to_owned()),
            (
                "200 OK",
                "content-type: application/json\r\n",
                "x".repeat(MAX_OIDC_RESPONSE_BYTES + 1),
            ),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("listen");
            let address = listener.local_addr().expect("address");
            let server = tokio::spawn(receive_request(listener, status, headers, body));
            let client = GithubActionsClient::new(
                reqwest::Url::parse("https://cloud.example").expect("cloud URL"),
            )
            .expect("client");
            assert_eq!(
                client
                    .oidc_token(
                        &reqwest::Url::parse(&format!("http://{address}/oidc")).expect("test URL"),
                        "request-secret",
                        "https://cloud.example/github-actions-build-authority",
                    )
                    .await
                    .expect_err("response rejected"),
                GithubActionsBuildError::OidcRequestFailed
            );
            server.await.expect("server");
        }
    }

    #[tokio::test]
    async fn exchange_rejects_status_content_type_cache_policy_and_oversized_bodies() {
        let platform = ployz_build_executor::native_oci_platform().expect("native platform");
        let public_key = MintedNatsUser::generate().expect("NKey").public;
        for (status, headers, body) in [
            (
                "503 Service Unavailable",
                "content-type: application/json\r\ncache-control: no-store\r\n",
                "{}".to_owned(),
            ),
            (
                "200 OK",
                "content-type: text/plain\r\ncache-control: no-store\r\n",
                "{}".to_owned(),
            ),
            (
                "200 OK",
                "content-type: application/json\r\ncache-control: private\r\n",
                "{}".to_owned(),
            ),
            (
                "200 OK",
                "content-type: application/json\r\ncache-control: no-store\r\n",
                "x".repeat(MAX_EXCHANGE_RESPONSE_BYTES + 1),
            ),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("listen");
            let address = listener.local_addr().expect("address");
            let server = tokio::spawn(receive_request(listener, status, headers, body));
            let client = GithubActionsClient::new(
                reqwest::Url::parse(&format!("http://{address}")).expect("test URL"),
            )
            .expect("client");
            assert_eq!(
                client
                    .exchange(
                        "00000000-0000-4000-8000-000000000501",
                        &platform,
                        &public_key,
                        "oidc-secret",
                    )
                    .await
                    .expect_err("response rejected"),
                GithubActionsBuildError::ExchangeFailed
            );
            server.await.expect("server");
        }
    }

    #[tokio::test]
    async fn authority_client_never_follows_redirects() {
        let redirect_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("redirect listener");
        let redirect_address = redirect_listener.local_addr().expect("redirect address");
        let target_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("target listener");
        let target_address = target_listener.local_addr().expect("target address");
        let redirect = tokio::spawn(receive_request(
            redirect_listener,
            "302 Found",
            format!("location: http://{target_address}/stolen\r\n"),
            String::new(),
        ));
        let platform = ployz_build_executor::native_oci_platform().expect("native platform");
        let public_key = MintedNatsUser::generate().expect("NKey").public;
        let client = GithubActionsClient::new(
            reqwest::Url::parse(&format!("http://{redirect_address}")).expect("test URL"),
        )
        .expect("client");

        assert_eq!(
            client
                .exchange(
                    "00000000-0000-4000-8000-000000000501",
                    &platform,
                    &public_key,
                    "oidc-secret",
                )
                .await
                .expect_err("redirect is not followed"),
            GithubActionsBuildError::ExchangeFailed
        );
        redirect.await.expect("redirect server");
        assert!(
            tokio::time::timeout(Duration::from_millis(200), target_listener.accept())
                .await
                .is_err(),
            "redirect target must not receive the authority bearer token"
        );
    }

    #[test]
    fn failures_never_render_secret_material() {
        let rendered = [
            GithubActionsBuildError::OidcRequestFailed,
            GithubActionsBuildError::ExchangeFailed,
            GithubActionsBuildError::CredentialMaterialUnavailable,
        ]
        .map(|error| error.to_string())
        .join(" ");
        assert!(!rendered.contains("request-secret"));
        assert!(!rendered.contains("seed-secret"));
        assert!(!rendered.contains("trusted-ca"));
    }
}
