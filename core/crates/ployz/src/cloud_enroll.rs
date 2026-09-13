//! Cloud enroll HTTP: POST identity, consume `initialize` / `join`, enrollment callback.

use std::{net::IpAddr, time::Duration};

use ployz_core::{
    AdvertisedEndpoint, CloudEnrollToken, CloudPairing, MachineId, MachineName, MachineToken,
    PairingCredential, Registered, StorageChoice, TailcatCapability, WireGuardPublicKey,
};
use serde::{Deserialize, Serialize, Serializer};
use thiserror::Error;

const DEFAULT_RETRY_AFTER: u64 = 2;
const PROTOCOL_VERSION: u8 = 2;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Failures talking to Cloud enroll.
#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("enroll timed out waiting for Cloud: {}", crate::setup_retry::detail(.0))]
    Timeout(#[source] reqwest::Error),
    #[error("could not connect to Cloud: {}", crate::setup_retry::detail(.0))]
    Connect(#[source] reqwest::Error),
    #[error("{}", crate::setup_retry::detail(.0))]
    Http(#[source] reqwest::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("enroll HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error(
        "Cloud {operation} failed: {detail}; rerun the same ployz cloud enroll command without --reset (keep all other options)"
    )]
    RetrySameCommand {
        operation: &'static str,
        detail: String,
    },
}

impl Error {
    fn is_transport(&self) -> bool {
        match self {
            Self::Timeout(error) | Self::Connect(error) | Self::Http(error) => {
                crate::setup_retry::transient_http(error)
            }
            Self::Json(_) | Self::Status { .. } | Self::RetrySameCommand { .. } => false,
        }
    }
}

/// Identity POSTed to `POST /api/enroll/<token>`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EnrollIdentity {
    machine_id: MachineId,
    protocol_version: u8,
    name: MachineName,
    // Cloud enroll HTTP expects Display/base64, not the RPC `number[]` wire.
    #[serde(serialize_with = "serialize_as_wireguard_base64")]
    public_key: WireGuardPublicKey,
    advertised_endpoints: Vec<AdvertisedEndpoint>,
    public_ip: Option<IpAddr>,
    requested_storage: StorageChoice,
    initial_policy: ployz_core::InitialMachinePolicy,
    memory_total_bytes: Option<u64>,
    disk_total_bytes: Option<u64>,
    disk_available_bytes: Option<u64>,
}

fn serialize_as_wireguard_base64<S>(
    key: &WireGuardPublicKey,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&key.to_string())
}

impl EnrollIdentity {
    pub(crate) fn from_machine_token(
        name: MachineName,
        token: &MachineToken,
        requested_storage: StorageChoice,
        initial_policy: ployz_core::InitialMachinePolicy,
    ) -> Self {
        Self {
            machine_id: token.id,
            protocol_version: PROTOCOL_VERSION,
            name,
            public_key: token.public_key,
            advertised_endpoints: token.advertised_endpoints.clone(),
            public_ip: token.public_ip,
            requested_storage,
            initial_policy,
            memory_total_bytes: token.memory_total_bytes,
            disk_total_bytes: token.disk_total_bytes,
            disk_available_bytes: token.disk_available_bytes,
        }
    }
}

/// Successful enroll `join` payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Join {
    pub pairing: CloudPairing,
    pub storage: StorageChoice,
    pub registration: Registered,
}

/// Enroll outcome after `not_yet` retries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Outcome {
    Join(Box<Join>),
    Initialize {
        mode: InitializeMode,
        pairing: CloudPairing,
        storage: StorageChoice,
    },
}

/// Whether Cloud granted a new Founding Claim or resumed the current one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InitializeMode {
    New,
    Resume,
}

/// One enroll POST body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Response {
    Join(Box<Join>),
    Initialize {
        mode: InitializeMode,
        pairing: CloudPairing,
        storage: StorageChoice,
    },
    NotYet {
        retry_after: Duration,
    },
}

/// Enrollment responses admit only the declared wire fields.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum EnrollWire {
    Join {
        pairing: CloudPairing,
        #[serde(default)]
        storage: StorageChoice,
        registration: Box<Registered>,
    },
    NotYet {
        #[serde(default, rename = "retryAfter")]
        retry_after: Option<u64>,
    },
    Initialize {
        resumed: bool,
        pairing: CloudPairing,
        #[serde(default)]
        storage: StorageChoice,
    },
}

/// Enroll URL: host without a scheme is HTTPS.
#[must_use]
pub(crate) fn enroll_url(cloud_url: &str, token: &CloudEnrollToken) -> String {
    format!("{}/api/enroll/{}", cloud_origin(cloud_url), token.as_str())
}

/// Final enrollment completion: `POST /api/enroll/<token>/callback`.
#[must_use]
pub(crate) fn callback_url(cloud_url: &str, token: &CloudEnrollToken) -> String {
    format!("{}/callback", enroll_url(cloud_url, token))
}

fn cloud_origin(cloud_url: &str) -> String {
    let host = cloud_url.trim().trim_end_matches('/');
    if host.contains("://") {
        host.to_owned()
    } else {
        format!("https://{host}")
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EnrollCallback<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    stage: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tailcat: Option<&'a TailcatCapability>,
    machine_id: MachineId,
    pairing_credential: &'a PairingCredential,
}

/// POST identity until Cloud returns `initialize` or `join`.
///
/// Only pre-dispatch connection failures and explicit `not_yet` responses retry.
/// A lost response may follow Register dispatch; the caller must explicitly
/// resume the same saved enrollment attempt instead of replaying it here.
///
/// # Errors
///
/// HTTP status, unexpected JSON, or a non-transport HTTP failure.
pub(crate) async fn enroll(url: &str, identity: &EnrollIdentity) -> Result<Outcome, Error> {
    let http = http_client()?;
    let mut announced_wait = false;
    loop {
        let response = crate::setup_retry::run(
            &mut (),
            &format!("Cloud enrollment at {}", diagnostic_origin(url)),
            crate::setup_retry::WAIT,
            |error| matches!(error, Error::Connect(_)),
            async |_| post_json(&http, url, identity).await,
        )
        .await
        .map_err(|error| retry_error("enrollment", error))?;
        match parse_enroll(&response)? {
            Response::Join(join) => return Ok(Outcome::Join(join)),
            Response::Initialize {
                mode,
                pairing,
                storage,
            } => {
                return Ok(Outcome::Initialize {
                    mode,
                    pairing,
                    storage,
                });
            }
            Response::NotYet { retry_after } => {
                if !announced_wait {
                    eprintln!("Another Machine is founding this Organization; waiting...");
                    announced_wait = true;
                }
                tokio::time::sleep(retry_after).await;
            }
        }
    }
}

/// Request enrollment completion after publishing its protected connection candidate.
///
/// # Errors
///
/// HTTP failure.
pub(crate) async fn callback(
    url: &str,
    machine_id: MachineId,
    pairing_credential: &PairingCredential,
) -> Result<(), Error> {
    post_callback(url, machine_id, pairing_credential, None).await
}

/// Publish a candidate against the authenticated enrollment attempt before completion.
pub(crate) async fn publish(
    url: &str,
    machine_id: MachineId,
    pairing_credential: &PairingCredential,
    tailcat: &TailcatCapability,
) -> Result<(), Error> {
    post_callback(url, machine_id, pairing_credential, Some(tailcat)).await
}

async fn post_callback(
    url: &str,
    machine_id: MachineId,
    pairing_credential: &PairingCredential,
    tailcat: Option<&TailcatCapability>,
) -> Result<(), Error> {
    let http = http_client()?;
    let body = EnrollCallback {
        stage: tailcat.map(|_| "publish"),
        tailcat,
        machine_id,
        pairing_credential,
    };
    let operation = if tailcat.is_some() {
        "candidate publication"
    } else {
        "enrollment completion"
    };
    crate::setup_retry::run(
        &mut (),
        &format!("Cloud {operation} at {}", diagnostic_origin(url)),
        crate::setup_retry::WAIT,
        Error::is_transport,
        async |_| {
            post_json(&http, url, &body)
                .await
                .map(|_| ())
                .map_err(|error| {
                    // Both stages carry credentials; a server may echo them in any encoding.
                    if let Error::Status { status, .. } = error {
                        Error::Status {
                            status,
                            body: format!("{operation} rejected"),
                        }
                    } else {
                        error
                    }
                })
        },
    )
    .await
    .map_err(|error| Error::RetrySameCommand {
        operation,
        detail: error.to_string(),
    })
}

fn retry_error(operation: &'static str, error: crate::setup_retry::Error<Error>) -> Error {
    match error {
        crate::setup_retry::Error::Permanent(error) => {
            if matches!(error, Error::Timeout(_) | Error::Http(_)) {
                Error::RetrySameCommand {
                    operation,
                    detail: format!("response lost; enrollment outcome may be uncertain: {error}"),
                }
            } else {
                error
            }
        }
        crate::setup_retry::Error::Exhausted(detail) => {
            Error::RetrySameCommand { operation, detail }
        }
    }
}

fn http_client() -> Result<reqwest::Client, Error> {
    // Bound each request; `not_yet` polling may legitimately outlive one request.
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(classify_http)
}

fn diagnostic_origin(url: &str) -> String {
    reqwest::Url::parse(url).map_or_else(
        |_| "invalid Cloud URL".to_owned(),
        |url| url.origin().ascii_serialization(),
    )
}

fn classify_http(error: reqwest::Error) -> Error {
    let error = error.without_url();
    if error.is_connect() {
        Error::Connect(error)
    } else if error.is_timeout() {
        Error::Timeout(error)
    } else {
        Error::Http(error)
    }
}

async fn post_json(
    http: &reqwest::Client,
    url: &str,
    body: &impl Serialize,
) -> Result<Vec<u8>, Error> {
    let sent = http
        .post(url)
        .json(body)
        .send()
        .await
        .map_err(classify_http)?;
    let status = sent.status();
    let bytes = sent.bytes().await.map_err(classify_http)?;
    if !status.is_success() {
        return Err(Error::Status {
            status: status.as_u16(),
            body: String::from_utf8_lossy(&bytes).into_owned(),
        });
    }
    Ok(bytes.to_vec())
}

fn parse_enroll(bytes: &[u8]) -> Result<Response, Error> {
    match serde_json::from_slice::<EnrollWire>(bytes)? {
        EnrollWire::Join {
            pairing,
            storage,
            registration,
        } => Ok(Response::Join(Box::new(Join {
            pairing,
            storage,
            registration: *registration,
        }))),
        EnrollWire::NotYet { retry_after } => Ok(Response::NotYet {
            retry_after: Duration::from_secs(retry_after.unwrap_or(DEFAULT_RETRY_AFTER)),
        }),
        EnrollWire::Initialize {
            resumed,
            pairing,
            storage,
        } => Ok(Response::Initialize {
            mode: if resumed {
                InitializeMode::Resume
            } else {
                InitializeMode::New
            },
            pairing,
            storage,
        }),
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::{Machine, MachineId, MachineName, PairingCredential, Registered};

    use super::*;

    fn pairing() -> CloudPairing {
        CloudPairing::new(PairingCredential::parse("pairing-secret").unwrap())
    }

    fn registration() -> Registered {
        Registered {
            assigned_machine: Machine {
                labels: Default::default(),
                accepts_builds: true,
                accepts_services: true,
                accepts_ingress: true,
                id: MachineId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
                name: MachineName::parse("joiner").unwrap(),
                subnet: "10.210.1.0/24".parse().unwrap(),
                public_key: WireGuardPublicKey([1; 32]),
                public_ip: None,
                advertised_endpoints: Vec::new(),
                runtime: Default::default(),
            },
            visible_peers: Vec::new(),
            target_versions: Default::default(),
        }
    }

    fn token() -> CloudEnrollToken {
        CloudEnrollToken::parse("pmet_test").unwrap()
    }

    #[test]
    fn host_without_scheme_is_https_under_ployz_dev() {
        assert_eq!(
            enroll_url("ployz.dev", &token()),
            "https://ployz.dev/api/enroll/pmet_test"
        );
        assert_eq!(
            enroll_url("http://127.0.0.1:9", &token()),
            "http://127.0.0.1:9/api/enroll/pmet_test"
        );
        assert_eq!(
            callback_url("ployz.dev", &token()),
            "https://ployz.dev/api/enroll/pmet_test/callback"
        );
    }

    #[test]
    fn join_payload_is_pairing_plus_registration() {
        let value = serde_json::json!({
            "kind": "join",
            "storage": "zfs",
            "pairing": {
                "secret": "pairing-secret",
            },
            "registration": registration(),
        });
        let Response::Join(join) =
            parse_enroll(serde_json::to_vec(&value).unwrap().as_slice()).unwrap()
        else {
            panic!("expected join");
        };
        assert_eq!(join.pairing, pairing());
        assert_eq!(join.storage, ployz_core::StorageChoice::Zfs);
        assert_eq!(join.registration, registration());
    }

    #[test]
    fn pairing_with_an_unknown_field_is_rejected() {
        let value = serde_json::json!({
            "kind": "join",
            "storage": "none",
            "pairing": {
                "secret": "pairing-secret",
                "unexpectedCredential": "unexpected-value",
            },
            "registration": registration(),
        });
        let error = parse_enroll(serde_json::to_vec(&value).unwrap().as_slice()).unwrap_err();
        assert!(error.to_string().contains("unknown field"), "{error}");
    }

    #[test]
    fn top_level_unknown_field_is_rejected() {
        let value = serde_json::json!({
            "kind": "join",
            "storage": "none",
            "pairing": {
                "secret": "pairing-secret",
            },
            "registration": registration(),
            "unexpectedCredential": "unexpected-value",
        });
        let error = parse_enroll(serde_json::to_vec(&value).unwrap().as_slice()).unwrap_err();
        assert!(error.to_string().contains("unknown field"), "{error}");
    }

    #[test]
    fn enrollment_rejects_empty_pairing_credential() {
        let error = parse_enroll(
            br#"{"kind":"initialize","resumed":false,"storage":"none","pairing":{"secret":""}}"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("Pairing Credential"), "{error}");
    }

    #[test]
    fn initialize_payload_is_cloud_pairing() {
        let value = serde_json::json!({
            "kind": "initialize",
            "resumed": false,
            "storage": "none",
            "pairing": {
                "secret": "pairing-secret",
            },
        });
        let Response::Initialize {
            mode, pairing: got, ..
        } = parse_enroll(serde_json::to_vec(&value).unwrap().as_slice()).unwrap()
        else {
            panic!("expected initialize");
        };
        assert_eq!(mode, InitializeMode::New);
        assert_eq!(got, pairing());
    }

    #[test]
    fn initialize_requires_the_protocol_two_resume_directive() {
        let error = parse_enroll(br#"{"kind":"initialize","pairing":{"secret":"pairing-secret"}}"#)
            .unwrap_err();
        assert!(error.to_string().contains("resumed"), "{error}");
    }

    #[test]
    fn initialize_pairing_with_an_unknown_field_is_rejected() {
        let value = serde_json::json!({
            "kind": "initialize",
            "resumed": false,
            "storage": "none",
            "pairing": {
                "secret": "pairing-secret",
                "unexpectedCredential": "unexpected-value",
            },
        });
        let error = parse_enroll(serde_json::to_vec(&value).unwrap().as_slice()).unwrap_err();
        assert!(error.to_string().contains("unknown field"), "{error}");
    }

    #[test]
    fn enrollment_defaults_missing_storage_to_none() {
        let Response::Initialize { storage, .. } = parse_enroll(
            br#"{"kind":"initialize","resumed":false,"pairing":{"secret":"pairing-secret"}}"#,
        )
        .unwrap() else {
            panic!("expected initialize");
        };
        assert_eq!(storage, StorageChoice::None);
    }

    #[test]
    fn not_yet_defaults_retry_after_to_two_seconds() {
        let Response::NotYet { retry_after } = parse_enroll(br#"{"kind":"not_yet"}"#).unwrap()
        else {
            panic!("expected not_yet");
        };
        assert_eq!(retry_after, Duration::from_secs(2));
    }

    #[test]
    fn not_yet_honors_retry_after_seconds() {
        let Response::NotYet { retry_after } =
            parse_enroll(br#"{"kind":"not_yet","retryAfter":5}"#).unwrap()
        else {
            panic!("expected not_yet");
        };
        assert_eq!(retry_after, Duration::from_secs(5));
    }

    #[test]
    fn enrollment_rejects_unknown_fields() {
        for payload in [
            br#"{"kind":"not_yet","retryAfter":5,"futureHint":"cloud-defined"}"#.as_slice(),
            br#"{"kind":"initialize","resumed":false,"storage":"none","pairing":{"secret":"pairing-secret"},"issuedAt":"2026-08-19T22:58:13.733Z"}"#.as_slice(),
        ] {
            let error = parse_enroll(payload).unwrap_err();
            assert!(error.to_string().contains("unknown field"), "{error}");
        }
    }

    fn identity() -> EnrollIdentity {
        EnrollIdentity::from_machine_token(
            MachineName::parse("joiner").unwrap(),
            &MachineToken {
                id: MachineId::random(),
                public_key: WireGuardPublicKey([1; 32]),
                public_ip: None,
                advertised_endpoints: Vec::new(),
                runtime: Default::default(),
                memory_total_bytes: None,
                disk_total_bytes: None,
                disk_available_bytes: None,
            },
            StorageChoice::None,
            Default::default(),
        )
    }

    fn join_body() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "kind": "join",
            "storage": "none",
            "pairing": {
                "secret": "pairing-secret",
            },
            "registration": registration(),
        }))
        .unwrap()
    }

    fn http_response(status: u16, reason: &str, body: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        out
    }

    async fn listen() -> (tokio::net::TcpListener, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        (listener, url)
    }

    async fn read_http(stream: &mut tokio::net::TcpStream) {
        let mut buf = vec![0; 8192];
        let _ = tokio::io::AsyncReadExt::read(stream, &mut buf).await;
    }

    #[test]
    fn cloud_destination_omits_credentials_and_token_path() {
        assert_eq!(
            diagnostic_origin(
                "https://user:secret@cloud.example:8443/api/enroll/private-token?secret=yes"
            ),
            "https://cloud.example:8443"
        );
    }

    #[tokio::test]
    async fn timeout_is_distinct_from_connection_failure() {
        let (listener, hang) = listen().await;
        tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(1))
            .read_timeout(Duration::from_millis(100))
            .build()
            .unwrap();
        let timeout = post_json(&http, &hang, &identity()).await.unwrap_err();
        assert!(
            timeout
                .to_string()
                .starts_with("enroll timed out waiting for Cloud:")
        );
        assert!(!timeout.to_string().contains(&hang));

        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let refused = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let connect = post_json(&http, &refused, &identity()).await.unwrap_err();
        assert!(
            connect
                .to_string()
                .starts_with("could not connect to Cloud:")
        );
        assert_ne!(timeout.to_string(), connect.to_string());
        assert!(
            connect.to_string().contains("Connection refused"),
            "{connect}"
        );
        assert!(!connect.to_string().contains(&refused));
    }

    #[tokio::test]
    async fn enrollment_never_reposts_after_losing_a_dispatched_response() {
        for timeout in [false, true] {
            let (listener, url) = listen().await;
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                read_http(&mut stream).await;
                if timeout {
                    tokio::time::pause();
                    tokio::time::advance(REQUEST_TIMEOUT).await;
                    tokio::time::resume();
                }
                drop(stream);
                assert!(
                    tokio::time::timeout(Duration::from_millis(200), listener.accept())
                        .await
                        .is_err()
                );
            });
            let error = tokio::time::timeout(Duration::from_secs(25), enroll(&url, &identity()))
                .await
                .expect("lost response must return without retrying")
                .unwrap_err();
            assert!(error.to_string().contains("uncertain"), "{error}");
            assert!(error.to_string().contains("without --reset"), "{error}");
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn enrollment_publication_and_completion_retry_within_stage_budget() {
        for stage in ["publish", "complete"] {
            let (listener, url) = listen().await;
            let server = tokio::spawn(async move {
                let (mut held, _) = listener.accept().await.unwrap();
                read_http(&mut held).await;
                // The request has reached the server, so its timeout is armed.
                // Advance only that wait; use real time again for the retry's I/O.
                tokio::time::pause();
                tokio::time::advance(REQUEST_TIMEOUT).await;
                tokio::time::resume();
                // Keep the first response open until the retry has succeeded.
                let (mut retry, _) = listener.accept().await.unwrap();
                read_http(&mut retry).await;
                let body = b"{}".to_vec();
                tokio::io::AsyncWriteExt::write_all(&mut retry, &http_response(200, "OK", &body))
                    .await
                    .unwrap();
                drop(held);
            });
            tokio::time::timeout(Duration::from_secs(25), async {
                if stage == "publish" {
                    publish(
                        &url,
                        MachineId::random(),
                        &PairingCredential::parse("pairing-secret").unwrap(),
                        &TailcatCapability::parse("fixture-tailcat-capability").unwrap(),
                    )
                    .await
                    .unwrap();
                } else {
                    callback(
                        &url,
                        MachineId::random(),
                        &PairingCredential::parse("pairing-secret").unwrap(),
                    )
                    .await
                    .unwrap();
                }
                server.await.unwrap();
            })
            .await
            .expect("held request must retry before the stage deadline");
        }
    }

    #[tokio::test]
    async fn callback_errors_redact_capability_and_pairing_credentials() {
        for publishing in [true, false] {
            let (listener, url) = listen().await;
            let secret = "secret-tailcat-capability";
            let pairing = "pairing-secret";
            tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                read_http(&mut stream).await;
                tokio::io::AsyncWriteExt::write_all(
                    &mut stream,
                    &http_response(400, "Error", format!("{secret} {pairing}").as_bytes()),
                )
                .await
                .unwrap();
            });
            let credential = PairingCredential::parse(pairing).unwrap();
            let (result, operation) = if publishing {
                (
                    publish(
                        &url,
                        MachineId::random(),
                        &credential,
                        &TailcatCapability::parse(secret).unwrap(),
                    )
                    .await,
                    "candidate publication",
                )
            } else {
                (
                    callback(&url, MachineId::random(), &credential).await,
                    "enrollment completion",
                )
            };
            let error = result.unwrap_err();
            for rendered in [error.to_string(), format!("{error:?}")] {
                assert!(!rendered.contains(secret));
                assert!(!rendered.contains(pairing));
            }
            assert!(
                error
                    .to_string()
                    .contains(&format!("HTTP 400: {operation} rejected"))
            );
            assert!(error.to_string().contains("without --reset"));
        }
    }

    #[tokio::test]
    async fn enroll_http_status_error_is_not_retried() {
        let (listener, url) = listen().await;
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http(&mut stream).await;
            let response = http_response(503, "Error", b"busy");
            tokio::io::AsyncWriteExt::write_all(&mut stream, &response)
                .await
                .unwrap();
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http(&mut stream).await;
            let response = http_response(200, "OK", &join_body());
            tokio::io::AsyncWriteExt::write_all(&mut stream, &response)
                .await
                .unwrap();
        });
        let error = enroll(&url, &identity()).await.unwrap_err();
        assert!(
            matches!(error, Error::Status { status: 503, .. }),
            "{error}"
        );
    }

    #[test]
    fn enroll_identity_public_key_is_wireguard_base64_string() {
        let identity = EnrollIdentity::from_machine_token(
            MachineName::parse("ployz-c1").unwrap(),
            &MachineToken {
                id: MachineId::random(),
                public_key: WireGuardPublicKey([
                    93, 8, 112, 97, 17, 191, 217, 250, 110, 95, 143, 145, 148, 219, 136, 176, 78,
                    82, 126, 17, 157, 176, 106, 76, 85, 91, 240, 187, 92, 182, 2, 77,
                ]),
                public_ip: Some("207.246.89.244".parse().unwrap()),
                advertised_endpoints: vec![AdvertisedEndpoint(
                    "207.246.89.244:51820".parse().unwrap(),
                )],
                runtime: Default::default(),
                memory_total_bytes: Some(8_589_934_592),
                disk_total_bytes: Some(107_374_182_400),
                disk_available_bytes: Some(85_899_345_920),
            },
            StorageChoice::Zfs,
            Default::default(),
        );
        let json = serde_json::to_value(&identity).unwrap();
        assert_eq!(json.get("protocolVersion"), Some(&serde_json::json!(2)));
        assert_eq!(
            json.get("publicKey"),
            Some(&serde_json::json!(
                "XQhwYRG/2fpuX4+RlNuIsE5SfhGdsGpMVVvwu1y2Ak0="
            ))
        );
        assert_eq!(
            json.get("requestedStorage"),
            Some(&serde_json::json!("zfs"))
        );
        assert_eq!(
            json.get("memoryTotalBytes"),
            Some(&serde_json::json!(8_589_934_592_u64))
        );
        assert_eq!(
            json.get("diskTotalBytes"),
            Some(&serde_json::json!(107_374_182_400_u64))
        );
        assert_eq!(
            json.get("diskAvailableBytes"),
            Some(&serde_json::json!(85_899_345_920_u64))
        );
    }
}
