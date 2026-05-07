use crate::daemon::ssh::SshOptions;
use ipnet::Ipv4Net;
use ployz_api::{
    MachineAddPayload, MachineAwaitingSelfPublication, MachineInstallOptions, MachineListPayload,
    MachineListRow,
};
use ployz_nats::NatsNodeRpcClient;
use ployz_store_api::StoreDriver;
use ployz_types::model::{MachineId, NetworkId};
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Clone)]
pub(super) struct MachineAddContext {
    pub network_name: String,
    pub network_dir: PathBuf,
    pub network_id: NetworkId,
    pub local_machine_id: MachineId,
    pub cluster_cidr: String,
    pub store: StoreDriver,
    pub nats_rpc: Option<NatsNodeRpcClient>,
    pub ssh_options: SshOptions,
    pub install: MachineInstallOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MachineAddStage {
    Preflight,
    Bootstrapped,
    BootstrapPublished,
    Joined,
    SelfRecorded,
    Ready,
    Enabled,
    Finalized,
}

impl fmt::Display for MachineAddStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Preflight => "preflight",
            Self::Bootstrapped => "bootstrapped",
            Self::BootstrapPublished => "bootstrap-published",
            Self::Joined => "joined",
            Self::SelfRecorded => "self-recorded",
            Self::Ready => "ready",
            Self::Enabled => "enabled",
            Self::Finalized => "finalized",
        };
        f.write_str(value)
    }
}

impl FromStr for MachineAddStage {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "preflight" => Ok(Self::Preflight),
            "bootstrapped" => Ok(Self::Bootstrapped),
            "bootstrap-published" => Ok(Self::BootstrapPublished),
            "joined" => Ok(Self::Joined),
            "self-recorded" => Ok(Self::SelfRecorded),
            "ready" => Ok(Self::Ready),
            "enabled" => Ok(Self::Enabled),
            "finalized" => Ok(Self::Finalized),
            _ => Err(format!("unknown machine add stage '{value}'")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum MachineAddFailure {
    Preflight { reason: String },
    Join { reason: String },
    SelfRecord { reason: String },
    Ready { reason: String },
    Enable { reason: String },
}

impl MachineAddFailure {
    #[must_use]
    pub(super) fn reason(&self) -> &str {
        match self {
            Self::Preflight { reason }
            | Self::Join { reason }
            | Self::SelfRecord { reason }
            | Self::Ready { reason }
            | Self::Enable { reason } => reason,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum MachineAddTargetResult {
    AwaitingSelfPublication {
        target: String,
        joiner_id: MachineId,
    },
    Failed {
        target: String,
        failure: MachineAddFailure,
    },
}

#[derive(Debug, Clone, Default)]
pub(super) struct MachineAddReport {
    pub warnings: Vec<String>,
    pub awaiting_self_publication: Vec<String>,
    pub failed_preflight: Vec<String>,
    pub failed_join: Vec<String>,
    pub failed_self_record: Vec<String>,
    pub failed_ready: Vec<String>,
    pub failed_enable: Vec<String>,
    awaiting_payload: Vec<MachineAwaitingSelfPublication>,
}

impl MachineAddReport {
    #[must_use]
    pub(super) fn with_warnings(warnings: Vec<String>) -> Self {
        Self {
            warnings,
            ..Self::default()
        }
    }

    pub(super) fn push(&mut self, outcome: MachineAddTargetResult) {
        match outcome {
            MachineAddTargetResult::AwaitingSelfPublication { target, joiner_id } => {
                self.awaiting_payload.push(MachineAwaitingSelfPublication {
                    target: target.clone(),
                    joiner_id: joiner_id.0.clone(),
                });
                self.awaiting_self_publication
                    .push(format!("{target} -> {}", joiner_id.0));
            }
            MachineAddTargetResult::Failed { target, failure } => {
                let line = format!("{target}: {}", failure.reason());
                match failure {
                    MachineAddFailure::Preflight { .. } => self.failed_preflight.push(line),
                    MachineAddFailure::Join { .. } => self.failed_join.push(line),
                    MachineAddFailure::SelfRecord { .. } => self.failed_self_record.push(line),
                    MachineAddFailure::Ready { .. } => self.failed_ready.push(line),
                    MachineAddFailure::Enable { .. } => self.failed_enable.push(line),
                }
            }
        }
    }

    #[must_use]
    pub(super) fn has_failures(&self) -> bool {
        !self.failed_preflight.is_empty()
            || !self.failed_join.is_empty()
            || !self.failed_self_record.is_empty()
            || !self.failed_ready.is_empty()
            || !self.failed_enable.is_empty()
    }

    #[must_use]
    pub(super) fn payload(&self) -> MachineAddPayload {
        MachineAddPayload {
            warnings: self.warnings.clone(),
            awaiting_self_publication: self.awaiting_payload.clone(),
            failed_preflight: self.failed_preflight.clone(),
            failed_join: self.failed_join.clone(),
            failed_self_record: self.failed_self_record.clone(),
            failed_ready: self.failed_ready.clone(),
            failed_enable: self.failed_enable.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MachineListReport {
    pub rows: Vec<MachineListReportRow>,
}

impl MachineListReport {
    #[must_use]
    pub(super) fn payload(&self) -> MachineListPayload {
        MachineListPayload {
            rows: self
                .rows
                .iter()
                .map(MachineListReportRow::payload)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MachineListReportRow {
    pub id: String,
    pub lifecycle: &'static str,
    pub region: String,
    pub availability_zone: Option<String>,
    pub availability_zone_display: String,
    pub overlay: String,
    pub subnet: Option<Ipv4Net>,
    pub subnet_display: String,
    pub created_at: u64,
    pub created_display: String,
}

impl MachineListReportRow {
    #[must_use]
    fn payload(&self) -> MachineListRow {
        MachineListRow {
            id: self.id.clone(),
            lifecycle: self.lifecycle.into(),
            region: self.region.clone(),
            availability_zone: self.availability_zone.clone(),
            overlay_ip: self.overlay.clone(),
            subnet: self.subnet.map(|subnet| subnet.to_string()),
            created_at: self.created_at,
        }
    }
}
