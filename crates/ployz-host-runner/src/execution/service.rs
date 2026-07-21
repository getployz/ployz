//! Supervisor-neutral service contracts and systemd rendering.

use std::path::{Path, PathBuf};

use super::artifacts::ArtifactTarget;
use ployz_core::roles::DaemonProcessRole;

pub(crate) const PLOYZD_MACHINE_UNIT_PREFIX: &str = "ployzd-machine-";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorUnitTarget {
    NatsServer,
    PloyzdRole(DaemonProcessRole),
    OwnedZfsImport,
}

impl SupervisorUnitTarget {
    #[must_use]
    pub fn unit_name(&self) -> String {
        match self {
            Self::NatsServer => "nats-server.service".to_owned(),
            Self::PloyzdRole(role) => role_unit_name(role),
            Self::OwnedZfsImport => "ployz-owned-zfs-import.service".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorUnitSpec {
    NatsServer(NatsServerUnitTarget),
    PloyzdRole {
        role: DaemonProcessRole,
        artifact: ArtifactTarget,
        environment_file: PloyzdRoleEnvironmentFile,
    },
    OwnedZfsImport,
}

impl SupervisorUnitSpec {
    #[must_use]
    pub fn target(&self) -> SupervisorUnitTarget {
        match self {
            Self::NatsServer(_) => SupervisorUnitTarget::NatsServer,
            Self::PloyzdRole { role, .. } => SupervisorUnitTarget::PloyzdRole(role.clone()),
            Self::OwnedZfsImport => SupervisorUnitTarget::OwnedZfsImport,
        }
    }

    #[must_use]
    pub fn unit_name(&self) -> String {
        self.target().unit_name()
    }

    pub fn render(&self) -> Result<String, SupervisorUnitFileError> {
        match self {
            Self::NatsServer(target) => {
                Ok(NatsServerUnit::new(target.binary_path(), target.config_path())?.render())
            }
            Self::PloyzdRole {
                role,
                artifact,
                environment_file,
            } => Ok(PloyzdRoleUnit::new(role.clone(), artifact, environment_file)?.render()),
            Self::OwnedZfsImport => Ok("[Unit]\nRequires=zfs-import.target\nAfter=zfs-import.target\nBefore=docker.service\n\n[Service]\nType=oneshot\nExecStart=/usr/local/bin/ployz host internal-storage-recover\nRemainAfterExit=yes\n\n[Install]\nWantedBy=multi-user.target\n".to_owned()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PloyzdRoleEnvironmentFile {
    path: PathBuf,
}

impl PloyzdRoleEnvironmentFile {
    pub fn new(path: PathBuf) -> Result<Self, SupervisorUnitFileError> {
        let path = validate_supervisor_path(path)?;
        let value = path
            .to_str()
            .expect("validated supervisor path is UTF-8")
            .to_owned();
        if !value.bytes().all(is_plain_environment_file_token_byte) {
            return Err(SupervisorUnitFileError::UnsupportedEnvironmentFilePath { value: path });
        }
        Ok(Self { path })
    }

    #[must_use]
    pub fn default_path() -> Self {
        Self::new(PathBuf::from("/etc/ployz/ployzd.env"))
            .expect("default ployzd environment path is valid")
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsServerUnitTarget {
    binary_path: std::path::PathBuf,
    config_path: std::path::PathBuf,
}

impl NatsServerUnitTarget {
    pub fn new(
        binary_path: std::path::PathBuf,
        config_path: std::path::PathBuf,
    ) -> Result<Self, SupervisorUnitFileError> {
        Ok(Self {
            binary_path: validate_supervisor_path(binary_path)?,
            config_path: validate_supervisor_path(config_path)?,
        })
    }

    #[must_use]
    pub fn default_paths() -> Self {
        Self::new(
            std::path::PathBuf::from("/usr/local/bin/nats-server"),
            std::path::PathBuf::from("/etc/nats/nats-server.conf"),
        )
        .expect("default nats-server unit paths are valid")
    }

    #[must_use]
    pub fn binary_path(&self) -> &Path {
        &self.binary_path
    }

    #[must_use]
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsServerUnit {
    exec_start: String,
}

impl NatsServerUnit {
    pub fn new(
        binary_path: impl AsRef<Path>,
        config_path: impl AsRef<Path>,
    ) -> Result<Self, SupervisorUnitFileError> {
        let exec_start = render_exec_start(
            binary_path.as_ref(),
            ["--config".to_owned(), path_token(config_path.as_ref())?],
        )?;
        Ok(Self { exec_start })
    }

    #[cfg(test)]
    #[must_use]
    pub const fn target(&self) -> SupervisorUnitTarget {
        SupervisorUnitTarget::NatsServer
    }

    #[cfg(test)]
    #[must_use]
    pub fn unit_name(&self) -> String {
        self.target().unit_name()
    }

    /// The reload goes through `sh`'s builtin `kill`: `/bin/kill` is owned
    /// by procps, which minimal Debian installs do not ship, and a missing
    /// reload binary turns every machine-add mint into a terminal
    /// `NatsReloadFailed`. systemd substitutes `$MAINPID` before `sh` runs.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "[Unit]\nDescription=Ployz NATS Server\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=exec\nExecStart={}\nExecReload=/bin/sh -c \"kill -s HUP $MAINPID\"\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n",
            self.exec_start,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PloyzdRoleUnit {
    role: DaemonProcessRole,
    exec_start: String,
    environment_file: PloyzdRoleEnvironmentFile,
}

impl PloyzdRoleUnit {
    pub fn new(
        role: DaemonProcessRole,
        artifact: &ArtifactTarget,
        environment_file: &PloyzdRoleEnvironmentFile,
    ) -> Result<Self, SupervisorUnitFileError> {
        let exec_start = render_exec_start(artifact.install_path(), role.argv())?;
        Ok(Self {
            role,
            exec_start,
            environment_file: environment_file.clone(),
        })
    }

    #[cfg(test)]
    #[must_use]
    pub fn target(&self) -> SupervisorUnitTarget {
        SupervisorUnitTarget::PloyzdRole(self.role.clone())
    }

    #[cfg(test)]
    #[must_use]
    pub fn unit_name(&self) -> String {
        self.target().unit_name()
    }

    #[must_use]
    pub fn render(&self) -> String {
        // The machine unit only orders after sys-fs-bpf.mount; it does not
        // require it. bpffs is a dataplane substrate the machine agent
        // establishes and verifies itself at runtime (EnsureBpfFs), so a
        // failed mount must surface as typed dataplane evidence, not block the
        // agent from activating and taking its control-plane RPC down with it.
        let (after, wants) = match &self.role {
            DaemonProcessRole::Machine(_) => (
                "network-online.target docker.service sys-fs-bpf.mount",
                "network-online.target docker.service",
            ),
            DaemonProcessRole::Control | DaemonProcessRole::Gateway | DaemonProcessRole::Dns => {
                ("network-online.target", "network-online.target")
            }
        };
        format!(
            "[Unit]\nDescription=Ployz {}\nAfter={}\nWants={}\n\n[Service]\nType=exec\nEnvironmentFile={}\nExecStart={}\nTimeoutStopSec=10s\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n",
            self.role.process_name(),
            after,
            wants,
            self.environment_file.path().display(),
            self.exec_start,
        )
    }
}

fn render_exec_start(
    program_path: &Path,
    args: impl IntoIterator<Item = String>,
) -> Result<String, SupervisorUnitFileError> {
    let Some(program_path) = program_path.to_str() else {
        return Err(SupervisorUnitFileError::UnsupportedExecToken {
            value: program_path.display().to_string(),
        });
    };

    let mut tokens = vec![render_exec_token(program_path)?];
    for arg in args {
        tokens.push(render_exec_token(arg)?);
    }
    Ok(tokens.join(" "))
}

fn path_token(path: &Path) -> Result<String, SupervisorUnitFileError> {
    let Some(value) = path.to_str() else {
        return Err(SupervisorUnitFileError::UnsupportedExecToken {
            value: path.display().to_string(),
        });
    };
    Ok(value.to_owned())
}

fn render_exec_token(value: impl Into<String>) -> Result<String, SupervisorUnitFileError> {
    let value = value.into();
    if value.is_empty() {
        return Err(SupervisorUnitFileError::EmptyExecToken);
    }
    if value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(SupervisorUnitFileError::UnsupportedExecToken { value });
    }

    let escaped_percent = value.replace('%', "%%");
    if escaped_percent.bytes().all(is_plain_systemd_token_byte) {
        return Ok(escaped_percent);
    }

    let mut escaped = String::with_capacity(escaped_percent.len());
    for character in escaped_percent.chars() {
        match character {
            '$' => escaped.push_str("$$"),
            '\\' | '"' | '`' => {
                escaped.push('\\');
                escaped.push(character);
            }
            _ => escaped.push(character),
        }
    }
    Ok(format!("\"{escaped}\""))
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SupervisorUnitFileError {
    #[error("systemd exec token is empty")]
    EmptyExecToken,
    #[error("systemd exec token {value:?} is unsupported")]
    UnsupportedExecToken { value: String },
    #[error("systemd path is empty")]
    EmptyPath,
    #[error("systemd path {} must be absolute", value.display())]
    RelativePath { value: std::path::PathBuf },
    #[error("systemd path {} needs a file name", value.display())]
    MissingFileName { value: std::path::PathBuf },
    #[error("systemd environment file path {} must be an absolute plain token", value.display())]
    UnsupportedEnvironmentFilePath { value: std::path::PathBuf },
    #[error("supervisor unit {unit} is unsupported by {supervisor}")]
    UnsupportedSupervisor {
        unit: String,
        supervisor: &'static str,
    },
}

fn is_plain_systemd_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'/' | b'.' | b'_' | b'-' | b':' | b'=' | b'@' | b'+' | b'%'
        )
}

fn is_plain_environment_file_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-')
}

fn validate_supervisor_path(
    path: std::path::PathBuf,
) -> Result<std::path::PathBuf, SupervisorUnitFileError> {
    if path.as_os_str().is_empty() {
        return Err(SupervisorUnitFileError::EmptyPath);
    }
    if !path.is_absolute() {
        return Err(SupervisorUnitFileError::RelativePath { value: path });
    }
    if path.file_name().is_none() {
        return Err(SupervisorUnitFileError::MissingFileName { value: path });
    }
    if path.to_str().is_none() {
        return Err(SupervisorUnitFileError::UnsupportedExecToken {
            value: path.display().to_string(),
        });
    }
    Ok(path)
}

#[must_use]
pub fn role_unit_name(role: &DaemonProcessRole) -> String {
    match role {
        DaemonProcessRole::Control => "ployzd-control.service".to_owned(),
        DaemonProcessRole::Machine(machine_id) => {
            format!(
                "{PLOYZD_MACHINE_UNIT_PREFIX}{}.service",
                machine_id.as_str()
            )
        }
        DaemonProcessRole::Gateway => "ployzd-gateway.service".to_owned(),
        DaemonProcessRole::Dns => "ployzd-dns.service".to_owned(),
    }
}
