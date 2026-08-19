//! Cloud enroll HTTP: POST identity, consume `initialize` / `join`, founder callback.

use std::{net::IpAddr, time::Duration};

use ployz_core::{
    AdvertisedEndpoint, CloudEnrollToken, CloudPairing, MachineId, MachineName, MachineToken,
    Registered, WireGuardPublicKey,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const DEFAULT_RETRY_AFTER: u64 = 2;

/// Failures talking to Cloud enroll.
#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("enroll HTTP {status}: {body}")]
    Status { status: u16, body: String },
}

/// Identity POSTed to `POST /api/enroll/<token>`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EnrollIdentity {
    name: MachineName,
    public_key: WireGuardPublicKey,
    advertised_endpoints: Vec<AdvertisedEndpoint>,
    public_ip: Option<IpAddr>,
}

impl EnrollIdentity {
    pub(crate) fn from_machine_token(name: MachineName, token: &MachineToken) -> Self {
        Self {
            name,
            public_key: token.public_key,
            advertised_endpoints: token.advertised_endpoints.clone(),
            public_ip: token.public_ip,
        }
    }
}

/// Successful enroll `join` payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Join {
    pub pairing: CloudPairing,
    pub registration: Registered,
}

/// Enroll outcome after `not_yet` retries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Outcome {
    Join(Box<Join>),
    Initialize { pairing: CloudPairing },
}

/// One enroll POST body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Response {
    Join(Box<Join>),
    Initialize { pairing: CloudPairing },
    NotYet { retry_after: Duration },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum EnrollWire {
    Join {
        pairing: CloudPairing,
        registration: Box<Registered>,
    },
    NotYet {
        #[serde(default, rename = "retryAfter")]
        retry_after: Option<u64>,
    },
    Initialize {
        pairing: CloudPairing,
    },
}

/// Enroll URL: host without a scheme is HTTPS.
#[must_use]
pub(crate) fn enroll_url(cloud_url: &str, token: &CloudEnrollToken) -> String {
    format!("{}/api/enroll/{}", cloud_origin(cloud_url), token.as_str())
}

/// Hosted DNS API on the same Cloud host as enroll. Not `dns.uncloud.run`.
#[must_use]
pub(crate) fn dns_endpoint(cloud_url: &str) -> String {
    format!("{}/api/dns/v1", cloud_origin(cloud_url))
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
/// # Errors
///
/// HTTP or unexpected JSON.
pub(crate) async fn enroll(url: &str, identity: &EnrollIdentity) -> Result<Outcome, Error> {
    let http = http_client()?;
    loop {
        match parse_enroll(&post_json(&http, url, identity).await?)? {
            Response::Join(join) => return Ok(Outcome::Join(join)),
            Response::Initialize { pairing } => return Ok(Outcome::Initialize { pairing }),
            Response::NotYet { retry_after } => tokio::time::sleep(retry_after).await,
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
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?)
}

async fn post_json(
    http: &reqwest::Client,
    url: &str,
    body: &impl Serialize,
) -> Result<Vec<u8>, Error> {
    let sent = http.post(url).json(body).send().await?;
    let status = sent.status();
    let bytes = sent.bytes().await?;
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
            registration,
        } => Ok(Response::Join(Box::new(Join {
            pairing,
            registration: *registration,
        }))),
        EnrollWire::NotYet { retry_after } => Ok(Response::NotYet {
            retry_after: Duration::from_secs(retry_after.unwrap_or(DEFAULT_RETRY_AFTER)),
        }),
        EnrollWire::Initialize { pairing } => Ok(Response::Initialize { pairing }),
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
    fn hosted_dns_is_on_the_same_cloud_host_not_uncloud_run() {
        assert_eq!(dns_endpoint("ployz.dev"), "https://ployz.dev/api/dns/v1");
        assert_eq!(
            dns_endpoint("http://127.0.0.1:9"),
            "http://127.0.0.1:9/api/dns/v1"
        );
        assert!(!dns_endpoint("ployz.dev").contains("dns.uncloud.run"));
    }

    #[test]
    fn join_payload_is_pairing_plus_registration() {
        let value = serde_json::json!({
            "kind": "join",
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
        assert_eq!(join.registration, registration());
    }

    #[test]
    fn pairing_with_a_dial_field_is_rejected() {
        let value = serde_json::json!({
            "kind": "join",
            "pairing": {
                "relayUrl": "https://relay.example.invalid",
                "secret": "pairing-secret",
                "dial": "dial-credential",
            },
            "registration": registration(),
        });
        let error = parse_enroll(serde_json::to_vec(&value).unwrap().as_slice()).unwrap_err();
        assert!(error.to_string().contains("unknown field"), "{error}");
    }

    #[test]
    fn top_level_dial_is_rejected() {
        let value = serde_json::json!({
            "kind": "join",
            "pairing": {
                "relayUrl": "https://relay.example.invalid",
                "secret": "pairing-secret",
            },
            "registration": registration(),
            "dial": "dial-credential",
        });
        let error = parse_enroll(serde_json::to_vec(&value).unwrap().as_slice()).unwrap_err();
        assert!(error.to_string().contains("unknown field"), "{error}");
    }

    #[test]
    fn initialize_payload_is_cloud_pairing() {
        let value = serde_json::json!({
            "kind": "initialize",
            "pairing": {
                "relayUrl": "https://relay.example.invalid",
                "secret": "pairing-secret",
            },
        });
        let Response::Initialize { pairing: got } =
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
            "pairing": {
                "relayUrl": "https://relay.example.invalid",
                "secret": "pairing-secret",
                "dial": "dial-credential",
            },
        });
        let error = parse_enroll(serde_json::to_vec(&value).unwrap().as_slice()).unwrap_err();
        assert!(error.to_string().contains("unknown field"), "{error}");
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
}
