//! Running Cloud's enroll instruction in-process, as `ployz server add` does, while the
//! loop keeps reporting the attempt's stage on each check-in.

use ployz_core::error_chain;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use ployz_core::cloud_contract::{CHECKIN_PROTOCOL_VERSION, ENROLL_OPTIONS};
use tokio::sync::watch;

use super::plan::{Next, Pass, read_prep_state};
use super::status::{LocalCheckin, Seen, record};
use super::{Attempt, EnrollInstruction, Prep};
use super::{
    CheckinClient, CheckinEnd, CheckinKey, Host, LoopControl, Paths, System, ZfsPrep, client, unit,
    wire::{
        Answer, AttemptFailure, CheckinRequest, EnrollStorage, HostReport, PrepRecord, StopReason,
    },
};
use crate::cloud::enroll::EnrollStage;

const HEARTBEAT: Duration = Duration::from_secs(15);
const ROOT_CONFIG: &str = "/root/.config/ployz/config.yaml";

/// Enrollment committed; a failed follow-up does not undo the join.
#[derive(Clone, Debug)]
pub(crate) struct Committed {
    pub(crate) follow_up: Option<String>,
}

impl Committed {
    fn from_setup(setup: crate::handlers::cloud::Setup) -> Result<Self, String> {
        setup
            .map_err(|error| failure_message(&error))
            .map(|follow_up| Self {
                follow_up: follow_up.err().map(|error| failure_message(&error)),
            })
    }
}

pub(super) struct Report<'a> {
    pub(super) report: &'a HostReport,
    pub(super) prep: Prep,
}

pub(super) fn title(stage: EnrollStage) -> &'static str {
    match stage {
        EnrollStage::Unknown => "Unknown stage",
        EnrollStage::Installing => "Installing",
        EnrollStage::Storage => "Setting up ZFS",
        EnrollStage::Enrolling => "Enrolling",
        EnrollStage::Queued => "Queued to join",
        EnrollStage::Joining => "Joining",
        EnrollStage::Publishing => "Publishing",
    }
}

pub(super) fn failure_line(failure: &AttemptFailure) -> String {
    format!(
        "Enroll failed at {}: {}",
        title(failure.stage()).to_lowercase(),
        failure.message()
    )
}

/// Run `instruction`, reporting monotonic progress on changes and every [`HEARTBEAT`].
/// Rejection awaits installation and stops at live Enrolling or Queued. Once the
/// attempt has reported Joining, saved or in this run, rejection cannot cancel its
/// committed join. A successful join is recorded for cleanup; a failure keeps its
/// live stage, or the committed stage it holds, for the next check-in and returns
/// to polling.
pub(super) async fn attempt<H: Host>(
    host: &H,
    client: &CheckinClient,
    key: &CheckinKey,
    paths: &Paths,
    report: &Report<'_>,
    instruction: &EnrollInstruction,
    mut status: LocalCheckin,
) -> Pass {
    status.view.answer = Seen::Enroll;
    status.view.message = None;
    let mut attempt = status
        .memory
        .attempt
        .take()
        .filter(|attempt| attempt.attempt_id == instruction.attempt_id && attempt.failure.is_none())
        .unwrap_or_else(|| Attempt {
            attempt_id: instruction.attempt_id.clone(),
            stage: EnrollStage::Installing,
            failure: None,
        });
    let discard = instruction.storage == EnrollStorage::None
        && read_prep_state(&paths.prepare_state).is_some_and(|(state, _)| {
            matches!(
                state.prep(false).zfs,
                ZfsPrep::Installing | ZfsPrep::Ready | ZfsPrep::Failed
            )
        });
    let (stages, mut moved) = watch::channel(EnrollStage::Installing);
    let progress = |stage| {
        stages.send_if_modified(|current| {
            if stage != *current {
                *current = stage;
                true
            } else {
                false
            }
        });
    };
    let enrolling = async {
        if discard {
            host.discard_zfs_build()
                .await
                .map_err(|why| format!("Could not discard the ZFS build: {why}"))?;
        }
        host.enroll(instruction, &progress).await
    };
    tokio::pin!(enrolling);
    let mut rejected: Option<String> = None;
    let outcome = loop {
        if let Some(message) = &rejected
            && matches!(
                *moved.borrow(),
                EnrollStage::Enrolling | EnrollStage::Queued
            )
        {
            break Err(message.clone());
        }
        if rejected.is_some() {
            tokio::select! {
                outcome = &mut enrolling => break outcome,
                _ = moved.changed() => {}
            }
        } else {
            tokio::select! {
                outcome = &mut enrolling => break outcome,
                message = heartbeat(client, key, paths, report, &mut status, &mut attempt, &mut moved) => {
                    rejected = message;
                }
            }
        }
    };
    attempt.stage = attempt.stage.max(*moved.borrow());
    match outcome {
        Ok(committed) => {
            if let Some(message) = &committed.follow_up {
                tracing::warn!(%message, "Enrollment committed; follow-up failed");
                crate::ui::note(format!("Enrolled, but follow-up failed: {message}"));
            }
            status.view.message = committed.follow_up;
            status.view.answer = Seen::Joined;
            status.memory.attempt = Some(attempt);
            record(&status, paths);
            Pass::Joined
        }
        _ if rejected.is_some() => {
            let message = rejected.expect("rejection checked above");
            status.view.answer = Seen::Stop;
            status.view.reason = Some(StopReason::Rejected);
            status.view.message = Some(message.clone());
            status.memory.attempt = Some(attempt);
            record(&status, paths);
            Pass::Next(Next::Stop(StopReason::Rejected, message))
        }
        Err(message) => {
            let line = failure_line(attempt.fail(*moved.borrow(), message));
            crate::ui::note(error_chain::then_sentence(&line, "Waiting for Cloud."));
            status.view.answer = Seen::Wait;
            status.view.message = Some(line);
            status.memory.attempt = Some(attempt);
            record(&status, paths);
            Pass::Next(Next::Wait {
                after: Duration::ZERO,
            })
        }
    }
}

async fn heartbeat(
    client: &CheckinClient,
    key: &CheckinKey,
    paths: &Paths,
    report: &Report<'_>,
    status: &mut LocalCheckin,
    attempt: &mut Attempt,
    moved: &mut watch::Receiver<EnrollStage>,
) -> Option<String> {
    status.memory.attempt = Some(attempt.clone());
    record(status, paths);
    let request = CheckinRequest {
        protocol_version: CHECKIN_PROTOCOL_VERSION,
        checkin_key: key.public_hex(),
        cli_version: env!("CARGO_PKG_VERSION"),
        enroll_options: ENROLL_OPTIONS,
        report: report.report,
        prep: Some(report.prep),
        attempt: Some(attempt),
        probe: None,
    };
    match client.post(key, &request).await {
        client::Reply::Answer(Answer::Stop(StopReason::Rejected, message))
            if attempt.stage.committed() =>
        {
            tracing::warn!(%message, "Ignoring rejection of a committed enrollment");
        }
        client::Reply::Answer(Answer::Stop(StopReason::Rejected, message)) => return Some(message),
        client::Reply::Unreachable(_, why)
        | client::Reply::Throttled(_, why)
        | client::Reply::NotCloud(why) => {
            crate::ui::note(format!("Could not report the enroll stage: {why}"));
        }
        client::Reply::Answer(_)
        | client::Reply::Held(..)
        | client::Reply::Refused(_)
        | client::Reply::ClockOff(_)
        | client::Reply::Outdated(_) => {}
    }
    let _ = tokio::time::timeout(HEARTBEAT, moved.changed()).await;
    attempt.stage = attempt.stage.max(*moved.borrow_and_update());
    None
}

pub(super) async fn finish<H: LoopControl>(host: &H, paths: &Paths) -> CheckinEnd {
    if let Err(error) = fs::remove_file(&paths.token_file)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        crate::ui::note(format!(
            "Could not remove the Fleet Token at {}: {error}",
            paths.token_file.display()
        ));
    }
    if let Err(why) = host.disable_loop().await {
        crate::ui::note(format!("Could not disable {}: {why}", unit::CHECKIN));
    }
    crate::ui::note("Enrolled. The check-in loop stops.");
    CheckinEnd::Enrolled
}

pub(super) async fn server_add(
    system: &System,
    instruction: &EnrollInstruction,
    progress: &dyn Fn(EnrollStage),
) -> Result<Committed, String> {
    let instruction = &EnrollInstruction {
        name: add_name(&instruction.name, &system.hostname()),
        ..instruction.clone()
    };
    let plan =
        crate::handlers::server::EnrollPlan::from_instruction(instruction, this_host_config())
            .map_err(|error| error_chain::row(&error))?;
    let install = |installation| {
        let ployzd = system.ployzd.clone();
        async move {
            crate::provisioning::install_with(&ployzd, installation)
                .await
                .map_err(crate::handlers::Error::from)
        }
    };
    let setup = crate::handlers::cloud::run_enrollment(
        crate::cloud::enroll::Command::ServerEnroll,
        &plan,
        &instruction.token,
        &system.cloud,
        &install,
        crate::handlers::cloud::Reader::Fleet,
        progress,
    )
    .await;
    Committed::from_setup(setup)
}

/// The config enrolling keeps on this host: the one `ployz` reads under `HOME`, or root's when
/// the loop runs without `HOME`, as systemd starts it.
fn this_host_config() -> PathBuf {
    if std::env::var_os("HOME").is_none() {
        return PathBuf::from(ROOT_CONFIG);
    }
    let config = std::env::var_os(crate::env::CONFIG).map_or_else(
        || PathBuf::from(crate::context::default_config_path()),
        PathBuf::from,
    );
    crate::context::expand_home(&config)
}

fn failure_message(error: &crate::handlers::Error) -> String {
    let mut message = error.to_string();
    for cause in error.causes() {
        message.push_str("; cause: ");
        message.push_str(&cause);
    }
    message
}

/// The name to enroll as, empty to leave it out. Cloud names a Server by its raw hostname by
/// default, so a hostname that isn't a Machine Name is normalised rather than refused as
/// typed. One with nothing usable is left to `server add`'s Machine ID fallback.
pub(super) fn add_name(name: &str, hostname: &str) -> String {
    if name != hostname || ployz_core::MachineName::parse(name).is_ok() {
        return name.to_owned();
    }
    ployz_core::MachineName::from_hostname(name).map_or_else(String::new, String::from)
}

pub(super) async fn discard_zfs_build(ployzd: &Path) -> Result<(), String> {
    let output = tokio::process::Command::new(ployzd)
        .args(["install", "--discard-zfs-build"])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| format!("run {}: {error}", ployzd.display()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "ployzd install --discard-zfs-build exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[cfg(test)]
#[path = "enroll_tests.rs"]
mod tests;
