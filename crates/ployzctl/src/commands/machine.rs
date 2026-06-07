use std::fmt;

use ployz_core::ids::NodeId;
use ployz_sdk_types::AcceptedOperation;

pub use ployz_sdk_types::{BootstrapCommandError, MachineBootstrapUrl, MachineJoinToken};

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
            shell_quote(self.bootstrap_url.as_str()),
            shell_quote(self.join_token.as_str())
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

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
