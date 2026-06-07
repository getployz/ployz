//! Local keeper effects.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ployz_core::ops::FailureMessage;

use crate::artifacts::{
    ArtifactInstallDurability, ArtifactTarget, install_verified_artifact, verify_artifact_file,
};
use crate::executor::KeeperStepEffects;
use crate::steps::{HostPrerequisite, KeeperStep};
use crate::systemd::{SupervisorUnitSpec, SupervisorUnitTarget};

#[derive(Debug, Clone)]
pub struct KeeperLocalConfig {
    pub systemd_dir: PathBuf,
}

pub struct KeeperLocalEffects<R> {
    config: KeeperLocalConfig,
    runner: R,
}

impl<R> KeeperLocalEffects<R> {
    #[must_use]
    pub const fn new(config: KeeperLocalConfig, runner: R) -> Self {
        Self { config, runner }
    }

    #[must_use]
    pub const fn runner(&self) -> &R {
        &self.runner
    }
}

impl<R: KeeperCommandRunner> KeeperStepEffects for KeeperLocalEffects<R> {
    fn apply_step(&mut self, step: &KeeperStep) -> Result<(), FailureMessage> {
        match step {
            KeeperStep::VerifyHost(prerequisite) => self.verify_host(*prerequisite),
            KeeperStep::VerifyArtifact(target) => verify_artifact_source(target),
            KeeperStep::InstallArtifact(target) => install_artifact_source(target),
            KeeperStep::WriteSupervisorUnit(target) => self.write_supervisor_unit(target),
            KeeperStep::StartSupervisorUnit(target) => self.start_supervisor_unit(target),
            KeeperStep::RestartSupervisorUnit(target) => self.restart_supervisor_unit(target),
            KeeperStep::RedeemJoinToken(_) => Err(failure_message(
                "join token redemption is not wired to NATS yet",
            )),
            KeeperStep::StoreJoinMaterial(_) => Err(failure_message(
                "join material storage is not wired to NATS yet",
            )),
        }
    }
}

impl<R: KeeperCommandRunner> KeeperLocalEffects<R> {
    fn verify_host(&mut self, prerequisite: HostPrerequisite) -> Result<(), FailureMessage> {
        match prerequisite {
            HostPrerequisite::LinuxRootSystemd => {
                if !self.runner.is_linux() {
                    return Err(failure_message("keeper requires Linux"));
                }
                let uid = self.runner.current_uid()?;
                if uid != 0 {
                    return Err(failure_message("keeper must run as root"));
                }
                if !self.config.systemd_dir.is_dir() {
                    return Err(failure_message(format!(
                        "systemd unit directory {} is missing",
                        self.config.systemd_dir.display()
                    )));
                }
                Ok(())
            }
        }
    }

    fn write_supervisor_unit(&self, spec: &SupervisorUnitSpec) -> Result<(), FailureMessage> {
        let unit_name = spec.unit_name();
        if let SupervisorUnitSpec::NatsServer(target) = spec {
            validate_nats_config(target.config_path())?;
        }
        let contents = spec
            .render()
            .map_err(|error| failure_message(error.to_string()))?;
        write_unit_file(&self.config.systemd_dir, &unit_name, contents.as_bytes())
    }

    fn start_supervisor_unit(
        &mut self,
        target: &SupervisorUnitTarget,
    ) -> Result<(), FailureMessage> {
        self.runner.systemctl(&["daemon-reload"])?;
        let unit_name = target.unit_name();
        self.runner.systemctl(&["enable", "--now", &unit_name])
    }

    fn restart_supervisor_unit(
        &mut self,
        target: &SupervisorUnitTarget,
    ) -> Result<(), FailureMessage> {
        self.runner.systemctl(&["daemon-reload"])?;
        let unit_name = target.unit_name();
        self.runner.systemctl(&["restart", &unit_name])
    }
}

pub trait KeeperCommandRunner {
    fn is_linux(&mut self) -> bool;
    fn current_uid(&mut self) -> Result<u32, FailureMessage>;
    fn systemctl(&mut self, args: &[&str]) -> Result<(), FailureMessage>;
}

#[derive(Debug, Clone, Copy)]
pub struct SystemKeeperCommandRunner {
    timeout: Duration,
}

impl SystemKeeperCommandRunner {
    #[must_use]
    pub const fn new(timeout: Duration) -> Self {
        Self { timeout }
    }

    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }
}

impl Default for SystemKeeperCommandRunner {
    fn default() -> Self {
        Self::new(Duration::from_secs(30))
    }
}

impl KeeperCommandRunner for SystemKeeperCommandRunner {
    fn is_linux(&mut self) -> bool {
        std::env::consts::OS == "linux"
    }

    fn current_uid(&mut self) -> Result<u32, FailureMessage> {
        let output = run_command("id", &["-u"], self.timeout)?;
        if !output.status.success() {
            return Err(failure_message(format!(
                "id -u failed: {}",
                output.failure_summary()
            )));
        }
        output
            .stdout
            .trim()
            .parse::<u32>()
            .map_err(|error| failure_message(format!("id -u returned invalid uid: {error}")))
    }

    fn systemctl(&mut self, args: &[&str]) -> Result<(), FailureMessage> {
        let output = run_command("systemctl", args, self.timeout)?;
        if output.status.success() {
            return Ok(());
        }
        Err(failure_message(format!(
            "systemctl failed: {}",
            output.failure_summary()
        )))
    }
}

struct CapturedCommandOutput {
    command: String,
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

impl CapturedCommandOutput {
    fn failure_summary(&self) -> String {
        let mut summary = format!("{} exited with status {}", self.command, self.status);
        let stdout = self.stdout.trim();
        if !stdout.is_empty() {
            summary.push_str("; stdout: ");
            summary.push_str(stdout);
        }
        let stderr = self.stderr.trim();
        if !stderr.is_empty() {
            summary.push_str("; stderr: ");
            summary.push_str(stderr);
        }
        summary
    }
}

fn run_command(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<CapturedCommandOutput, FailureMessage> {
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            failure_message(format!(
                "failed to run {} {}: {error}",
                program,
                args.join(" ")
            ))
        })?;
    let stdout = child
        .stdout
        .take()
        .map(|pipe| thread::spawn(move || read_limited_pipe(pipe)))
        .expect("stdout is piped");
    let stderr = child
        .stderr
        .take()
        .map(|pipe| thread::spawn(move || read_limited_pipe(pipe)))
        .expect("stderr is piped");
    let status = wait_for_child(program, args, &mut child, timeout)?;
    Ok(CapturedCommandOutput {
        command: render_command(program, args),
        status,
        stdout: stdout
            .join()
            .unwrap_or_else(|_error| Err("stdout reader panicked".to_owned()))
            .map_err(failure_message)?,
        stderr: stderr
            .join()
            .unwrap_or_else(|_error| Err("stderr reader panicked".to_owned()))
            .map_err(failure_message)?,
    })
}

fn read_limited_pipe(pipe: impl Read) -> Result<String, String> {
    let mut output = String::new();
    pipe.take(4096)
        .read_to_string(&mut output)
        .map_err(|error| error.to_string())?;
    Ok(output)
}

fn render_command(program: &str, args: &[&str]) -> String {
    if args.is_empty() {
        return program.to_owned();
    }
    format!("{} {}", program, args.join(" "))
}

fn wait_for_child(
    program: &str,
    args: &[&str],
    child: &mut Child,
    timeout: Duration,
) -> Result<ExitStatus, FailureMessage> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(|error| {
            failure_message(format!(
                "failed to wait for {} {}: {error}",
                program,
                args.join(" ")
            ))
        })? {
            return Ok(status);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(failure_message(format!(
                "{} timed out after {}s",
                render_command(program, args),
                timeout.as_secs()
            )));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn verify_artifact_source(target: &ArtifactTarget) -> Result<(), FailureMessage> {
    let source_path = local_artifact_source_path(target)?;
    verify_artifact_file(source_path, target.digest())
        .map(|_verified| ())
        .map_err(|error| failure_message(error.to_string()))
}

fn install_artifact_source(target: &ArtifactTarget) -> Result<(), FailureMessage> {
    let source_path = local_artifact_source_path(target)?;
    let verified = verify_artifact_file(source_path, target.digest())
        .map_err(|error| failure_message(error.to_string()))?;
    let installed = install_verified_artifact(&verified, target)
        .map_err(|error| failure_message(error.to_string()))?;
    match installed.durability {
        ArtifactInstallDurability::Confirmed => Ok(()),
        ArtifactInstallDurability::Unconfirmed { message } => Err(failure_message(format!(
            "artifact {} was installed at {} but durability is unconfirmed: {message}",
            installed.source_path.display(),
            installed.install_path.display()
        ))),
    }
}

fn local_artifact_source_path(target: &ArtifactTarget) -> Result<&Path, FailureMessage> {
    target.source().local_path().ok_or_else(|| {
        failure_message("artifact download is not wired yet; local execution requires a local path")
    })
}

fn validate_nats_config(path: &Path) -> Result<(), FailureMessage> {
    if path.is_file() {
        return Ok(());
    }
    Err(failure_message(format!(
        "nats-server config {} is missing",
        path.display()
    )))
}

fn write_unit_file(
    systemd_dir: &Path,
    unit_name: &str,
    contents: &[u8],
) -> Result<(), FailureMessage> {
    fs::create_dir_all(systemd_dir).map_err(|error| {
        failure_message(format!(
            "failed to create systemd unit directory {}: {error}",
            systemd_dir.display()
        ))
    })?;
    let path = systemd_dir.join(unit_name);
    let mut staged = create_staged_unit_file(systemd_dir, unit_name)?;
    staged.file.write_all(contents).map_err(|error| {
        failure_message(format!(
            "failed to write staged systemd unit {}: {error}",
            staged.path.display()
        ))
    })?;
    staged.file.sync_all().map_err(|error| {
        failure_message(format!(
            "failed to sync staged systemd unit {}: {error}",
            staged.path.display()
        ))
    })?;
    staged.commit_to(&path)?;
    sync_directory(systemd_dir).map_err(|error| {
        failure_message(format!(
            "systemd unit {} was installed but directory sync failed for {}: {error}",
            path.display(),
            systemd_dir.display()
        ))
    })
}

struct StagedUnitFile {
    path: PathBuf,
    file: File,
    committed: bool,
}

impl StagedUnitFile {
    fn new(path: PathBuf, file: File) -> Self {
        Self {
            path,
            file,
            committed: false,
        }
    }

    fn commit_to(&mut self, path: &Path) -> Result<(), FailureMessage> {
        fs::rename(&self.path, path).map_err(|error| {
            failure_message(format!(
                "failed to commit staged systemd unit {} to {}: {error}",
                self.path.display(),
                path.display()
            ))
        })?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for StagedUnitFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn create_staged_unit_file(
    systemd_dir: &Path,
    unit_name: &str,
) -> Result<StagedUnitFile, FailureMessage> {
    for attempt in 0..16 {
        let staged_path = unique_staged_unit_path(systemd_dir, unit_name, attempt)?;
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged_path)
        {
            Ok(file) => return Ok(StagedUnitFile::new(staged_path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(failure_message(format!(
                    "failed to create staged systemd unit {}: {error}",
                    staged_path.display()
                )));
            }
        }
    }
    Err(failure_message(format!(
        "failed to create a unique staged systemd unit in {}",
        systemd_dir.display()
    )))
}

fn unique_staged_unit_path(
    systemd_dir: &Path,
    unit_name: &str,
    attempt: u8,
) -> Result<PathBuf, FailureMessage> {
    let entropy = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| failure_message(format!("failed to create staged unit name: {error}")))?
        .as_nanos();
    Ok(systemd_dir.join(format!(
        ".{unit_name}.ployz-unit-{}-{entropy}-{attempt}",
        std::process::id()
    )))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message).expect("keeper generated a non-empty failure message")
}
