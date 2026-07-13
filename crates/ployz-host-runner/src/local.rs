//! Local Host Runner effects.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ployz_core::install::HostPortAssurance;
use ployz_core::ops::FailureMessage;
use tempfile::{Builder, TempDir};

use crate::artifacts::{
    ArtifactInstallDurability, ArtifactKind, ArtifactSourceView, ArtifactTarget,
    install_verified_artifact, install_verified_nats_server_archive, verify_artifact_file,
};
use crate::assigned_substrate::{
    AssignedHostPort, AssignedSubstrateState, write_assigned_substrate_state,
};
use crate::command::{HostRunnerCommandOutput, HostRunnerCommandRunner};
use crate::executor::HostRunnerStepEffects;
use crate::firewall::{FirewallBackend, detect_firewall_backend};
use crate::fsx::{FileMode, StagedDirectory, ensure_directory, write_durable_file};
use crate::host_platform::{
    DockerInstall, HostPackageFamily, HostPlatformProfile, detect_host_platform,
};
use crate::join::{
    JOIN_CORE_SEEDS_FILE, JOIN_MATERIAL_DIR, JOIN_MATERIAL_FILE, JOIN_NATS_CREDENTIALS_FILE,
    JOIN_RECOVERY_KEY_FILE, JOIN_TRUSTED_CA_FILE, render_redacted_join_material,
};
use crate::steps::{
    ContainerRuntime, HostPrerequisite, HostRunnerJoinMaterial, HostRunnerStep,
    HostRunnerStepEffectError, HostRunnerStepFailureReason, MachineJoinTemplateTarget,
    NatsAuthorizedUsersTarget, NatsClientCredentialsTarget, NatsServerConfigTarget,
    NatsTlsMaterialTarget, PloyzdRoleEnvironmentStep,
};
use crate::supervisor::{
    SupervisorBackend, SupervisorChange, SupervisorDirectories, execute_supervisor_commands,
};
use crate::systemd::{SupervisorUnitSpec, SupervisorUnitTarget};

const DOCKER_INSTALL_SCRIPT_URL: &str = "https://get.docker.com";
const DOCKER_INSTALL_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone)]
pub struct HostRunnerLocalConfig {
    pub supervisor_dirs: SupervisorDirectories,
    pub state_dir: PathBuf,
    pub docker_daemon_config: PathBuf,
    pub docker_repository_dir: PathBuf,
}

pub struct HostRunnerLocalEffects<R> {
    config: HostRunnerLocalConfig,
    runner: R,
    platform: Option<HostPlatformProfile>,
}

impl<R> HostRunnerLocalEffects<R> {
    #[must_use]
    pub fn new(config: HostRunnerLocalConfig, runner: R) -> Self {
        Self {
            config,
            runner,
            platform: None,
        }
    }

    #[must_use]
    pub const fn runner(&self) -> &R {
        &self.runner
    }
}

impl<R: HostRunnerCommandRunner> HostRunnerStepEffects for HostRunnerLocalEffects<R> {
    fn apply_step(&mut self, step: &HostRunnerStep) -> Result<(), HostRunnerStepEffectError> {
        match step {
            HostRunnerStep::VerifyHost(prerequisite) => {
                self.verify_host(*prerequisite).map_err(Into::into)
            }
            HostRunnerStep::PreflightHostPorts(state) => self.preflight_host_ports(state),
            HostRunnerStep::AssureHostPorts(state) => self.assure_host_ports(state),
            HostRunnerStep::StoreAssignedSubstrate(state) => self.store_assigned_substrate(state),
            HostRunnerStep::PrepareDataplaneHost => self.prepare_dataplane_host(),
            HostRunnerStep::PrepareContainerRuntime(runtime, endpoint_supernet) => {
                self.prepare_container_runtime(*runtime, endpoint_supernet)
            }
            HostRunnerStep::VerifyContainerRuntime(runtime) => {
                self.verify_container_runtime(*runtime)
            }
            HostRunnerStep::InstallArtifact(target) => self.install_artifact_source(target),
            HostRunnerStep::WritePloyzdRoleEnvironment(step) => {
                self.write_ployzd_role_environment(step).map_err(Into::into)
            }
            HostRunnerStep::WriteNatsTlsMaterial(target) => {
                self.write_nats_tls_material(target).map_err(Into::into)
            }
            HostRunnerStep::WriteNatsAuthorizedUsers(target) => {
                self.write_nats_authorized_users(target).map_err(Into::into)
            }
            HostRunnerStep::WriteNatsClientCredentials(target) => self
                .write_nats_client_credentials(target)
                .map_err(Into::into),
            HostRunnerStep::WriteNatsServerConfig(target) => {
                self.write_nats_server_config(target).map_err(Into::into)
            }
            HostRunnerStep::WriteMachineJoinTemplate(target) => {
                self.write_machine_join_template(target).map_err(Into::into)
            }
            HostRunnerStep::WriteSupervisorUnit(target) => {
                self.write_supervisor_unit(target).map_err(Into::into)
            }
            HostRunnerStep::StartSupervisorUnit(target) => {
                self.start_supervisor_unit(target).map_err(Into::into)
            }
            HostRunnerStep::RestartSupervisorUnit(target) => {
                self.restart_supervisor_unit(target).map_err(Into::into)
            }
            HostRunnerStep::StoreJoinMaterial(material) => self.store_join_material(material),
        }
    }
}

impl<R: HostRunnerCommandRunner> HostRunnerLocalEffects<R> {
    fn preflight_host_ports(
        &mut self,
        state: &AssignedSubstrateState,
    ) -> Result<(), HostRunnerStepEffectError> {
        if state.host_ports == HostPortAssurance::External {
            return Ok(());
        }
        let supervisor = self.supervisor_backend().map_err(|message| {
            HostRunnerStepEffectError::new(
                HostRunnerStepFailureReason::HostPortsPreflightFailed,
                message,
            )
        })?;
        let backend = detect_firewall_backend(supervisor, &mut self.runner).map_err(|message| {
            HostRunnerStepEffectError::new(
                HostRunnerStepFailureReason::HostPortsPreflightFailed,
                message,
            )
        })?;
        if let FirewallBackend::Unmanaged(backend) = backend {
            return Err(HostRunnerStepEffectError::new(
                HostRunnerStepFailureReason::HostPortsPreflightFailed,
                failure_message(format!(
                    "host firewall backend {backend} is unmanaged; open exactly: {}",
                    render_assigned_host_ports(&state.required_host_ports())
                )),
            ));
        }
        Ok(())
    }

    fn assure_host_ports(
        &mut self,
        state: &AssignedSubstrateState,
    ) -> Result<(), HostRunnerStepEffectError> {
        if state.host_ports == HostPortAssurance::External {
            return Ok(());
        }
        let supervisor = self
            .supervisor_backend()
            .map_err(host_ports_assurance_error)?;
        let backend = detect_firewall_backend(supervisor, &mut self.runner)
            .map_err(host_ports_assurance_error)?;
        for port in state.required_host_ports() {
            backend
                .open_with(port, &mut self.runner)
                .map_err(host_ports_assurance_error)?;
        }
        Ok(())
    }

    fn store_assigned_substrate(
        &mut self,
        state: &AssignedSubstrateState,
    ) -> Result<(), HostRunnerStepEffectError> {
        ensure_directory(&self.config.state_dir).map_err(|message| {
            HostRunnerStepEffectError::new(
                HostRunnerStepFailureReason::AssignedSubstrateStoreFailed,
                failure_message(message.to_string()),
            )
        })?;
        write_assigned_substrate_state(&self.config.state_dir, state).map_err(|error| {
            HostRunnerStepEffectError::new(
                HostRunnerStepFailureReason::AssignedSubstrateStoreFailed,
                failure_message(error.to_string()),
            )
        })
    }

    fn verify_host(&mut self, prerequisite: HostPrerequisite) -> Result<(), FailureMessage> {
        match prerequisite {
            HostPrerequisite::LinuxRoot => {
                if !self.runner.is_linux() {
                    return Err(failure_message("Host Runner requires Linux"));
                }
                let uid = self.runner.current_uid()?;
                if uid != 0 {
                    return Err(failure_message("Host Runner must run as root"));
                }
                let os_release = self.runner.read_os_release()?;
                let profile = detect_host_platform(&os_release)
                    .map_err(|error| failure_message(error.to_string()))?;
                let backend = SupervisorBackend::from(profile.supervisor());
                let supervisor_dir = self.config.supervisor_dirs.directory(backend);
                if !supervisor_dir.is_dir() {
                    return Err(failure_message(format!(
                        "supervisor service directory {} is missing",
                        supervisor_dir.display()
                    )));
                }
                self.platform = Some(profile);
                Ok(())
            }
        }
    }

    fn prepare_container_runtime(
        &mut self,
        runtime: ContainerRuntime,
        endpoint_supernet: &ployz_core::dataplane::MachineEndpointSupernet,
    ) -> Result<(), HostRunnerStepEffectError> {
        match runtime {
            ContainerRuntime::Docker => self.prepare_docker(endpoint_supernet).map_err(|message| {
                let reason = match message {
                    PrepareDockerError::ClassicStore(message) => {
                        return HostRunnerStepEffectError::new(
                            HostRunnerStepFailureReason::ContainerRuntimeClassicStoreUnsupported,
                            message,
                        );
                    }
                    PrepareDockerError::Other(message) => message,
                };
                HostRunnerStepEffectError::new(
                    HostRunnerStepFailureReason::ContainerRuntimePrepareFailed,
                    reason,
                )
            }),
        }
    }

    fn prepare_dataplane_host(&mut self) -> Result<(), HostRunnerStepEffectError> {
        self.prepare_dataplane_packages().map_err(|message| {
            HostRunnerStepEffectError::new(
                HostRunnerStepFailureReason::DataplaneHostPrepareFailed,
                message,
            )
        })
    }

    fn prepare_dataplane_packages(&mut self) -> Result<(), FailureMessage> {
        if self.runner.dataplane_host_ready() {
            return Ok(());
        }
        match self.host_platform()?.package_family() {
            HostPackageFamily::Debian => {
                self.require_long_command("apt-get", &["update"])?;
                self.require_long_command(
                    "env",
                    &[
                        "DEBIAN_FRONTEND=noninteractive",
                        "apt-get",
                        "install",
                        "-y",
                        "wireguard-tools",
                        "iproute2",
                        "iputils-ping",
                    ],
                )?;
            }
            HostPackageFamily::Rpm => {
                let _ = self.rpm_command(&["install", "-y", "epel-release"]);
                self.require_rpm_command(&[
                    "install",
                    "-y",
                    "wireguard-tools",
                    "iproute",
                    "iputils",
                ])?;
            }
            HostPackageFamily::Arch => self.require_long_command(
                "pacman",
                &[
                    "-Sy",
                    "--noconfirm",
                    "--needed",
                    "wireguard-tools",
                    "iproute2",
                    "iputils",
                ],
            )?,
            HostPackageFamily::Alpine => self
                .require_long_command("apk", &["add", "wireguard-tools", "iproute2", "iputils"])?,
            HostPackageFamily::Suse => {
                self.require_long_command("zypper", &["refresh"])?;
                self.require_long_command(
                    "zypper",
                    &[
                        "--non-interactive",
                        "install",
                        "wireguard-tools",
                        "iproute2",
                        "iputils",
                    ],
                )?;
            }
        }
        if self.runner.dataplane_host_ready() {
            return Ok(());
        }
        Err(failure_message(
            "dataplane host packages installed but wg/ip/tc/tun are not ready",
        ))
    }

    fn verify_container_runtime(
        &mut self,
        runtime: ContainerRuntime,
    ) -> Result<(), HostRunnerStepEffectError> {
        match runtime {
            ContainerRuntime::Docker => {
                self.runner.docker_info().map_err(|message| {
                    HostRunnerStepEffectError::new(
                        HostRunnerStepFailureReason::ContainerRuntimeVerifyFailed,
                        message,
                    )
                })?;
                let containerd =
                    self.runner
                        .docker_uses_containerd_snapshotter()
                        .map_err(|message| {
                            HostRunnerStepEffectError::new(
                                HostRunnerStepFailureReason::ContainerRuntimeVerifyFailed,
                                message,
                            )
                        })?;
                if !containerd {
                    return Err(classic_store_failure());
                }
                Ok(())
            }
        }
    }

    fn prepare_docker(
        &mut self,
        endpoint_supernet: &ployz_core::dataplane::MachineEndpointSupernet,
    ) -> Result<(), PrepareDockerError> {
        let platform = self.host_platform()?.clone();
        let endpoint_supernet = endpoint_supernet.as_string();
        if self.runner.docker_info().is_ok() {
            return self.prepare_running_docker(&endpoint_supernet);
        }

        let enable_result = self.apply_docker_supervisor_change(SupervisorChange::InstallAndStart);
        if enable_result.is_ok() && self.runner.docker_info().is_ok() {
            return self.prepare_running_docker(&endpoint_supernet);
        }
        if self.runner.docker_is_installed() {
            return Err(PrepareDockerError::Other(
                enable_result.err().unwrap_or_else(|| {
                    failure_message("Docker is installed but did not become ready after enabling")
                }),
            ));
        }

        merge_docker_daemon_config(&self.config.docker_daemon_config, &endpoint_supernet)?;
        self.install_docker(&platform)?;
        self.apply_docker_supervisor_change(SupervisorChange::InstallAndStart)?;
        self.verify_running_docker(&endpoint_supernet)
    }

    fn install_docker(&mut self, platform: &HostPlatformProfile) -> Result<(), FailureMessage> {
        match platform.docker_install() {
            DockerInstall::GetDocker => {
                let script = TemporaryDownload::new("get-docker.sh")?;
                self.runner
                    .download(DOCKER_INSTALL_SCRIPT_URL, script.path())?;
                let script = script.path().to_string_lossy();
                self.require_long_command("sh", &[script.as_ref()])
            }
            DockerInstall::AlpinePackages => {
                self.require_long_command("apk", &["add", "docker", "docker-cli-compose"])
            }
            DockerInstall::ArchPackages => self.require_long_command(
                "pacman",
                &[
                    "-Syu",
                    "--noconfirm",
                    "--needed",
                    "docker",
                    "docker-compose",
                ],
            ),
            DockerInstall::AmazonPackages => self.require_rpm_command(&["install", "-y", "docker"]),
            DockerInstall::RhelRepositoryFile => self
                .install_docker_repository("https://download.docker.com/linux/rhel/docker-ce.repo"),
            DockerInstall::CentosRepositoryFile => self.install_docker_repository(
                "https://download.docker.com/linux/centos/docker-ce.repo",
            ),
        }
    }

    fn install_docker_repository(&mut self, url: &str) -> Result<(), FailureMessage> {
        let repository = TemporaryDownload::new("docker-ce.repo")?;
        self.runner.download(url, repository.path())?;
        let contents = fs::read(repository.path()).map_err(|error| {
            failure_message(format!(
                "failed to read downloaded Docker repository: {error}"
            ))
        })?;
        fs::create_dir_all(&self.config.docker_repository_dir).map_err(|error| {
            failure_message(format!(
                "failed to create Docker repository directory {}: {error}",
                self.config.docker_repository_dir.display()
            ))
        })?;
        write_durable_file(
            &self.config.docker_repository_dir,
            "docker-ce.repo",
            FileMode::Plain,
            &contents,
        )?;
        self.require_rpm_command(&[
            "install",
            "-y",
            "docker-ce",
            "docker-ce-cli",
            "containerd.io",
            "docker-buildx-plugin",
            "docker-compose-plugin",
        ])
    }

    fn rpm_command(&mut self, args: &[&str]) -> Result<HostRunnerCommandOutput, FailureMessage> {
        self.runner
            .command_with_timeout("dnf", args, DOCKER_INSTALL_TIMEOUT)
            .or_else(|_| {
                self.runner
                    .command_with_timeout("yum", args, DOCKER_INSTALL_TIMEOUT)
            })
    }

    fn require_rpm_command(&mut self, args: &[&str]) -> Result<(), FailureMessage> {
        let output = self.rpm_command(args)?;
        if output.success {
            return Ok(());
        }
        Err(failure_message(output.failure))
    }

    fn require_long_command(&mut self, program: &str, args: &[&str]) -> Result<(), FailureMessage> {
        let output = self
            .runner
            .command_with_timeout(program, args, DOCKER_INSTALL_TIMEOUT)?;
        if output.success {
            return Ok(());
        }
        Err(failure_message(output.failure))
    }

    fn prepare_running_docker(
        &mut self,
        endpoint_supernet: &str,
    ) -> Result<(), PrepareDockerError> {
        if !self.runner.docker_uses_containerd_snapshotter()? {
            return Err(PrepareDockerError::ClassicStore(classic_store_message()));
        }
        if merge_docker_daemon_config(&self.config.docker_daemon_config, endpoint_supernet)? {
            self.apply_docker_supervisor_change(SupervisorChange::Restart)?;
        }
        self.verify_running_docker(endpoint_supernet)
    }

    fn verify_running_docker(&mut self, endpoint_supernet: &str) -> Result<(), PrepareDockerError> {
        self.runner.docker_info()?;
        if !self.runner.docker_uses_containerd_snapshotter()? {
            return Err(PrepareDockerError::ClassicStore(classic_store_message()));
        }
        if !self
            .runner
            .docker_has_insecure_registry(endpoint_supernet)?
        {
            return Err(PrepareDockerError::Other(failure_message(format!(
                "Docker is not running with insecure registry CIDR {endpoint_supernet}"
            ))));
        }
        Ok(())
    }

    fn write_supervisor_unit(&self, spec: &SupervisorUnitSpec) -> Result<(), FailureMessage> {
        let backend = self.supervisor_backend()?;
        let rendered = backend
            .render(spec)
            .map_err(|error| failure_message(error.to_string()))?;
        write_supervisor_file(
            self.config.supervisor_dirs.directory(backend),
            rendered.file_name(),
            match backend {
                SupervisorBackend::Systemd => FileMode::Plain,
                SupervisorBackend::OpenRc => FileMode::Executable0755,
            },
            rendered.contents().as_bytes(),
        )
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
            target.recovery_key_wrapped().as_bytes(),
        )?;
        write_durable_file(
            target.state_dir(),
            &nats_file_name(&target.material().core_seeds_file()),
            FileMode::Secret0600,
            target.core_seeds_wrapped().as_bytes(),
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
        self.apply_supervisor_change(SupervisorChange::InstallAndStart, target)
    }

    fn restart_supervisor_unit(
        &mut self,
        target: &SupervisorUnitTarget,
    ) -> Result<(), FailureMessage> {
        self.apply_supervisor_change(SupervisorChange::ReloadAndRestart, target)
    }

    fn supervisor_backend(&self) -> Result<SupervisorBackend, FailureMessage> {
        Ok(self.host_platform()?.supervisor().into())
    }

    fn host_platform(&self) -> Result<&HostPlatformProfile, FailureMessage> {
        let Some(platform) = &self.platform else {
            return Err(failure_message(
                "host platform was not validated before host mutation",
            ));
        };
        Ok(platform)
    }

    fn apply_supervisor_change(
        &mut self,
        change: SupervisorChange,
        target: &SupervisorUnitTarget,
    ) -> Result<(), FailureMessage> {
        let commands = self.supervisor_backend()?.commands(change, target);
        self.run_supervisor_commands(commands)
    }

    fn apply_docker_supervisor_change(
        &mut self,
        change: SupervisorChange,
    ) -> Result<(), FailureMessage> {
        let commands = self.supervisor_backend()?.docker_commands(change);
        self.run_supervisor_commands(commands)
    }

    fn run_supervisor_commands(
        &mut self,
        commands: Vec<(&'static str, Vec<String>)>,
    ) -> Result<(), FailureMessage> {
        execute_supervisor_commands(&mut self.runner, commands)
    }

    fn install_artifact_source(
        &mut self,
        target: &ArtifactTarget,
    ) -> Result<(), HostRunnerStepEffectError> {
        let artifact = self.acquire_artifact(target).map_err(|message| {
            HostRunnerStepEffectError::new(
                HostRunnerStepFailureReason::ArtifactDownloadFailed,
                message,
            )
        })?;
        let verified = verify_artifact_file(artifact.path(), &target.digest).map_err(|error| {
            HostRunnerStepEffectError::new(
                HostRunnerStepFailureReason::ArtifactVerificationFailed,
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
        material: &HostRunnerJoinMaterial,
    ) -> Result<(), HostRunnerStepEffectError> {
        ensure_directory(&self.config.state_dir).map_err(|error| {
            HostRunnerStepEffectError::new(
                HostRunnerStepFailureReason::JoinMaterialStoreFailed,
                failure_message(format!(
                    "failed to create Host Runner state directory {}: {error}",
                    self.config.state_dir.display()
                )),
            )
        })?;
        commit_join_material_directory(&self.config.state_dir, material)
    }
}

fn render_assigned_host_ports(ports: &[AssignedHostPort]) -> String {
    ports
        .iter()
        .map(|port| {
            let protocol = match port.protocol {
                crate::assigned_substrate::HostPortProtocol::Tcp => "tcp",
                crate::assigned_substrate::HostPortProtocol::Udp => "udp",
            };
            format!("{}/{protocol}", port.port)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn host_ports_assurance_error(message: FailureMessage) -> HostRunnerStepEffectError {
    HostRunnerStepEffectError::new(
        HostRunnerStepFailureReason::HostPortsAssuranceFailed,
        message,
    )
}

#[derive(Debug)]
enum PrepareDockerError {
    ClassicStore(FailureMessage),
    Other(FailureMessage),
}

impl From<FailureMessage> for PrepareDockerError {
    fn from(message: FailureMessage) -> Self {
        Self::Other(message)
    }
}

fn classic_store_failure() -> HostRunnerStepEffectError {
    HostRunnerStepEffectError::new(
        HostRunnerStepFailureReason::ContainerRuntimeClassicStoreUnsupported,
        classic_store_message(),
    )
}

fn classic_store_message() -> FailureMessage {
    failure_message(
        "Docker is running with the classic image store; explicitly enable features.containerd-snapshotter in /etc/docker/daemon.json and restart Docker (switching stores hides existing images)",
    )
}

fn merge_docker_daemon_config(
    path: &Path,
    endpoint_supernet: &str,
) -> Result<bool, FailureMessage> {
    let mut config = if path.exists() {
        let bytes = fs::read(path).map_err(|error| {
            failure_message(format!(
                "failed to read Docker daemon config {}: {error}",
                path.display()
            ))
        })?;
        serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|error| {
            failure_message(format!(
                "failed to parse Docker daemon config {}: {error}",
                path.display()
            ))
        })?
    } else {
        serde_json::Value::Object(serde_json::Map::new())
    };
    let Some(config) = config.as_object_mut() else {
        return Err(failure_message(format!(
            "Docker daemon config {} must be a JSON object",
            path.display()
        )));
    };

    let features = config
        .entry("features")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(features) = features.as_object_mut() else {
        return Err(failure_message(
            "Docker daemon config features must be a JSON object",
        ));
    };
    let mut changed =
        features.get("containerd-snapshotter") != Some(&serde_json::Value::Bool(true));
    features.insert(
        "containerd-snapshotter".to_owned(),
        serde_json::Value::Bool(true),
    );

    let insecure_registries = config
        .entry("insecure-registries")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    let Some(insecure_registries) = insecure_registries.as_array_mut() else {
        return Err(failure_message(
            "Docker daemon config insecure-registries must be a JSON array",
        ));
    };
    if !insecure_registries
        .iter()
        .any(|entry| entry.as_str() == Some(endpoint_supernet))
    {
        insecure_registries.push(serde_json::Value::String(endpoint_supernet.to_owned()));
        changed = true;
    }
    if !changed {
        return Ok(false);
    }

    let directory = path.parent().ok_or_else(|| {
        failure_message(format!(
            "Docker daemon config path {} has no parent directory",
            path.display()
        ))
    })?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            failure_message(format!(
                "Docker daemon config path {} has no UTF-8 file name",
                path.display()
            ))
        })?;
    fs::create_dir_all(directory).map_err(|error| {
        failure_message(format!(
            "failed to create Docker daemon config directory {}: {error}",
            directory.display()
        ))
    })?;
    let mut rendered = serde_json::to_vec_pretty(&config).map_err(|error| {
        failure_message(format!("failed to render Docker daemon config: {error}"))
    })?;
    rendered.push(b'\n');
    write_durable_file(directory, file_name, FileMode::Plain, &rendered)?;
    Ok(true)
}

fn commit_join_material_directory(
    state_dir: &Path,
    material: &HostRunnerJoinMaterial,
) -> Result<(), HostRunnerStepEffectError> {
    let staged =
        StagedDirectory::create(state_dir, JOIN_MATERIAL_DIR, "ployz-join").map_err(|message| {
            HostRunnerStepEffectError::new(
                HostRunnerStepFailureReason::JoinMaterialStoreFailed,
                message,
            )
        })?;
    commit_join_material_files(staged.path(), material)?;
    staged
        .commit_to(&state_dir.join(JOIN_MATERIAL_DIR))
        .map_err(|message| {
            HostRunnerStepEffectError::new(
                HostRunnerStepFailureReason::JoinMaterialStoreFailed,
                message,
            )
        })
}

fn commit_join_material_files(
    directory: &Path,
    material: &HostRunnerJoinMaterial,
) -> Result<(), HostRunnerStepEffectError> {
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
    )?;
    // Deliver the wrapped CA key and core seeds so this joined machine can be
    // core-promoted, reusing the old core's identity verbatim (ADR 0031).
    join_material_write(
        directory,
        JOIN_RECOVERY_KEY_FILE,
        FileMode::Secret0600,
        material.recovery_key_wrapped().as_bytes(),
    )?;
    join_material_write(
        directory,
        JOIN_CORE_SEEDS_FILE,
        FileMode::Secret0600,
        material.core_seeds_wrapped().as_bytes(),
    )
}

fn join_material_write(
    directory: &Path,
    file_name: &str,
    mode: FileMode,
    contents: &[u8],
) -> Result<(), HostRunnerStepEffectError> {
    write_durable_file(directory, file_name, mode, contents).map_err(|message| {
        HostRunnerStepEffectError::new(
            HostRunnerStepFailureReason::JoinMaterialStoreFailed,
            message,
        )
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

struct TemporaryDownload {
    _directory: TempDir,
    path: PathBuf,
}

impl TemporaryDownload {
    fn new(file_name: &str) -> Result<Self, FailureMessage> {
        let directory = Builder::new()
            .prefix("ployz-download-")
            .tempdir()
            .map_err(|error| {
                failure_message(format!("failed to create download directory: {error}"))
            })?;
        let path = directory.path().join(file_name);
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

fn write_supervisor_file(
    supervisor_dir: &Path,
    unit_name: &str,
    mode: FileMode,
    contents: &[u8],
) -> Result<(), FailureMessage> {
    fs::create_dir_all(supervisor_dir).map_err(|error| {
        failure_message(format!(
            "failed to create supervisor service directory {}: {error}",
            supervisor_dir.display()
        ))
    })?;
    write_durable_file(supervisor_dir, unit_name, mode, contents)
}

fn failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message).expect("Host Runner generated a non-empty failure message")
}
