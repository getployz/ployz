//! Cloud enroll HTTP: POST identity, consume `initialize` / `join`, founder callback.

use std::{net::IpAddr, time::Duration};

use ployz_core::{
    AdvertisedEndpoint, CloudEnrollToken, CloudPairing, MachineId, MachineName, MachineToken,
    Registered, StorageChoice, WireGuardPublicKey,
};
use serde::{Deserialize, Serialize, Serializer};
use thiserror::Error;

const DEFAULT_RETRY_AFTER: u64 = 2;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
// Healthy production enroll measured 8.15s for Relay List + registerHeld.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Failures talking to Cloud enroll.
#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("enroll timed out waiting for Cloud")]
    Timeout(#[source] reqwest::Error),
    #[error("could not connect to Cloud")]
    Connect(#[source] reqwest::Error),
    #[error(transparent)]
    Http(reqwest::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("enroll HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("Cloud response must not carry a Dial Credential")]
    DialOffered,
}

impl Error {
    fn is_transport(&self) -> bool {
        matches!(self, Self::Timeout(_) | Self::Connect(_) | Self::Http(_))
    }
}

/// Identity POSTed to `POST /api/enroll/<token>`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EnrollIdentity {
    name: MachineName,
    // Cloud enroll HTTP expects Display/base64, not the RPC `number[]` wire.
    #[serde(serialize_with = "serialize_as_wireguard_base64")]
    public_key: WireGuardPublicKey,
    advertised_endpoints: Vec<AdvertisedEndpoint>,
    public_ip: Option<IpAddr>,
    requested_storage: StorageChoice,
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
    ) -> Self {
        Self {
            name,
            public_key: token.public_key,
            advertised_endpoints: token.advertised_endpoints.clone(),
            public_ip: token.public_ip,
            requested_storage,
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
        pairing: CloudPairing,
        storage: StorageChoice,
    },
}

/// One enroll POST body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Response {
    Join(Box<Join>),
    Initialize {
        pairing: CloudPairing,
        storage: StorageChoice,
    },
    NotYet {
        retry_after: Duration,
    },
}

/// Cloud enrollment is a rolling-compatibility boundary: official Cloud and
/// self-hosted Cloud can be older or newer than the CLI. Keep unknown response
/// fields ignored, and give every additive field a safe default when one exists.
/// Making a response field required can make the founder exit before callback
/// and leave every other Machine polling an enrollment that can never finish.
/// Fields without a safe default require a future enrollment protocol version.
/// `dial` remains the deliberate exception: a Machine must never hold Dial.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum EnrollWire {
    Join {
        pairing: CloudPairing,
        #[serde(default)]
        storage: StorageChoice,
        registration: Box<Registered>,
        #[serde(default)]
        dial: Option<serde::de::IgnoredAny>,
    },
    NotYet {
        #[serde(default, rename = "retryAfter")]
        retry_after: Option<u64>,
    },
    Initialize {
        pairing: CloudPairing,
        #[serde(default)]
        storage: StorageChoice,
        #[serde(default)]
        dial: Option<serde::de::IgnoredAny>,
    },
}

/// Enroll URL: host without a scheme is HTTPS.
#[must_use]
pub(crate) fn enroll_url(cloud_url: &str, token: &CloudEnrollToken) -> String {
    format!("{}/api/enroll/{}", cloud_origin(cloud_url), token.as_str())
}

/// Founder callback: `POST /api/enroll/<token>/callback`. UX, not the lock.
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
struct EnrollCallback {
    machine_id: MachineId,
}

/// POST identity until Cloud returns `initialize` or `join`.
///
/// Transport errors and `not_yet` are retried with backoff. A timed-out enroll
/// is safe to repeat: the founding claim is CAS and joins are idempotent.
///
/// # Errors
///
/// HTTP status, unexpected JSON, or a non-transport HTTP failure.
pub(crate) async fn enroll(url: &str, identity: &EnrollIdentity) -> Result<Outcome, Error> {
    let http = http_client()?;
    loop {
        match post_json(&http, url, identity).await {
            Ok(bytes) => match parse_enroll(&bytes)? {
                Response::Join(join) => return Ok(Outcome::Join(join)),
                Response::Initialize { pairing, storage } => {
                    return Ok(Outcome::Initialize { pairing, storage });
                }
                Response::NotYet { retry_after } => tokio::time::sleep(retry_after).await,
            },
            Err(error) if error.is_transport() => {
                tokio::time::sleep(Duration::from_secs(DEFAULT_RETRY_AFTER)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

/// POST `{ machineId }` after Relay Register is held. Not what makes waiters `join`.
///
/// # Errors
///
/// HTTP failure.
pub(crate) async fn callback(url: &str, machine_id: MachineId) -> Result<(), Error> {
    post_json(&http_client()?, url, &EnrollCallback { machine_id }).await?;
    Ok(())
}

fn http_client() -> Result<reqwest::Client, Error> {
    // No total timeout: `not_yet` polling bounds overall wait.
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()
        .map_err(classify_http)
}

fn classify_http(error: reqwest::Error) -> Error {
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
        EnrollWire::Join { dial: Some(_), .. } | EnrollWire::Initialize { dial: Some(_), .. } => {
            Err(Error::DialOffered)
        }
        EnrollWire::Join {
            pairing,
            storage,
            registration,
            dial: None,
        } => Ok(Response::Join(Box::new(Join {
            pairing,
            storage,
            registration: *registration,
        }))),
        EnrollWire::NotYet { retry_after } => Ok(Response::NotYet {
            retry_after: Duration::from_secs(retry_after.unwrap_or(DEFAULT_RETRY_AFTER)),
        }),
        EnrollWire::Initialize {
            pairing,
            storage,
            dial: None,
        } => Ok(Response::Initialize { pairing, storage }),
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::{Machine, MachineId, MachineName, PairingCredential, Registered};

    use super::*;

    fn pairing() -> CloudPairing {
        CloudPairing::parse(
            "https://relay.example.invalid",
            PairingCredential::parse("pairing-secret").unwrap(),
        )
        .unwrap()
    }

    fn registration() -> Registered {
        Registered {
            assigned_machine: Machine {
                id: MachineId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
                name: MachineName::parse("joiner").unwrap(),
                subnet: "10.210.1.0/24".parse().unwrap(),
                management_address: ployz_core::ManagementAddress("fd00::1".parse().unwrap()),
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
                "relayUrl": "https://relay.example.invalid",
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
    fn pairing_with_a_dial_field_is_rejected() {
        let value = serde_json::json!({
            "kind": "join",
            "storage": "none",
            "pairing": {
                "relayUrl": "https://relay.example.invalid",
                "secret": "pairing-secret",
                "dial": "dial-credential",
            },
            "registration": registration(),
        });
        let error = parse_enroll(serde_json::to_vec(&value).unwrap().as_slice()).unwrap_err();
        assert!(error.to_string().contains("Dial Credential"), "{error}");
    }

    #[test]
    fn top_level_dial_is_rejected() {
        let value = serde_json::json!({
            "kind": "join",
            "storage": "none",
            "pairing": {
                "relayUrl": "https://relay.example.invalid",
                "secret": "pairing-secret",
            },
            "registration": registration(),
            "dial": "dial-credential",
        });
        let error = parse_enroll(serde_json::to_vec(&value).unwrap().as_slice()).unwrap_err();
        assert!(error.to_string().contains("Dial Credential"), "{error}");
    }

    #[test]
    fn initialize_payload_is_cloud_pairing() {
        let value = serde_json::json!({
            "kind": "initialize",
            "storage": "none",
            "pairing": {
                "relayUrl": "https://relay.example.invalid",
                "secret": "pairing-secret",
            },
        });
        let Response::Initialize { pairing: got, .. } =
            parse_enroll(serde_json::to_vec(&value).unwrap().as_slice()).unwrap()
        else {
            panic!("expected initialize");
        };
        assert_eq!(got, pairing());
    }

    #[test]
    fn initialize_pairing_with_a_dial_field_is_rejected() {
        let value = serde_json::json!({
            "kind": "initialize",
            "storage": "none",
            "pairing": {
                "relayUrl": "https://relay.example.invalid",
                "secret": "pairing-secret",
                "dial": "dial-credential",
            },
        });
        let error = parse_enroll(serde_json::to_vec(&value).unwrap().as_slice()).unwrap_err();
        assert!(error.to_string().contains("Dial Credential"), "{error}");
    }

    #[test]
    fn enrollment_defaults_missing_storage_to_none() {
        let Response::Initialize { storage, .. } = parse_enroll(
            br#"{"kind":"initialize","pairing":{"relayUrl":"https://relay.example.invalid","secret":"pairing-secret"}}"#,
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
    fn enroll_ignores_fields_the_cloud_adds_later() {
        let Response::NotYet { retry_after } = parse_enroll(
            br#"{"kind":"not_yet","retryAfter":5,"claimExpiresAt":"2026-08-19T22:58:13.733Z"}"#,
        )
        .unwrap() else {
            panic!("expected not_yet");
        };
        assert_eq!(retry_after, Duration::from_secs(5));
    }

    #[test]
    fn initialize_ignores_fields_the_cloud_adds_later() {
        let Response::Initialize { pairing: parsed, .. } = parse_enroll(
            br#"{"kind":"initialize","storage":"none","pairing":{"relayUrl":"https://relay.example.invalid","secret":"pairing-secret","privateRelayUrl":"http://relay.internal"},"issuedAt":"2026-08-19T22:58:13.733Z"}"#,
        )
        .unwrap() else {
            panic!("expected initialize");
        };
        assert_eq!(parsed, pairing());
    }

    fn identity() -> EnrollIdentity {
        EnrollIdentity::from_machine_token(
            MachineName::parse("joiner").unwrap(),
            &MachineToken {
                public_key: WireGuardPublicKey([1; 32]),
                public_ip: None,
                advertised_endpoints: Vec::new(),
                runtime: Default::default(),
                memory_total_bytes: None,
                disk_total_bytes: None,
                disk_available_bytes: None,
            },
            StorageChoice::None,
        )
    }

    fn join_body() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "kind": "join",
            "storage": "none",
            "pairing": {
                "relayUrl": "https://relay.example.invalid",
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
        assert_eq!(timeout.to_string(), "enroll timed out waiting for Cloud");
        assert!(
            !timeout.to_string().contains("error sending request"),
            "{timeout}"
        );

        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let refused = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let connect = post_json(&http, &refused, &identity()).await.unwrap_err();
        assert_eq!(connect.to_string(), "could not connect to Cloud");
        assert_ne!(timeout.to_string(), connect.to_string());
    }

    #[tokio::test]
    async fn enroll_retries_transport_error_then_joins() {
        let (listener, url) = listen().await;
        let body = join_body();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http(&mut stream).await;
            drop(stream);
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http(&mut stream).await;
            let response = http_response(200, "OK", &body);
            tokio::io::AsyncWriteExt::write_all(&mut stream, &response)
                .await
                .unwrap();
        });
        let Outcome::Join(join) = enroll(&url, &identity()).await.unwrap() else {
            panic!("expected join");
        };
        assert_eq!(join.pairing, pairing());
        assert_eq!(join.registration, registration());
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
        );
        let json = serde_json::to_value(&identity).unwrap();
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
