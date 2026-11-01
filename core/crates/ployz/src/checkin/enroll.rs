//! Running Cloud's enroll instruction in-process, as `ployz server add` does, while the
//! loop keeps reporting the attempt's stage on each check-in.

use std::{fs, path::Path, time::Duration};

use tokio::sync::watch;

use super::{Attempt, EnrollInstruction, Prep};
use super::{
    CheckinClient, CheckinEnd, CheckinKey, CheckinRequest, Host, LocalCheckin, Next, Paths, Seen,
    System, ZfsPrep, client, read_prep_state, record, unit,
    wire::{self, Answer, AttemptFailure, EnrollStorage, HostReport, StopReason},
};
use crate::cloud_enroll::EnrollStage;

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
        title(failure.stage).to_lowercase(),
        failure.message
    )
}

/// Run `instruction`, reporting monotonic progress on changes and every [`HEARTBEAT`].
/// Rejection awaits installation and stops at live Enrolling or Queued. Once the
/// attempt has reported Joining, saved or in this run, rejection cannot cancel its
/// committed join. A successful join is recorded for cleanup; a failure keeps its
/// live stage for the next check-in and returns to polling.
pub(super) async fn attempt<H: Host>(
    host: &H,
    client: &CheckinClient,
    key: &CheckinKey,
    paths: &Paths,
    report: &Report<'_>,
    instruction: &EnrollInstruction,
    mut status: LocalCheckin,
) -> Next {
    status.answer = Seen::Enroll;
    status.message = None;
    let mut attempt = status
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
                state.prep().zfs,
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
            status.message = committed.follow_up;
            status.answer = Seen::Joined;
            status.attempt = Some(attempt);
            record(&status, paths);
            Next::Enrolled
        }
        _ if rejected.is_some() => {
            let message = rejected.expect("rejection checked above");
            status.answer = Seen::Stop;
            status.reason = Some(StopReason::Rejected);
            status.message = Some(message.clone());
            status.attempt = Some(attempt);
            record(&status, paths);
            Next::Stop(StopReason::Rejected, message)
        }
        Err(message) => {
            let failure = AttemptFailure {
                stage: *moved.borrow(),
                message,
            };
            let line = failure_line(&failure);
            crate::ui::note(crate::ui::then_sentence(&line, "Waiting for Cloud."));
            attempt.failure = Some(failure);
            status.answer = Seen::Wait;
            status.message = Some(line);
            status.attempt = Some(attempt);
            record(&status, paths);
            Next::Wait {
                after: Duration::ZERO,
            }
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
    status.attempt = Some(attempt.clone());
    record(status, paths);
    let request = CheckinRequest {
        protocol_version: wire::PROTOCOL_VERSION,
        checkin_key: key.public_hex(),
        cli_version: env!("CARGO_PKG_VERSION"),
        report: report.report,
        prep: Some(report.prep),
        attempt: Some(attempt),
        probe: None,
        machine_phase: None,
    };
    match client.post(key, &request).await {
        client::Reply::Answer(Answer::Stop(StopReason::Rejected, message))
            if attempt.stage >= EnrollStage::Joining =>
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

pub(super) async fn finish<H: Host>(host: &H, paths: &Paths) -> CheckinEnd {
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
    let root = crate::cli::command()
        .try_get_matches_from(add_arguments(
            instruction,
            &system.cloud,
            std::env::var_os("HOME").is_some(),
        ))
        .map_err(|error| error.to_string())?;
    let install = |installation| {
        let ployzd = system.ployzd.clone();
        async move {
            crate::provisioning::install_with(&ployzd, installation)
                .await
                .map_err(crate::handlers::Error::from)
        }
    };
    let setup = crate::handlers::cloud::run_enrollment(
        crate::cloud_enroll::Command::ServerEnroll,
        &root,
        &instruction.token,
        &system.cloud,
        &install,
        crate::handlers::cloud::Reader::Fleet,
        progress,
    )
    .await;
    Committed::from_setup(setup)
}

fn failure_message(error: &crate::handlers::Error) -> String {
    let mut message = error.to_string();
    for cause in error.causes() {
        message.push_str("; cause: ");
        message.push_str(&cause);
    }
    message
}

/// The `--name` for `server add`, empty to leave it out. Cloud names a Server by its raw
/// hostname by default, so a hostname that isn't a Machine Name is normalised rather than
/// refused as typed. One with nothing usable is left to `server add`'s Machine ID fallback.
pub(super) fn add_name(name: &str, hostname: &str) -> String {
    if name != hostname || ployz_core::MachineName::parse(name).is_ok() {
        return name.to_owned();
    }
    ployz_core::MachineName::from_hostname(name).map_or_else(String::new, String::from)
}

pub(super) fn add_arguments(
    instruction: &EnrollInstruction,
    cloud: &str,
    home_set: bool,
) -> Vec<String> {
    let policy = &instruction.policy;
    let storage = match &instruction.storage {
        EnrollStorage::None => "none".to_owned(),
        EnrollStorage::Zfs { device: None } => "zfs".to_owned(),
        EnrollStorage::Zfs {
            device: Some(device),
        } => format!("zfs:{device}"),
    };
    let mut arguments: Vec<String> = [
        "ployz",
        "server",
        "add",
        "--yes",
        "--connect",
        &format!("unix://{}", crate::connect::DEFAULT_LOCAL_SOCKET),
        "--storage",
        &storage,
        "--cloud-url",
        cloud,
    ]
    .map(str::to_owned)
    .into();
    if !instruction.name.is_empty() {
        arguments.extend(["--name".to_owned(), instruction.name.clone()]);
    }
    arguments.extend([
        format!("--accepts-builds={}", policy.accepts_builds),
        format!("--accepts-services={}", policy.accepts_services),
        format!("--accepts-ingress={}", policy.accepts_ingress),
    ]);
    for (label, value) in &policy.labels {
        arguments.extend(["--label-add".to_owned(), format!("{label}={value}")]);
    }
    if !home_set {
        arguments.extend(["--ployz-config".to_owned(), ROOT_CONFIG.to_owned()]);
    }
    arguments
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
