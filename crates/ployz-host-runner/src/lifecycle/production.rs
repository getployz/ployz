//! Shared Linux substrate mechanics composed by founding and join workflows.

use std::fs;
use std::net::Ipv6Addr;
use std::path::Path;
use std::thread;
use std::time::Duration;

use ployz_core::install::InstallArtifactSpec;
use ployz_core::operation::FailureMessage;

use crate::{
    ArtifactKind, ArtifactSourceView, ArtifactTarget, DockerInstall, FileMode, HostPlatformProfile,
    HostRunnerCommandRunner, PloyzdRole, PloyzdRoleEnvironmentFile, SupervisorBackend,
    SupervisorChange, SupervisorDirectories, SupervisorUnitSpec, SupervisorUnitTarget,
    acquire_remote_artifact_content_addressed, artifact_target, detect_host_platform,
    install_verified_artifact, verify_artifact_file, write_durable_file,
};

const CORROSION_API_PORT: u16 = 8_080;
const CORROSION_GOSSIP_PORT: u16 = 8_787;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CorrosionServiceChange {
    Enable,
    Restart,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CorrosionBootstrap<'a> {
    Founder,
    Seed(&'a str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GeneratedSecretPersistence {
    InMemory,
    Durable,
}

pub(super) struct CorrosionConfig<'a> {
    pub state: &'a Path,
    pub schema_path: &'a str,
    pub gossip_addr: Ipv6Addr,
    pub bootstrap: CorrosionBootstrap<'a>,
    pub bearer_token: &'a str,
}

pub(super) struct LinuxSubstrate<'a, R> {
    state: &'a Path,
    runner: &'a mut R,
    profile: &'a mut Option<HostPlatformProfile>,
    supervisor_directories: &'a SupervisorDirectories,
}

impl<'a, R: HostRunnerCommandRunner> LinuxSubstrate<'a, R> {
    pub(super) fn new(
        state: &'a Path,
        runner: &'a mut R,
        profile: &'a mut Option<HostPlatformProfile>,
        supervisor_directories: &'a SupervisorDirectories,
    ) -> Self {
        Self {
            state,
            runner,
            profile,
            supervisor_directories,
        }
    }

    pub(super) fn profile(&mut self) -> Result<&HostPlatformProfile, FailureMessage> {
        if self.profile.is_none() {
            let release = self.runner.read_os_release()?;
            *self.profile = Some(detect_host_platform(&release).map_err(failure)?);
        }
        let Some(profile) = self.profile.as_ref() else {
            unreachable!("profile is populated immediately above")
        };
        Ok(profile)
    }

    pub(super) fn supervisor(&mut self) -> Result<SupervisorBackend, FailureMessage> {
        Ok(self.profile()?.supervisor().into())
    }

    pub(super) fn require(&mut self, program: &str, args: &[&str]) -> Result<(), FailureMessage> {
        let output = self.runner.command(program, args)?;
        if output.success {
            Ok(())
        } else {
            Err(failure(output.failure))
        }
    }

    pub(super) fn run_supervisor(
        &mut self,
        change: SupervisorChange,
        target: &SupervisorUnitTarget,
    ) -> Result<(), FailureMessage> {
        let backend = self.supervisor()?;
        for (program, args) in backend.commands(change, target) {
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            self.require(program, &refs)?;
        }
        Ok(())
    }

    pub(super) fn install_artifact(
        &mut self,
        kind: ArtifactKind,
        spec: &InstallArtifactSpec,
    ) -> Result<(), FailureMessage> {
        let target = artifact_target(kind, spec).map_err(failure)?;
        let verified = match target.source_view() {
            ArtifactSourceView::LocalPath(path) => {
                verify_artifact_file(path, &target.digest).map_err(failure)?
            }
            ArtifactSourceView::RemoteUrl(url) => {
                let downloads = self.state.join("downloads");
                acquire_remote_artifact_content_addressed(
                    url,
                    &target.digest,
                    &downloads,
                    |staged| self.runner.download(url, staged),
                )
                .map_err(failure)?
            }
        };
        install_verified_artifact(&verified, &target).map_err(failure)?;
        Ok(())
    }

    pub(super) fn ensure_docker(&mut self) -> Result<(), FailureMessage> {
        if !self.runner.docker_is_installed() {
            let install = self.profile()?.docker_install();
            match install {
                DockerInstall::GetDocker => {
                    let script = self.state.join("get-docker.sh");
                    self.runner.download("https://get.docker.com", &script)?;
                    self.require("sh", &[script.to_string_lossy().as_ref()])?;
                }
                DockerInstall::AlpinePackages => self.require("apk", &["add", "docker"])?,
                DockerInstall::ArchPackages => {
                    self.require("pacman", &["--noconfirm", "-S", "docker"])?;
                }
                DockerInstall::SusePackages => {
                    self.require("zypper", &["--non-interactive", "install", "docker"])?;
                }
                DockerInstall::AmazonPackages => {
                    self.require("dnf", &["install", "-y", "docker"])?;
                }
                DockerInstall::RhelRepositoryFile | DockerInstall::CentosRepositoryFile => {
                    self.require("dnf", &["install", "-y", "docker-ce"])?;
                }
            }
        }
        let backend = self.supervisor()?;
        for (program, args) in backend.docker_commands(SupervisorChange::InstallAndStart) {
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            self.require(program, &refs)?;
        }
        self.runner.docker_info()
    }

    pub(super) fn restart_docker_and_verify(&mut self) -> Result<(), FailureMessage> {
        let backend = self.supervisor()?;
        for (program, args) in backend.docker_commands(SupervisorChange::Restart) {
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            self.require(program, &refs)?;
        }
        self.runner.docker_info()
    }

    pub(super) fn install_ployzd_units(
        &mut self,
        ployzd: &ArtifactTarget,
        environment: &PloyzdRoleEnvironmentFile,
    ) -> Result<(), FailureMessage> {
        let backend = self.supervisor()?;
        for role in [
            PloyzdRole::Keeper,
            PloyzdRole::Api,
            PloyzdRole::Gateway,
            PloyzdRole::Dns,
        ] {
            let spec = SupervisorUnitSpec::PloyzdRole {
                role,
                artifact: ployzd.clone(),
                environment_file: environment.clone(),
            };
            let rendered = backend.render(&spec).map_err(failure)?;
            write_durable_file(
                self.supervisor_directories.directory(backend),
                rendered.file_name(),
                FileMode::Executable0755,
                rendered.contents().as_bytes(),
            )?;
            let target = spec.target();
            match role {
                PloyzdRole::Keeper | PloyzdRole::Api => {
                    self.run_supervisor(SupervisorChange::Enable, &target)?;
                }
                PloyzdRole::Gateway | PloyzdRole::Dns => {
                    self.run_supervisor(SupervisorChange::Disable, &target)?;
                    self.run_supervisor(SupervisorChange::Stop, &target)?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn install_corrosion_unit(&mut self, config: &Path) -> Result<(), FailureMessage> {
        let backend = self.supervisor()?;
        let (name, contents) = corrosion_unit(backend, config);
        write_durable_file(
            self.supervisor_directories.directory(backend),
            name,
            FileMode::Executable0755,
            contents.as_bytes(),
        )
    }

    pub(super) fn change_corrosion_service(
        &mut self,
        change: CorrosionServiceChange,
    ) -> Result<(), FailureMessage> {
        let backend = self.supervisor()?;
        for (program, args) in corrosion_commands(backend, change) {
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            self.require(program, &refs)?;
        }
        Ok(())
    }

    pub(super) fn wait_for_role(
        &mut self,
        role: PloyzdRole,
        description: &str,
    ) -> Result<(), FailureMessage> {
        let backend = self.supervisor()?;
        let target = SupervisorUnitTarget::PloyzdRole(role);
        for _ in 0..30 {
            let mut healthy = true;
            for (program, args) in backend.commands(SupervisorChange::IsActive, &target) {
                let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
                healthy &= self.runner.command(program, &refs)?.success;
            }
            if healthy {
                return Ok(());
            }
            thread::sleep(Duration::from_secs(1));
        }
        Err(failure(format!(
            "{description} did not become ready within 30 seconds"
        )))
    }
}

pub(super) fn render_corrosion_config(config: CorrosionConfig<'_>) -> String {
    let CorrosionConfig {
        state,
        schema_path,
        gossip_addr,
        bootstrap,
        bearer_token,
    } = config;
    let bootstrap = match bootstrap {
        CorrosionBootstrap::Founder => "[]".to_owned(),
        CorrosionBootstrap::Seed(address) => format!("[{address:?}]"),
    };
    format!(
        "[db]\npath = {db:?}\nschema_paths = [{schema:?}]\nsubscriptions_path = {subscriptions:?}\n\n[gossip]\naddr = {gossip:?}\nbootstrap = {bootstrap}\nplaintext = true\nmax_mtu = 1232\n\n[api]\naddr = {api:?}\nauthz.bearer-token = {token:?}\n\n[admin]\npath = {admin:?}\n",
        db = state.join("corrosion.db").display().to_string(),
        schema = schema_path,
        subscriptions = state.join("subscriptions").display().to_string(),
        gossip = format!("[{gossip_addr}]:{CORROSION_GOSSIP_PORT}"),
        api = format!("127.0.0.1:{CORROSION_API_PORT}"),
        token = bearer_token,
        admin = state.join("corrosion-admin.sock").display().to_string(),
    )
}

pub(super) fn read_or_generate_secret(
    path: &Path,
    persistence: GeneratedSecretPersistence,
) -> Result<String, FailureMessage> {
    match fs::read_to_string(path) {
        Ok(secret) if !secret.trim().is_empty() => Ok(secret.trim().to_owned()),
        Ok(_) => Err(failure(format!("secret file {} is empty", path.display()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let secret = defguard_wireguard_rs::key::Key::generate().to_string();
            if persistence == GeneratedSecretPersistence::Durable {
                let parent = path
                    .parent()
                    .ok_or_else(|| failure("secret path has no parent"))?;
                let file_name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| failure("secret path has no file name"))?;
                write_durable_file(
                    parent,
                    file_name,
                    FileMode::Secret0600,
                    format!("{secret}\n").as_bytes(),
                )?;
            }
            Ok(secret)
        }
        Err(error) => Err(failure(error)),
    }
}

pub(super) fn corrosion_unit(backend: SupervisorBackend, config: &Path) -> (&'static str, String) {
    match backend {
        SupervisorBackend::Systemd => (
            "ployz-corrosion.service",
            format!(
                "[Unit]\nDescription=Ployz Corrosion\nAfter=network-online.target ployzd-keeper.service\nWants=network-online.target\n\n[Service]\nType=exec\nExecStart=/usr/local/bin/corrosion --config {} agent\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n",
                config.display()
            ),
        ),
        SupervisorBackend::OpenRc => (
            "ployz-corrosion",
            format!(
                "#!/sbin/openrc-run\nname=ployz-corrosion\nsupervisor=supervise-daemon\ncommand=/usr/local/bin/corrosion\ncommand_args=\"--config {} agent\"\nrespawn_delay=5\n\ndepend() {{ need net; after ployzd-keeper; }}\n",
                config.display()
            ),
        ),
    }
}

fn corrosion_commands(
    backend: SupervisorBackend,
    change: CorrosionServiceChange,
) -> Vec<(&'static str, Vec<String>)> {
    match (backend, change) {
        (SupervisorBackend::Systemd, CorrosionServiceChange::Enable) => vec![
            ("systemctl", vec!["daemon-reload".to_owned()]),
            (
                "systemctl",
                vec!["enable".to_owned(), "ployz-corrosion.service".to_owned()],
            ),
        ],
        (SupervisorBackend::Systemd, CorrosionServiceChange::Restart) => vec![(
            "systemctl",
            vec!["restart".to_owned(), "ployz-corrosion.service".to_owned()],
        )],
        (SupervisorBackend::Systemd, CorrosionServiceChange::Stop) => vec![(
            "systemctl",
            vec!["stop".to_owned(), "ployz-corrosion.service".to_owned()],
        )],
        (SupervisorBackend::OpenRc, CorrosionServiceChange::Enable) => vec![(
            "rc-update",
            vec![
                "add".to_owned(),
                "ployz-corrosion".to_owned(),
                "default".to_owned(),
            ],
        )],
        (SupervisorBackend::OpenRc, CorrosionServiceChange::Restart) => vec![(
            "rc-service",
            vec!["ployz-corrosion".to_owned(), "restart".to_owned()],
        )],
        (SupervisorBackend::OpenRc, CorrosionServiceChange::Stop) => vec![(
            "rc-service",
            vec!["ployz-corrosion".to_owned(), "stop".to_owned()],
        )],
    }
}

fn failure(error: impl std::fmt::Display) -> FailureMessage {
    FailureMessage::try_new(error.to_string()).unwrap_or_else(|_| {
        FailureMessage::try_new("Linux substrate effect failed").expect("constant is non-empty")
    })
}
