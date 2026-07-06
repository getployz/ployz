//! Local keeper effects.

use std::fs;
use std::path::{Path, PathBuf};

use ployz_core::ops::FailureMessage;
use tempfile::{Builder, TempDir};

use crate::artifacts::{
    ArtifactInstallDurability, ArtifactKind, ArtifactSourceView, ArtifactTarget,
    install_verified_artifact, install_verified_nats_server_archive, verify_artifact_file,
};
use crate::command::KeeperCommandRunner;
use crate::executor::KeeperStepEffects;
use crate::fsx::{FileMode, StagedDirectory, ensure_directory, write_durable_file};
use crate::join::{
    JOIN_MATERIAL_DIR, JOIN_MATERIAL_FILE, JOIN_NATS_CREDENTIALS_FILE, JOIN_TRUSTED_CA_FILE,
    render_redacted_join_material,
};
use crate::steps::{
    ContainerRuntime, HostPrerequisite, KeeperJoinMaterial, KeeperStep, KeeperStepEffectError,
    KeeperStepFailureReason, MachineJoinTemplateTarget, NatsAuthorizedUsersTarget,
    NatsClientCredentialsTarget, NatsServerConfigTarget, NatsTlsMaterialTarget,
    PloyzdRoleEnvironmentStep,
};
use crate::systemd::{SupervisorUnitSpec, SupervisorUnitTarget};

const DOCKER_INSTALL_SCRIPT_URL: &str = "https://get.docker.com";

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
            KeeperStep::PrepareDataplaneHost => self.prepare_dataplane_host(),
            KeeperStep::PrepareContainerRuntime(runtime) => {
                self.prepare_container_runtime(*runtime)
            }
            KeeperStep::VerifyContainerRuntime(runtime) => self.verify_container_runtime(*runtime),
            KeeperStep::InstallArtifact(target) => self.install_artifact_source(target),
            KeeperStep::WritePloyzdRoleEnvironment(step) => {
                self.write_ployzd_role_environment(step).map_err(Into::into)
            }
            KeeperStep::WriteNatsTlsMaterial(target) => {
                self.write_nats_tls_material(target).map_err(Into::into)
            }
            KeeperStep::WriteNatsAuthorizedUsers(target) => {
                self.write_nats_authorized_users(target).map_err(Into::into)
            }
            KeeperStep::WriteNatsClientCredentials(target) => self
                .write_nats_client_credentials(target)
                .map_err(Into::into),
            KeeperStep::WriteNatsServerConfig(target) => {
                self.write_nats_server_config(target).map_err(Into::into)
            }
            KeeperStep::WriteMachineJoinTemplate(target) => {
                self.write_machine_join_template(target).map_err(Into::into)
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

    fn prepare_container_runtime(
        &mut self,
        runtime: ContainerRuntime,
    ) -> Result<(), KeeperStepEffectError> {
        match runtime {
            ContainerRuntime::Docker => self.prepare_docker().map_err(|message| {
                KeeperStepEffectError::new(
                    KeeperStepFailureReason::ContainerRuntimePrepareFailed,
                    message,
                )
            }),
        }
    }

    fn prepare_dataplane_host(&mut self) -> Result<(), KeeperStepEffectError> {
        self.runner.prepare_dataplane_host().map_err(|message| {
            KeeperStepEffectError::new(KeeperStepFailureReason::DataplaneHostPrepareFailed, message)
        })
    }

    fn verify_container_runtime(
        &mut self,
        runtime: ContainerRuntime,
    ) -> Result<(), KeeperStepEffectError> {
        match runtime {
            ContainerRuntime::Docker => self.runner.docker_info().map_err(|message| {
                KeeperStepEffectError::new(
                    KeeperStepFailureReason::ContainerRuntimeVerifyFailed,
                    message,
                )
            }),
        }
    }

    fn prepare_docker(&mut self) -> Result<(), FailureMessage> {
        if self.runner.docker_info().is_ok() {
            return Ok(());
        }

        if self.runner.enable_docker_service().is_ok() && self.runner.docker_info().is_ok() {
            return Ok(());
        }

        let script = DockerInstallScript::new()?;
        self.runner
            .download(DOCKER_INSTALL_SCRIPT_URL, script.path())?;
        self.runner.run_docker_install_script(script.path())?;
        self.runner.enable_docker_service()
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
            FileMode::Plain,
            target.render_config().as_bytes(),
        )
    }

    fn write_ployzd_role_environment(
        &self,
        step: &PloyzdRoleEnvironmentStep,
    ) -> Result<(), FailureMessage> {
        let file = step.file();
        let directory = file
            .path()
            .parent()
            .expect("validated ployzd role env path has a directory")
            .to_path_buf();
        let file_name = file
            .path()
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .expect("validated ployzd role env path has a UTF-8 file name");
        fs::create_dir_all(&directory).map_err(|error| {
            failure_message(format!(
                "failed to create ployzd environment directory {}: {error}",
                directory.display()
            ))
        })?;
        write_durable_file(
            &directory,
            file_name,
            FileMode::Plain,
            step.render().as_bytes(),
        )
    }

    fn write_machine_join_template(
        &self,
        target: &MachineJoinTemplateTarget,
    ) -> Result<(), FailureMessage> {
        let path = target.path();
        let directory = path
            .parent()
            .expect("validated machine join template path has a directory")
            .to_path_buf();
        let file_name = path
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .expect("validated machine join template path has a UTF-8 file name");
        fs::create_dir_all(&directory).map_err(|error| {
            failure_message(format!(
                "failed to create machine join template directory {}: {error}",
                directory.display()
            ))
        })?;
        write_durable_file(
            &directory,
            file_name,
            FileMode::Plain,
            target.render().as_bytes(),
        )
    }

    fn write_nats_tls_material(
        &self,
        target: &NatsTlsMaterialTarget,
    ) -> Result<(), FailureMessage> {
        create_nats_state_dir(target.state_dir())?;
        write_durable_file(
            target.state_dir(),
            &nats_file_name(&target.material().ca_file()),
            FileMode::Plain,
            target.ca_pem().as_str().as_bytes(),
        )?;
        write_durable_file(
            target.state_dir(),
            &nats_file_name(&target.material().server_cert_file()),
            FileMode::Plain,
            target.server_cert_pem().as_str().as_bytes(),
        )?;
        write_durable_file(
            target.state_dir(),
            &nats_file_name(&target.material().server_key_file()),
            FileMode::Secret0600,
            target.server_key_pem().secret().as_bytes(),
        )?;
        write_durable_file(
            target.state_dir(),
            &nats_file_name(&target.material().recovery_key_file()),
            FileMode::Secret0600,
            target.recovery_key_wrapped(),
        )
    }

    fn write_nats_authorized_users(
        &self,
        target: &NatsAuthorizedUsersTarget,
    ) -> Result<(), FailureMessage> {
        fs::create_dir_all(target.config_dir()).map_err(|error| {
            failure_message(format!(
                "failed to create nats-server config directory {}: {error}",
                target.config_dir().display()
            ))
        })?;
        write_durable_file(
            target.config_dir(),
            target.file_name(),
            FileMode::Plain,
            target.render().as_bytes(),
        )
    }

    fn write_nats_client_credentials(
        &self,
        target: &NatsClientCredentialsTarget,
    ) -> Result<(), FailureMessage> {
        create_nats_state_dir(target.state_dir())?;
        write_durable_file(
            target.state_dir(),
            &nats_file_name(&target.material().controller_seed_file()),
            FileMode::Secret0600,
            target.controller_seed().secret().as_bytes(),
        )?;
        write_durable_file(
            target.state_dir(),
            &nats_file_name(&target.material().operator_seed_file()),
            FileMode::Secret0600,
            target.operator_seed().secret().as_bytes(),
        )?;
        write_durable_file(
            target.state_dir(),
            &nats_file_name(&target.material().join_seed_file()),
            FileMode::Secret0600,
            target.join_seed().secret().as_bytes(),
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
        let verified = verify_artifact_file(artifact.path(), &target.digest).map_err(|error| {
            KeeperStepEffectError::new(
                KeeperStepFailureReason::ArtifactVerificationFailed,
                failure_message(error.to_string()),
            )
        })?;
        let installed = match target.kind {
            ArtifactKind::NatsServer => install_verified_nats_server_archive(&verified, target),
            ArtifactKind::EbpfBytecode | ArtifactKind::EbpfCtl | ArtifactKind::Ployzd => {
                install_verified_artifact(&verified, target)
            }
        }
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
        match target.source_view() {
            ArtifactSourceView::LocalPath(path) => Ok(AcquiredArtifact::local(path.to_path_buf())),
            ArtifactSourceView::RemoteUrl(url) => {
                let artifact = AcquiredArtifact::downloaded()?;
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
    join_material_write(
        directory,
        JOIN_MATERIAL_FILE,
        FileMode::Plain,
        &render_redacted_join_material(&material.redacted()),
    )?;
    join_material_write(
        directory,
        JOIN_TRUSTED_CA_FILE,
        FileMode::Plain,
        material.trusted_ca_pem().as_str().as_bytes(),
    )?;
    join_material_write(
        directory,
        JOIN_NATS_CREDENTIALS_FILE,
        FileMode::Secret0600,
        material.nats_credentials().secret().as_bytes(),
    )
}

fn join_material_write(
    directory: &Path,
    file_name: &str,
    mode: FileMode,
    contents: &[u8],
) -> Result<(), KeeperStepEffectError> {
    write_durable_file(directory, file_name, mode, contents).map_err(|message| {
        KeeperStepEffectError::new(KeeperStepFailureReason::JoinMaterialStoreFailed, message)
    })
}

struct AcquiredArtifact {
    source: AcquiredArtifactSource,
}

impl AcquiredArtifact {
    fn local(path: PathBuf) -> Self {
        Self {
            source: AcquiredArtifactSource::Local(path),
        }
    }

    fn downloaded() -> Result<Self, FailureMessage> {
        let directory = Builder::new()
            .prefix("ployz-artifact-")
            .tempdir()
            .map_err(|error| {
                failure_message(format!(
                    "failed to create artifact download directory: {error}"
                ))
            })?;
        let path = directory.path().join("artifact");
        Ok(Self {
            source: AcquiredArtifactSource::Downloaded {
                _directory: directory,
                path,
            },
        })
    }

    fn path(&self) -> &Path {
        match &self.source {
            AcquiredArtifactSource::Local(path) => path,
            AcquiredArtifactSource::Downloaded { path, .. } => path,
        }
    }
}

enum AcquiredArtifactSource {
    Local(PathBuf),
    Downloaded { _directory: TempDir, path: PathBuf },
}

struct DockerInstallScript {
    _directory: TempDir,
    path: PathBuf,
}

impl DockerInstallScript {
    fn new() -> Result<Self, FailureMessage> {
        let directory = Builder::new()
            .prefix("ployz-docker-install-")
            .tempdir()
            .map_err(|error| {
                failure_message(format!(
                    "failed to create Docker install directory: {error}"
                ))
            })?;
        let path = directory.path().join("install-docker.sh");
        Ok(Self {
            _directory: directory,
            path,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

fn create_nats_state_dir(state_dir: &Path) -> Result<(), FailureMessage> {
    if let Some(parent) = state_dir.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            failure_message(format!(
                "failed to create NATS state parent directory {}: {error}",
                parent.display()
            ))
        })?;
    }
    ensure_directory(state_dir).map_err(|error| {
        failure_message(format!(
            "failed to create NATS state directory {}: {error}",
            state_dir.display()
        ))
    })
}

fn nats_file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|file_name| file_name.to_str())
        .expect("NATS material paths have UTF-8 file names")
        .to_owned()
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
    write_durable_file(systemd_dir, unit_name, FileMode::Plain, contents)
}

fn failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message).expect("keeper generated a non-empty failure message")
}
