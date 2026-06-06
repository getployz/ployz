use std::fmt;

use ployz_core::ids::NodeId;
use ployz_sdk_types::AcceptedOperation;

#[derive(Clone, PartialEq, Eq)]
pub struct MachineAddOutput {
    pub node_id: NodeId,
    pub accepted: AcceptedOperation,
    pub bootstrap_url: MachineBootstrapUrl,
    pub join_token: MachineJoinToken,
}

impl MachineAddOutput {
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "operation {}\nnode {}\ninstall curl -fsSL -- {} | sh -s -- --join-token {}\n",
            self.accepted.operation_id.as_str(),
            self.node_id.as_str(),
            self.bootstrap_url.shell_arg(),
            self.join_token.shell_arg()
        )
    }
}

impl fmt::Debug for MachineAddOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MachineAddOutput")
            .field("node_id", &self.node_id)
            .field("accepted", &self.accepted)
            .field("bootstrap_url", &self.bootstrap_url)
            .field("join_token", &self.join_token)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineBootstrapUrl(String);

impl MachineBootstrapUrl {
    pub fn try_new(value: impl Into<String>) -> Result<Self, BootstrapCommandError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(BootstrapCommandError::EmptyBootstrapUrl);
        }
        if !value.starts_with("https://")
            || value
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(BootstrapCommandError::InvalidBootstrapUrl);
        }

        Ok(Self(value))
    }

    #[must_use]
    pub fn shell_arg(&self) -> String {
        shell_quote(&self.0)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct MachineJoinToken(String);

impl MachineJoinToken {
    pub fn try_new(value: impl Into<String>) -> Result<Self, BootstrapCommandError> {
        let value = value.into();
        if value.is_empty() {
            return Err(BootstrapCommandError::EmptyJoinToken);
        }
        if value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(BootstrapCommandError::InvalidJoinToken);
        }

        Ok(Self(value))
    }

    #[must_use]
    pub fn shell_arg(&self) -> String {
        shell_quote(&self.0)
    }
}

impl fmt::Debug for MachineJoinToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("MachineJoinToken")
            .field(&"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapCommandError {
    EmptyBootstrapUrl,
    InvalidBootstrapUrl,
    EmptyJoinToken,
    InvalidJoinToken,
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
