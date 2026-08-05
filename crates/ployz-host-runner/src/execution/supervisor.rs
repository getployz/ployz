//! Native supervisor translations for Host Runner service contracts.

use std::path::{Path, PathBuf};

use super::InstalledRolePrivilege;
use super::host_platform::SupervisorKind;
use super::service::{
    PloyzdRole, SupervisorUnitFileError, SupervisorUnitSpec, SupervisorUnitTarget,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorBackend {
    Systemd,
    OpenRc,
}

/// Whether this supervisor can recover a failed upgraded Keeper without
/// executing the candidate again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PloyzdUpgradeRollback {
    SystemdOnFailure,
    Unsupported,
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
    Enable,
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
    pub const fn ployzd_upgrade_rollback(self) -> PloyzdUpgradeRollback {
        match self {
            Self::Systemd => PloyzdUpgradeRollback::SystemdOnFailure,
            Self::OpenRc => PloyzdUpgradeRollback::Unsupported,
        }
    }

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
            (Self::Systemd, SupervisorChange::Enable) => vec![
                command("systemctl", ["daemon-reload"]),
                command("systemctl", ["enable", systemd_name.as_str()]),
            ],
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
            (Self::OpenRc, SupervisorChange::Enable) => vec![command(
                "rc-update",
                ["add", openrc_name.as_str(), "default"],
            )],
            (Self::OpenRc, SupervisorChange::Restart) => {
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
            (Self::Systemd, SupervisorChange::Enable) => {
                vec![command("systemctl", ["enable", "docker"])]
            }
            (Self::Systemd, SupervisorChange::Restart) => {
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
            (Self::OpenRc, SupervisorChange::Enable) => {
                vec![command("rc-update", ["add", "docker", "default"])]
            }
            (Self::OpenRc, SupervisorChange::Restart) => {
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
}

fn command<const N: usize>(program: &'static str, args: [&str; N]) -> (&'static str, Vec<String>) {
    (program, args.into_iter().map(str::to_owned).collect())
}

fn render_openrc(
    spec: &SupervisorUnitSpec,
) -> Result<RenderedSupervisorUnit, SupervisorUnitFileError> {
    let target = spec.target();
    let file_name = openrc_service_name(&target);
    let (description, command, args, environment_file, dependencies, command_user) = match spec {
        SupervisorUnitSpec::PloyzdRole {
            role,
            artifact_store,
            environment_file,
        } => (
            format!("Ployz {}", role.as_str()),
            artifact_store.current_path(),
            (*role).argv(),
            Some(environment_file.path()),
            match role {
                PloyzdRole::Api => "need net docker",
                PloyzdRole::Keeper | PloyzdRole::Gateway | PloyzdRole::Dns => "need net",
            },
            InstalledRolePrivilege::for_role(*role)
                .map(|privilege| format!("{}:{}", privilege.user(), privilege.primary_group())),
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
    if let Some(command_user) = command_user {
        contents.push_str(&format!(
            "\ncommand_user={}\n",
            shell_double_quote(&command_user)?
        ));
    }
    if let Some(environment_file) = environment_file {
        let environment_file = shell_double_quote(&environment_file.display().to_string())?;
        contents.push_str(&format!(
            "\nstart_pre() {{\n    set -a\n    . {}\n    set +a\n}}\n",
            environment_file
        ));
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
