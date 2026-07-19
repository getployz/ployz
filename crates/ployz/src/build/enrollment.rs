//! One-shot enrollment of a persistent external Build Executor.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use ployz_core::ids::SubjectToken;
use ployz_core::nats_config::{BuildExecutorCredentialExpiresAt, NatsCaCertificatePem};
use ployz_nats::connect::NatsClientUrl;
use serde::{Deserialize, Serialize};

use super::command::BuildEnrollCommand;
use super::executor_context::{
    ExecutorContextError, ExecutorContextPublication, identity_paths, load_or_create_identity,
    publish_executor_context, resolve_executor_context_root,
};
use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_support::PloyzctlExecutionOutput;

#[derive(Clone, PartialEq, Eq)]
pub struct EnrollmentUrl {
    value: String,
    loopback_http: bool,
}

impl EnrollmentUrl {
    pub fn try_new(value: impl Into<String>) -> Result<Self, EnrollmentUrlError> {
        let value = value.into();
        let uri = value
            .parse::<ureq::http::Uri>()
            .map_err(|_| EnrollmentUrlError::Invalid)?;
        let Some(scheme) = uri.scheme_str() else {
            return Err(EnrollmentUrlError::Invalid);
        };
        let Some(host) = uri.host() else {
            return Err(EnrollmentUrlError::Invalid);
        };
        if uri.path().is_empty() || uri.query().is_some() {
            return Err(EnrollmentUrlError::Invalid);
        }
        let loopback_http = scheme == "http" && matches!(host, "127.0.0.1" | "localhost" | "::1");
        if scheme != "https" && !loopback_http {
            return Err(EnrollmentUrlError::HttpsRequired);
        }
        Ok(Self {
            value,
            loopback_http,
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }

    #[must_use]
    pub const fn is_loopback_http(&self) -> bool {
        self.loopback_http
    }
}

impl fmt::Debug for EnrollmentUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EnrollmentUrl([redacted])")
    }
}

#[derive(Clone, PartialEq, Eq)]
struct EnrollmentToken(String);

impl EnrollmentToken {
    fn try_new(value: String) -> Result<Self, BuildEnrollmentError> {
        if value.is_empty() {
            return Err(BuildEnrollmentError::EmptyToken);
        }
        Ok(Self(value))
    }

    fn secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for EnrollmentToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EnrollmentToken([redacted])")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnrollmentUrlError {
    #[error("must be an absolute URL without a query string")]
    Invalid,
    #[error("must use HTTPS")]
    HttpsRequired,
}

#[derive(Debug, Serialize)]
struct EnrollmentRequest {
    pool_id: String,
    executor_id: String,
    public_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrollmentResponse {
    organization_id: String,
    pool_id: String,
    executor_id: String,
    runtime_nats_url: String,
    nats_ca_pem: String,
    credential_expires_at: PositiveDecimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
struct PositiveDecimal(u64);

impl TryFrom<String> for PositiveDecimal {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("must be a positive decimal string");
        }
        value
            .parse::<u64>()
            .ok()
            .filter(|parsed| *parsed > 0)
            .map(Self)
            .ok_or("must be a positive decimal string")
    }
}

pub(crate) async fn enroll(
    command: BuildEnrollCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, BuildEnrollmentError> {
    let BuildEnrollCommand {
        enrollment_url,
        token_env,
        pool_id,
        executor_id,
    } = command;
    if enrollment_url.is_loopback_http() && !config.build_executor.allow_insecure_enrollment {
        return Err(BuildEnrollmentError::HttpsRequired);
    }
    let token = std::env::var(&token_env)
        .map_err(|_| BuildEnrollmentError::ReadToken { name: token_env })
        .and_then(EnrollmentToken::try_new)?;
    enroll_with_token(enrollment_url, token, pool_id, executor_id, config).await
}

async fn enroll_with_token(
    enrollment_url: EnrollmentUrl,
    token: EnrollmentToken,
    pool_id: ployz_core::ids::BuildPoolId,
    executor_id: ployz_core::ids::BuildExecutorId,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, BuildEnrollmentError> {
    let root = executor_context_root(config)?;
    let paths = identity_paths(&root, &pool_id, &executor_id);
    let identity = load_or_create_identity(&paths)?;
    let request = EnrollmentRequest {
        pool_id: pool_id.as_str().to_owned(),
        executor_id: executor_id.as_str().to_owned(),
        public_key: identity.public.as_str().to_owned(),
    };
    let response = exchange(
        enrollment_url,
        token,
        request,
        config
            .build_executor
            .enrollment_timeout
            .unwrap_or(Duration::from_secs(15)),
    )
    .await?;
    let organization_id = SubjectToken::try_new(response.organization_id)
        .map_err(|_| BuildEnrollmentError::InvalidOrganization)?;
    if response.pool_id != pool_id.as_str() || response.executor_id != executor_id.as_str() {
        return Err(BuildEnrollmentError::IdentityMismatch);
    }
    let nats_url = validate_runtime_nats_url(response.runtime_nats_url)?;
    let ca_pem = NatsCaCertificatePem::try_new(response.nats_ca_pem)
        .map_err(|_| BuildEnrollmentError::InvalidCa)?;
    let context = publish_executor_context(
        &paths,
        ExecutorContextPublication {
            organization_id: &organization_id,
            pool_id: &pool_id,
            executor_id: &executor_id,
            nats_url,
            ca_pem: ca_pem.as_str(),
            credential_expires_at: BuildExecutorCredentialExpiresAt::try_new(
                response.credential_expires_at.0,
            )
            .map_err(|_| BuildEnrollmentError::InvalidResponse)?,
        },
    )?;
    Ok(PloyzctlExecutionOutput::stdout(format!(
        "enrolled Build Executor {}/{} for organization {}\ncredentials expire at {}\ncontext {}\n",
        context.pool_id.as_str(),
        context.executor_id.as_str(),
        context.organization_id.as_str(),
        context.credential_expires_at.unix_seconds(),
        paths.context.display(),
    )))
}

async fn exchange(
    enrollment_url: EnrollmentUrl,
    token: EnrollmentToken,
    request: EnrollmentRequest,
    timeout: Duration,
) -> Result<EnrollmentResponse, BuildEnrollmentError> {
    tokio::task::spawn_blocking(move || {
        let authorization = format!("Bearer {}", token.secret());
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .timeout_connect(Some(timeout.min(Duration::from_secs(5))))
            .max_redirects(0)
            .build()
            .into();
        let mut response = agent
            .post(enrollment_url.as_str())
            .header("Authorization", &authorization)
            .send_json(request)
            .map_err(|error| {
                if let ureq::Error::StatusCode(status) = error {
                    BuildEnrollmentError::Rejected { status }
                } else {
                    BuildEnrollmentError::Unavailable
                }
            })?;
        response
            .body_mut()
            .read_json::<EnrollmentResponse>()
            .map_err(|_| BuildEnrollmentError::InvalidResponse)
    })
    .await
    .map_err(|_| BuildEnrollmentError::Unavailable)?
}

fn executor_context_root(config: &PloyzctlRuntimeConfig) -> Result<PathBuf, BuildEnrollmentError> {
    resolve_executor_context_root(config.build_executor.context_root.as_deref())
        .ok_or(BuildEnrollmentError::MissingConfigRoot)
}

fn validate_runtime_nats_url(value: String) -> Result<NatsClientUrl, BuildEnrollmentError> {
    let uri = value
        .parse::<ureq::http::Uri>()
        .map_err(|_| BuildEnrollmentError::InvalidNatsUrl)?;
    let authority = uri
        .authority()
        .ok_or(BuildEnrollmentError::InvalidNatsUrl)?;
    if uri.scheme_str() != Some("tls")
        || authority.as_str().contains('@')
        || uri.query().is_some()
        || !matches!(uri.path(), "" | "/")
    {
        return Err(BuildEnrollmentError::InvalidNatsUrl);
    }
    NatsClientUrl::try_new(value).map_err(|_| BuildEnrollmentError::InvalidNatsUrl)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuildEnrollmentError {
    #[error("Build Executor enrollment requires HTTPS")]
    HttpsRequired,
    #[error("could not resolve a user config directory for Build Executor enrollment")]
    MissingConfigRoot,
    #[error(
        "environment variable {name} containing the enrollment token is not set or is not valid Unicode"
    )]
    ReadToken { name: String },
    #[error("Build Executor enrollment token must not be empty")]
    EmptyToken,
    #[error("Build Executor enrollment service is unavailable; retry the same command")]
    Unavailable,
    #[error("Build Executor enrollment was rejected with HTTP status {status}")]
    Rejected { status: u16 },
    #[error("Build Executor enrollment returned an invalid response")]
    InvalidResponse,
    #[error("Build Executor enrollment returned an invalid organization id")]
    InvalidOrganization,
    #[error("Build Executor enrollment response identity did not match the request")]
    IdentityMismatch,
    #[error("Build Executor enrollment returned an invalid NATS URL")]
    InvalidNatsUrl,
    #[error("Build Executor enrollment returned invalid NATS CA material")]
    InvalidCa,
    #[error("Build Executor context failed: {message}")]
    Context { message: String },
}

impl From<ExecutorContextError> for BuildEnrollmentError {
    fn from(error: ExecutorContextError) -> Self {
        Self::Context {
            message: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::thread;

    use ployz_core::ids::{BuildExecutorId, BuildPoolId};
    use serde_json::Value;

    use super::{BuildEnrollmentError, EnrollmentToken, EnrollmentUrl, enroll, enroll_with_token};
    use crate::build::command::BuildEnrollCommand;
    use crate::build::executor_context::{identity_paths, load_executor_context};
    use crate::dispatcher::{BuildExecutorRuntimeConfig, PloyzctlRuntimeConfig};

    const TOKEN: &str = "enrollment-secret-never-persist";
    const TOKEN_ENV: &str = "PLOYZ_TEST_BUILD_ENROLLMENT_TOKEN";
    const TEST_CA: &str = "-----BEGIN CERTIFICATE-----\ntest\n-----END CERTIFICATE-----";

    #[test]
    fn enrollment_token_rejects_empty_secret() {
        assert_eq!(
            EnrollmentToken::try_new(String::new()),
            Err(BuildEnrollmentError::EmptyToken)
        );
    }

    #[tokio::test]
    async fn enrollment_reads_the_token_environment_only_at_execution() {
        let config = PloyzctlRuntimeConfig::default();
        let error = enroll(
            BuildEnrollCommand {
                enrollment_url: EnrollmentUrl::try_new("https://cloud.example/enroll")
                    .expect("HTTPS URL"),
                token_env: "PLOYZ_TEST_TOKEN_THAT_IS_NOT_SET_630".to_owned(),
                pool_id: BuildPoolId::try_new("homelab").expect("pool id"),
                executor_id: BuildExecutorId::try_new("builder-1").expect("executor id"),
            },
            &config,
        )
        .await
        .expect_err("missing execution secret fails");
        assert_eq!(
            error,
            BuildEnrollmentError::ReadToken {
                name: "PLOYZ_TEST_TOKEN_THAT_IS_NOT_SET_630".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn retry_reuses_seed_and_publishes_only_matching_context() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("executor-contexts");
        let pool_id = BuildPoolId::try_new("homelab").expect("pool id");
        let executor_id = BuildExecutorId::try_new("builder-1").expect("executor id");
        let paths = identity_paths(&root, &pool_id, &executor_id);
        let (url, requests, server) = ambiguous_then_success_server(paths.seed.clone());
        let config = PloyzctlRuntimeConfig {
            build_executor: BuildExecutorRuntimeConfig {
                context_root: Some(root.clone()),
                allow_insecure_enrollment: true,
                enrollment_timeout: None,
            },
            nats_seed_file: Some(temporary.path().join("operator.seed")),
            cluster_context_path: Some(temporary.path().join("operator-context.json")),
            ..PloyzctlRuntimeConfig::default()
        };
        fs::write(
            config.nats_seed_file.as_ref().expect("operator seed path"),
            "operator seed must never be loaded",
        )
        .expect("operator seed fixture writes");

        let first_error = enroll_test(command(&url, &pool_id, &executor_id), &config)
            .await
            .expect_err("closed response is ambiguous");
        assert_eq!(first_error, BuildEnrollmentError::Unavailable);
        assert!(paths.seed.is_file(), "seed precedes enrollment exchange");
        assert!(
            !paths.context.exists(),
            "no context publishes without a response"
        );

        let output = enroll_test(command(&url, &pool_id, &executor_id), &config)
            .await
            .expect("same command retries successfully");
        server.join().expect("server thread exits");
        let first = requests.recv().expect("first request");
        let second = requests.recv().expect("second request");
        assert_eq!(first.public_key, second.public_key);
        assert_eq!(first.authorization, format!("Bearer {TOKEN}"));
        assert_eq!(
            first.body_keys,
            vec!["executor_id", "pool_id", "public_key"]
        );

        let connection =
            load_executor_context(&root, &pool_id, &executor_id).expect("executor context loads");
        assert_eq!(connection.nats_seed_file, paths.seed);
        assert_ne!(
            connection.nats_seed_file,
            config.nats_seed_file.expect("operator seed")
        );
        assert_eq!(
            connection.credential_expires_at.unix_seconds(),
            1_900_000_000
        );
        assert_private_material(&root, &paths.directory, &connection.nats_ca_file);
        for file in [&paths.context, &paths.seed, &connection.nats_ca_file] {
            let contents = fs::read_to_string(file).expect("material is text");
            assert!(!contents.contains(TOKEN));
        }
        assert!(!output.stdout.contains(TOKEN));
        assert!(
            !output
                .stdout
                .contains(&fs::read_to_string(&paths.seed).expect("seed reads"))
        );
        assert!(!first_error.to_string().contains(TOKEN));
    }

    #[tokio::test]
    async fn mismatched_response_never_publishes_context_or_exposes_secret() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("executor-contexts");
        let pool_id = BuildPoolId::try_new("homelab").expect("pool id");
        let executor_id = BuildExecutorId::try_new("builder-1").expect("executor id");
        let paths = identity_paths(&root, &pool_id, &executor_id);
        let (url, server) = single_response_server("other-builder");
        let config = PloyzctlRuntimeConfig {
            build_executor: BuildExecutorRuntimeConfig {
                context_root: Some(root),
                allow_insecure_enrollment: true,
                enrollment_timeout: None,
            },
            ..PloyzctlRuntimeConfig::default()
        };

        let error = enroll_test(command(&url, &pool_id, &executor_id), &config)
            .await
            .expect_err("mismatched identity fails");
        server.join().expect("server thread exits");

        assert_eq!(error, BuildEnrollmentError::IdentityMismatch);
        assert!(paths.seed.is_file());
        assert!(!paths.context.exists());
        assert!(!error.to_string().contains(TOKEN));
        assert!(!format!("{error:?}").contains(TOKEN));
    }

    #[tokio::test]
    async fn whole_request_timeout_is_retry_safe_and_redacted() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("executor-contexts");
        let pool_id = BuildPoolId::try_new("homelab").expect("pool id");
        let executor_id = BuildExecutorId::try_new("builder-1").expect("executor id");
        let (url, server) = stalled_server();
        let config = PloyzctlRuntimeConfig {
            build_executor: BuildExecutorRuntimeConfig {
                context_root: Some(root),
                allow_insecure_enrollment: true,
                enrollment_timeout: Some(std::time::Duration::from_millis(50)),
            },
            ..PloyzctlRuntimeConfig::default()
        };

        let error = enroll_test(command(&url, &pool_id, &executor_id), &config)
            .await
            .expect_err("whole request times out");
        server.join().expect("server thread exits");

        assert_eq!(error, BuildEnrollmentError::Unavailable);
        assert!(!error.to_string().contains(TOKEN));
    }

    fn command(
        url: &str,
        pool_id: &BuildPoolId,
        executor_id: &BuildExecutorId,
    ) -> BuildEnrollCommand {
        BuildEnrollCommand {
            enrollment_url: EnrollmentUrl::try_new(url).expect("loopback enrollment URL"),
            token_env: TOKEN_ENV.to_owned(),
            pool_id: pool_id.clone(),
            executor_id: executor_id.clone(),
        }
    }

    async fn enroll_test(
        command: BuildEnrollCommand,
        config: &PloyzctlRuntimeConfig,
    ) -> Result<crate::execution_support::PloyzctlExecutionOutput, BuildEnrollmentError> {
        let BuildEnrollCommand {
            enrollment_url,
            token_env: _,
            pool_id,
            executor_id,
        } = command;
        enroll_with_token(
            enrollment_url,
            EnrollmentToken::try_new(TOKEN.to_owned()).expect("non-empty token"),
            pool_id,
            executor_id,
            config,
        )
        .await
    }

    #[derive(Debug)]
    struct ObservedRequest {
        authorization: String,
        public_key: String,
        body_keys: Vec<&'static str>,
    }

    fn ambiguous_then_success_server(
        expected_seed: PathBuf,
    ) -> (
        String,
        mpsc::Receiver<ObservedRequest>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let url = format!(
            "http://{}/enroll",
            listener.local_addr().expect("server address")
        );
        let (sender, receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut first, _) = listener.accept().expect("first request accepts");
            let observed = read_request(&mut first, &expected_seed);
            sender.send(observed).expect("first observation sends");
            drop(first);

            let (mut second, _) = listener.accept().expect("second request accepts");
            let observed = read_request(&mut second, &expected_seed);
            sender.send(observed).expect("second observation sends");
            write_success_response(&mut second, "builder-1");
        });
        (url, receiver, server)
    }

    fn single_response_server(executor_id: &'static str) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let url = format!(
            "http://{}/enroll",
            listener.local_addr().expect("server address")
        );
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request accepts");
            let _ = read_http_request(&mut stream);
            write_success_response(&mut stream, executor_id);
        });
        (url, server)
    }

    fn stalled_server() -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let url = format!(
            "http://{}/enroll",
            listener.local_addr().expect("server address")
        );
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request accepts");
            let _ = read_http_request(&mut stream);
            thread::sleep(std::time::Duration::from_millis(100));
        });
        (url, server)
    }

    fn read_request(stream: &mut TcpStream, expected_seed: &Path) -> ObservedRequest {
        assert!(expected_seed.is_file(), "seed exists before HTTP request");
        let request = read_http_request(stream);
        let (headers, body) = request.split_once("\r\n\r\n").expect("request has headers");
        let authorization = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(": ")?;
                name.eq_ignore_ascii_case("authorization")
                    .then(|| value.to_owned())
            })
            .expect("authorization header");
        let json: Value = serde_json::from_str(body).expect("request JSON");
        let object = json.as_object().expect("request is object");
        let public_key = object["public_key"]
            .as_str()
            .expect("public key string")
            .to_owned();
        let mut body_keys = object
            .keys()
            .map(|key| match key.as_str() {
                "executor_id" => "executor_id",
                "pool_id" => "pool_id",
                "public_key" => "public_key",
                other => panic!("unexpected request key {other}"),
            })
            .collect::<Vec<_>>();
        body_keys.sort_unstable();
        ObservedRequest {
            authorization,
            public_key,
            body_keys,
        }
    }

    fn read_http_request(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = stream.read(&mut buffer).expect("request reads");
            assert!(read > 0, "request does not close before body");
            bytes.extend_from_slice(buffer.get(..read).expect("read fits buffer"));
            let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
                continue;
            };
            let headers =
                String::from_utf8_lossy(bytes.get(..header_end).expect("header end is in request"));
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|value| value.parse::<usize>().ok())
                })
                .expect("content length");
            if bytes.len() >= header_end + 4 + content_length {
                break;
            }
        }
        String::from_utf8(bytes).expect("request is UTF-8")
    }

    fn write_success_response(stream: &mut TcpStream, executor_id: &str) {
        let body = serde_json::json!({
            "organization_id": "org-1",
            "pool_id": "homelab",
            "executor_id": executor_id,
            "runtime_nats_url": "tls://nats.example:4222",
            "nats_ca_pem": TEST_CA,
            "credential_expires_at": "1900000000",
        })
        .to_string();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .expect("response writes");
    }

    fn assert_private_material(root: &Path, directory: &Path, ca_file: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            let pool_directory = directory.parent().expect("pool directory");
            for private_directory in [root, pool_directory, directory] {
                assert_eq!(
                    fs::metadata(private_directory)
                        .expect("directory metadata")
                        .permissions()
                        .mode()
                        & 0o777,
                    0o700
                );
            }
            for file in [
                directory.join("context.json"),
                directory.join("nkey.seed"),
                ca_file.to_owned(),
            ] {
                assert_eq!(
                    fs::metadata(&file)
                        .expect("file metadata")
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600,
                    "{} mode",
                    file.display()
                );
            }
        }
    }
}
