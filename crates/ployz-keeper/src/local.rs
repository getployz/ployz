//! Local keeper effects.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ployz_core::ops::FailureMessage;

use crate::artifacts::{
    ArtifactInstallDurability, ArtifactSourceView, ArtifactTarget, install_verified_artifact,
    verify_artifact_file,
};
use crate::executor::KeeperStepEffects;
use crate::join::{
    JOIN_MATERIAL_DIR, JOIN_MATERIAL_FILE, JOIN_NATS_CREDENTIALS_FILE,
    render_redacted_join_material,
};
use crate::steps::{
    HostPrerequisite, KeeperJoinMaterial, KeeperStep, KeeperStepEffectError,
    KeeperStepFailureReason, NatsServerConfigTarget, PloyzdRoleEnvironmentTarget,
};
use crate::systemd::{SupervisorUnitSpec, SupervisorUnitTarget};

#[derive(Debug, Clone)]
pub struct KeeperLocalConfig {
    pub systemd_dir: PathBuf,
    pub state_dir: PathBuf,
}

pub struct KeeperLocalEffects<R> {
    config: KeeperLocalConfig,
    runner: R,
}

impl<R> KeeperLocalEffects<R> {
    #[must_use]
    pub fn new(config: KeeperLocalConfig, runner: R) -> Self {
        Self { config, runner }
    }

    #[must_use]
    pub const fn runner(&self) -> &R {
        &self.runner
    }
}

impl<R: KeeperCommandRunner> KeeperStepEffects for KeeperLocalEffects<R> {
    fn apply_step(&mut self, step: &KeeperStep) -> Result<(), KeeperStepEffectError> {
        match step {
            KeeperStep::VerifyHost(prerequisite) => {
                self.verify_host(*prerequisite).map_err(Into::into)
            }
            KeeperStep::InstallArtifact(target) => self.install_artifact_source(target),
            KeeperStep::WritePloyzdRoleEnvironment(target) => self
                .write_ployzd_role_environment(target)
                .map_err(Into::into),
            KeeperStep::WriteNatsServerConfig(target) => {
                self.write_nats_server_config(target).map_err(Into::into)
            }
            KeeperStep::WriteSupervisorUnit(target) => {
                self.write_supervisor_unit(target).map_err(Into::into)
            }
            KeeperStep::StartSupervisorUnit(target) => {
                self.start_supervisor_unit(target).map_err(Into::into)
            }
            KeeperStep::RestartSupervisorUnit(target) => {
                self.restart_supervisor_unit(target).map_err(Into::into)
            }
            KeeperStep::StoreJoinMaterial(material) => self.store_join_material(material),
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
        let contents = spec
            .render()
            .map_err(|error| failure_message(error.to_string()))?;
        write_unit_file(&self.config.systemd_dir, &unit_name, contents.as_bytes())
    }

    fn write_nats_server_config(
        &self,
        target: &NatsServerConfigTarget,
    ) -> Result<(), FailureMessage> {
        fs::create_dir_all(target.config_dir()).map_err(|error| {
            failure_message(format!(
                "failed to create nats-server config directory {}: {error}",
                target.config_dir().display()
            ))
        })?;
        write_durable_file(
            target.config_dir(),
            target.config_file_name(),
            "ployz-nats",
            target.render_config().as_bytes(),
        )
    }

    fn write_ployzd_role_environment(
        &self,
        target: &PloyzdRoleEnvironmentTarget,
    ) -> Result<(), FailureMessage> {
        fs::create_dir_all(target.directory()).map_err(|error| {
            failure_message(format!(
                "failed to create ployzd environment directory {}: {error}",
                target.directory().display()
            ))
        })?;
        write_durable_file(
            target.directory(),
            target.file_name(),
            "ployz-role-env",
            target.render().as_bytes(),
        )
    }

    fn start_supervisor_unit(
        &mut self,
        target: &SupervisorUnitTarget,
    ) -> Result<(), FailureMessage> {
        self.runner.systemctl(&["daemon-reload"])?;
        let unit_name = target.unit_name();
        self.runner.systemctl(&["enable", &unit_name])?;
        self.runner.systemctl(&["restart", &unit_name])
    }

    fn restart_supervisor_unit(
        &mut self,
        target: &SupervisorUnitTarget,
    ) -> Result<(), FailureMessage> {
        self.runner.systemctl(&["daemon-reload"])?;
        let unit_name = target.unit_name();
        self.runner.systemctl(&["restart", &unit_name])
    }

    fn install_artifact_source(
        &mut self,
        target: &ArtifactTarget,
    ) -> Result<(), KeeperStepEffectError> {
        let artifact = self.acquire_artifact(target).map_err(|message| {
            KeeperStepEffectError::new(KeeperStepFailureReason::ArtifactDownloadFailed, message)
        })?;
        let verified = verify_artifact_file(artifact.path(), target.digest()).map_err(|error| {
            KeeperStepEffectError::new(
                KeeperStepFailureReason::ArtifactVerificationFailed,
                failure_message(error.to_string()),
            )
        })?;
        let installed = install_verified_artifact(&verified, target)
            .map_err(|error| failure_message(error.to_string()))?;
        match installed.durability {
            ArtifactInstallDurability::Confirmed => Ok(()),
            ArtifactInstallDurability::Unconfirmed { message } => Err(failure_message(format!(
                "artifact {} was installed at {} but durability is unconfirmed: {message}",
                installed.source_path.display(),
                installed.install_path.display()
            ))
            .into()),
        }
    }

    fn acquire_artifact(
        &mut self,
        target: &ArtifactTarget,
    ) -> Result<AcquiredArtifact, FailureMessage> {
        match target.source().view() {
            ArtifactSourceView::LocalPath(path) => Ok(AcquiredArtifact::local(path.to_path_buf())),
            ArtifactSourceView::RemoteUrl(url) => {
                let artifact = AcquiredArtifact::downloaded(create_download_path(target)?);
                self.runner.download(url, artifact.path())?;
                Ok(artifact)
            }
        }
    }

    fn store_join_material(
        &self,
        material: &KeeperJoinMaterial,
    ) -> Result<(), KeeperStepEffectError> {
        ensure_directory(&self.config.state_dir).map_err(|error| {
            KeeperStepEffectError::new(
                KeeperStepFailureReason::JoinMaterialStoreFailed,
                failure_message(format!(
                    "failed to create keeper state directory {}: {error}",
                    self.config.state_dir.display()
                )),
            )
        })?;
        commit_join_material_directory(&self.config.state_dir, material)
    }
}

fn commit_join_material_directory(
    state_dir: &Path,
    material: &KeeperJoinMaterial,
) -> Result<(), KeeperStepEffectError> {
    let staged =
        StagedDirectory::create(state_dir, JOIN_MATERIAL_DIR, "ployz-join").map_err(|message| {
            KeeperStepEffectError::new(KeeperStepFailureReason::JoinMaterialStoreFailed, message)
        })?;
    commit_join_material_files(staged.path(), material)?;
    staged
        .commit_to(&state_dir.join(JOIN_MATERIAL_DIR))
        .map_err(|message| {
            KeeperStepEffectError::new(KeeperStepFailureReason::JoinMaterialStoreFailed, message)
        })
}

fn commit_join_material_files(
    directory: &Path,
    material: &KeeperJoinMaterial,
) -> Result<(), KeeperStepEffectError> {
    commit_join_material_file(
        directory,
        &render_redacted_join_material(&material.redacted()),
    )?;
    commit_join_material_secret_file(
        directory,
        JOIN_NATS_CREDENTIALS_FILE,
        material.nats_credentials().as_bytes(),
    )
}

pub trait KeeperCommandRunner {
    fn is_linux(&mut self) -> bool;
    fn current_uid(&mut self) -> Result<u32, FailureMessage>;
    fn systemctl(&mut self, args: &[&str]) -> Result<(), FailureMessage>;
    fn download(&mut self, url: &str, destination: &Path) -> Result<(), FailureMessage>;
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

    fn download(&mut self, url: &str, destination: &Path) -> Result<(), FailureMessage> {
        let output = run_os_command_with_display(
            "curl",
            &[
                OsString::from("-fsSL"),
                OsString::from("--output"),
                destination.as_os_str().to_os_string(),
                OsString::from("--"),
                OsString::from(url),
            ],
            "curl -fsSL --output <artifact> -- <redacted-url>".to_owned(),
            self.timeout,
        )?;
        if output.status.success() {
            return Ok(());
        }
        Err(failure_message(format!(
            "artifact download failed: {}",
            output.failure_summary()
        )))
    }
}

struct AcquiredArtifact {
    path: PathBuf,
    cleanup: AcquiredArtifactCleanup,
}

impl AcquiredArtifact {
    fn local(path: PathBuf) -> Self {
        Self {
            path,
            cleanup: AcquiredArtifactCleanup::Keep,
        }
    }

    fn downloaded(path: PathBuf) -> Self {
        Self {
            path,
            cleanup: AcquiredArtifactCleanup::Remove,
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for AcquiredArtifact {
    fn drop(&mut self) {
        if self.cleanup == AcquiredArtifactCleanup::Remove {
            let _ = fs::remove_file(&self.path);
            if let Some(parent) = self.path.parent() {
                let _ = fs::remove_dir(parent);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AcquiredArtifactCleanup {
    Keep,
    Remove,
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
    let args = args.iter().map(OsString::from).collect::<Vec<_>>();
    run_os_command(program, &args, timeout)
}

fn run_os_command(
    program: &str,
    args: &[OsString],
    timeout: Duration,
) -> Result<CapturedCommandOutput, FailureMessage> {
    run_os_command_with_display(program, args, render_command(program, args), timeout)
}

fn run_os_command_with_display(
    program: &str,
    args: &[OsString],
    display_command: String,
    timeout: Duration,
) -> Result<CapturedCommandOutput, FailureMessage> {
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| failure_message(format!("failed to run {display_command}: {error}")))?;
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
    let status = wait_for_child(&display_command, &mut child, timeout)?;
    Ok(CapturedCommandOutput {
        command: display_command,
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

fn render_command(program: &str, args: &[OsString]) -> String {
    if args.is_empty() {
        return program.to_owned();
    }
    let args = args
        .iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>();
    format!("{} {}", program, args.join(" "))
}

fn wait_for_child(
    command: &str,
    child: &mut Child,
    timeout: Duration,
) -> Result<ExitStatus, FailureMessage> {
    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| failure_message(format!("failed to wait for {command}: {error}")))?
        {
            return Ok(status);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(failure_message(format!(
                "{command} timed out after {}s",
                timeout.as_secs()
            )));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn create_download_path(target: &ArtifactTarget) -> Result<PathBuf, FailureMessage> {
    let directory = create_private_download_dir(target)?;
    Ok(directory.join("artifact"))
}

fn create_private_download_dir(target: &ArtifactTarget) -> Result<PathBuf, FailureMessage> {
    for attempt in 0..16 {
        let entropy = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| failure_message(format!("failed to create download path: {error}")))?
            .as_nanos();
        let name = format!(
            "ployz-download-{:?}-{}-{entropy}-{attempt}",
            target.kind(),
            std::process::id()
        );
        let directory = std::env::temp_dir().join(name);
        match create_private_directory(&directory) {
            Ok(()) => return Ok(directory),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(failure_message(format!(
                    "failed to create private artifact download directory {}: {error}",
                    directory.display()
                )));
            }
        }
    }

    Err(failure_message(format!(
        "failed to create unique artifact download directory in {}",
        std::env::temp_dir().display()
    )))
}

#[cfg(unix)]
fn create_private_directory(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private_directory(path: &Path) -> std::io::Result<()> {
    fs::create_dir(path)
}

fn ensure_directory(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    create_private_directory(path)
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
    write_durable_file(systemd_dir, unit_name, "ployz-unit", contents)
}

fn commit_join_material_file(
    directory: &Path,
    contents: &[u8],
) -> Result<(), KeeperStepEffectError> {
    write_durable_file(directory, JOIN_MATERIAL_FILE, "ployz-join", contents).map_err(|message| {
        KeeperStepEffectError::new(KeeperStepFailureReason::JoinMaterialStoreFailed, message)
    })
}

fn commit_join_material_secret_file(
    directory: &Path,
    file_name: &str,
    contents: &[u8],
) -> Result<(), KeeperStepEffectError> {
    write_durable_secret_file(directory, file_name, "ployz-join-secret", contents).map_err(
        |message| {
            KeeperStepEffectError::new(KeeperStepFailureReason::JoinMaterialStoreFailed, message)
        },
    )
}

fn write_durable_file(
    directory: &Path,
    file_name: &str,
    staged_tag: &str,
    contents: &[u8],
) -> Result<(), FailureMessage> {
    write_durable_file_with(
        directory,
        file_name,
        staged_tag,
        contents,
        create_staged_file,
    )
}

fn write_durable_secret_file(
    directory: &Path,
    file_name: &str,
    staged_tag: &str,
    contents: &[u8],
) -> Result<(), FailureMessage> {
    write_durable_file_with(
        directory,
        file_name,
        staged_tag,
        contents,
        create_staged_secret_file,
    )
}

fn write_durable_file_with(
    directory: &Path,
    file_name: &str,
    staged_tag: &str,
    contents: &[u8],
    create: fn(&Path, &str, &str) -> Result<StagedFile, FailureMessage>,
) -> Result<(), FailureMessage> {
    let path = directory.join(file_name);
    let mut staged = create(directory, file_name, staged_tag)?;
    staged.file.write_all(contents).map_err(|error| {
        failure_message(format!(
            "failed to write staged file {}: {error}",
            staged.path.display()
        ))
    })?;
    staged.file.sync_all().map_err(|error| {
        failure_message(format!(
            "failed to sync staged file {}: {error}",
            staged.path.display()
        ))
    })?;
    staged.commit_to(&path)?;
    sync_directory(directory).map_err(|error| {
        failure_message(format!(
            "file {} was installed but directory sync failed for {}: {error}",
            path.display(),
            directory.display()
        ))
    })
}

struct StagedFile {
    path: PathBuf,
    file: File,
    committed: bool,
}

impl StagedFile {
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
                "failed to commit staged file {} to {}: {error}",
                self.path.display(),
                path.display()
            ))
        })?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct StagedDirectory {
    path: PathBuf,
    committed: bool,
}

impl StagedDirectory {
    fn create(directory: &Path, name: &str, staged_tag: &str) -> Result<Self, FailureMessage> {
        for attempt in 0..16 {
            let staged_path = unique_staged_path(directory, name, staged_tag, attempt)?;
            match create_private_directory(&staged_path) {
                Ok(()) => {
                    return Ok(Self {
                        path: staged_path,
                        committed: false,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(failure_message(format!(
                        "failed to create staged directory {}: {error}",
                        staged_path.display()
                    )));
                }
            }
        }

        Err(failure_message(format!(
            "failed to create a unique staged directory in {}",
            directory.display()
        )))
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn commit_to(mut self, path: &Path) -> Result<(), FailureMessage> {
        let backup = unique_staged_path(
            path.parent().unwrap_or_else(|| Path::new(".")),
            JOIN_MATERIAL_DIR,
            "previous",
            0,
        )?;
        let had_previous = path.exists();
        if had_previous {
            fs::rename(path, &backup).map_err(|error| {
                failure_message(format!(
                    "failed to move previous join material directory {} to {}: {error}",
                    path.display(),
                    backup.display()
                ))
            })?;
        }

        if let Err(error) = fs::rename(&self.path, path) {
            if had_previous {
                let _ = fs::rename(&backup, path);
            }
            return Err(failure_message(format!(
                "failed to commit staged directory {} to {}: {error}",
                self.path.display(),
                path.display()
            )));
        }

        self.committed = true;
        if had_previous {
            let _ = fs::remove_dir_all(&backup);
        }
        sync_directory(path.parent().unwrap_or_else(|| Path::new("."))).map_err(|error| {
            failure_message(format!(
                "directory {} was installed but parent sync failed: {error}",
                path.display()
            ))
        })
    }
}

impl Drop for StagedDirectory {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn create_staged_file(
    directory: &Path,
    file_name: &str,
    staged_tag: &str,
) -> Result<StagedFile, FailureMessage> {
    create_staged_file_with(directory, file_name, staged_tag, open_staged_file)
}

fn create_staged_secret_file(
    directory: &Path,
    file_name: &str,
    staged_tag: &str,
) -> Result<StagedFile, FailureMessage> {
    create_staged_file_with(directory, file_name, staged_tag, open_secret_staged_file)
}

fn create_staged_file_with(
    directory: &Path,
    file_name: &str,
    staged_tag: &str,
    open: fn(&Path) -> std::io::Result<File>,
) -> Result<StagedFile, FailureMessage> {
    for attempt in 0..16 {
        let staged_path = unique_staged_file_path(directory, file_name, staged_tag, attempt)?;
        match open(&staged_path) {
            Ok(file) => return Ok(StagedFile::new(staged_path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(failure_message(format!(
                    "failed to create staged file {}: {error}",
                    staged_path.display()
                )));
            }
        }
    }

    Err(failure_message(format!(
        "failed to create a unique staged file in {}",
        directory.display()
    )))
}

fn open_staged_file(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

#[cfg(unix)]
fn open_secret_staged_file(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_secret_staged_file(path: &Path) -> std::io::Result<File> {
    open_staged_file(path)
}

fn unique_staged_file_path(
    directory: &Path,
    file_name: &str,
    staged_tag: &str,
    attempt: u8,
) -> Result<PathBuf, FailureMessage> {
    unique_staged_path(directory, file_name, staged_tag, attempt)
}

fn unique_staged_path(
    directory: &Path,
    file_name: &str,
    staged_tag: &str,
    attempt: u8,
) -> Result<PathBuf, FailureMessage> {
    let entropy = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| failure_message(format!("failed to create staged file name: {error}")))?
        .as_nanos();
    Ok(directory.join(format!(
        ".{file_name}.{staged_tag}-{}-{entropy}-{attempt}",
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

#[cfg(test)]
mod tests {
    use super::{OsString, run_os_command_with_display};
    use std::time::Duration;

    #[test]
    fn command_failure_summary_uses_redacted_display_command() {
        let secret_url = "https://example.invalid/artifact?token=secret";
        let output = run_os_command_with_display(
            "false",
            &[OsString::from(secret_url)],
            "download <redacted-url>".to_owned(),
            Duration::from_secs(1),
        )
        .expect("false command runs");

        let summary = output.failure_summary();

        assert!(summary.contains("download <redacted-url>"));
        assert!(!summary.contains(secret_url));
        assert!(!summary.contains("secret"));
    }
}
