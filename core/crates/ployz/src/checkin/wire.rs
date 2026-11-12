//! The `/api/checkin` wire format, and the host facts it is built from.
//!
//! The request side matches `dashboard/src/modules/machines/waiting/checkin-wire.ts`.
//! The probe side reads `ployzd install --probe --json`, whose document core owns, and its
//! preparation `state.json`, which serialize with serde defaults.

use std::{collections::BTreeMap, time::Duration};

use chrono::{DateTime, SubsecRound, Utc};
use ployz_core::CloudEnrollToken;
use ployz_core::host_preparation::{DockerOutcome, PrepStep, PrepSteps, ZfsOutcome};
use ployz_core::probe_document::{
    Assessment, Blocker, BlockerCode, CgroupVersion, DiskRole, DockerFacts, ProbeDocument,
    Virtualization, ZfsEligibility, ZfsTarget,
};
use serde::{Deserialize, Serialize};

use crate::cloud_enroll::EnrollStage;

use super::probe;

pub(super) const UNKNOWN_WAIT: Duration = Duration::from_secs(60);
pub(super) const WAIT_CAP: Duration = Duration::from_secs(60 * 60);
pub(crate) const DOCKER_INSTALL_FAILED: &str = "docker_install_failed";
const GAVE_UP: &str = "Preparation stopped mid-step on every try. Run sudo ployz server checkin --install to try again.";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CheckinRequest<'a> {
    pub(super) protocol_version: u8,
    pub(super) checkin_key: &'a str,
    pub(super) cli_version: &'static str,
    /// The optional instruction fields this CLI carries out, so Cloud never sends one an
    /// older CLI would drop.
    pub(super) enroll_options: &'static [&'static str],
    pub(super) report: &'a HostReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) prep: Option<Prep>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) probe: Option<probe::Report>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) attempt: Option<&'a Attempt>,
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

impl Attempt {
    /// Fail the attempt at the `live` stage of the run, unless the attempt, already raised to
    /// `live`, holds a committed stage. So a committed attempt this release fails never fails
    /// below Joining. A record from a newer release can still carry a failure this release
    /// reads as Unknown.
    pub(crate) fn fail(&mut self, live: EnrollStage, message: String) -> &AttemptFailure {
        let stage = if self.stage.committed() {
            self.stage
        } else {
            live
        };
        self.failure.insert(AttemptFailure { stage, message })
    }
}

/// This build writes it only through [`Attempt::fail`]. A record an older build wrote can still
/// hold a committed attempt with a failure below Joining.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct AttemptFailure {
    stage: EnrollStage,
    message: String,
}

impl AttemptFailure {
    pub(crate) const fn stage(&self) -> EnrollStage {
        self.stage
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EnrollInstruction {
    pub(crate) attempt_id: String,
    pub(crate) token: CloudEnrollToken,
    pub(crate) name: String,
    pub(crate) storage: EnrollStorage,
    pub(crate) policy: EnrollPolicy,
    #[serde(default, rename = "clusterName")]
    pub(crate) founding_cluster_name: Option<String>,
    /// Found the Cluster with no Door open. Cloud sends it only to a CLI whose check-in
    /// lists it in `enrollOptions`.
    #[serde(default, rename = "ingressClosed")]
    pub(crate) founding_ingress_closed: bool,
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

/// What check-in judges from ployzd's record of host preparation.
pub(super) trait PrepRecord {
    /// Whether a step has yet to finish. A step this release can't read counts too: only a run of
    /// ployzd rewrites it.
    fn unfinished(&self) -> bool;

    /// Whether a step finished failed.
    fn failed(&self) -> bool;

    /// When the latest failed step failed, when its time reads.
    fn failed_at(&self) -> Option<DateTime<Utc>>;

    /// The latest time either step records.
    fn last_dated(&self) -> Option<DateTime<Utc>>;

    /// Each step as check-in reports it. Once it has given up, a step left unfinished counts as
    /// failed.
    fn prep(&self, gave_up: bool) -> Prep;

    /// Whether Docker's install failed before `started`, so the preparation started then tries it
    /// again and the failure no longer stands.
    fn docker_retried(&self, started: Option<DateTime<Utc>>) -> bool;
}

/// Whether a step has yet to finish. A step this release can't read counts too: only a run of
/// ployzd rewrites it.
const fn step_unfinished<O>(step: &PrepStep<O>) -> bool {
    match step {
        PrepStep::NotStarted | PrepStep::Running { .. } | PrepStep::Unknown(_) => true,
        PrepStep::Finished { .. } => false,
    }
}

fn time(at: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(at).ok().map(|at| at.to_utc())
}

/// How check-in has judged a step it reports.
enum Reported<'s, O> {
    Unfinished,
    Finished(&'s O),
    /// Check-in gave up on it unfinished.
    GaveUp,
}

impl PrepRecord for PrepSteps {
    fn unfinished(&self) -> bool {
        step_unfinished(&self.docker) || step_unfinished(&self.zfs)
    }

    fn failed(&self) -> bool {
        self.docker.failed_at().is_some() || self.zfs.failed_at().is_some()
    }

    fn failed_at(&self) -> Option<DateTime<Utc>> {
        let docker = self.docker.failed_at().and_then(time);
        docker.max(self.zfs.failed_at().and_then(time))
    }

    fn last_dated(&self) -> Option<DateTime<Utc>> {
        let docker = self.docker.last_dated().and_then(time);
        docker.max(self.zfs.last_dated().and_then(time))
    }

    fn prep(&self, gave_up: bool) -> Prep {
        Prep {
            docker: match reported(&self.docker, gave_up) {
                Reported::Unfinished => DockerPrep::Installing,
                Reported::Finished(DockerOutcome::Ready) => DockerPrep::Ready,
                Reported::Finished(DockerOutcome::Failed { .. }) | Reported::GaveUp => {
                    DockerPrep::Failed
                }
            },
            zfs: match reported(&self.zfs, gave_up) {
                Reported::Unfinished => ZfsPrep::Installing,
                Reported::Finished(ZfsOutcome::Ready) => ZfsPrep::Ready,
                Reported::Finished(ZfsOutcome::Failed { .. }) | Reported::GaveUp => ZfsPrep::Failed,
                Reported::Finished(ZfsOutcome::Skipped { .. }) => ZfsPrep::Skipped,
                Reported::Finished(ZfsOutcome::Off) => ZfsPrep::Off,
            },
        }
    }

    fn docker_retried(&self, started: Option<DateTime<Utc>>) -> bool {
        let failed = self.docker.failed_at().and_then(time);
        // ployzd dates the failure to the second, so one in the start's own second is this run's.
        failed
            .zip(started)
            .is_some_and(|(failed, started)| failed < started.trunc_subsecs(0))
    }
}

fn reported<O>(step: &PrepStep<O>, gave_up: bool) -> Reported<'_, O> {
    match step.outcome() {
        Some(outcome) => Reported::Finished(outcome),
        None if gave_up => Reported::GaveUp,
        None => Reported::Unfinished,
    }
}

impl From<Blocker> for Issue {
    fn from(blocker: Blocker) -> Self {
        Self {
            code: blocker.code.as_str().to_owned(),
            message: blocker.message,
        }
    }
}

/// What this host reports to Cloud, from ployzd's probe and its record of preparation.
/// `preparing` is whether preparation was seen running now, by its unit or its lock's mark,
/// not merely assumed to be. `gave_up` is whether check-in gave up on what preparation left unfinished.
/// `prepare_started` is when the last preparation was started.
pub(super) fn host_report(
    probe: ProbeDocument,
    hostname: String,
    prep: &PrepSteps,
    preparing: bool,
    gave_up: bool,
    prepare_started: Option<DateTime<Utc>>,
) -> HostReport {
    let ProbeDocument { facts, assessment } = probe;
    let Assessment {
        mut blockers,
        warnings,
        zfs,
    } = assessment;
    // Part-way through Ployz's own install, dpkg has registered Docker before its unit starts.
    // Before ployzd records that step, a stopped Docker is the user's own.
    if preparing && matches!(prep.docker, PrepStep::Running { .. }) {
        blockers.retain(|blocker| blocker.code != BlockerCode::DockerStopped);
    }
    let compatible = !blockers.iter().any(|blocker| {
        let code = blocker.code.as_str();
        code.starts_with("docker_") || code.starts_with("buildx_")
    });
    let mut blockers: Vec<Issue> = blockers.into_iter().map(Issue::from).collect();
    let mut warnings: Vec<Issue> = warnings.into_iter().map(Issue::from).collect();
    let mut zfs = match zfs {
        ZfsEligibility::Eligible {
            targets,
            prepare_minutes,
        } => ReportZfs::Eligible {
            targets: targets
                .into_iter()
                .map(|target| match target {
                    ZfsTarget::MainDisk => ReportTarget::MainDisk,
                    ZfsTarget::Disk { by_id } => ReportTarget::Disk { by_id },
                })
                .collect(),
            build_minutes: prepare_minutes,
        },
        ZfsEligibility::Ineligible { reason } => ReportZfs::Ineligible {
            code: reason.code.as_str().to_owned(),
            message: reason.message,
        },
    };
    // Once preparation is no longer seen running, by its unit or its lock's mark, the state
    // file is final: an older failure the run never rewrote still stands.
    let docker_failure = match reported(&prep.docker, gave_up) {
        Reported::Finished(DockerOutcome::Failed { message })
            if !(preparing && prep.docker_retried(prepare_started)) =>
        {
            Some(message.as_str())
        }
        Reported::GaveUp => Some(GAVE_UP),
        Reported::Unfinished
        | Reported::Finished(DockerOutcome::Ready | DockerOutcome::Failed { .. }) => None,
    };
    if let Some(message) = docker_failure {
        blockers.push(Issue {
            code: DOCKER_INSTALL_FAILED.to_owned(),
            message: message.to_owned(),
        });
    }
    let zfs_issue = match reported(&prep.zfs, gave_up) {
        Reported::Finished(ZfsOutcome::Failed { message }) => {
            Some(("zfs_build_failed", message.as_str()))
        }
        Reported::Finished(ZfsOutcome::Skipped { reason }) => {
            Some(("zfs_build_skipped", reason.as_str()))
        }
        Reported::GaveUp => Some(("zfs_build_failed", GAVE_UP)),
        Reported::Unfinished | Reported::Finished(ZfsOutcome::Ready | ZfsOutcome::Off) => None,
    };
    if let Some((code, message)) = zfs_issue {
        let issue = Issue {
            code: code.to_owned(),
            message: message.to_owned(),
        };
        zfs = ReportZfs::Ineligible {
            code: issue.code.clone(),
            message: issue.message.clone(),
        };
        warnings.push(issue);
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
                    DiskRole::Root { available } => ("root", Some(available)),
                    DiskRole::Empty => ("empty", None),
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
            DockerFacts::Present { version, api, .. } => ReportDocker::Present {
                version,
                api,
                compatible,
            },
            DockerFacts::Absent | DockerFacts::Stopped | DockerFacts::SocketActivated => {
                ReportDocker::Absent
            }
        },
        cgroup: match facts.cgroup {
            Some(CgroupVersion::V2) => "v2",
            Some(CgroupVersion::Hybrid) => "hybrid",
            Some(CgroupVersion::V1) | None => "v1",
        },
        zfs,
        blockers,
        warnings,
    }
}

fn virt(virt: Virtualization) -> String {
    match virt {
        Virtualization::BareMetal => "bare_metal".to_owned(),
        Virtualization::Vm { kind } => format!("vm:{kind}"),
        Virtualization::OpenVz => "openvz".to_owned(),
        Virtualization::Lxc {
            privileged: Some(true),
        } => "lxc:privileged".to_owned(),
        Virtualization::Lxc {
            privileged: Some(false),
        } => "lxc:unprivileged".to_owned(),
        Virtualization::Lxc { privileged: None } => "lxc".to_owned(),
        Virtualization::Container { kind } => format!("container:{kind}"),
        Virtualization::Unknown => "unknown".to_owned(),
    }
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod tests;
