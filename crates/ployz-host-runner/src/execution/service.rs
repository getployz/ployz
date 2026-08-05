//! Supervisor-neutral service contracts and systemd rendering.

use std::path::{Path, PathBuf};

use super::artifacts::ArtifactTarget;
pub use ployz_core::roles::PloyzdRole;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorUnitTarget {
    PloyzdRole(PloyzdRole),
}

impl SupervisorUnitTarget {
    #[must_use]
    pub fn unit_name(&self) -> String {
        match self {
            Self::PloyzdRole(role) => role_unit_name(role),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorUnitSpec {
    PloyzdRole {
        role: PloyzdRole,
        artifact: ArtifactTarget,
        environment_file: PloyzdRoleEnvironmentFile,
    },
}

impl SupervisorUnitSpec {
    #[must_use]
    pub fn target(&self) -> SupervisorUnitTarget {
        match self {
            Self::PloyzdRole { role, .. } => SupervisorUnitTarget::PloyzdRole(*role),
        }
    }

    #[must_use]
    pub fn unit_name(&self) -> String {
        self.target().unit_name()
    }

    pub fn render(&self) -> Result<String, SupervisorUnitFileError> {
        match self {
            Self::PloyzdRole {
                role,
                artifact,
                environment_file,
            } => Ok(PloyzdRoleUnit::new(*role, artifact, environment_file)?.render()),
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
pub struct PloyzdRoleUnit {
    role: PloyzdRole,
    exec_start: String,
    environment_file: PloyzdRoleEnvironmentFile,
}

impl PloyzdRoleUnit {
    pub fn new(
        role: PloyzdRole,
        artifact: &ArtifactTarget,
        environment_file: &PloyzdRoleEnvironmentFile,
    ) -> Result<Self, SupervisorUnitFileError> {
        let exec_start = render_exec_start(artifact.install_path(), [role.as_str().to_owned()])?;
        Ok(Self {
            role,
            exec_start,
            environment_file: environment_file.clone(),
        })
    }

    #[cfg(test)]
    #[must_use]
    pub fn target(&self) -> SupervisorUnitTarget {
        SupervisorUnitTarget::PloyzdRole(self.role)
    }

    #[cfg(test)]
    #[must_use]
    pub fn unit_name(&self) -> String {
        self.target().unit_name()
    }

    #[must_use]
    pub fn render(&self) -> String {
        // The API unit only orders after sys-fs-bpf.mount; Keeper establishes
        // host network substrate independently, so a failed mount does not
        // prevent the API process from starting and reporting the failure.
        let (after, wants) = match self.role {
            PloyzdRole::Api => (
                "network-online.target docker.service sys-fs-bpf.mount",
                "network-online.target docker.service",
            ),
            PloyzdRole::Dns => (
                "network-online.target docker.service ployz-corrosion.service ployzd-api.service",
                "network-online.target docker.service ployz-corrosion.service ployzd-api.service",
            ),
            PloyzdRole::Keeper | PloyzdRole::Gateway => {
                ("network-online.target", "network-online.target")
            }
        };
        let role_security = match self.role {
            PloyzdRole::Dns => {
                "DynamicUser=yes\nUser=ployz-dns\nAmbientCapabilities=CAP_NET_BIND_SERVICE\nCapabilityBoundingSet=CAP_NET_BIND_SERVICE\nNoNewPrivileges=yes\n"
            }
            PloyzdRole::Keeper | PloyzdRole::Api | PloyzdRole::Gateway => "",
        };
        format!(
            "[Unit]\nDescription=Ployz {}\nAfter={}\nWants={}\n\n[Service]\nType=exec\n{}EnvironmentFile={}\nExecStart={}\nTimeoutStopSec=10s\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n",
            self.role.as_str(),
            after,
            wants,
            role_security,
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
pub fn role_unit_name(role: &PloyzdRole) -> String {
    match role {
        PloyzdRole::Keeper => "ployzd-keeper.service".to_owned(),
        PloyzdRole::Api => "ployzd-api.service".to_owned(),
        PloyzdRole::Gateway => "ployzd-gateway.service".to_owned(),
        PloyzdRole::Dns => "ployzd-dns.service".to_owned(),
    }
}
