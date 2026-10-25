//! Cloud enroll HTTP: POST identity, consume `initialize` / `join`, enrollment callback.

use std::{net::IpAddr, time::Duration};

use ployz_core::{
    AdvertisedEndpoint, CloudEnrollToken, MachineId, MachineName, MachineToken,
    ManagementCapability, RecoverySeed, Registered, SealingKeyBackup, SealingPublicKey,
    StorageChoice, WireGuardPublicKey,
};
use serde::{Deserialize, Serialize, Serializer};
use thiserror::Error;

pub(crate) mod recovery_compat;

const DEFAULT_RETRY_AFTER: u64 = 2;
const PROTOCOL_VERSION: u8 = 1;
const RECOVERY_SUPPORT: u8 = 1;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// How long an enroll request waits for Cloud's answer. Cloud reads its database
/// unbounded, then may spend 10s reaching the Cluster, so the client's 15s default
/// would give up on an enrollment Cloud goes on to finish. It ends inside
/// `setup_retry::WAIT`, so a held request reports its outcome as uncertain.
const ENROLL_TIMEOUT: Duration = Duration::from_secs(50);
/// How long enrollment waits for another Server to finish founding: past Cloud's own
/// ten minutes for a fresh founding claim, so Cloud answers first.
const FOUNDER_WAIT: Duration = Duration::from_secs(15 * 60);
/// How Cloud's answer starts while it can't reach the Server. A proxy in front of a down
/// Cloud also answers 503, so Cloud's message is the only signal its answer carries.
/// The prefix also matches 0.3.0 Cloud's "Cloud can't reach this Server yet.".
const CLOUD_CANT_REACH: &str = "Cloud can't reach this Server";

#[derive(Clone, Copy, Debug)]
pub(crate) enum Command {
    ServerAdd,
    ServerEnroll,
}

impl Command {
    /// The step that recovers an enrollment the Server half finished, as a sentence.
    pub(crate) fn again(self) -> &'static str {
        match self {
            Self::ServerAdd => {
                "Rerun the same ployz server add command without --reset, keeping all other options"
            }
            Self::ServerEnroll => {
                "Run ployz server enroll again, or use Enroll in Servers > Waiting to join"
            }
        }
    }

    fn retry(self) -> &'static str {
        match self {
            Self::ServerAdd => {
                "rerun the same ployz server add command without --reset (keep all other options)"
            }
            Self::ServerEnroll => {
                "run ployz server enroll again, or use Enroll in Servers > Waiting to join"
            }
        }
    }
}

/// Failures talking to Cloud enroll.
#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("Enrollment timed out waiting for Cloud.")]
    Timeout(#[source] reqwest::Error),
    #[error("Could not connect to Cloud.")]
    Connect(#[source] reqwest::Error),
    #[error("The Cloud enroll request failed.")]
    Http(#[source] reqwest::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{}", crate::cloud_login::answered("enroll", *.status, .body))]
    Status { status: u16, body: String },
    #[error(
        "Cloud no longer takes this enrollment token: it is unknown, used or expired, or this Organization's Servers were forgotten; make a new one with ployz server add --command"
    )]
    TokenRefused,
    #[error(
        "waited {} minutes for another Server to found this Organization; if it was deleted, run ployz server forget or use Forget Servers in the dashboard",
        FOUNDER_WAIT.as_secs() / 60
    )]
    FounderWait,
    #[error(
        "Cloud {operation} failed; {}.", .command.retry()
    )]
    RetrySameCommand {
        command: Command,
        operation: &'static str,
        #[source]
        cause: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error(
        "Cloud can't reach this Server through the relay or directly. Allow https to relay.ployz.dev from this Server, and from Cloud if you host it yourself, then try again."
    )]
    CloudCantReach,
}

#[derive(Debug, Error)]
#[error("The response was lost; the enrollment outcome may be uncertain.")]
struct ResponseLost(#[source] Option<Error>);

impl Error {
    fn is_transport(&self) -> bool {
        match self {
            Self::Timeout(error) | Self::Connect(error) | Self::Http(error) => {
                crate::setup_retry::transient_http(error)
            }
            Self::Json(_)
            | Self::Status { .. }
            | Self::TokenRefused
            | Self::RetrySameCommand { .. }
            | Self::CloudCantReach
            | Self::FounderWait => false,
        }
    }
}

/// Bearer identifying the current Cluster-scoped Cloud pairing.
///
/// Only the CLI↔Cloud exchange carries it; it never reaches the daemon.
/// Enrollment callbacks and endpoint removal authenticate against this credential.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub(crate) struct PairingCredential(String);

#[derive(Debug, Error)]
#[error("invalid Pairing Credential: expected a non-empty bearer")]
pub(crate) struct EmptyPairingCredential;

impl PairingCredential {
    /// Parse a non-empty Pairing Credential bearer.
    ///
    /// # Errors
    /// Returns [`EmptyPairingCredential`] when `value` is empty.
    pub(crate) fn parse(value: impl Into<String>) -> Result<Self, EmptyPairingCredential> {
        let value = value.into();
        if value.is_empty() {
            Err(EmptyPairingCredential)
        } else {
            Ok(Self(value))
        }
    }
}

impl std::fmt::Debug for PairingCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PairingCredential(..)")
    }
}

impl TryFrom<String> for PairingCredential {
    type Error = EmptyPairingCredential;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

/// Cloud Pairing as Cloud's enroll response names it: `{"secret": ...}`.
///
/// Strict by the Stable promise's security exception: it holds secret material.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudPairing {
    /// Credential identifying this Cloud pairing.
    pub(crate) secret: PairingCredential,
}

/// Identity POSTed to `POST /api/enroll`.
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
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery_support: Option<u8>,
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
        recovery_supported: bool,
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
            recovery_support: recovery_supported.then_some(RECOVERY_SUPPORT),
        }
    }
}

/// Successful enroll `join` payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Join {
    pub pairing: CloudPairing,
    pub storage: ployz_core::StoragePreparation,
    pub registration: Registered,
}

/// Enroll outcome after `not_yet` retries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Outcome {
    Join(Box<Join>),
    Initialize {
        mode: InitializeMode,
        pairing: CloudPairing,
        storage: ployz_core::StoragePreparation,
        recovery: Option<Recovery>,
    },
}

/// What Cloud asks of the founder's Sealing Key: back a new one up under a
/// recovery code, or recover the Organization's existing one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Recovery {
    BackUp,
    Seed(RecoverySeed),
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
        storage: ployz_core::StoragePreparation,
        recovery: Option<Recovery>,
    },
    NotYet {
        retry_after: Duration,
        waiting: Waiting,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Waiting {
    Founder,
    Queued { ahead: u32 },
}

/// Where an in-process enrollment has reached, reported to Cloud on each check-in. Declared in
/// the order an enrollment runs them, which Cloud's `ENROLLMENT_STAGES` shares.
#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, serde::Serialize, serde::Deserialize,
)]
#[serde(into = "&'static str", try_from = "String")]
pub(crate) enum EnrollStage {
    Installing,
    Enrolling,
    Queued,
    Storage,
    Joining,
    Publishing,
}

impl EnrollStage {
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Installing => "installing",
            Self::Storage => "storage",
            Self::Enrolling => "enrolling",
            Self::Queued => "queued",
            Self::Joining => "joining",
            Self::Publishing => "publishing",
        }
    }
}

impl From<EnrollStage> for &'static str {
    fn from(stage: EnrollStage) -> Self {
        stage.as_str()
    }
}

impl TryFrom<String> for EnrollStage {
    type Error = String;

    fn try_from(stage: String) -> Result<Self, Self::Error> {
        Ok(match stage.as_str() {
            "installing" => Self::Installing,
            "storage" => Self::Storage,
            "enrolling" => Self::Enrolling,
            "queued" => Self::Queued,
            "joining" => Self::Joining,
            "publishing" => Self::Publishing,
            _ => return Err(format!("unknown enrollment stage {stage}")),
        })
    }
}

/// Enrollment responses ignore fields a newer Cloud adds; only the pairing is strict.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum EnrollWire {
    Join {
        pairing: CloudPairing,
        #[serde(default, deserialize_with = "deserialize_storage")]
        storage: ployz_core::StoragePreparation,
        registration: Box<Registered>,
    },
    NotYet {
        #[serde(default, rename = "retryAfter")]
        retry_after: Option<u64>,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        ahead: Option<u32>,
    },
    Initialize {
        resumed: bool,
        pairing: CloudPairing,
        #[serde(default, deserialize_with = "deserialize_storage")]
        storage: ployz_core::StoragePreparation,
        #[serde(default)]
        recovery: Option<RecoveryWire>,
    },
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum RecoveryWire {
    BackUp,
    Seed {
        expected_public_key: SealingPublicKey,
        #[serde(default)]
        backup: Option<SealingKeyBackup>,
    },
}

impl From<RecoveryWire> for Recovery {
    fn from(wire: RecoveryWire) -> Self {
        match wire {
            RecoveryWire::BackUp => Self::BackUp,
            RecoveryWire::Seed {
                expected_public_key,
                backup,
            } => Self::Seed(RecoverySeed {
                expected_public_key,
                backup,
            }),
        }
    }
}

/// Enroll URL: host without a scheme is HTTPS. The token travels only as a bearer header,
/// so it never reaches a URL that proxies and access logs record.
#[must_use]
pub(crate) fn enroll_url(cloud_url: &str) -> String {
    format!("{}/api/enroll", cloud_origin(cloud_url))
}

/// Final enrollment completion: `POST /api/enroll/callback`.
#[must_use]
pub(crate) fn callback_url(cloud_url: &str) -> String {
    format!("{}/callback", enroll_url(cloud_url))
}

/// Best-effort setup report: `POST /api/enroll/report`.
#[must_use]
pub(crate) fn report_url(cloud_url: &str) -> String {
    format!("{}/report", enroll_url(cloud_url))
}

/// `https://` + a bare host, or the URL as given; no trailing slash.
pub(crate) fn cloud_origin(cloud_url: &str) -> String {
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
    capability: Option<&'a ManagementCapability>,
    machine_id: MachineId,
    pairing_credential: &'a PairingCredential,
    #[serde(skip_serializing_if = "Option::is_none")]
    sealing: Option<Sealing>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery_backup: Option<&'a RecoveryBackup>,
}

/// The founder's Sealing Key wrapped under its recovery code, for Cloud to store.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecoveryBackup {
    pub(crate) public_key: SealingPublicKey,
    pub(crate) backup: SealingKeyBackup,
}

impl RecoveryBackup {
    /// Lowercase hex SHA-256 of the envelope bytes: what Cloud echoes once it stored them.
    #[must_use]
    pub(crate) fn digest(&self) -> String {
        use sha2::Digest as _;

        hex::encode(sha2::Sha256::digest(self.backup.as_bytes()))
    }
}

/// Cloud's answer to enrollment completion.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CallbackReply {
    pub(crate) machine_id: MachineId,
    #[serde(default)]
    pub(crate) recovery_backup_digest: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum Sealing {
    Held { public_key: SealingPublicKey },
    NeedsAccess,
}

impl From<ployz_core::Sealing> for Sealing {
    fn from(sealing: ployz_core::Sealing) -> Self {
        match sealing {
            ployz_core::Sealing::Held { public_key } => Self::Held { public_key },
            ployz_core::Sealing::NeedsAccess | ployz_core::Sealing::AwaitsRecovery { .. } => {
                Self::NeedsAccess
            }
        }
    }
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
pub(crate) async fn enroll(
    command: Command,
    url: &str,
    token: &CloudEnrollToken,
    identity: &EnrollIdentity,
    progress: &dyn Fn(EnrollStage),
) -> Result<Outcome, Error> {
    let http = http_client()?;
    let mut announced_wait = false;
    let mut announced_queue = None;
    let mut deadline = None;
    loop {
        let response = crate::setup_retry::run(
            &mut (),
            &format!("Cloud enrollment at {}", diagnostic_origin(url)),
            crate::setup_retry::WAIT,
            |error| matches!(error, Error::Connect(_)),
            async |_| send(post(&http, url, token, identity).timeout(ENROLL_TIMEOUT)).await,
        )
        .await
        .map_err(|error| retry_error(command, "enrollment", error))?;
        match parse_enroll(&response)? {
            Response::Join(join) => return Ok(Outcome::Join(join)),
            Response::Initialize {
                mode,
                pairing,
                storage,
                recovery,
            } => {
                return Ok(Outcome::Initialize {
                    mode,
                    pairing,
                    storage,
                    recovery,
                });
            }
            Response::NotYet {
                retry_after,
                waiting: Waiting::Queued { ahead },
            } => {
                progress(EnrollStage::Queued);
                if announced_queue != Some(ahead) {
                    crate::ui::note(format_args!("Queued to join; {ahead} ahead. Waiting..."));
                    announced_queue = Some(ahead);
                }
                tokio::time::sleep(retry_after).await;
            }
            Response::NotYet {
                retry_after,
                waiting: Waiting::Founder,
            } => {
                if !announced_wait {
                    crate::ui::note(format_args!(
                        "Another Machine is founding this Organization; waiting..."
                    ));
                    announced_wait = true;
                }
                let now = tokio::time::Instant::now();
                if now + retry_after > *deadline.get_or_insert(now + FOUNDER_WAIT) {
                    return Err(Error::FounderWait);
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
/// HTTP failure, or a reply Cloud's `/api/enroll/callback` never sends.
pub(crate) async fn callback(
    command: Command,
    url: &str,
    token: &CloudEnrollToken,
    machine_id: MachineId,
    pairing_credential: &PairingCredential,
    sealing: Option<ployz_core::Sealing>,
    recovery_backup: Option<&RecoveryBackup>,
) -> Result<CallbackReply, Error> {
    let body = EnrollCallback {
        stage: None,
        capability: None,
        machine_id,
        pairing_credential,
        sealing: sealing.map(Sealing::from),
        recovery_backup,
    };
    let reply = post_callback(command, url, token, &body, "enrollment completion").await?;
    Ok(serde_json::from_slice(&reply)?)
}

/// Publish a candidate against the authenticated enrollment attempt before completion.
pub(crate) async fn publish(
    command: Command,
    url: &str,
    token: &CloudEnrollToken,
    machine_id: MachineId,
    pairing_credential: &PairingCredential,
    capability: &ManagementCapability,
) -> Result<(), Error> {
    let body = EnrollCallback {
        stage: Some("publish"),
        capability: Some(capability),
        machine_id,
        pairing_credential,
        sealing: None,
        recovery_backup: None,
    };
    post_callback(command, url, token, &body, "candidate publication")
        .await
        .map(|_| ())
}

async fn post_callback(
    command: Command,
    url: &str,
    token: &CloudEnrollToken,
    body: &EnrollCallback<'_>,
    operation: &'static str,
) -> Result<Vec<u8>, Error> {
    let http = http_client()?;
    crate::setup_retry::run(
        &mut (),
        &format!("Cloud {operation} at {}", diagnostic_origin(url)),
        crate::setup_retry::WAIT,
        // Cloud answers 503 until it can reach the Machine; both stages are safe to repeat.
        |error: &Error| {
            error.is_transport()
                || matches!(
                    error,
                    Error::Status { status: 503, .. } | Error::CloudCantReach
                )
        },
        async |_| {
            post_json(&http, url, token, body).await.map_err(|error| {
                let Error::Status { status, body } = error else {
                    return error;
                };
                if crate::cloud_login::cloud_message(body.as_bytes())
                    .is_some_and(|message| message.starts_with(CLOUD_CANT_REACH))
                {
                    return Error::CloudCantReach;
                }
                // Both stages carry credentials; a server may echo them in any encoding.
                Error::Status {
                    status,
                    body: format!("{operation} rejected"),
                }
            })
        },
    )
    .await
    .map_err(|error| match error {
        crate::setup_retry::Error::Exhausted {
            last: Some(Error::CloudCantReach),
            ..
        } => Error::CloudCantReach,
        error @ (crate::setup_retry::Error::Permanent(_)
        | crate::setup_retry::Error::Exhausted { .. }) => Error::RetrySameCommand {
            command,
            operation,
            cause: Box::new(error),
        },
    })
}

fn retry_error(
    command: Command,
    operation: &'static str,
    error: crate::setup_retry::Error<Error>,
) -> Error {
    match error {
        crate::setup_retry::Error::Permanent(error) => {
            if matches!(error, Error::Timeout(_) | Error::Http(_)) {
                Error::RetrySameCommand {
                    command,
                    operation,
                    cause: Box::new(ResponseLost(Some(error))),
                }
            } else {
                error
            }
        }
        // The deadline cut an attempt that may already have sent its POST.
        crate::setup_retry::Error::Exhausted {
            in_flight: true, ..
        } => Error::RetrySameCommand {
            command,
            operation,
            cause: Box::new(ResponseLost(None)),
        },
        exhausted => Error::RetrySameCommand {
            command,
            operation,
            cause: Box::new(exhausted),
        },
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

fn post(
    http: &reqwest::Client,
    url: &str,
    token: &CloudEnrollToken,
    body: &impl Serialize,
) -> reqwest::RequestBuilder {
    http.post(url).bearer_auth(token.as_str()).json(body)
}

async fn post_json(
    http: &reqwest::Client,
    url: &str,
    token: &CloudEnrollToken,
    body: &impl Serialize,
) -> Result<Vec<u8>, Error> {
    send(post(http, url, token, body)).await
}

async fn send(request: reqwest::RequestBuilder) -> Result<Vec<u8>, Error> {
    let sent = request.send().await.map_err(classify_http)?;
    let status = sent.status();
    let bytes = sent.bytes().await.map_err(classify_http)?;
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(Error::TokenRefused);
    }
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
        EnrollWire::NotYet {
            retry_after,
            reason,
            ahead,
        } => Ok(Response::NotYet {
            retry_after: Duration::from_secs(retry_after.unwrap_or(DEFAULT_RETRY_AFTER)),
            waiting: if reason.as_deref() == Some("queued") {
                Waiting::Queued {
                    ahead: ahead.unwrap_or(0),
                }
            } else {
                Waiting::Founder
            },
        }),
        EnrollWire::Initialize {
            resumed,
            pairing,
            storage,
            recovery,
        } => Ok(Response::Initialize {
            mode: if resumed {
                InitializeMode::Resume
            } else {
                InitializeMode::New
            },
            pairing,
            storage,
            recovery: recovery.map(Recovery::from),
        }),
    }
}

pub(crate) fn storage_with_requested_device(
    returned: &ployz_core::StoragePreparation,
    requested: &ployz_core::StoragePreparation,
) -> ployz_core::StoragePreparation {
    match (returned, requested) {
        (
            ployz_core::StoragePreparation::Zfs { device: None },
            ployz_core::StoragePreparation::Zfs { .. },
        ) => requested.clone(),
        (returned, _) => returned.clone(),
    }
}

fn deserialize_storage<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<ployz_core::StoragePreparation, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Wire {
        Legacy(StorageChoice),
        Target {
            kind: StorageChoice,
            device: Option<String>,
        },
    }
    match Wire::deserialize(deserializer)? {
        Wire::Legacy(choice) => Ok(choice.into()),
        Wire::Target {
            kind: StorageChoice::None,
            device: None,
        } => Ok(StorageChoice::None.into()),
        Wire::Target {
            kind: StorageChoice::Zfs,
            device,
        } => {
            let value = device.map_or_else(|| "zfs".to_owned(), |device| format!("zfs:{device}"));
            ployz_core::StoragePreparation::parse(value).map_err(serde::de::Error::custom)
        }
        Wire::Target {
            kind: StorageChoice::None,
            device: Some(_),
        } => Err(serde::de::Error::custom(
            "Docker-only storage cannot name a device",
        )),
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::{Machine, MachineId, MachineName, Registered};

    use super::*;

    #[test]
    fn an_enroll_error_prints_cloud_s_sentence_or_its_status_alone() {
        let page = Error::Status {
            status: 502,
            body: "<html><body>Bad gateway</body></html>".to_owned(),
        };
        assert_eq!(page.to_string(), "enroll HTTP 502");
        let json = Error::Status {
            status: 409,
            body: r#"{"error":"taken"}"#.to_owned(),
        };
        assert_eq!(json.to_string(), "taken");
        let public = Error::Status {
            status: 422,
            body: r#"{"_tag":"PublicError","code":"VALIDATION_FAILED","message":"Bad host."}"#
                .to_owned(),
        };
        assert_eq!(public.to_string(), "Bad host.");
        let nested = Error::Status {
            status: 409,
            body: r#"{"error":{"cause":[],"code":"conflict","details":null,"message":"The request conflicts with the current state."}}"#
                .to_owned(),
        };
        assert_eq!(
            nested.to_string(),
            "The request conflicts with the current state."
        );
        let unknown = Error::Status {
            status: 409,
            body: r#"{"error":{"message":"no code"}}"#.to_owned(),
        };
        assert_eq!(
            unknown.to_string(),
            r#"enroll HTTP 409: {"error":{"message":"no code"}}"#
        );
    }

    #[test]
    fn cloud_enroll_storage_wire_accepts_zfs_with_and_without_a_device() {
        for (wire, device) in [
            (serde_json::json!("zfs"), None),
            (serde_json::json!({"kind": "zfs"}), None),
            (
                serde_json::json!({"kind": "zfs", "device": "/dev/vdb"}),
                Some("/dev/vdb"),
            ),
        ] {
            let Response::Initialize { storage, .. } = parse_enroll(&serde_json::to_vec(&serde_json::json!({
                "kind": "initialize", "resumed": false, "pairing": {"secret": "pairing-secret"}, "storage": wire,
            })).unwrap()).unwrap() else { panic!("initialize response") };
            assert_eq!(
                storage,
                ployz_core::StoragePreparation::Zfs {
                    device: device.map(|device| ployz_core::ZfsDevice::parse(device).unwrap())
                }
            );
        }
    }

    #[test]
    fn cloud_enroll_storage_preserves_the_selected_device_only_when_zfs_is_allowed() {
        let requested = ployz_core::StoragePreparation::parse("zfs:/dev/vdb").unwrap();
        assert_eq!(
            storage_with_requested_device(&StorageChoice::Zfs.into(), &requested),
            requested
        );
        assert_eq!(
            storage_with_requested_device(&StorageChoice::None.into(), &requested),
            ployz_core::StoragePreparation::None
        );
        let returned = ployz_core::StoragePreparation::parse("zfs:/dev/vdc").unwrap();
        assert_eq!(
            storage_with_requested_device(&returned, &requested),
            returned
        );
    }

    fn pairing() -> CloudPairing {
        CloudPairing {
            secret: PairingCredential::parse("pairing-secret").unwrap(),
        }
    }

    #[test]
    fn cloud_pairing_accepts_only_a_non_empty_secret() {
        assert_eq!(
            serde_json::from_value::<CloudPairing>(
                serde_json::json!({ "secret": "pairing-secret" })
            )
            .unwrap(),
            pairing()
        );
        assert!(PairingCredential::parse("").is_err());
        for value in [
            serde_json::json!({}),
            serde_json::json!({ "secret": "" }),
            serde_json::json!({ "secret": "pairing-secret", "futureField": 1 }),
        ] {
            assert!(serde_json::from_value::<CloudPairing>(value).is_err());
        }
    }

    #[test]
    fn pairing_credential_debug_redacts_the_bearer() {
        assert!(!format!("{:?}", pairing()).contains("pairing-secret"));
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
                build_concurrency: None,
            },
            visible_peers: Vec::new(),
            target_versions: Default::default(),
            sealing_key: None,
            devices: Vec::new(),
        }
    }

    fn token() -> CloudEnrollToken {
        CloudEnrollToken::parse("pmet_test").unwrap()
    }

    #[test]
    fn host_without_scheme_is_https_under_ployz_dev() {
        assert_eq!(enroll_url("ployz.dev"), "https://ployz.dev/api/enroll");
        assert_eq!(
            enroll_url("http://127.0.0.1:9"),
            "http://127.0.0.1:9/api/enroll"
        );
        assert_eq!(
            callback_url("ployz.dev"),
            "https://ployz.dev/api/enroll/callback"
        );
        assert_eq!(
            report_url("ployz.dev"),
            "https://ployz.dev/api/enroll/report"
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
        assert_eq!(join.storage, ployz_core::StorageChoice::Zfs.into());
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
    fn initialize_requires_the_resume_directive() {
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
        assert_eq!(storage, StorageChoice::None.into());
    }

    #[test]
    fn not_yet_defaults_retry_after_to_two_seconds() {
        let Response::NotYet {
            retry_after,
            waiting,
        } = parse_enroll(br#"{"kind":"not_yet"}"#).unwrap()
        else {
            panic!("expected not_yet");
        };
        assert_eq!(retry_after, Duration::from_secs(2));
        assert_eq!(waiting, Waiting::Founder);
    }

    #[test]
    fn not_yet_reads_queue_position() {
        for (body, waiting) in [
            (
                br#"{"kind":"not_yet","retryAfter":5,"reason":"queued","ahead":3}"#.as_slice(),
                Waiting::Queued { ahead: 3 },
            ),
            (
                br#"{"kind":"not_yet","reason":"queued"}"#.as_slice(),
                Waiting::Queued { ahead: 0 },
            ),
            (
                br#"{"kind":"not_yet","retryAfter":2,"reason":"founder"}"#.as_slice(),
                Waiting::Founder,
            ),
        ] {
            let Response::NotYet { waiting: got, .. } = parse_enroll(body).unwrap() else {
                panic!("expected not_yet");
            };
            assert_eq!(got, waiting);
        }
    }

    #[test]
    fn not_yet_honors_retry_after_seconds() {
        let Response::NotYet { retry_after, .. } =
            parse_enroll(br#"{"kind":"not_yet","retryAfter":5}"#).unwrap()
        else {
            panic!("expected not_yet");
        };
        assert_eq!(retry_after, Duration::from_secs(5));
    }

    #[test]
    fn enrollment_ignores_fields_from_a_newer_cloud() {
        for (newer, known) in [
            (
                br#"{"kind":"not_yet","retryAfter":5,"futureHint":"cloud-defined"}"#.as_slice(),
                br#"{"kind":"not_yet","retryAfter":5}"#.as_slice(),
            ),
            (
                br#"{"kind":"initialize","resumed":false,"storage":"none","pairing":{"secret":"pairing-secret"},"issuedAt":"2026-08-19T22:58:13.733Z"}"#.as_slice(),
                br#"{"kind":"initialize","resumed":false,"storage":"none","pairing":{"secret":"pairing-secret"}}"#.as_slice(),
            ),
        ] {
            assert_eq!(parse_enroll(newer).unwrap(), parse_enroll(known).unwrap());
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
            false,
        )
    }

    const ACCEPTED: &[u8] = br#"{"machineId":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#;

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

    #[tokio::test]
    async fn token_travels_as_a_bearer_and_never_in_the_url() {
        let (listener, origin) = listen().await;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0; 8192];
            let read = tokio::io::AsyncReadExt::read(&mut stream, &mut buf)
                .await
                .unwrap();
            let response = http_response(200, "OK", &join_body());
            tokio::io::AsyncWriteExt::write_all(&mut stream, &response)
                .await
                .unwrap();
            String::from_utf8_lossy(buf.get(..read).unwrap()).into_owned()
        });
        let url = enroll_url(&origin);
        enroll(Command::ServerAdd, &url, &token(), &identity(), &|_| {})
            .await
            .unwrap();
        let request = server.await.unwrap();
        let request_line = request.lines().next().unwrap();
        assert_eq!(request_line, "POST /api/enroll HTTP/1.1");
        assert!(
            request
                .lines()
                .any(|line| line.eq_ignore_ascii_case("authorization: Bearer pmet_test"))
        );
        for url in [url, callback_url(&origin), report_url(&origin)] {
            assert!(!url.contains(token().as_str()), "{url}");
        }
    }

    #[tokio::test]
    async fn a_token_cloud_no_longer_takes_says_so_and_how_to_get_another() {
        let (listener, origin) = listen().await;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http(&mut stream).await;
            let body = br#"{"error":"Authentication is required."}"#;
            tokio::io::AsyncWriteExt::write_all(
                &mut stream,
                &http_response(401, "Unauthorized", body),
            )
            .await
            .unwrap();
        });
        let error = enroll(
            Command::ServerAdd,
            &enroll_url(&origin),
            &token(),
            &identity(),
            &|_| {},
        )
        .await
        .unwrap_err();
        server.await.unwrap();
        assert!(matches!(error, Error::TokenRefused), "{error:?}");
        assert!(
            error
                .to_string()
                .ends_with("make a new one with ployz server add --command"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn queued_answers_report_the_queued_stage_and_keep_polling() {
        let (listener, origin) = listen().await;
        let server = tokio::spawn(async move {
            for body in [
                br#"{"kind":"not_yet","retryAfter":0,"reason":"queued","ahead":2}"#.to_vec(),
                br#"{"kind":"not_yet","retryAfter":0,"reason":"queued","ahead":1}"#.to_vec(),
                join_body(),
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                read_http(&mut stream).await;
                tokio::io::AsyncWriteExt::write_all(&mut stream, &http_response(200, "OK", &body))
                    .await
                    .unwrap();
            }
        });
        let stages = std::cell::RefCell::new(Vec::new());
        let outcome = enroll(
            Command::ServerAdd,
            &enroll_url(&origin),
            &token(),
            &identity(),
            &|stage| {
                stages.borrow_mut().push(stage);
            },
        )
        .await
        .unwrap();
        server.await.unwrap();
        assert!(matches!(outcome, Outcome::Join(_)));
        assert_eq!(
            stages.into_inner(),
            [EnrollStage::Queued, EnrollStage::Queued]
        );
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
    async fn an_enroll_waits_past_the_cluster_time_cloud_may_spend_on_it() {
        // Cloud gives the Cluster 10s after its SQL, which runs unbounded.
        let answers_after = Duration::from_secs(10 + 5 + 2);
        let (listener, origin) = listen().await;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http(&mut stream).await;
            tokio::time::pause();
            tokio::time::advance(answers_after).await;
            tokio::time::resume();
            tokio::io::AsyncWriteExt::write_all(
                &mut stream,
                &http_response(200, "OK", &join_body()),
            )
            .await
            .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(200), listener.accept())
                    .await
                    .is_err()
            );
        });
        let outcome = tokio::time::timeout(
            answers_after + Duration::from_secs(10),
            enroll(
                Command::ServerAdd,
                &enroll_url(&origin),
                &token(),
                &identity(),
                &|_| {},
            ),
        )
        .await
        .expect("enroll returns once Cloud answers")
        .unwrap();
        let Outcome::Join(join) = outcome else {
            panic!("expected join");
        };
        assert_eq!(join.registration, registration());
        server.await.unwrap();
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
        let timeout = post_json(&http, &hang, &token(), &identity())
            .await
            .unwrap_err();
        assert_eq!(
            timeout.to_string(),
            "Enrollment timed out waiting for Cloud."
        );
        assert!(!crate::ui::chain_text(&timeout).contains(&hang));

        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let refused = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let connect = post_json(&http, &refused, &token(), &identity())
            .await
            .unwrap_err();
        assert_eq!(connect.to_string(), "Could not connect to Cloud.");
        assert_ne!(timeout.to_string(), connect.to_string());
        let connect = crate::ui::chain_text(&connect);
        assert!(connect.contains("Connection refused"), "{connect}");
        assert!(!connect.contains(&refused));
    }

    #[tokio::test]
    async fn enrollment_never_reposts_after_losing_a_dispatched_response() {
        for (timeout, command) in [false, true].into_iter().flat_map(|timeout| {
            [Command::ServerAdd, Command::ServerEnroll].map(|command| (timeout, command))
        }) {
            let (listener, url) = listen().await;
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                read_http(&mut stream).await;
                if timeout {
                    tokio::time::pause();
                    tokio::time::advance(ENROLL_TIMEOUT).await;
                    tokio::time::resume();
                }
                drop(stream);
                assert!(
                    tokio::time::timeout(Duration::from_millis(200), listener.accept())
                        .await
                        .is_err()
                );
            });
            let error = tokio::time::timeout(
                ENROLL_TIMEOUT + Duration::from_secs(10),
                enroll(command, &url, &token(), &identity(), &|_| {}),
            )
            .await
            .expect("lost response must return without retrying")
            .unwrap_err();
            let rendered = crate::ui::chain_text(&error);
            assert!(rendered.contains("uncertain"), "{rendered}");
            let message = error.to_string();
            match command {
                Command::ServerAdd => assert!(message.contains("without --reset"), "{message}"),
                Command::ServerEnroll => {
                    assert!(message.contains("ployz server enroll"), "{message}");
                    assert!(!message.contains("server add"), "{message}");
                    assert!(!message.contains("--reset"), "{message}");
                }
            }
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn an_enroll_cut_by_the_retry_budget_after_dispatch_is_uncertain() {
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = closed.local_addr().unwrap();
        drop(closed);
        let server = tokio::spawn(async move {
            // Cloud is unreachable for the first 11s of the retry budget.
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(11)).await;
            tokio::time::resume();
            let listener = tokio::net::TcpListener::bind(address).await.unwrap();
            let (mut held, _) = listener.accept().await.unwrap();
            read_http(&mut held).await;
            // Hold the POST past the budget's remaining 49s, short of its own timeout.
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(49)).await;
            tokio::time::resume();
            assert!(
                tokio::time::timeout(Duration::from_millis(200), listener.accept())
                    .await
                    .is_err()
            );
        });
        let error = tokio::time::timeout(
            crate::setup_retry::WAIT + Duration::from_secs(10),
            enroll(
                Command::ServerAdd,
                &format!("http://{address}/api/enroll"),
                &token(),
                &identity(),
                &|_| {},
            ),
        )
        .await
        .expect("the retry budget ends the held request")
        .unwrap_err();
        let rendered = crate::ui::chain_text(&error);
        assert!(rendered.contains("uncertain"), "{rendered}");
        assert!(!rendered.contains("did not recover"), "{rendered}");
        assert!(error.to_string().contains("without --reset"), "{error}");
        server.await.unwrap();
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
                tokio::io::AsyncWriteExt::write_all(
                    &mut retry,
                    &http_response(200, "OK", ACCEPTED),
                )
                .await
                .unwrap();
                drop(held);
            });
            tokio::time::timeout(Duration::from_secs(25), async {
                if stage == "publish" {
                    publish(
                        Command::ServerAdd,
                        &url,
                        &token(),
                        MachineId::random(),
                        &PairingCredential::parse("pairing-secret").unwrap(),
                        &ManagementCapability::new(
                            ployz_core::ManagementIdentity::from_bytes([1; 32]),
                            [2; 32],
                        ),
                    )
                    .await
                    .unwrap();
                } else {
                    callback(
                        Command::ServerAdd,
                        &url,
                        &token(),
                        MachineId::random(),
                        &PairingCredential::parse("pairing-secret").unwrap(),
                        None,
                        None,
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
    async fn enrollment_publication_and_completion_retry_while_cloud_is_unavailable() {
        for publishing in [true, false] {
            let (listener, url) = listen().await;
            let server = tokio::spawn(async move {
                for (status, reason) in [(503, "Service Unavailable"), (200, "OK")] {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    read_http(&mut stream).await;
                    tokio::io::AsyncWriteExt::write_all(
                        &mut stream,
                        &http_response(status, reason, ACCEPTED),
                    )
                    .await
                    .unwrap();
                }
            });
            let credential = PairingCredential::parse("pairing-secret").unwrap();
            let result = if publishing {
                publish(
                    Command::ServerAdd,
                    &url,
                    &token(),
                    MachineId::random(),
                    &credential,
                    &ManagementCapability::new(
                        ployz_core::ManagementIdentity::from_bytes([1; 32]),
                        [2; 32],
                    ),
                )
                .await
            } else {
                callback(
                    Command::ServerAdd,
                    &url,
                    &token(),
                    MachineId::random(),
                    &credential,
                    None,
                    None,
                )
                .await
                .map(drop)
            };
            result.unwrap();
            server.await.unwrap();
        }
    }

    async fn read_json_body(stream: &mut tokio::net::TcpStream) -> serde_json::Value {
        let mut request = Vec::new();
        let mut buf = vec![0; 8192];
        loop {
            let read = tokio::io::AsyncReadExt::read(stream, &mut buf)
                .await
                .unwrap();
            request.extend_from_slice(buf.get(..read).unwrap());
            let text = String::from_utf8_lossy(&request);
            if let Some((head, body)) = text.split_once("\r\n\r\n") {
                let length = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                if body.len() >= length {
                    return serde_json::from_str(body).unwrap();
                }
            }
        }
    }

    #[tokio::test]
    async fn completion_resends_the_sealing_state_until_cloud_accepts_it() {
        let public_key = ployz_core::SealingPublicKey::from_bytes([5; 32]);
        for (sealing, expected) in [
            (
                ployz_core::Sealing::Held { public_key },
                serde_json::json!({ "kind": "held", "publicKey": public_key.to_string() }),
            ),
            (
                ployz_core::Sealing::NeedsAccess,
                serde_json::json!({ "kind": "needs_access" }),
            ),
            (
                ployz_core::Sealing::AwaitsRecovery { public_key },
                serde_json::json!({ "kind": "needs_access" }),
            ),
        ] {
            let (listener, url) = listen().await;
            let server = tokio::spawn(async move {
                let mut bodies = Vec::new();
                for (status, reason) in [(503, "Service Unavailable"), (200, "OK")] {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    bodies.push(read_json_body(&mut stream).await);
                    tokio::io::AsyncWriteExt::write_all(
                        &mut stream,
                        &http_response(status, reason, ACCEPTED),
                    )
                    .await
                    .unwrap();
                }
                bodies
            });
            let credential = PairingCredential::parse("pairing-secret").unwrap();
            callback(
                Command::ServerAdd,
                &url,
                &token(),
                MachineId::random(),
                &credential,
                Some(sealing),
                None,
            )
            .await
            .unwrap();
            let bodies = server.await.unwrap();
            assert_eq!(bodies.len(), 2);
            for body in bodies {
                assert_eq!(body.get("sealing"), Some(&expected), "{body}");
                assert!(body.get("stage").is_none(), "{body}");
            }
        }
    }

    /// Every attempt gets the same 503, so the stage's last error is Cloud's answer wherever
    /// the deadline lands. Paused tokio jumps to the next timer whenever it waits on real IO,
    /// so a 10ms ticker keeps each jump far short of reqwest's own timeouts.
    #[tokio::test(start_paused = true)]
    async fn enrollment_stages_name_the_relay_only_when_cloud_says_it_never_reaches_the_server() {
        let cant_reach: &[u8] =
            br#"{"error":"Cloud can't reach this Server through the relay or directly."}"#;
        let ticker = tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        for (publishing, body) in [true, false]
            .into_iter()
            .flat_map(|publishing| [(publishing, cant_reach), (publishing, b"busy".as_slice())])
        {
            let (listener, url) = listen().await;
            let server = tokio::spawn(async move {
                loop {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    read_http(&mut stream).await;
                    // A client the deadline cut has gone; the next attempt must still connect.
                    let _ = tokio::io::AsyncWriteExt::write_all(
                        &mut stream,
                        &http_response(503, "Service Unavailable", body),
                    )
                    .await;
                }
            });
            let credential = PairingCredential::parse("pairing-secret").unwrap();
            let result = if publishing {
                publish(
                    Command::ServerAdd,
                    &url,
                    &token(),
                    MachineId::random(),
                    &credential,
                    &ManagementCapability::new(
                        ployz_core::ManagementIdentity::from_bytes([1; 32]),
                        [2; 32],
                    ),
                )
                .await
            } else {
                callback(
                    Command::ServerAdd,
                    &url,
                    &token(),
                    MachineId::random(),
                    &credential,
                    None,
                    None,
                )
                .await
                .map(drop)
            };
            server.abort();
            let error = result.unwrap_err();
            if body != cant_reach {
                // A proxy in front of a down Cloud answers 503 too; that is not Cloud's diagnosis.
                let Error::RetrySameCommand { cause, .. } = &error else {
                    panic!("{error:?}");
                };
                assert!(
                    matches!(
                        cause.downcast_ref::<crate::setup_retry::Error<Error>>(),
                        Some(crate::setup_retry::Error::Exhausted {
                            last: Some(Error::Status { status: 503, .. }),
                            ..
                        })
                    ),
                    "{error:?}"
                );
                continue;
            }
            assert!(matches!(error, Error::CloudCantReach), "{error:?}");
            assert_eq!(
                error.to_string(),
                "Cloud can't reach this Server through the relay or directly. Allow https to relay.ployz.dev from this Server, and from Cloud if you host it yourself, then try again."
            );
        }
        ticker.abort();
    }

    #[tokio::test]
    async fn cloud_enroll_callback_failure_tells_the_user_to_enroll_again() {
        for publishing in [true, false] {
            let (listener, url) = listen().await;
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                read_http(&mut stream).await;
                tokio::io::AsyncWriteExt::write_all(
                    &mut stream,
                    &http_response(400, "Bad Request", b"rejected"),
                )
                .await
                .unwrap();
            });
            let credential = PairingCredential::parse("pairing-secret").unwrap();
            let result = if publishing {
                publish(
                    Command::ServerEnroll,
                    &url,
                    &token(),
                    MachineId::random(),
                    &credential,
                    &ManagementCapability::new(
                        ployz_core::ManagementIdentity::from_bytes([1; 32]),
                        [2; 32],
                    ),
                )
                .await
            } else {
                callback(
                    Command::ServerEnroll,
                    &url,
                    &token(),
                    MachineId::random(),
                    &credential,
                    None,
                    None,
                )
                .await
                .map(drop)
            };
            let message = result.unwrap_err().to_string();
            assert!(!message.contains("server add"), "{message}");
            assert!(!message.contains("--reset"), "{message}");
            assert!(message.contains("ployz server enroll"), "{message}");
            assert!(message.contains("Servers > Waiting to join"), "{message}");
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn callback_errors_redact_capability_and_pairing_credentials() {
        for publishing in [true, false] {
            let (listener, url) = listen().await;
            let capability = ManagementCapability::new(
                ployz_core::ManagementIdentity::from_bytes([3; 32]),
                [4; 32],
            );
            let secret = capability.to_secret_string();
            let pairing = "pairing-secret";
            let echoed = format!("{secret} {pairing}");
            tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                read_http(&mut stream).await;
                tokio::io::AsyncWriteExt::write_all(
                    &mut stream,
                    &http_response(400, "Error", echoed.as_bytes()),
                )
                .await
                .unwrap();
            });
            let credential = PairingCredential::parse(pairing).unwrap();
            let (result, operation) = if publishing {
                (
                    publish(
                        Command::ServerAdd,
                        &url,
                        &token(),
                        MachineId::random(),
                        &credential,
                        &capability,
                    )
                    .await,
                    "candidate publication",
                )
            } else {
                (
                    callback(
                        Command::ServerAdd,
                        &url,
                        &token(),
                        MachineId::random(),
                        &credential,
                        None,
                        None,
                    )
                    .await
                    .map(drop),
                    "enrollment completion",
                )
            };
            let error = result.unwrap_err();
            for rendered in [crate::ui::chain_text(&error), format!("{error:?}")] {
                assert!(!rendered.contains(&secret));
                assert!(!rendered.contains(pairing));
            }
            assert!(
                crate::ui::chain_text(&error).contains(&format!("HTTP 400: {operation} rejected"))
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
        let error = enroll(Command::ServerAdd, &url, &token(), &identity(), &|_| {})
            .await
            .unwrap_err();
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
            false,
        );
        let json = serde_json::to_value(&identity).unwrap();
        assert_eq!(json.get("protocolVersion"), Some(&serde_json::json!(1)));
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
