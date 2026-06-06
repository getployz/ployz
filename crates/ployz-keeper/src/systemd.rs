//! Supervisor units managed by keeper.

use std::path::Path;

use crate::artifacts::PloyzdArtifactTarget;
use ployz_core::roles::{DaemonProcessRole, TunnelSide};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorUnitTarget {
    Keeper,
    PloyzdRole(DaemonProcessRole),
}

impl SupervisorUnitTarget {
    #[must_use]
    pub fn unit_name(&self) -> String {
        match self {
            Self::Keeper => "ployz-keeper.service".to_owned(),
            Self::PloyzdRole(role) => role_unit_name(role),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PloyzdRoleUnit {
    role: DaemonProcessRole,
    exec_start: String,
}

impl PloyzdRoleUnit {
    pub fn new(
        role: DaemonProcessRole,
        artifact: &PloyzdArtifactTarget,
    ) -> Result<Self, SupervisorUnitFileError> {
        let exec_start = render_exec_start(artifact.install_path(), role.command_args())?;
        Ok(Self { role, exec_start })
    }

    #[must_use]
    pub fn target(&self) -> SupervisorUnitTarget {
        SupervisorUnitTarget::PloyzdRole(self.role.clone())
    }

    #[must_use]
    pub fn unit_name(&self) -> String {
        self.target().unit_name()
    }

    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "[Unit]\nDescription=Ployz {}\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nExecStart={}\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n",
            self.role.process_name(),
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
        if matches!(character, '\\' | '"' | '$' | '`') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    Ok(format!("\"{escaped}\""))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorUnitFileError {
    EmptyExecToken,
    UnsupportedExecToken { value: String },
}

fn is_plain_systemd_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'/' | b'.' | b'_' | b'-' | b':' | b'=' | b'@' | b'+' | b'%'
        )
}

#[must_use]
pub fn role_unit_name(role: &DaemonProcessRole) -> String {
    match role {
        DaemonProcessRole::Control => "ployzd-control.service".to_owned(),
        DaemonProcessRole::Node(node_id) => format!("ployzd-node-{}.service", node_id.as_str()),
        DaemonProcessRole::Gateway => "ployzd-gateway.service".to_owned(),
        DaemonProcessRole::Dns => "ployzd-dns.service".to_owned(),
        DaemonProcessRole::Tunnel(TunnelSide::Edge) => "ployzd-tunnel-edge.service".to_owned(),
        DaemonProcessRole::Tunnel(TunnelSide::Core) => "ployzd-tunnel-core.service".to_owned(),
    }
}
