//! The step machine's vocabulary: what a tool asks for, what it writes, and the
//! keys that make a retried write land once.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ployz_core::{MachineId, MachineName, Namespace, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::LineCap;
use super::machine::Stop;
use super::refusal::{Refusal, ToolResult};
use crate::github::{GithubFileQuery, GithubTreeQuery};
use crate::{Command, DeploymentId, EnvironmentId, Query, VolumeId, VolumeName};

/// A Server's name. Machine names stand in until Servers get their own.
pub type ServerName = MachineName;

/// What a tool needs next, or what it answered.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    /// Answer every ask, in order, then resume.
    Read { asks: Vec<Ask> },
    /// Run the one write, then resume with its answer.
    Write { write: Box<Write> },
    /// Sleep `ms`, answer every ask in `then`, then resume.
    Wait { ms: u32, then: Vec<Ask> },
    /// The call's result.
    Done { result: ToolResult },
}

/// The one right to write in a call. Only [`WriteSlot::spend`] builds a
/// [`Step::Write`], and a call holds one slot.
#[derive(Debug)]
pub struct WriteSlot(());

impl WriteSlot {
    pub(crate) fn new() -> Self {
        Self(())
    }

    /// The write step, spending the slot.
    pub(crate) fn spend(self, write: Write) -> Step {
        Step::Write {
            write: Box::new(write),
        }
    }
}

/// One read a step needs answered.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "at", content = "ask", rename_all = "snake_case")]
pub enum Ask {
    /// A Config Store query.
    Store(Query),
    /// A question only Cloud answers.
    Cloud(CloudQuery),
}

/// The one write a call may make.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "at", content = "write", rename_all = "snake_case")]
pub enum Write {
    /// A Config Store command.
    Store(Command),
    /// An operation Cloud runs on Servers.
    Act(Act),
}

/// A question Cloud answers from GitHub or from its Servers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "query", rename_all = "snake_case")]
pub enum CloudQuery {
    GithubTree(GithubTreeQuery),
    GithubFile(GithubFileQuery),
    GithubConnection,
    GithubBranches {
        repository: String,
    },
    Servers {
        only: Option<ServerName>,
    },
    Containers {
        namespace: Namespace,
        service: Option<ServiceName>,
    },
    Logs(LogsQuery),
    Copies {
        namespace: Namespace,
        volume: Option<VolumeName>,
    },
    Operations(OperationsQuery),
}

/// Log lines of one Service's containers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogsQuery {
    pub environment: EnvironmentId,
    pub namespace: Namespace,
    pub service: ServiceName,
    pub deployment: Option<DeploymentId>,
    pub server: Option<ServerRef>,
    pub lines: LineCap,
    pub since: Option<Duration>,
}

/// Recent operations on one subject.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OperationsQuery {
    pub subject: Subject,
    pub attempt: Option<AttemptId>,
    pub limit: u8,
}

/// What an operation ran on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Subject {
    Server {
        machine: MachineId,
    },
    Service {
        environment: EnvironmentId,
        service: ServiceName,
    },
    Volume {
        volume: VolumeId,
    },
    Leftover {
        namespace: Namespace,
    },
}

/// A Server by ID, with the name it had when the call read it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerRef {
    pub machine: MachineId,
    pub name: ServerName,
}

/// An operation Cloud runs once per attempt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Act {
    pub attempt: AttemptId,
    pub digest: BodyDigest,
    pub action: Action,
}

/// What an [`Act`] does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    Service {
        environment: EnvironmentId,
        service: ServiceName,
        only: Option<ServerRef>,
        verb: ServiceVerb,
    },
    Drain {
        server: ServerRef,
    },
    Upgrade {
        server: ServerRef,
    },
    Clean {
        leftover: Namespace,
        loss: Option<AcceptedLoss>,
    },
    RemoveServer {
        server: ServerRef,
        loss: Option<AcceptedLoss>,
    },
    Volume {
        volume: VolumeId,
        run: VolumeRun,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceVerb {
    Restart,
    Start,
    Stop,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "run", rename_all = "snake_case")]
pub enum VolumeRun {
    Mirror { to: ServerRef, full: bool },
    Sync { full: bool },
    Move { to: ServerRef },
    Release { loss: Option<AcceptedLoss> },
    RemoveMirror { loss: Option<AcceptedLoss> },
}

/// Volumes whose data a caller accepted losing, bound to the version it read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AcceptedLoss {
    pub volumes: Vec<String>,
    pub version: String,
}

impl AcceptedLoss {
    /// The loss a call accepted: none when it names no Volume.
    ///
    /// # Errors
    /// `invalid_argument` on `version` when Volumes come without one.
    pub(crate) fn from_input(
        volumes: Vec<String>,
        version: Option<String>,
    ) -> Result<Option<Self>, Stop> {
        if volumes.is_empty() {
            return Ok(None);
        }
        let Some(version) = version else {
            return Err(Refusal::invalid(
                "Accepting Volume loss needs the version the refusal named",
                "version",
            )
            .into());
        };
        Ok(Some(Self { volumes, version }))
    }
}

/// Who asks a human, if anyone.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Gate {
    Never,
    Store,
    Cloud,
}

impl Gate {
    /// The sentence every tool description ends with.
    #[must_use]
    pub const fn sentence(self) -> &'static str {
        match self {
            Self::Never => "Never asks a human to approve.",
            Self::Store | Self::Cloud => {
                "It may stop to ask a human to approve; call it alone, never alongside another tool."
            }
        }
    }
}

/// The caller's key for one intended write: the same key lands it once.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RequestKey(String);

impl RequestKey {
    /// A key of 8 to 64 letters, digits, `_` or `-`.
    ///
    /// # Errors
    /// Why anything else is not a key.
    pub fn parse(key: impl Into<String>) -> Result<Self, &'static str> {
        let key = key.into();
        let fits = (8..=64).contains(&key.len())
            && key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
        match fits {
            true => Ok(Self(key)),
            false => Err("A request key is 8 to 64 letters, digits, '_' or '-'"),
        }
    }

    /// The caller's key, else one derived from the transport's call key, so a
    /// replayed call keeps its key.
    #[must_use]
    pub fn effective(given: Option<Self>, call_key: &str) -> Self {
        given.unwrap_or_else(|| {
            let digest = ring::digest::digest(&ring::digest::SHA256, call_key.as_bytes());
            let mut text = URL_SAFE_NO_PAD.encode(digest.as_ref());
            text.truncate(22);
            Self(text)
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RequestKey {
    type Error = String;

    fn try_from(key: String) -> Result<Self, String> {
        Self::parse(key).map_err(str::to_owned)
    }
}

impl From<RequestKey> for String {
    fn from(key: RequestKey) -> Self {
        key.0
    }
}

/// One attempt at a write, the same for every retry with the same key.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AttemptId(Uuid);

impl AttemptId {
    /// The ID for `tool` under `key`, as Cloud's `uuidFrom` derives it.
    #[must_use]
    pub fn derive(tool: &str, key: &RequestKey) -> Self {
        let seed = format!("{tool}:{}", key.as_str());
        let digest = ring::digest::digest(&ring::digest::SHA256, seed.as_bytes());
        let bytes = digest
            .as_ref()
            .get(..16)
            .and_then(|head| <[u8; 16]>::try_from(head).ok())
            .unwrap_or_default();
        // Version 8 with the RFC 4122 variant: the bits `uuidFrom` sets.
        Self(uuid::Builder::from_custom_bytes(bytes).into_uuid())
    }

    #[must_use]
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl std::fmt::Display for AttemptId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// The SHA-256 of a call's canonical input, without its wait and key: a reused
/// key with another body is a conflict.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BodyDigest([u8; 32]);

impl BodyDigest {
    #[must_use]
    pub fn of(tool: &str, input: &Value) -> Self {
        let mut input = canonical(input);
        if let Value::Object(fields) = &mut input {
            fields.remove("wait_seconds");
            fields.remove("request_key");
        }
        let body = format!("{tool}\n{input}");
        let digest = ring::digest::digest(&ring::digest::SHA256, body.as_bytes());
        let mut bytes = [0_u8; 32];
        bytes.copy_from_slice(digest.as_ref());
        Self(bytes)
    }
}

impl TryFrom<String> for BodyDigest {
    type Error = String;

    fn try_from(text: String) -> Result<Self, String> {
        let bytes = hex::decode(text).map_err(|_| "a hex SHA-256".to_owned())?;
        bytes
            .try_into()
            .map(Self)
            .map_err(|_| "a hex SHA-256".to_owned())
    }
}

impl From<BodyDigest> for String {
    fn from(digest: BodyDigest) -> Self {
        hex::encode(digest.0)
    }
}

/// `value` with every object's keys sorted.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(fields) => {
            let mut sorted = fields.iter().collect::<Vec<_>>();
            sorted.sort_by(|a, b| a.0.cmp(b.0));
            Value::Object(
                sorted
                    .into_iter()
                    .map(|(key, value)| (key.clone(), canonical(value)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => value.clone(),
    }
}

/// Seconds since the Unix epoch.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub i64);

/// How fresh an answer from Servers is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Freshness {
    pub seen_by: Option<ServerName>,
    pub observed_at: Option<Timestamp>,
    pub read_at: Timestamp,
    pub age_seconds: Option<u32>,
    pub from: Source,
    pub unanswered: Vec<ServerName>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Server,
    CloudCache,
}

/// What a human does to unblock a refusal, when no tool can.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "fix", rename_all = "snake_case")]
pub enum Fix {
    UseCloud,
    ConnectServers,
    OpenDashboard { page: DashboardPage },
    UpdateClient,
    ServerRemove { server: ServerName },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DashboardPage {
    Projects,
    Environments,
    Variables,
    Servers,
    Github,
}

#[cfg(test)]
#[path = "step_tests.rs"]
mod tests;
