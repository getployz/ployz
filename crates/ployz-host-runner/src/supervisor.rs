//! Native supervisor translations for Host Runner service contracts.

use std::path::{Path, PathBuf};

use crate::command::HostRunnerCommandRunner;
use crate::host_platform::SupervisorKind;
use crate::systemd::{SupervisorUnitFileError, SupervisorUnitSpec, SupervisorUnitTarget};
use ployz_core::ops::FailureMessage;
use ployz_core::roles::DaemonProcessRole;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorBackend {
    Systemd,
    OpenRc,
}

impl From<SupervisorKind> for SupervisorBackend {
    fn from(value: SupervisorKind) -> Self {
        match value {
            SupervisorKind::Systemd => Self::Systemd,
            SupervisorKind::OpenRc => Self::OpenRc,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorDirectories {
    systemd: PathBuf,
    openrc: PathBuf,
}

impl SupervisorDirectories {
    #[must_use]
    pub fn new(systemd: PathBuf, openrc: PathBuf) -> Self {
        Self { systemd, openrc }
    }

    #[must_use]
    pub fn host_defaults() -> Self {
        Self::new("/etc/systemd/system".into(), "/etc/init.d".into())
    }

    #[must_use]
    pub fn directory(&self, backend: SupervisorBackend) -> &Path {
        match backend {
            SupervisorBackend::Systemd => &self.systemd,
            SupervisorBackend::OpenRc => &self.openrc,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorChange {
    InstallAndStart,
    ReloadAndRestart,
    Restart,
    Stop,
    Disable,
    Kill,
    IsActive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedSupervisorUnit {
    file_name: String,
    contents: String,
}

impl RenderedSupervisorUnit {
    #[must_use]
    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    #[must_use]
    pub fn contents(&self) -> &str {
        &self.contents
    }
}

impl SupervisorBackend {
    #[must_use]
    pub fn service_name(self, target: &SupervisorUnitTarget) -> String {
        match self {
            Self::Systemd => target.unit_name(),
            Self::OpenRc => openrc_service_name(target),
        }
    }

    pub fn render(
        self,
        spec: &SupervisorUnitSpec,
    ) -> Result<RenderedSupervisorUnit, SupervisorUnitFileError> {
        match self {
            Self::Systemd => Ok(RenderedSupervisorUnit {
                file_name: spec.unit_name(),
                contents: spec.render()?,
            }),
            Self::OpenRc => render_openrc(spec),
        }
    }

    #[must_use]
    pub fn commands(
        self,
        change: SupervisorChange,
        target: &SupervisorUnitTarget,
    ) -> Vec<(&'static str, Vec<String>)> {
        let systemd_name = target.unit_name();
        let openrc_name = openrc_service_name(target);
        match (self, change) {
            (Self::Systemd, SupervisorChange::InstallAndStart) => vec![
                command("systemctl", ["daemon-reload"]),
                command("systemctl", ["enable", systemd_name.as_str()]),
                command("systemctl", ["restart", systemd_name.as_str()]),
            ],
            (Self::Systemd, SupervisorChange::ReloadAndRestart) => {
                vec![
                    command("systemctl", ["daemon-reload"]),
                    command("systemctl", ["restart", systemd_name.as_str()]),
                ]
            }
            (Self::Systemd, SupervisorChange::Restart) => {
                vec![command("systemctl", ["restart", systemd_name.as_str()])]
            }
            (Self::Systemd, SupervisorChange::Stop) => {
                vec![command("systemctl", ["stop", systemd_name.as_str()])]
            }
            (Self::Systemd, SupervisorChange::Disable) => {
                vec![command("systemctl", ["disable", systemd_name.as_str()])]
            }
            (Self::Systemd, SupervisorChange::Kill) => vec![command(
                "systemctl",
                ["kill", "--signal=SIGKILL", systemd_name.as_str()],
            )],
            (Self::Systemd, SupervisorChange::IsActive) => vec![command(
                "systemctl",
                ["is-active", "--quiet", systemd_name.as_str()],
            )],
            (Self::OpenRc, SupervisorChange::InstallAndStart) => vec![
                command("rc-update", ["add", openrc_name.as_str(), "default"]),
                command("rc-service", [openrc_name.as_str(), "restart"]),
            ],
            (Self::OpenRc, SupervisorChange::ReloadAndRestart | SupervisorChange::Restart) => {
                vec![command("rc-service", [openrc_name.as_str(), "restart"])]
            }
            (Self::OpenRc, SupervisorChange::Stop) => {
                vec![command("rc-service", [openrc_name.as_str(), "stop"])]
            }
            (Self::OpenRc, SupervisorChange::Disable) => {
                vec![command(
                    "rc-update",
                    ["del", openrc_name.as_str(), "default"],
                )]
            }
            (Self::OpenRc, SupervisorChange::Kill) => {
                let service_environment = format!("RC_SVCNAME={openrc_name}");
                vec![command(
                    "env",
                    [
                        service_environment.as_str(),
                        "supervise-daemon",
                        openrc_name.as_str(),
                        "--signal",
                        "KILL",
                    ],
                )]
            }
            (Self::OpenRc, SupervisorChange::IsActive) => {
                vec![command("rc-service", [openrc_name.as_str(), "status"])]
            }
        }
    }

    #[must_use]
    pub fn commands_for_targets(
        self,
        change: SupervisorChange,
        targets: &[SupervisorUnitTarget],
    ) -> Vec<(&'static str, Vec<String>)> {
        if self == Self::Systemd && change == SupervisorChange::Disable {
            let mut args = vec!["disable".to_owned()];
            args.extend(targets.iter().map(SupervisorUnitTarget::unit_name));
            return vec![("systemctl", args)];
        }
        targets
            .iter()
            .flat_map(|target| self.commands(change, target))
            .collect()
    }

    #[must_use]
    pub fn docker_commands(self, change: SupervisorChange) -> Vec<(&'static str, Vec<String>)> {
        match (self, change) {
            (Self::Systemd, SupervisorChange::InstallAndStart) => {
                vec![command("systemctl", ["enable", "--now", "docker"])]
            }
            (Self::Systemd, SupervisorChange::ReloadAndRestart | SupervisorChange::Restart) => {
                vec![command("systemctl", ["restart", "docker"])]
            }
            (Self::Systemd, SupervisorChange::Stop) => {
                vec![command("systemctl", ["stop", "docker"])]
            }
            (Self::Systemd, SupervisorChange::Disable) => {
                vec![command("systemctl", ["disable", "docker"])]
            }
            (Self::Systemd, SupervisorChange::Kill) => {
                vec![command("systemctl", ["kill", "--signal=SIGKILL", "docker"])]
            }
            (Self::Systemd, SupervisorChange::IsActive) => {
                vec![command("systemctl", ["is-active", "--quiet", "docker"])]
            }
            (Self::OpenRc, SupervisorChange::InstallAndStart) => vec![
                command("rc-update", ["add", "docker", "default"]),
                command("rc-service", ["docker", "start"]),
            ],
            (Self::OpenRc, SupervisorChange::ReloadAndRestart | SupervisorChange::Restart) => {
                vec![command("rc-service", ["docker", "restart"])]
            }
            (Self::OpenRc, SupervisorChange::Stop) => {
                vec![command("rc-service", ["docker", "stop"])]
            }
            (Self::OpenRc, SupervisorChange::Disable) => {
                vec![command("rc-update", ["del", "docker", "default"])]
            }
            (Self::OpenRc, SupervisorChange::Kill) => {
                vec![command("pkill", ["-KILL", "dockerd"])]
            }
            (Self::OpenRc, SupervisorChange::IsActive) => {
                vec![command("rc-service", ["docker", "status"])]
            }
        }
    }

    pub(crate) fn restart_installed_commands(
        self,
        services: &[String],
    ) -> Vec<(&'static str, Vec<String>)> {
        let mut commands = Vec::new();
        if self == Self::Systemd {
            commands.push(command("systemctl", ["daemon-reload"]));
        }
        commands.extend(services.iter().map(|service| match self {
            Self::Systemd => command("systemctl", ["restart", service.as_str()]),
            Self::OpenRc => command("rc-service", [service.as_str(), "restart"]),
        }));
        commands
    }
}

pub(crate) fn execute_supervisor_commands(
    runner: &mut impl HostRunnerCommandRunner,
    commands: Vec<(&'static str, Vec<String>)>,
) -> Result<(), FailureMessage> {
    for (program, args) in commands {
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        let output = runner.command(program, &args)?;
        if !output.success {
            return Err(failure_message(if output.failure.is_empty() {
                format!("{} {} failed", program, args.join(" "))
            } else {
                output.failure
            }));
        }
    }
    Ok(())
}

fn failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message).expect("supervisor failure message is non-empty")
}

fn command<const N: usize>(program: &'static str, args: [&str; N]) -> (&'static str, Vec<String>) {
    (program, args.into_iter().map(str::to_owned).collect())
}

fn render_openrc(
    spec: &SupervisorUnitSpec,
) -> Result<RenderedSupervisorUnit, SupervisorUnitFileError> {
    let target = spec.target();
    let file_name = openrc_service_name(&target);
    let (description, command, args, environment_file, dependencies, reload) = match spec {
        SupervisorUnitSpec::NatsServer(target) => (
            "Ployz NATS Server".to_owned(),
            target.binary_path(),
            vec![
                "--config".to_owned(),
                target.config_path().display().to_string(),
            ],
            None,
            "need net",
            true,
        ),
        SupervisorUnitSpec::PloyzdRole {
            role,
            artifact,
            environment_file,
        } => (
            format!("Ployz {}", role.process_name()),
            artifact.install_path(),
            role.argv(),
            Some(environment_file.path()),
            match role {
                DaemonProcessRole::Machine(_) => "need net docker",
                DaemonProcessRole::Control
                | DaemonProcessRole::Gateway
                | DaemonProcessRole::Dns => "need net",
            },
            false,
        ),
    };
    let command = shell_double_quote(&command.display().to_string())?;
    let command_args = shell_double_quote(&args.join(" "))?;
    let mut contents = format!(
        "#!/sbin/openrc-run\nname={}\ndescription={}\nsupervisor=\"supervise-daemon\"\ncommand={}\ncommand_args={}\nrespawn_delay=5\nrespawn_max=0\n\ndepend() {{\n    {dependencies}\n}}\n",
        shell_double_quote(&file_name)?,
        shell_double_quote(&description)?,
        command,
        command_args,
    );
    if let Some(environment_file) = environment_file {
        let environment_file = shell_double_quote(&environment_file.display().to_string())?;
        contents.push_str(&format!(
            "\nstart_pre() {{\n    set -a\n    . {}\n    set +a\n}}\n",
            environment_file
        ));
    }
    if reload {
        contents.push_str(
            "\nreload() {\n    ebegin \"Reloading ${RC_SVCNAME}\"\n    supervise-daemon \"${RC_SVCNAME}\" --signal HUP\n    eend $?\n}\n",
        );
    }
    Ok(RenderedSupervisorUnit {
        file_name,
        contents,
    })
}

fn openrc_service_name(target: &SupervisorUnitTarget) -> String {
    target
        .unit_name()
        .strip_suffix(".service")
        .expect("Host Runner systemd unit names have a .service suffix")
        .to_owned()
}

fn shell_double_quote(value: &str) -> Result<String, SupervisorUnitFileError> {
    if value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(SupervisorUnitFileError::UnsupportedExecToken {
            value: value.to_owned(),
        });
    }
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`");
    Ok(format!("\"{escaped}\""))
}
