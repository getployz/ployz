//! Cloud enroll HTTP: POST identity, consume `initialize` / `join`, enrollment callback.

use std::{net::IpAddr, time::Duration};

use ployz_core::{
    AdvertisedEndpoint, CloudEnrollToken, MachineId, MachineName, MachineToken,
    ManagementCapability, RecoverySeed, Registered, SealingKeyBackup, SealingPublicKey,
    StorageChoice, WireGuardPublicKey,
};
use serde::{Deserialize, Serialize, Serializer};
use thiserror::Error;

pub(crate) use ployz_core::EnrollStage;

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
    #[error("{}", crate::cloud_login::answered("enroll", 409, .body))]
    CodedConflict { body: String },
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
    pub(crate) fn capability(&self) -> Option<String> {
        let body = match self {
            Self::Status { body, .. } | Self::CodedConflict { body } => body,
            Self::Timeout(_)
            | Self::Connect(_)
            | Self::Http(_)
            | Self::Json(_)
            | Self::TokenRefused
            | Self::FounderWait
            | Self::RetrySameCommand { .. }
            | Self::CloudCantReach => return None,
        };
        serde_json::from_str::<serde_json::Value>(body)
            .ok()?
            .pointer("/details/capability")?
            .as_str()
            .map(str::to_owned)
    }
    fn is_connect(&self) -> bool {
        matches!(self, Self::Connect(_))
    }
    /// Cloud answers 503 until it can reach the Machine; both stages are safe to repeat.
    fn is_callback_retryable(&self) -> bool {
        self.is_transport()
            || matches!(
                self,
                Self::Status { status: 503, .. } | Self::CloudCantReach
            )
    }
    fn is_transport(&self) -> bool {
        match self {
            Self::Timeout(error) | Self::Connect(error) | Self::Http(error) => {
                crate::setup_retry::transient_http(error)
            }
            Self::Json(_)
            | Self::Status { .. }
            | Self::CodedConflict { .. }
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

impl PairingCredential {
    /// The Founding Claim this pairing names, which a founding daemon keeps in place of the bearer.
    pub(crate) fn founding_claim(&self) -> ployz_core::FoundingClaim {
        ployz_core::FoundingClaim::of_pairing(&self.0)
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
    #[serde(skip_serializing_if = "Option::is_none")]
    join_confirmation: Option<ployz_core::authority::join::JoinConfirmation>,
    /// What this client honors when it runs the joiner's catch-up. Cloud relays it in
    /// Register, and the Machine refuses a joiner that claims too little for the
    /// Cluster's declared Doors.
    capabilities: Vec<&'static str>,
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
            join_confirmation: None,
            capabilities: vec![ployz_core::SET_CLUSTER_DOORS_CAPABILITY],
        }
    }

    /// The same identity, carrying the joiner's confirmation of its approved capsule for Cloud
    /// to relay in Register.
    pub(crate) fn confirming(
        &self,
        confirmation: ployz_core::authority::join::JoinConfirmation,
    ) -> Self {
        Self {
            join_confirmation: Some(confirmation),
            ..self.clone()
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
            &crate::ui::WaitNotices,
            &format!("Cloud enrollment at {}", diagnostic_origin(url)),
            crate::setup_retry::WAIT,
            Error::is_connect,
            async |_| send(post(&http, url, token, identity).timeout(ENROLL_TIMEOUT)).await,
        )
        .await
        .map_err(|error| {
            let error = retry_error(command, "enrollment", error);
            match error {
                Error::CodedConflict { body } => {
                    coded_refusal_status(&body, &[token.as_str().to_owned()], "enrollment")
                }
                error @ (Error::Timeout(_)
                | Error::Connect(_)
                | Error::Http(_)
                | Error::Json(_)
                | Error::Status { .. }
                | Error::TokenRefused
                | Error::FounderWait
                | Error::RetrySameCommand { .. }
                | Error::CloudCantReach) => error,
            }
        })?;
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

/// Ask Cloud to let go of a Founding Claim this Server refused to found under. Best-effort and
/// sent once: a Cloud from before 0.3.15 refuses the stage, and the refusal tells the user how
/// to reset the attempt instead.
pub(crate) async fn release(
    url: &str,
    token: &CloudEnrollToken,
    machine_id: MachineId,
    pairing_credential: &PairingCredential,
) -> bool {
    let body = EnrollCallback {
        stage: Some("release"),
        capability: None,
        machine_id,
        pairing_credential,
        sealing: None,
        recovery_backup: None,
    };
    let Ok(http) = http_client() else {
        return false;
    };
    post_json(&http, url, token, &body).await.is_ok()
}

async fn post_callback(
    command: Command,
    url: &str,
    token: &CloudEnrollToken,
    body: &EnrollCallback<'_>,
    operation: &'static str,
) -> Result<Vec<u8>, Error> {
    let http = http_client()?;
    let sent = sent_strings(token, body);
    crate::setup_retry::run(
        &mut (),
        &crate::ui::WaitNotices,
        &format!("Cloud {operation} at {}", diagnostic_origin(url)),
        crate::setup_retry::WAIT,
        Error::is_callback_retryable,
        async |_| {
            post_json(&http, url, token, body).await.map_err(|error| {
                let (status, body) = match error {
                    Error::CodedConflict { body } => {
                        return coded_refusal_status(&body, &sent, operation);
                    }
                    Error::Status { status, body } => (status, body),
                    error @ (Error::Timeout(_)
                    | Error::Connect(_)
                    | Error::Http(_)
                    | Error::Json(_)
                    | Error::TokenRefused
                    | Error::FounderWait
                    | Error::RetrySameCommand { .. }
                    | Error::CloudCantReach) => return error,
                };
                if crate::cloud_login::cloud_message(body.as_bytes())
                    .is_some_and(|message| message.starts_with(CLOUD_CANT_REACH))
                {
                    return Error::CloudCantReach;
                }
                // Both stages carry credentials; a server may echo them in any encoding, so
                // only Cloud's own sentence survives, and only when it holds none of them.
                Error::Status {
                    status,
                    body: cloud_refusal(&body, &sent).map_or_else(
                        || format!("{operation} rejected"),
                        |said| serde_json::json!({ "error": said }).to_string(),
                    ),
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
        crate::setup_retry::Error::Permanent(error @ Error::CodedConflict { .. }) => error,
        error @ (crate::setup_retry::Error::Permanent(_)
        | crate::setup_retry::Error::Exhausted { .. }) => Error::RetrySameCommand {
            command,
            operation,
            cause: Box::new(error),
        },
    })
}

/// The longest refusal Cloud writes for a person; a longer one is not Cloud's.
const CLOUD_REFUSAL_MAX: usize = 500;

/// Strings this long are credentials or values the request sent, never Cloud's prose.
const SENT_MIN: usize = 16;

/// A piece of a sent string this long in a refusal means the refusal echoes it.
const ECHO_FRAGMENT: usize = 12;

/// The most data a refusal may hold and still read as Cloud's prose: the letters of its
/// data-like runs (see [`data_letters`]). A version or an address fits; any dump of a
/// credential, in hex, base64, or byte by byte, does not.
const DATA_MAX: usize = 24;

/// The strings a callback sends: its bearer, its Pairing Credential, and every long
/// string in its body.
fn sent_strings(token: &CloudEnrollToken, body: &EnrollCallback<'_>) -> Vec<String> {
    fn leaves(value: serde_json::Value, into: &mut Vec<String>) {
        match value {
            serde_json::Value::String(text) => into.push(text),
            serde_json::Value::Array(items) => {
                items.into_iter().for_each(|item| leaves(item, into));
            }
            serde_json::Value::Object(fields) => {
                fields
                    .into_iter()
                    .for_each(|(_, field)| leaves(field, into));
            }
            serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            }
        }
    }
    let mut sent = Vec::new();
    if let Ok(body) = serde_json::to_value(body) {
        leaves(body, &mut sent);
    }
    sent.retain(|text| text.len() >= SENT_MIN);
    sent.push(token.as_str().to_owned());
    sent.push(body.pairing_credential.0.clone());
    sent
}

fn coded_refusal_status(body: &str, sent: &[String], operation: &str) -> Error {
    let join_approval = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|mut parsed| {
            let details = parsed.as_object_mut()?.remove("details")?;
            (details == serde_json::json!({ "capability": ployz_core::JOIN_APPROVAL_CAPABILITY }))
                .then(|| parsed.to_string())
        });
    let said = coded_cloud_refusal(join_approval.as_deref().unwrap_or(body), sent);
    Error::CodedConflict {
        body: match (said, join_approval.is_some()) {
            (Some(said), false) => serde_json::json!({ "error": said }).to_string(),
            (None, false) => format!("{operation} rejected"),
            (said, true) => serde_json::json!({
                "error": said.unwrap_or_else(|| format!("{operation} rejected")),
                "details": { "capability": ployz_core::JOIN_APPROVAL_CAPABILITY },
            })
            .to_string(),
        },
    }
}

fn coded_cloud_refusal(body: &str, sent: &[String]) -> Option<String> {
    let serde_json::Value::Object(mut fields) = serde_json::from_str(body).ok()? else {
        return None;
    };
    if fields.len() != 1 {
        return None;
    }
    let serde_json::Value::String(message) = fields.remove("error")? else {
        return None;
    };
    if message.chars().count() > CLOUD_REFUSAL_MAX || message.contains(char::REPLACEMENT_CHARACTER)
    {
        return None;
    }
    let said: String = message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .filter(|c| !invisible(*c))
        .collect();
    let lower = said.to_ascii_lowercase();
    if ["pmet_", "ppair_", "bearer"]
        .iter()
        .any(|prefix| lower.contains(prefix))
    {
        return None;
    }
    cloud_refusal(&serde_json::json!({"error": said}).to_string(), sent)
}

/// Cloud's refusal sentence, when `body` is exactly the `{"error": "..."}` its enroll
/// routes answer with, short, free of control and direction characters, and holding no
/// piece of anything `sent` nor anything that reads as an encoded value.
fn cloud_refusal(body: &str, sent: &[String]) -> Option<String> {
    let serde_json::Value::Object(fields) = serde_json::from_str(body).ok()? else {
        return None;
    };
    let mut fields = fields.into_iter();
    let (Some((name, serde_json::Value::String(said))), None) = (fields.next(), fields.next())
    else {
        return None;
    };
    let said = said.trim();
    (name == "error"
        && !said.is_empty()
        && said.chars().count() <= CLOUD_REFUSAL_MAX
        && !said.chars().any(|c| c.is_control() || invisible(c))
        && !echoes(said, sent))
    .then(|| said.to_owned())
}

/// Characters that reorder or hide text on a terminal.
fn invisible(c: char) -> bool {
    matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
}

/// Whether `said` is not prose Cloud wrote for a person: it holds a piece of a sent
/// string, or more data than [`DATA_MAX`] allows, which any encoding of one would.
fn echoes(said: &str, sent: &[String]) -> bool {
    data_letters(said) > DATA_MAX
        || sent.iter().any(|sent| {
            if sent.len() <= ECHO_FRAGMENT {
                return said.contains(sent.as_str());
            }
            sent.as_bytes()
                .windows(ECHO_FRAGMENT)
                .filter_map(|piece| std::str::from_utf8(piece).ok())
                .any(|piece| said.contains(piece))
        })
}

/// The letters of `said`'s data-like runs. A run is an unbroken stretch of base64 or hex
/// letters, wherever it starts; it is data-like when it holds a digit or is longer than any
/// word, as each run of a hex, base64 or byte-by-byte dump is.
fn data_letters(said: &str) -> usize {
    said.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-')))
        .filter(|run| run.len() > DATA_MAX || run.chars().any(|c| c.is_ascii_digit()))
        .map(str::len)
        .sum()
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
    let coded_conflict = status == reqwest::StatusCode::CONFLICT
        && sent
            .headers()
            .get(ployz_core::cloud_contract::ERROR_CODE_HEADER)
            .is_some_and(|code| code == "CONFLICT");
    let bytes = sent.bytes().await.map_err(classify_http)?;
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(Error::TokenRefused);
    }
    if coded_conflict {
        return Err(Error::CodedConflict {
            body: String::from_utf8_lossy(&bytes).into_owned(),
        });
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
    use ployz_core::error_chain;

    #[tokio::test]
    async fn public_conflict_messages_follow_clouds_enroll_contract() {
        let said = "This Organization's Servers can't be reached. If they were deleted, run `ployz server forget` or use Forget Servers in the dashboard.";
        let legacy = |message: &str| {
            serde_json::json!({"error": message})
                .to_string()
                .into_bytes()
        };
        let cases = [
            (409, Some("CONFLICT"), legacy(said), Some(said)),
            (409, None, legacy(said), Some(said)),
            (409, Some("UNAVAILABLE"), legacy(said), Some(said)),
            (409, Some("conflict"), legacy(said), Some(said)),
            (409, Some("CONFLICT, INTERNAL"), legacy(said), Some(said)),
            (400, Some("CONFLICT"), legacy(said), Some(said)),
            (409, Some("CONFLICT"), b"{FAKE malformed".to_vec(), None),
            (
                409,
                Some("CONFLICT"),
                br#"{"error":"FAKE reply","code":"CONFLICT","_tag":"PublicError"}"#.to_vec(),
                None,
            ),
            (
                409,
                Some("CONFLICT"),
                br#"{"error":"FAKE reply","details":"FAKE detail"}"#.to_vec(),
                None,
            ),
            (409, Some("CONFLICT"), br#"{"error":12}"#.to_vec(), None),
            (409, Some("CONFLICT"), legacy("pmet_FAKEtoken"), None),
            (409, Some("CONFLICT"), legacy("ppair_FAKEpair"), None),
            (409, Some("CONFLICT"), legacy("Bearer FAKEbearer"), None),
            (
                409,
                Some("CONFLICT"),
                legacy("FAKE MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY="),
                None,
            ),
            (
                409,
                Some("CONFLICT"),
                legacy("Check\nMY-SERVER.\u{001b}\u{202e}"),
                Some("Check MY-SERVER."),
            ),
            (409, Some("CONFLICT"), legacy(&"FAKE ".repeat(101)), None),
            (
                409,
                Some("CONFLICT"),
                b"{\"error\":\"FAKE \xff\"}".to_vec(),
                None,
            ),
        ];
        for path in 0..3 {
            for (status, code, answer, expected) in &cases {
                let (listener, url) = listen().await;
                let response = http_response_with_code(*status, answer, *code);
                let server = tokio::spawn(async move {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    read_http(&mut stream).await;
                    tokio::io::AsyncWriteExt::write_all(&mut stream, &response)
                        .await
                        .unwrap();
                });
                let credential = PairingCredential::parse("ppair_FAKEpairing").unwrap();
                let result = match path {
                    0 => enroll(Command::ServerAdd, &url, &token(), &identity(), &|_| {})
                        .await
                        .map(drop),
                    1 => {
                        publish(
                            Command::ServerAdd,
                            &url,
                            &token(),
                            MachineId::random(),
                            &credential,
                            &ManagementCapability::new(
                                ployz_core::ManagementIdentity::from_bytes([3; 32]),
                                [4; 32],
                            ),
                        )
                        .await
                    }
                    _ => callback(
                        Command::ServerEnroll,
                        &url,
                        &token(),
                        MachineId::random(),
                        &credential,
                        None,
                        None,
                    )
                    .await
                    .map(drop),
                };
                server.await.unwrap();
                let error = result.unwrap_err();
                if path == 0 && !(*status == 409 && *code == Some("CONFLICT")) {
                    assert_eq!(
                        error.to_string(),
                        crate::cloud_login::answered(
                            "enroll",
                            *status,
                            &String::from_utf8_lossy(answer)
                        )
                    );
                    continue;
                }
                for printed in [error_chain::row(&error), format!("{error:?}")] {
                    if let Some(expected) = expected {
                        assert!(printed.contains(expected), "path={path}: {printed}");
                    } else {
                        assert!(!printed.contains("FAKE"), "path={path}: {printed}");
                    }
                    if *status == 409 && *code == Some("CONFLICT") {
                        assert!(
                            !printed.contains("without --reset")
                                && !printed.contains("enroll again"),
                            "{printed}"
                        );
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn a_user_facing_callback_conflict_has_no_retry_advice() {
        let said = "This Server's Cluster is already connected to another Organization in Ployz Cloud. Contact Ployz support to move it.";
        let (listener, url) = listen().await;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http(&mut stream).await;
            let answer = serde_json::json!({"error": said}).to_string();
            tokio::io::AsyncWriteExt::write_all(
                &mut stream,
                &http_response_with_code(409, answer.as_bytes(), Some("CONFLICT")),
            )
            .await
            .unwrap();
        });
        let credential = PairingCredential::parse("ppair_FAKEpairing").unwrap();
        let error = callback(
            Command::ServerAdd,
            &url,
            &token(),
            MachineId::random(),
            &credential,
            None,
            None,
        )
        .await
        .unwrap_err();
        server.await.unwrap();
        assert_eq!(error_chain::row(&error), said);
        assert_eq!(
            crate::failure::Failure::from(error).report().code,
            ployz_core::rpc::RpcErrorCode::Conflict
        );
    }

    #[test]
    fn coded_refusals_accept_exactly_five_hundred_characters() {
        let at_limit = format!("{}End.", "Word ".repeat(99));
        let at_limit = format!("{at_limit} ");
        assert_eq!(at_limit.chars().count(), CLOUD_REFUSAL_MAX);
        let body = serde_json::json!({"error": at_limit}).to_string();
        assert_eq!(
            coded_cloud_refusal(&body, &[]),
            Some(at_limit.trim().to_owned())
        );
        let body = serde_json::json!({"error": format!("{at_limit}x")}).to_string();
        assert_eq!(coded_cloud_refusal(&body, &[]), None);
    }

    #[tokio::test(start_paused = true)]
    async fn uncoded_callback_refusals_keep_the_retry_advice() {
        let upgrade = "This Server runs Ployz 0.3.2, which can't finish joining a cluster that has a Sealing Key. Update ployz where you run ployz server add: curl -fsSL https://ployz.sh | sh (with sudo on the Server). Then rerun ployz server add while a server that holds your cluster's key is online.";
        let ticker = tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        for (status, said) in [
            (409, "The request conflicts with the current state."),
            (503, upgrade),
        ] {
            let (listener, url) = listen().await;
            let answer = serde_json::json!({"error": said}).to_string();
            let server = tokio::spawn(async move {
                loop {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    read_http(&mut stream).await;
                    let _ = tokio::io::AsyncWriteExt::write_all(
                        &mut stream,
                        &http_response(status, "Refused", answer.as_bytes()),
                    )
                    .await;
                }
            });
            let credential = PairingCredential::parse("ppair_FAKEpairing").unwrap();
            let error = callback(
                Command::ServerAdd,
                &url,
                &token(),
                MachineId::random(),
                &credential,
                None,
                None,
            )
            .await
            .unwrap_err();
            server.abort();
            assert_eq!(
                error.to_string(),
                "Cloud enrollment completion failed; rerun the same ployz server add command without --reset (keep all other options)."
            );
            let Error::RetrySameCommand { cause, .. } = &error else {
                panic!("{error:?}");
            };
            let last = match cause
                .downcast_ref::<crate::setup_retry::Error<Error>>()
                .unwrap()
            {
                crate::setup_retry::Error::Permanent(last) => last,
                crate::setup_retry::Error::Exhausted {
                    last: Some(last), ..
                } => last,
                error => panic!("{error:?}"),
            };
            assert_eq!(last.to_string(), said);
            assert_eq!(
                crate::failure::Failure::from(error).report().code,
                ployz_core::rpc::RpcErrorCode::Unavailable
            );
        }
        ticker.abort();
    }

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
                join_approval: None,
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

    fn http_response_with_code(status: u16, body: &[u8], code: Option<&str>) -> Vec<u8> {
        let mut response = format!(
            "HTTP/1.1 {status} Refused\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        if let Some(code) = code {
            response.push_str(&format!("Ployz-Error-Code: {code}\r\n"));
        }
        response.push_str("\r\n");
        [response.as_bytes(), body].concat()
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
        assert!(!error_chain::row(&timeout).contains(&hang));

        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let refused = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let connect = post_json(&http, &refused, &token(), &identity())
            .await
            .unwrap_err();
        assert_eq!(connect.to_string(), "Could not connect to Cloud.");
        assert_ne!(timeout.to_string(), connect.to_string());
        let connect = error_chain::row(&connect);
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
            let rendered = error_chain::row(&error);
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
        let rendered = error_chain::row(&error);
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

    /// What `ployz server add` prints when Cloud refuses its completion with `answer`.
    async fn completion_refused_with(answer: Vec<u8>) -> String {
        let (listener, url) = listen().await;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http(&mut stream).await;
            let response = http_response(409, "Conflict", &answer);
            tokio::io::AsyncWriteExt::write_all(&mut stream, &response)
                .await
                .unwrap();
        });
        let credential = PairingCredential::parse("pairing-secret").unwrap();
        let error = callback(
            Command::ServerAdd,
            &url,
            &token(),
            MachineId::random(),
            &credential,
            Some(ployz_core::Sealing::NeedsAccess),
            None,
        )
        .await
        .unwrap_err();
        server.await.unwrap();
        error_chain::row(&error)
    }

    #[tokio::test]
    async fn a_completion_refusal_shows_cloud_s_sentence() {
        for said in [
            "This Server's Cluster is already connected to another Organization in Ployz Cloud. Contact Ployz support to move it.",
            "This Server runs Ployz 0.3.2, which can't finish joining a cluster that has a Sealing Key. Update ployz where you run ployz server add: curl -fsSL https://ployz.sh | sh (with sudo on the Server). Then rerun ployz server add while a server that holds your cluster's key is online.",
        ] {
            let printed = completion_refused_with(
                serde_json::json!({ "error": said })
                    .to_string()
                    .into_bytes(),
            )
            .await;
            assert!(printed.contains(said), "{printed}");
            assert!(!printed.contains("rejected"), "{printed}");
            assert!(printed.contains("without --reset"), "{printed}");
        }
    }

    #[tokio::test]
    async fn a_completion_refusal_shows_nothing_else_from_the_answer() {
        let long = "word ".repeat(120);
        for answer in [
            String::new(),
            "FAKE garbage".to_owned(),
            "<html>FAKE gateway</html>".to_owned(),
            r#"{"error":""}"#.to_owned(),
            r#"{"error":"FAKE said","details":"FAKE detail"}"#.to_owned(),
            r#"{"error":{"code":"conflict","message":"FAKE said","details":{"cause":"FAKE cause"}}}"#
                .to_owned(),
            r#"{"_tag":"PublicError","code":"CONFLICT","message":"FAKE said"}"#.to_owned(),
            r#"{"_tag":"PublicError","code":"INTERNAL","error":"FAKE said"}"#.to_owned(),
            r#"["FAKE said"]"#.to_owned(),
            r#"{"error":"FAKE said\u001b[31m"}"#.to_owned(),
            r#"{"error":"FAKE said\nFAKE line"}"#.to_owned(),
            serde_json::json!({ "error": format!("FAKE {long}") }).to_string(),
            r#"{"error":"Your token pmet_test was refused"}"#.to_owned(),
            r#"{"error":"Your pairing pairing-secret was refused"}"#.to_owned(),
        ] {
            let printed = completion_refused_with(answer.clone().into_bytes()).await;
            assert!(
                printed.contains("HTTP 409: enrollment completion rejected"),
                "{answer}: {printed}"
            );
            for leaked in ["FAKE", "pmet_test", "pairing-secret"] {
                assert!(!printed.contains(leaked), "{answer}: {printed}");
            }
        }
    }

    #[tokio::test]
    async fn a_refusal_echoing_a_credential_in_part_or_encoded_is_not_shown() {
        use base64::Engine as _;
        let token = CloudEnrollToken::parse("pmet_FAKEtoken0123456789abcdefghij").unwrap();
        let pairing = "FAKEpairing0123456789abcdefghijkl";
        let capability =
            ManagementCapability::new(ployz_core::ManagementIdentity::from_bytes([5; 32]), [6; 32]);
        let secret = capability.to_secret_string();
        let half = |text: &str| text[..text.len() / 2].to_owned();
        for echoed in [
            secret.clone(),
            half(&secret),
            hex::encode(capability.client_secret()),
            base64::engine::general_purpose::STANDARD.encode(capability.client_secret()),
            pairing.to_owned(),
            half(pairing),
            base64::engine::general_purpose::STANDARD.encode(pairing),
            token.as_str().to_owned(),
            half(token.as_str()),
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token.as_str()),
            format!("secret:{}", hex::encode(capability.client_secret())),
            format!(
                r#"{{"secret":"{}"}}"#,
                hex::encode(capability.client_secret())
            ),
            format!(
                "<Buffer {}>",
                capability
                    .client_secret()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            format!("{:?}", capability.client_secret()),
            "\u{202e}".to_owned(),
        ] {
            let (listener, url) = listen().await;
            let answer = serde_json::json!({ "error": format!("Refused: {echoed}.") }).to_string();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                read_http(&mut stream).await;
                let response = http_response(409, "Conflict", answer.as_bytes());
                tokio::io::AsyncWriteExt::write_all(&mut stream, &response)
                    .await
                    .unwrap();
            });
            let error = publish(
                Command::ServerAdd,
                &url,
                &token,
                MachineId::random(),
                &PairingCredential::parse(pairing).unwrap(),
                &capability,
            )
            .await
            .unwrap_err();
            server.await.unwrap();
            for printed in [error_chain::row(&error), format!("{error:?}")] {
                assert!(!printed.contains(&echoed), "{echoed}: {printed}");
                assert!(!printed.contains("Refused"), "{echoed}: {printed}");
            }
            assert!(
                error_chain::row(&error).contains("HTTP 409: candidate publication rejected"),
                "{echoed}"
            );
        }
    }

    #[tokio::test]
    async fn callback_errors_redact_capability_and_pairing_credentials() {
        for (publishing, as_cloud) in [(true, false), (true, true), (false, false), (false, true)] {
            let (listener, url) = listen().await;
            let capability = ManagementCapability::new(
                ployz_core::ManagementIdentity::from_bytes([3; 32]),
                [4; 32],
            );
            let secret = capability.to_secret_string();
            let pairing = "pairing-secret";
            let mut echoed = format!("{secret} {pairing}");
            if as_cloud {
                // Cloud's own shape, so only the credentials in it keep it from printing.
                echoed = serde_json::json!({ "error": echoed }).to_string();
            }
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
            for rendered in [error_chain::row(&error), format!("{error:?}")] {
                assert!(!rendered.contains(&secret));
                assert!(!rendered.contains(pairing));
            }
            assert!(error_chain::row(&error).contains(&format!("HTTP 400: {operation} rejected")));
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

#[cfg(test)]
mod join_capability_tests {
    use super::coded_refusal_status;
    #[test]
    fn join_approval_capability_survives_the_refusal_sanitiser_and_only_from_details() {
        let capability = ployz_core::JOIN_APPROVAL_CAPABILITY;
        let refused = |body: serde_json::Value| {
            coded_refusal_status(
                &body.to_string(),
                &["pmet_secret_token_value".to_owned()],
                "enrollment",
            )
        };
        let approval = refused(serde_json::json!({
            "error": "Approve this join.", "details": {"capability": capability},
        }));
        assert_eq!(approval.capability().as_deref(), Some(capability));
        assert!(approval.to_string().contains("Approve this join."));
        assert_eq!(
            refused(serde_json::json!({"error": capability})).capability(),
            None
        );
        assert_eq!(
            refused(serde_json::json!({
                "error": "x", "details": {"capability": "pmet_secret_token_value"},
            }))
            .capability(),
            None
        );
    }
}
