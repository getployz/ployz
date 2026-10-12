//! What Ployz Cloud answers about Servers: the shapes a host returns for each
//! [`CloudQuery`](super::CloudQuery) and for an [`Act`](super::Act). Plans decode
//! these and never see a Machine directly.

use ployz_core::metrics::MarkerKind;
use ployz_core::{MachineId, Namespace, ServiceName};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::logs::LogText;
use super::step::{AttemptId, Freshness, ServerName, ServerRef, Timestamp};
use crate::{DeploymentId, VolumeName};

/// The answer to `CloudQuery::Servers`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ServersAnswer {
    pub servers: Vec<ServerReport>,
    pub seen: Freshness,
}

/// One Server as it last reported.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ServerReport {
    pub machine: MachineId,
    pub name: ServerName,
    pub state: ServerState,
    /// The Ployz release it runs.
    pub release: Option<String>,
    /// A newer release on its channel, if any.
    pub upgrade: Option<String>,
    pub draining: bool,
    pub disk: Option<Disk>,
    /// Every Namespace it runs containers or holds volumes for.
    pub namespaces: Vec<Namespace>,
}

impl ServerReport {
    #[must_use]
    pub fn reference(&self) -> ServerRef {
        ServerRef {
            machine: self.machine,
            name: self.name.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ServerState {
    Online,
    Offline,
    Removing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Disk {
    pub used_bytes: u64,
    pub total_bytes: u64,
}

/// The answer to `CloudQuery::Containers`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ContainersAnswer {
    pub containers: Vec<ContainerReport>,
    /// The Servers asked; `seen.unanswered` names those that did not answer.
    pub servers: Vec<ServerName>,
    pub seen: Freshness,
}

/// One container of a Service, by the runtime name its containers carry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ContainerReport {
    pub server: ServerName,
    pub service: ServiceName,
    pub state: ContainerState,
    pub exit_code: Option<i64>,
    pub health: Option<Health>,
    /// Absent when the Server does not report it.
    pub restarts: Option<u32>,
    pub image: String,
    pub deployment: Option<DeploymentId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ContainerState {
    Created,
    Running,
    Restarting,
    Paused,
    Exited,
    Dead,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Starting,
    Healthy,
    Unhealthy,
}

/// The answer to `CloudQuery::Logs`: each Server's lines, already scrubbed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct LogsAnswer {
    pub servers: Vec<ServerLog>,
    pub seen: Freshness,
    /// Why each unanswered Server did not answer.
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ServerLog {
    pub server: ServerName,
    pub text: LogText,
}

/// The answer to `CloudQuery::Metrics`: what `ployz metrics` prints.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct MetricsAnswer {
    pub since: Timestamp,
    pub until: Timestamp,
    /// Empty when no Server reported a reading in the range.
    pub metrics: Vec<MetricRow>,
    pub oom_kills: u32,
    pub exits: u32,
    pub restarts: u32,
    /// The newest OOM kills, exits and restarts, newest first.
    pub events: Vec<MetricEvent>,
    /// Spans with no readings, and why a Server did not answer.
    pub notes: Vec<String>,
    pub seen: Freshness,
}

/// One metric over the range. A metric with no reading has none of the three.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct MetricRow {
    pub metric: String,
    /// Absent when the newest reading is too old to be current.
    pub now: Option<String>,
    pub avg: Option<String>,
    pub peak: Option<String>,
}

/// Something Docker did to a Service container.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct MetricEvent {
    pub at: Timestamp,
    pub server: ServerName,
    pub namespace: Namespace,
    pub service: ServiceName,
    pub deployment: Option<DeploymentId>,
    #[serde(flatten)]
    #[ts(flatten)]
    pub kind: MarkerKind,
}

/// The answer to `CloudQuery::Copies`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct CopiesAnswer {
    pub volumes: Vec<VolumeCopies>,
    pub seen: Freshness,
}

/// Where one Volume's data is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct VolumeCopies {
    pub volume: VolumeName,
    /// The Server with its writable copy.
    pub writer: Option<ServerRef>,
    pub mirror: Option<Mirror>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct Mirror {
    pub server: ServerRef,
    pub synced_at: Option<Timestamp>,
}

/// The answer to `CloudQuery::Operations`, newest first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct OperationsAnswer {
    pub operations: Vec<Operation>,
}

/// One requested operation and how it ended so far. An [`Act`](super::Act)
/// answers with its row; the same attempt answers with the same row.
///
/// A refused act carries, in its details: `run` for the run already going,
/// `writes` when the reply was lost, `fix`, and for a release after the
/// handover `move_to`, the server the move hands the Volume to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct Operation {
    pub attempt: AttemptId,
    /// What was asked, in words, e.g. `restart web`.
    pub run: String,
    pub state: RunState,
    pub requested_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub reason: Option<String>,
    pub servers: Vec<Leg>,
    /// Services a drain left in place, and why.
    pub kept: Vec<Kept>,
    pub unanswered: Vec<ServerName>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Accepted,
    Running,
    Done,
    Failed,
    Unknown,
    /// A Deploy replaced the container before the step ran.
    Superseded,
}

impl RunState {
    #[must_use]
    pub const fn ended(self) -> bool {
        !matches!(self, Self::Accepted | Self::Running)
    }
}

/// What one Server did for an operation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct Leg {
    pub server: ServerName,
    pub stopped: Option<Receipt>,
    pub started: Option<Receipt>,
    pub reason: Option<String>,
}

/// Whether a Server confirmed a step. A missing receipt is `unknown`, never `no`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Receipt {
    Yes,
    No,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct Kept {
    pub service: ServiceName,
    pub why: String,
}
