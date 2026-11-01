//! The `/api/checkin` wire format, and the host facts it is built from.
//!
//! The request side matches `dashboard/src/modules/machines/waiting/checkin-wire.ts`.
//! The probe side reads `ployzd install --probe --json` and its preparation
//! `state.json`, which serialize with serde defaults.

use std::{collections::BTreeMap, time::Duration};

use chrono::{DateTime, SubsecRound, Utc};
use ployz_core::CloudEnrollToken;
use serde::{Deserialize, Serialize};

use crate::cloud_enroll::EnrollStage;

use super::probe;

pub(super) const PROTOCOL_VERSION: u8 = 1;
pub(super) const UNKNOWN_WAIT: Duration = Duration::from_secs(60);
pub(super) const WAIT_CAP: Duration = Duration::from_secs(60 * 60);
pub(crate) const DOCKER_INSTALL_FAILED: &str = "docker_install_failed";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CheckinRequest<'a> {
    pub(super) protocol_version: u8,
    pub(super) checkin_key: &'a str,
    pub(super) cli_version: &'static str,
    pub(super) report: &'a HostReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) prep: Option<Prep>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) probe: Option<probe::Report>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) attempt: Option<&'a Attempt>,
    /// Lets Cloud tell a host installed again under a joined Server's check-in key from that Server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) machine_phase: Option<ployz_core::LocalMachinePhase>,
}

/// The enroll attempt this host is running, or last ran, for Cloud's instruction `attempt_id`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Attempt {
    pub(crate) attempt_id: String,
    pub(crate) stage: EnrollStage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) failure: Option<AttemptFailure>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct AttemptFailure {
    pub(crate) stage: EnrollStage,
    pub(crate) message: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EnrollInstruction {
    pub(crate) attempt_id: String,
    pub(crate) token: CloudEnrollToken,
    pub(crate) name: String,
    pub(crate) storage: EnrollStorage,
    pub(crate) policy: EnrollPolicy,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(crate) enum EnrollStorage {
    None,
    Zfs {
        #[serde(default)]
        device: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EnrollPolicy {
    #[serde(default)]
    pub(crate) labels: BTreeMap<String, String>,
    pub(crate) accepts_builds: bool,
    pub(crate) accepts_services: bool,
    pub(crate) accepts_ingress: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct HostReport {
    pub(super) hostname: String,
    pub(super) os: Os,
    pub(super) kernel: String,
    pub(super) arch: String,
    pub(super) virt: String,
    pub(super) cpu_count: u32,
    pub(super) memory_total_bytes: u64,
    pub(super) disks: Vec<ReportDisk>,
    pub(super) docker: ReportDocker,
    pub(super) cgroup: &'static str,
    pub(super) zfs: ReportZfs,
    pub(super) blockers: Vec<Issue>,
    pub(super) warnings: Vec<Issue>,
}

#[derive(Debug, Serialize)]
pub(super) struct Os {
    pub(super) id: String,
    pub(super) version: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ReportDisk {
    pub(super) path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) by_id: Option<String>,
    pub(super) size_bytes: u64,
    pub(super) role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) available_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(super) enum ReportDocker {
    Absent,
    Present {
        version: String,
        api: String,
        compatible: bool,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(super) enum ReportZfs {
    Eligible {
        targets: Vec<ReportTarget>,
        #[serde(rename = "buildMinutes")]
        build_minutes: u32,
    },
    Ineligible {
        code: String,
        message: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum ReportTarget {
    MainDisk,
    Disk {
        #[serde(rename = "byId")]
        by_id: String,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct Issue {
    pub(crate) code: String,
    pub(crate) message: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct Prep {
    pub(crate) docker: DockerPrep,
    pub(crate) zfs: ZfsPrep,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DockerPrep {
    Installing,
    Ready,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ZfsPrep {
    Installing,
    Ready,
    Failed,
    Skipped,
    Off,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Answer {
    Wait(Wait),
    Probe(Wait, probe::Handout),
    Enroll(Box<EnrollInstruction>),
    Stop(StopReason, String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Wait {
    pub(super) after: Duration,
    /// The Organization has no first server, which an Enroll on arrival token waits on.
    pub(super) no_cluster: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Rejected,
    TokenExpired,
    TokenRevoked,
}

impl StopReason {
    #[must_use]
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Rejected => "Rejected",
            Self::TokenExpired => "Token expired",
            Self::TokenRevoked => "Token revoked",
        }
    }
}

/// Cloud's answer, or None when the body is not a JSON object with a `kind`, which Cloud always
/// sends. A kind or field this CLI does not know waits, since a newer Cloud may send it.
pub(super) fn answer(body: &[u8]) -> Option<Answer> {
    #[derive(Deserialize)]
    #[serde(tag = "kind", rename_all = "lowercase")]
    enum Wire {
        Wait {
            #[serde(rename = "retryAfter")]
            retry_after: f64,
            #[serde(default)]
            probe: Option<serde_json::Value>,
            #[serde(default, rename = "noCluster")]
            no_cluster: bool,
        },
        Enroll(EnrollInstruction),
        Stop {
            reason: StopReason,
            #[serde(default)]
            message: String,
        },
    }
    let body = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    body.get("kind")?.as_str()?;
    Some(match serde_json::from_value::<Wire>(body) {
        Ok(Wire::Wait {
            retry_after,
            probe,
            no_cluster,
        }) => {
            let wait = Wait {
                after: wait_for(retry_after),
                no_cluster,
            };
            match probe.map(serde_json::from_value) {
                Some(Ok(handout)) => Answer::Probe(wait, handout),
                Some(Err(why)) => {
                    crate::ui::note(format!("Could not read Cloud's probe handout: {why}"));
                    Answer::Wait(wait)
                }
                None => Answer::Wait(wait),
            }
        }
        Ok(Wire::Enroll(instruction)) => Answer::Enroll(Box::new(instruction)),
        Ok(Wire::Stop { reason, message }) => Answer::Stop(reason, message),
        Err(_) => Answer::Wait(Wait {
            after: UNKNOWN_WAIT,
            no_cluster: false,
        }),
    })
}

fn wait_for(seconds: f64) -> Duration {
    if seconds.is_finite() && seconds >= 1.0 {
        Duration::from_secs_f64(seconds.min(WAIT_CAP.as_secs_f64()))
    } else {
        Duration::from_secs(1)
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct Probe {
    facts: Facts,
    assessment: Assessment,
}

#[derive(Debug, Deserialize)]
struct Facts {
    os: ProbeOs,
    kernel: String,
    arch: String,
    virt: Virt,
    cgroup: Option<Cgroup>,
    cpu_count: Option<u32>,
    memory_total_bytes: Option<u64>,
    disks: Option<Vec<ProbeDisk>>,
    docker: ProbeDocker,
}

#[derive(Debug, Deserialize)]
struct ProbeOs {
    id: String,
    version_id: Option<String>,
}

#[derive(Debug, Deserialize)]
enum Virt {
    BareMetal,
    Vm { kind: String },
    OpenVz,
    Lxc { privileged: Option<bool> },
    Container { kind: String },
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize)]
enum Cgroup {
    V1,
    V2,
    Hybrid,
}

#[derive(Debug, Deserialize)]
struct ProbeDisk {
    path: String,
    by_id: Option<String>,
    size_bytes: u64,
    role: ProbeRole,
}

#[derive(Debug, Deserialize)]
enum ProbeRole {
    Root { available: u64 },
    Empty,
}

#[derive(Debug, Deserialize)]
enum ProbeDocker {
    Absent,
    Stopped,
    SocketActivated,
    Present { version: String, api: String },
}

#[derive(Debug, Deserialize)]
struct Assessment {
    blockers: Vec<Issue>,
    warnings: Vec<Issue>,
    zfs: ProbeZfs,
}

#[derive(Debug, Deserialize)]
enum ProbeZfs {
    Eligible {
        targets: Vec<ProbeTarget>,
        prepare_minutes: u32,
    },
    Ineligible {
        reason: Issue,
    },
}

#[derive(Debug, Deserialize)]
enum ProbeTarget {
    MainDisk,
    Disk { by_id: String },
}

#[derive(Debug, Deserialize)]
pub(super) struct PrepState {
    pub(super) docker: Step<DockerStep>,
    pub(super) zfs: Step<ZfsStep>,
}

#[derive(Debug, Deserialize)]
pub(super) struct Step<S> {
    #[serde(flatten)]
    pub(super) step: S,
    pub(super) finished_at: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub(super) enum DockerStep {
    Installing,
    Ready,
    Failed { message: String },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub(super) enum ZfsStep {
    Installing,
    Ready,
    Failed { message: String },
    Skipped { reason: String },
    Off,
}

impl PrepState {
    pub(super) fn prep(&self) -> Prep {
        Prep {
            docker: match self.docker.step {
                DockerStep::Installing => DockerPrep::Installing,
                DockerStep::Ready => DockerPrep::Ready,
                DockerStep::Failed { .. } => DockerPrep::Failed,
            },
            zfs: match self.zfs.step {
                ZfsStep::Installing => ZfsPrep::Installing,
                ZfsStep::Ready => ZfsPrep::Ready,
                ZfsStep::Failed { .. } => ZfsPrep::Failed,
                ZfsStep::Skipped { .. } => ZfsPrep::Skipped,
                ZfsStep::Off => ZfsPrep::Off,
            },
        }
    }

    pub(super) fn failed_at(&self) -> Option<&str> {
        let docker = matches!(self.docker.step, DockerStep::Failed { .. })
            .then_some(self.docker.finished_at.as_deref())
            .flatten();
        let zfs = matches!(self.zfs.step, ZfsStep::Failed { .. })
            .then_some(self.zfs.finished_at.as_deref())
            .flatten();
        docker.into_iter().chain(zfs).max()
    }

    /// Whether Docker's install failed before `started`, so the preparation started then tries it
    /// again and the failure no longer stands.
    pub(super) fn docker_retried(&self, started: Option<DateTime<Utc>>) -> bool {
        let failed = matches!(self.docker.step, DockerStep::Failed { .. })
            .then_some(self.docker.finished_at.as_deref())
            .flatten()
            .and_then(|at| DateTime::parse_from_rfc3339(at).ok());
        // ployzd dates the failure to the second, so one in the start's own second is this run's.
        failed
            .zip(started)
            .is_some_and(|(failed, started)| failed < started.trunc_subsecs(0))
    }

    pub(super) const fn fresh() -> Self {
        Self {
            docker: Step {
                step: DockerStep::Installing,
                finished_at: None,
            },
            zfs: Step {
                step: ZfsStep::Installing,
                finished_at: None,
            },
        }
    }

    /// Marks a step still installing as failed, once nothing will run it again.
    pub(super) fn give_up(&mut self, message: &str) {
        if matches!(self.docker.step, DockerStep::Installing) {
            self.docker.step = DockerStep::Failed {
                message: message.to_owned(),
            };
        }
        if matches!(self.zfs.step, ZfsStep::Installing) {
            self.zfs.step = ZfsStep::Failed {
                message: message.to_owned(),
            };
        }
    }

    pub(super) fn installing(&self) -> bool {
        matches!(self.docker.step, DockerStep::Installing)
            || matches!(self.zfs.step, ZfsStep::Installing)
    }
}

impl Probe {
    /// `preparing` is whether `ployz-prepare` was seen running now, not merely assumed to be.
    /// `prepare_started` is when the last preparation was started.
    pub(super) fn report(
        self,
        hostname: String,
        prep: Option<&PrepState>,
        preparing: bool,
        prepare_started: Option<DateTime<Utc>>,
    ) -> HostReport {
        let Self { facts, assessment } = self;
        let Assessment {
            mut blockers,
            mut warnings,
            zfs,
        } = assessment;
        // Part-way through Ployz's own install, dpkg has registered Docker before its unit starts.
        // Before ployzd records that step, a stopped Docker is the user's own.
        if preparing && prep.is_some_and(|prep| matches!(prep.docker.step, DockerStep::Installing))
        {
            blockers.retain(|issue| issue.code != "docker_stopped");
        }
        let compatible = !blockers
            .iter()
            .any(|issue| issue.code.starts_with("docker_") || issue.code.starts_with("buildx_"));
        let mut zfs = match zfs {
            ProbeZfs::Eligible {
                targets,
                prepare_minutes,
            } => ReportZfs::Eligible {
                targets: targets
                    .into_iter()
                    .map(|target| match target {
                        ProbeTarget::MainDisk => ReportTarget::MainDisk,
                        ProbeTarget::Disk { by_id } => ReportTarget::Disk { by_id },
                    })
                    .collect(),
                build_minutes: prepare_minutes,
            },
            ProbeZfs::Ineligible { reason } => ReportZfs::Ineligible {
                code: reason.code,
                message: reason.message,
            },
        };
        if let Some(prep) = prep {
            // Once the unit stops, the state file is final: an older failure the run never
            // rewrote still stands.
            if let DockerStep::Failed { message } = &prep.docker.step
                && !(preparing && prep.docker_retried(prepare_started))
            {
                blockers.push(Issue {
                    code: DOCKER_INSTALL_FAILED.to_owned(),
                    message: message.clone(),
                });
            }
            let zfs_issue = match &prep.zfs.step {
                ZfsStep::Failed { message } => Some(("zfs_build_failed", message)),
                ZfsStep::Skipped { reason } => Some(("zfs_build_skipped", reason)),
                ZfsStep::Installing | ZfsStep::Ready | ZfsStep::Off => None,
            };
            if let Some((code, message)) = zfs_issue {
                let issue = Issue {
                    code: code.to_owned(),
                    message: message.clone(),
                };
                zfs = ReportZfs::Ineligible {
                    code: issue.code.clone(),
                    message: issue.message.clone(),
                };
                warnings.push(issue);
            }
        }
        HostReport {
            hostname,
            os: Os {
                id: facts.os.id,
                version: facts.os.version_id.unwrap_or_default(),
            },
            kernel: facts.kernel,
            arch: facts.arch,
            virt: virt(facts.virt),
            cpu_count: facts.cpu_count.unwrap_or(0),
            memory_total_bytes: facts.memory_total_bytes.unwrap_or(0),
            disks: facts
                .disks
                .unwrap_or_default()
                .into_iter()
                .map(|disk| {
                    let (role, available_bytes) = match disk.role {
                        ProbeRole::Root { available } => ("root", Some(available)),
                        ProbeRole::Empty => ("empty", None),
                    };
                    ReportDisk {
                        path: disk.path,
                        by_id: disk.by_id,
                        size_bytes: disk.size_bytes,
                        role,
                        available_bytes,
                    }
                })
                .collect(),
            docker: match facts.docker {
                ProbeDocker::Present { version, api } => ReportDocker::Present {
                    version,
                    api,
                    compatible,
                },
                ProbeDocker::Absent | ProbeDocker::Stopped | ProbeDocker::SocketActivated => {
                    ReportDocker::Absent
                }
            },
            cgroup: match facts.cgroup {
                Some(Cgroup::V2) => "v2",
                Some(Cgroup::Hybrid) => "hybrid",
                Some(Cgroup::V1) | None => "v1",
            },
            zfs,
            blockers,
            warnings,
        }
    }
}

fn virt(virt: Virt) -> String {
    match virt {
        Virt::BareMetal => "bare_metal".to_owned(),
        Virt::Vm { kind } => format!("vm:{kind}"),
        Virt::OpenVz => "openvz".to_owned(),
        Virt::Lxc {
            privileged: Some(true),
        } => "lxc:privileged".to_owned(),
        Virt::Lxc {
            privileged: Some(false),
        } => "lxc:unprivileged".to_owned(),
        Virt::Lxc { privileged: None } => "lxc".to_owned(),
        Virt::Container { kind } => format!("container:{kind}"),
        Virt::Unknown => "unknown".to_owned(),
    }
}
