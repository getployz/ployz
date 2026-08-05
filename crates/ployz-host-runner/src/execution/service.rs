//! Supervisor-neutral service contracts and systemd rendering.

use std::path::{Path, PathBuf};

use super::artifacts::PloyzdArtifactStore;
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
        artifact_store: PloyzdArtifactStore,
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
                artifact_store,
                environment_file,
            } => Ok(PloyzdRoleUnit::new(*role, artifact_store, environment_file)?.render()),
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
    executable_access: String,
    environment_file: PloyzdRoleEnvironmentFile,
}

impl PloyzdRoleUnit {
    pub fn new(
        role: PloyzdRole,
        artifact_store: &PloyzdArtifactStore,
        environment_file: &PloyzdRoleEnvironmentFile,
    ) -> Result<Self, SupervisorUnitFileError> {
        let (exec_start, executable_access) = match role {
            PloyzdRole::Dns => {
                let source = artifact_store.current_path();
                let Some(source) = source.to_str() else {
                    return Err(SupervisorUnitFileError::UnsupportedExecToken {
                        value: source.display().to_string(),
                    });
                };
                if source.contains(':') {
                    return Err(SupervisorUnitFileError::UnsupportedBindPath {
                        value: source.to_owned(),
                    });
                }
                let bind = render_exec_token(format!("{source}:/run/ployz-dns/ployzd"))?;
                (
                    render_exec_start(
                        Path::new("/run/ployz-dns/ployzd"),
                        [role.as_str().to_owned()],
                    )?,
                    format!("RuntimeDirectory=ployz-dns\nBindReadOnlyPaths={bind}\n"),
                )
            }
            PloyzdRole::Keeper | PloyzdRole::Api | PloyzdRole::Gateway => (
                render_exec_start(&artifact_store.current_path(), [role.as_str().to_owned()])?,
                String::new(),
            ),
        };
        Ok(Self {
            role,
            exec_start,
            executable_access,
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
        let on_failure = match self.role {
            PloyzdRole::Keeper => "OnFailure=ployzd-keeper-revert.service",
            PloyzdRole::Api | PloyzdRole::Gateway | PloyzdRole::Dns => "",
        };
        let restart = match self.role {
            PloyzdRole::Keeper => "Restart=on-failure\nRestartPreventExitStatus=75\nRestartSec=5",
            PloyzdRole::Api | PloyzdRole::Gateway | PloyzdRole::Dns => {
                "Restart=always\nRestartSec=5"
            }
        };
        format!(
            "[Unit]\nDescription=Ployz {}\nAfter={}\nWants={}\n{}\n[Service]\nType=exec\n{}{}EnvironmentFile={}\nExecStart={}\nTimeoutStopSec=10s\n{}\n\n[Install]\nWantedBy=multi-user.target\n",
            self.role.as_str(),
            after,
            wants,
            on_failure,
            role_security,
            self.executable_access,
            self.environment_file.path().display(),
            self.exec_start,
            restart,
        )
    }
}

/// The systemd-only emergency rollback handler for a failed upgraded Keeper.
///
/// It deliberately uses only fixed system utilities. In particular, it never
/// executes `current` or a candidate artifact: its only job is restoring the
/// stable link before restarting the role processes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PloyzdKeeperRevertUnit {
    artifact_store: PloyzdArtifactStore,
}

impl PloyzdKeeperRevertUnit {
    #[must_use]
    pub fn new(artifact_store: PloyzdArtifactStore) -> Self {
        Self { artifact_store }
    }

    #[must_use]
    pub const fn unit_name() -> &'static str {
        "ployzd-keeper-revert.service"
    }

    pub fn render(&self) -> Result<String, SupervisorUnitFileError> {
        let previous = self.artifact_store.previous_path();
        let current = self.artifact_store.current_path();
        let pending = self.artifact_store.pending_path();
        let link = render_exec_start(
            Path::new("/usr/bin/ln"),
            [
                "-sfnT".to_owned(),
                previous.display().to_string(),
                current.display().to_string(),
            ],
        )?;
        let remove = render_exec_start(Path::new("/usr/bin/rm"), [pending.display().to_string()])?;
        let restarts = [
            PloyzdRole::Keeper,
            PloyzdRole::Api,
            PloyzdRole::Gateway,
            PloyzdRole::Dns,
        ]
        .map(|role| {
            render_exec_start(
                Path::new("/usr/bin/systemctl"),
                ["restart".to_owned(), role_unit_name(&role)],
            )
        })
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?
        .join("\nExecStart=");
        Ok(format!(
            "[Unit]\nDescription=Ployz Keeper upgrade revert\nConditionPathExists={}\n\n[Service]\nType=oneshot\nExecStart={}\nExecStart={}\nExecStart={}\n",
            pending.display(),
            link,
            remove,
            restarts,
        ))
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
    #[error("systemd bind source path {value:?} cannot contain ':'")]
    UnsupportedBindPath { value: String },
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
