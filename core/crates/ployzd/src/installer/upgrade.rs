//! Durable, bounded Machine upgrade attempts run by transient systemd services.

use std::{
    env, fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use ployz_core::{
    MachineRelease, MachineUpgradeAttempt, MachineUpgradeAttemptId, MachineUpgradeOutcome,
    MachineUpgradeStage, RequestMachineUpgradeRequest,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{process::Command, time::timeout};

use super::{
    Error as InstallError, InstallMode, InstallPaths, InstallRequest, ReleaseRequest, ReleaseSource,
};
use crate::mutation;

const RECEIPT_FILE: &str = "upgrade-attempt.json";
const QUALIFICATION_RELEASE_DIR: &str = "PLOYZ_UPGRADE_RELEASE_DIR";
const WORKER_RUNTIME: &str = "15min";
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Error)]
pub enum Error {
    /// No receipt exists for the requested attempt.
    #[error("no Machine upgrade attempt was found")]
    NotFound,
    /// A retry identity was reused with another release selector.
    #[error("Machine upgrade attempt {0} was reused with a different release")]
    AttemptConflict(MachineUpgradeAttemptId),
    /// Another installation or local mutation currently owns the Machine.
    #[error("a Machine upgrade or mutation is active")]
    Busy,
    /// The durable receipt could not be read.
    #[error("read Machine upgrade receipt: {0}")]
    Read(#[source] io::Error),
    /// The durable receipt was not valid JSON for the current contract.
    #[error("decode Machine upgrade receipt: {0}")]
    Decode(#[source] serde_json::Error),
    /// The durable receipt could not be atomically written.
    #[error("write Machine upgrade receipt: {0}")]
    Write(#[source] io::Error),
    /// The current receipt could not be encoded.
    #[error("encode Machine upgrade receipt: {0}")]
    Encode(#[source] serde_json::Error),
    /// The process-local qualification source was not a trusted absolute directory.
    #[error("invalid qualification release directory: {0}")]
    QualificationSource(String),
    /// Global activation was requested from a daemon using unsupported Machine paths.
    #[error("{0}")]
    NonstandardPaths(String),
    /// The requested release could not be parsed or resolved.
    #[error("resolve Machine release: {0}")]
    Resolve(#[source] InstallError),
    /// The system manager rejected or did not confirm worker launch.
    #[error("launch Machine upgrade worker: {0}")]
    Launch(String),
    /// The worker process or unit could not be queried.
    #[error("inspect Machine upgrade worker: {0}")]
    InspectWorker(#[source] io::Error),
    /// `systemctl` returned evidence that did not prove either a running or stopped worker.
    #[error("inspect Machine upgrade worker: {0}")]
    WorkerEvidence(String),
    /// The worker identity does not match the current nonterminal receipt.
    #[error("Machine upgrade worker does not own active attempt {0}")]
    NotActive(MachineUpgradeAttemptId),
    /// The shared installer recorded a terminal failure.
    #[error("Machine upgrade failed: {0}")]
    Installation(#[source] InstallError),
    /// Machine mutation ownership could not be claimed or inspected.
    #[error(transparent)]
    Admission(#[from] mutation::Error),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct StoredAttempt {
    requested: MachineRelease,
    source: ReleaseSource,
    attempt: MachineUpgradeAttempt,
}

fn current_source() -> Result<ReleaseSource, Error> {
    let Some(directory) = env::var_os(QUALIFICATION_RELEASE_DIR) else {
        return Ok(ReleaseSource::Published);
    };
    let directory = PathBuf::from(directory);
    if !directory.is_absolute() {
        return Err(Error::QualificationSource(format!(
            "{QUALIFICATION_RELEASE_DIR} must be an absolute path"
        )));
    }
    Ok(ReleaseSource::Local(directory))
}

/// Return the durable result of retrying the same request without requiring installation
/// ownership. The receipt is atomically replaced, so an active worker can be observed safely.
pub(crate) fn existing_request(
    request: &RequestMachineUpgradeRequest,
    data_dir: &Path,
) -> Result<Option<MachineUpgradeAttempt>, Error> {
    let Some(stored) = read_optional(data_dir)? else {
        return Ok(None);
    };
    if stored.attempt.attempt_id != request.attempt_id {
        return Ok(None);
    }
    if stored.requested != request.release {
        return Err(Error::AttemptConflict(request.attempt_id));
    }
    Ok(Some(stored.attempt))
}

/// Own acceptance and worker launch independently of the requesting client's lifetime.
/// Dropping the returned future only stops waiting; the task retains exclusive admission.
pub(crate) fn request(
    request: RequestMachineUpgradeRequest,
    data_dir: PathBuf,
    run_dir: PathBuf,
    guard: mutation::InstallationGuard,
) -> impl Future<Output = Result<MachineUpgradeAttempt, Error>> {
    let task = tokio::spawn(async move {
        let _guard = guard;
        request_locked(request, &data_dir, &run_dir).await
    });
    async move {
        task.await
            .map_err(|error| Error::Launch(format!("upgrade acceptance task failed: {error}")))?
    }
}

async fn request_locked(
    request: RequestMachineUpgradeRequest,
    data_dir: &Path,
    run_dir: &Path,
) -> Result<MachineUpgradeAttempt, Error> {
    let admission = mutation::MutationGate::new(run_dir, data_dir);
    if let Some(mut stored) = read_optional(data_dir)? {
        if stored.attempt.attempt_id == request.attempt_id {
            if stored.requested != request.release {
                return Err(Error::AttemptConflict(request.attempt_id));
            }
            reconcile_locked(&admission, data_dir, &mut stored).await?;
            return Ok(stored.attempt);
        }
        reconcile_locked(&admission, data_dir, &mut stored).await?;
        if !stored.attempt.is_terminal() {
            return Err(Error::Busy);
        }
    }

    if admission.active()? {
        return Err(Error::Busy);
    }
    let source = current_source()?;
    let release = request
        .release
        .as_str()
        .parse::<ReleaseRequest>()
        .map_err(Error::Resolve)?;
    let target = super::release::resolve_release(&release, &source)
        .await
        .map_err(Error::Resolve)?;
    let mut stored = StoredAttempt {
        requested: request.release,
        source,
        attempt: MachineUpgradeAttempt {
            attempt_id: request.attempt_id,
            target,
            outcome: MachineUpgradeOutcome::Accepted,
        },
    };
    write(data_dir, &stored)?;
    admission.mark_active(request.attempt_id.as_str())?;

    if let Err(error) = launch_worker(request.attempt_id, data_dir, run_dir).await {
        stored.attempt.outcome = MachineUpgradeOutcome::Failed {
            stage: MachineUpgradeStage::Launching,
            error: error.to_string(),
        };
        write(data_dir, &stored)?;
        admission.clear_active(request.attempt_id.as_str())?;
    }
    Ok(stored.attempt)
}

/// Read an attempt, turning a stopped worker without terminal evidence into interruption.
pub(crate) async fn inspect(
    attempt_id: Option<MachineUpgradeAttemptId>,
    data_dir: &Path,
    run_dir: &Path,
) -> Result<MachineUpgradeAttempt, Error> {
    let admission = mutation::MutationGate::new(run_dir, data_dir);
    let mut stored = read(data_dir)?;
    if attempt_id.is_some_and(|attempt_id| attempt_id != stored.attempt.attempt_id) {
        return Err(Error::NotFound);
    }
    if stored.attempt.is_terminal() {
        match admission.try_installation() {
            Ok(_guard) => admission.clear_active(stored.attempt.attempt_id.as_str())?,
            Err(mutation::Error::Busy) => {}
            Err(error) => return Err(error.into()),
        }
        return Ok(stored.attempt);
    }
    if worker_state(stored.attempt.attempt_id).await? == WorkerState::Active {
        return Ok(stored.attempt);
    }
    let _guard = match admission.try_installation() {
        Ok(guard) => guard,
        Err(mutation::Error::Busy) => return Ok(stored.attempt),
        Err(error) => return Err(error.into()),
    };
    stored = read(data_dir)?;
    reconcile_locked(&admission, data_dir, &mut stored).await?;
    Ok(stored.attempt)
}

/// Run the accepted attempt from the transient systemd service.
///
/// # Errors
///
/// Returns an ownership, receipt, installation, or durable result error when the worker cannot
/// complete and record the accepted attempt.
pub async fn run_worker(
    attempt_id: MachineUpgradeAttemptId,
    data_dir: &Path,
    run_dir: &Path,
) -> Result<(), Error> {
    super::require_standard_machine_paths(data_dir, &run_dir.join("ployz.sock"))
        .map_err(Error::NonstandardPaths)?;
    let admission = mutation::MutationGate::new(run_dir, data_dir);
    let guard = admission.lock_installation()?;
    let mut stored = read(data_dir)?;
    if stored.attempt.attempt_id != attempt_id || stored.attempt.is_terminal() {
        return Err(Error::NotActive(attempt_id));
    }
    let target = stored.attempt.target.clone();
    let mut stage = MachineUpgradeStage::Preparing;
    stored.attempt.outcome = MachineUpgradeOutcome::Running {
        stage: stage.clone(),
    };
    write(data_dir, &stored)?;

    let result = super::install_locked(
        InstallRequest {
            release: ReleaseRequest::Exact(target.clone()),
            source: stored.source.clone(),
            mode: InstallMode::SoftwareOnly,
        },
        InstallPaths::system(data_dir, run_dir),
        guard,
        |install_stage| {
            stage = install_stage;
            stored.attempt.outcome = MachineUpgradeOutcome::Running {
                stage: stage.clone(),
            };
            write(data_dir, &stored).map_err(|error| InstallError::Io {
                stage: "record Machine upgrade progress",
                source: io::Error::other(error),
            })
        },
    )
    .await;

    match result {
        Ok(_) => {
            stored.attempt.outcome = MachineUpgradeOutcome::Succeeded {
                version: target.clone(),
            };
            write(data_dir, &stored)?;
            admission.clear_active(attempt_id.as_str())?;
            Ok(())
        }
        Err(error) => {
            stored.attempt.outcome = MachineUpgradeOutcome::Failed {
                stage,
                error: error.to_string(),
            };
            write(data_dir, &stored)?;
            admission.clear_active(attempt_id.as_str())?;
            Err(Error::Installation(error))
        }
    }
}

/// Reconcile a retained receipt before serving requests after daemon restart.
pub(crate) async fn reconcile(data_dir: &Path, run_dir: &Path) -> Result<(), Error> {
    match inspect(None, data_dir, run_dir).await {
        Ok(_) | Err(Error::NotFound) => Ok(()),
        Err(error) => Err(error),
    }
}

pub(super) async fn reconcile_for_install(
    admission: &mutation::MutationGate,
    data_dir: &Path,
) -> Result<(), Error> {
    let Some(mut stored) = read_optional(data_dir)? else {
        return if admission.active()? {
            Err(Error::Busy)
        } else {
            Ok(())
        };
    };
    reconcile_locked(admission, data_dir, &mut stored).await?;
    if stored.attempt.is_terminal() {
        Ok(())
    } else {
        Err(Error::Busy)
    }
}

async fn reconcile_locked(
    admission: &mutation::MutationGate,
    data_dir: &Path,
    stored: &mut StoredAttempt,
) -> Result<(), Error> {
    if stored.attempt.is_terminal() {
        admission.clear_active(stored.attempt.attempt_id.as_str())?;
        return Ok(());
    }
    if worker_state(stored.attempt.attempt_id).await? == WorkerState::Active {
        return Ok(());
    }
    let stage = match &stored.attempt.outcome {
        MachineUpgradeOutcome::Accepted => MachineUpgradeStage::Launching,
        MachineUpgradeOutcome::Running { stage } => stage.clone(),
        MachineUpgradeOutcome::Succeeded { .. }
        | MachineUpgradeOutcome::Failed { .. }
        | MachineUpgradeOutcome::Interrupted { .. } => return Ok(()),
    };
    stored.attempt.outcome = MachineUpgradeOutcome::Interrupted { stage };
    write(data_dir, stored)?;
    admission.clear_active(stored.attempt.attempt_id.as_str())?;
    Ok(())
}

async fn launch_worker(
    attempt_id: MachineUpgradeAttemptId,
    data_dir: &Path,
    run_dir: &Path,
) -> Result<(), Error> {
    let executable = env::current_exe().map_err(Error::InspectWorker)?;
    let unit = unit_name(attempt_id);
    let writable = format!(
        "ReadWritePaths=/usr/local/bin /etc/systemd/system {} {}",
        data_dir.display(),
        run_dir.display()
    );
    let mut command = Command::new("systemd-run");
    command
        .arg(format!("--unit={unit}"))
        .args(["--collect", "--quiet"])
        .arg("--property=Type=exec")
        .arg(format!("--property=RuntimeMaxSec={WORKER_RUNTIME}"))
        .arg("--property=TimeoutStopSec=15s")
        .arg("--property=KillMode=control-group")
        .arg("--property=NoNewPrivileges=yes")
        .arg("--property=ProtectSystem=full")
        .arg("--property=ProtectHome=yes")
        .arg("--property=ProtectControlGroups=yes")
        .arg("--property=ProtectKernelTunables=yes")
        .arg("--property=PrivateTmp=yes")
        .arg("--property=RestrictNamespaces=yes")
        .arg("--property=RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX")
        .arg(format!("--property={writable}"))
        .arg(executable)
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--socket")
        .arg(run_dir.join("ployz.sock"))
        .arg("upgrade-worker")
        .arg("--attempt")
        .arg(attempt_id.as_str())
        .kill_on_drop(true);
    let output = timeout(LAUNCH_TIMEOUT, command.output())
        .await
        .map_err(|_| {
            Error::Launch(format!(
                "systemd-run timed out after {}s",
                LAUNCH_TIMEOUT.as_secs()
            ))
        })?
        .map_err(Error::InspectWorker)?;
    if output.status.success() {
        Ok(())
    } else {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(Error::Launch(if detail.is_empty() {
            format!("systemd-run exited with {}", output.status)
        } else {
            detail
        }))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkerState {
    Active,
    Stopped,
}

async fn worker_state(attempt_id: MachineUpgradeAttemptId) -> Result<WorkerState, Error> {
    let output = Command::new("systemctl")
        .args([
            "show",
            "--property=LoadState",
            "--property=ActiveState",
            &unit_name(attempt_id),
        ])
        .output()
        .await
        .map_err(Error::InspectWorker)?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(Error::WorkerEvidence(if detail.is_empty() {
            format!("systemctl show exited with {}", output.status)
        } else {
            format!("systemctl show exited with {}: {detail}", output.status)
        }));
    }
    let properties = String::from_utf8(output.stdout)
        .map_err(|_| Error::WorkerEvidence("systemctl show returned non-UTF-8 output".into()))?;
    let mut load = None;
    let mut active = None;
    for line in properties.lines() {
        if let Some(value) = line.strip_prefix("LoadState=") {
            if load.replace(value).is_some() {
                return Err(Error::WorkerEvidence(
                    "systemctl show repeated LoadState".into(),
                ));
            }
        } else if let Some(value) = line.strip_prefix("ActiveState=")
            && active.replace(value).is_some()
        {
            return Err(Error::WorkerEvidence(
                "systemctl show repeated ActiveState".into(),
            ));
        }
    }
    if load == Some("not-found") {
        return Ok(WorkerState::Stopped);
    }
    match active {
        Some(
            "active" | "activating" | "reloading" | "deactivating" | "maintenance" | "refreshing",
        ) => Ok(WorkerState::Active),
        Some("inactive" | "failed") => Ok(WorkerState::Stopped),
        _ => Err(Error::WorkerEvidence(format!(
            "systemctl show returned no recognized state: {}",
            properties.trim().escape_debug()
        ))),
    }
}

fn unit_name(attempt_id: MachineUpgradeAttemptId) -> String {
    format!("ployz-upgrade-{attempt_id}.service")
}

fn read(data_dir: &Path) -> Result<StoredAttempt, Error> {
    read_optional(data_dir)?.ok_or(Error::NotFound)
}

fn read_optional(data_dir: &Path) -> Result<Option<StoredAttempt>, Error> {
    let bytes = match fs::read(data_dir.join(RECEIPT_FILE)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Error::Read(error)),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(Error::Decode)
}

fn write(data_dir: &Path, stored: &StoredAttempt) -> Result<(), Error> {
    fs::create_dir_all(data_dir).map_err(Error::Write)?;
    let bytes = serde_json::to_vec(stored).map_err(Error::Encode)?;
    crate::filesystem::atomic_write(&data_dir.join(RECEIPT_FILE), &bytes, 0o600)
        .map_err(Error::Write)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::MachineVersion;

    const CONTRACT_CASE: &str = "PLOYZ_UPGRADE_CONTRACT_CASE";
    const CONTRACT_ROOT: &str = "PLOYZ_UPGRADE_CONTRACT_ROOT";

    #[tokio::test]
    async fn worker_rejects_nonstandard_paths_before_reading_local_state() {
        let root = tempfile::Builder::new()
            .prefix("ployzd-upgrade-worker-paths-")
            .tempdir()
            .unwrap();
        let data_dir = root.path().join("data");
        let run_dir = root.path().join("run");

        let error = run_worker(MachineUpgradeAttemptId::random(), &data_dir, &run_dir)
            .await
            .unwrap_err();

        assert!(matches!(error, Error::NonstandardPaths(_)));
        assert!(!data_dir.exists());
        assert!(!run_dir.exists());
    }

    #[test]
    fn stored_attempt_round_trips_without_exposing_local_source_in_response() {
        let attempt_id = MachineUpgradeAttemptId::random();
        let stored = StoredAttempt {
            requested: MachineRelease::parse("beta").unwrap(),
            source: ReleaseSource::Local("/root/qualification".into()),
            attempt: MachineUpgradeAttempt {
                attempt_id,
                target: MachineVersion::parse("1.2.3-beta.4").unwrap(),
                outcome: MachineUpgradeOutcome::Accepted,
            },
        };
        let encoded = serde_json::to_string(&stored).unwrap();
        assert!(encoded.contains("/root/qualification"));
        assert!(
            !serde_json::to_string(&stored.attempt)
                .unwrap()
                .contains("qualification")
        );
        assert_eq!(
            serde_json::from_str::<StoredAttempt>(&encoded).unwrap(),
            stored
        );
    }

    #[test]
    fn upgrade_attempt_contract() {
        if let Ok(case) = env::var(CONTRACT_CASE) {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(run_contract_case(&case));
            let root = PathBuf::from(env::var_os(CONTRACT_ROOT).unwrap());
            fs::write(root.join("child-completed"), case).unwrap();
            return;
        }

        for case in [
            "retry-active",
            "interrupted",
            "inspection-unknown",
            "launch-failed",
            "cancelled-launch",
            "cancelled-launch-failed",
        ] {
            let root = tempfile::Builder::new()
                .prefix(&format!("ployzd-upgrade-{case}-"))
                .tempdir()
                .unwrap();
            let commands = root.path().join("commands");
            fs::create_dir(&commands).unwrap();
            write_script(
                &commands.join("systemd-run"),
                if case.starts_with("cancelled-launch") {
                    "echo started > \"$PLOYZ_UPGRADE_CONTRACT_ROOT/launch-started\"\nwhile [ ! -f \"$PLOYZ_UPGRADE_CONTRACT_ROOT/launch-release\" ]; do /bin/sleep 0.01; done\nprintf '%s\\n' \"$*\" >> \"$PLOYZ_UPGRADE_COMMAND_LOG\"\nif [ \"$PLOYZ_UPGRADE_CONTRACT_CASE\" = cancelled-launch-failed ]; then echo worker launch refused >&2; exit 1; fi"
                } else if case == "launch-failed" {
                    "echo worker launch refused >&2; exit 1"
                } else {
                    "printf '%s\\n' \"$*\" >> \"$PLOYZ_UPGRADE_COMMAND_LOG\""
                },
            );
            write_script(
                &commands.join("systemctl"),
                match case {
                    "interrupted" => "printf 'LoadState=loaded\\nActiveState=inactive\\n'",
                    "inspection-unknown" => "echo manager unavailable >&2; exit 1",
                    _ => "printf 'LoadState=loaded\\nActiveState=active\\n'",
                },
            );
            fs::create_dir(root.path().join("release")).unwrap();
            let output = std::process::Command::new(env::current_exe().unwrap())
                .args([
                    "--exact",
                    "installer::upgrade::tests::upgrade_attempt_contract",
                    "--nocapture",
                ])
                .env(CONTRACT_CASE, case)
                .env(CONTRACT_ROOT, root.path())
                .env(QUALIFICATION_RELEASE_DIR, root.path().join("release"))
                .env(
                    "PLOYZ_UPGRADE_COMMAND_LOG",
                    root.path().join("commands.log"),
                )
                .env("PATH", &commands)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{case}: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                fs::read_to_string(root.path().join("child-completed")).unwrap(),
                case
            );
        }
    }

    async fn run_contract_case(case: &str) {
        let root = PathBuf::from(env::var_os(CONTRACT_ROOT).unwrap());
        let data = root.join("data");
        let run = root.join("run");
        let admission = mutation::MutationGate::new(&run, &data);
        let attempt_id = MachineUpgradeAttemptId::parse("a".repeat(32)).unwrap();
        let request = RequestMachineUpgradeRequest {
            attempt_id,
            release: MachineRelease::parse("1.2.3").unwrap(),
        };
        let guard = admission.try_installation().unwrap();
        if case.starts_with("cancelled-launch") {
            let launch = tokio::spawn({
                let data = data.clone();
                let run = run.clone();
                let request = request.clone();
                async move { super::request(request, data, run, guard).await }
            });
            timeout(Duration::from_secs(5), async {
                while !root.join("launch-started").exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("launch did not reach the cancellation window");
            launch.abort();
            assert!(launch.await.unwrap_err().is_cancelled());
            assert!(admission.active().unwrap());
            assert!(matches!(
                admission.try_installation(),
                Err(mutation::Error::Busy)
            ));
            fs::write(root.join("launch-release"), "continue").unwrap();
            timeout(Duration::from_secs(5), async {
                while admission.try_installation().is_err() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("detached launch did not finish");
            let observed = existing_request(&request, &data).unwrap().unwrap();
            if case == "cancelled-launch-failed" {
                assert!(matches!(
                    observed.outcome,
                    MachineUpgradeOutcome::Failed {
                        stage: MachineUpgradeStage::Launching,
                        ..
                    }
                ));
                assert!(!admission.active().unwrap());
                assert!(admission.try_mutation().is_ok());
            } else {
                assert!(matches!(observed.outcome, MachineUpgradeOutcome::Accepted));
                assert!(admission.active().unwrap());
            }
            assert_eq!(
                fs::read_to_string(root.join("commands.log"))
                    .unwrap()
                    .lines()
                    .count(),
                1
            );
            return;
        }
        let accepted = super::request(request.clone(), data.clone(), run.clone(), guard)
            .await
            .unwrap();

        match case {
            "retry-active" => {
                assert!(matches!(accepted.outcome, MachineUpgradeOutcome::Accepted));
                assert_eq!(
                    existing_request(&request, &data).unwrap(),
                    Some(accepted.clone())
                );
                assert!(matches!(
                    admission.try_mutation(),
                    Err(mutation::Error::Busy)
                ));
                let conflict = RequestMachineUpgradeRequest {
                    attempt_id,
                    release: MachineRelease::parse("1.2.4").unwrap(),
                };
                assert!(matches!(
                    existing_request(&conflict, &data),
                    Err(Error::AttemptConflict(id)) if id == attempt_id
                ));

                let guard = admission.try_installation().unwrap();
                assert_eq!(
                    super::request(request, data.clone(), run.clone(), guard)
                        .await
                        .unwrap(),
                    accepted
                );
                let guard = admission.try_installation().unwrap();
                let other = RequestMachineUpgradeRequest {
                    attempt_id: MachineUpgradeAttemptId::parse("b".repeat(32)).unwrap(),
                    release: MachineRelease::parse("1.2.3").unwrap(),
                };
                assert!(matches!(
                    super::request(other, data.clone(), run.clone(), guard).await,
                    Err(Error::Busy)
                ));
                let log = fs::read_to_string(root.join("commands.log")).unwrap();
                assert_eq!(log.lines().count(), 1, "{log}");
                for required in [
                    "--property=Type=exec",
                    "--property=RuntimeMaxSec=15min",
                    "--property=NoNewPrivileges=yes",
                    "--property=ProtectSystem=full",
                    "upgrade-worker",
                    "--attempt",
                    attempt_id.as_str(),
                ] {
                    assert!(log.contains(required), "missing {required}: {log}");
                }
            }
            "interrupted" => {
                let observed = inspect(Some(attempt_id), &data, &run).await.unwrap();
                assert!(matches!(
                    observed.outcome,
                    MachineUpgradeOutcome::Interrupted {
                        stage: MachineUpgradeStage::Launching,
                    }
                ));
                assert!(!admission.active().unwrap());
                assert!(admission.try_mutation().is_ok());
            }
            "inspection-unknown" => {
                assert!(matches!(
                    inspect(Some(attempt_id), &data, &run).await,
                    Err(Error::WorkerEvidence(ref message))
                        if message.contains("manager unavailable")
                ));
                assert!(admission.active().unwrap());
                assert!(matches!(
                    admission.try_mutation(),
                    Err(mutation::Error::Busy)
                ));
            }
            "launch-failed" => {
                assert!(matches!(
                    accepted.outcome,
                    MachineUpgradeOutcome::Failed {
                        stage: MachineUpgradeStage::Launching,
                        ref error,
                    } if error == "launch Machine upgrade worker: worker launch refused"
                ));
                assert!(!admission.active().unwrap());
                assert_eq!(
                    inspect(Some(attempt_id), &data, &run).await.unwrap(),
                    accepted
                );
            }
            other => panic!("unknown upgrade contract case {other}"),
        }
    }

    fn write_script(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;

        fs::write(path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}
