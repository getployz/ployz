use std::fmt;

use ployz_core::ids::{NodeId, OperationId};
use ployz_core::ops::OperationIdempotencyKey;
use ployz_core::state::GatewayServingStatus;
use ployz_sdk_types::{
    AcceptedOperation, MachineAddAccepted, MachineAddGateway, MachineAddRequest,
    MachineInspectRequest, MachineListRequest, MachineListResult, MachineSnapshot,
};

pub use ployz_sdk_types::MachineName;
pub use ployz_sdk_types::{
    BootstrapCommandError, MachineBootstrapUrl, MachineJoinRuntimeNatsUrl, MachineJoinToken,
};

use crate::commands::{ArgCursor, PloyzctlCliError, invalid_value, required, set_once};
use crate::shell::shell_quote;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineAddCommand {
    pub operation_id: OperationId,
    pub idempotency_key: OperationIdempotencyKey,
    pub node_id: NodeId,
    pub name: MachineName,
    pub gateway: MachineAddGateway,
}

impl MachineAddCommand {
    #[must_use]
    pub fn into_request(self) -> MachineAddRequest {
        MachineAddRequest {
            operation_id: self.operation_id,
            idempotency_key: self.idempotency_key,
            node_id: self.node_id,
            name: self.name,
            gateway: self.gateway,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct MachineAddOutput {
    pub node_id: NodeId,
    pub accepted: AcceptedOperation,
    pub bootstrap_url: MachineBootstrapUrl,
    pub bootstrap_nats_url: MachineJoinRuntimeNatsUrl,
    pub join_token: MachineJoinToken,
}

impl MachineAddOutput {
    #[must_use]
    pub fn from_accepted(accepted: MachineAddAccepted) -> Self {
        Self {
            node_id: accepted.node_id,
            accepted: accepted.accepted,
            bootstrap_url: accepted.bootstrap_url,
            bootstrap_nats_url: accepted.bootstrap_nats_url,
            join_token: accepted.join_token,
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "operation {}\nnode {}\njoin-token {}\ninstall curl -fsSL -- {} | {} -s -- --join-token {}\n",
            self.accepted.operation_id.as_str(),
            self.node_id.as_str(),
            self.join_token.as_str(),
            shell_quote(self.bootstrap_url.as_str()),
            format_args!(
                "PLOYZ_NATS_URL={} sh",
                shell_quote(self.bootstrap_nats_url.as_str())
            ),
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
            .field("bootstrap_nats_url", &self.bootstrap_nats_url)
            .field("join_token", &self.join_token)
            .finish()
    }
}

pub fn parse_machine_add_command(args: &[String]) -> Result<MachineAddCommand, PloyzctlCliError> {
    let mut node_id = None;
    let mut name = None;
    let mut gateway = MachineAddGateway::Skip;
    let mut operation_id = None;
    let mut idempotency_key = None;
    let mut args = ArgCursor::new(args);

    while !args.is_empty() {
        if args.take_flag("--gateway") {
            if gateway == MachineAddGateway::Install {
                return Err(PloyzctlCliError::DuplicateArgument { flag: "--gateway" });
            }
            gateway = MachineAddGateway::Install;
            continue;
        }
        if let Some(value) = args.take_value("--node")? {
            let parsed = NodeId::try_new(value).map_err(|error| invalid_value("--node", error))?;
            set_once(&mut node_id, parsed, "--node")?;
            continue;
        }
        if let Some(value) = args.take_value("--name")? {
            let parsed =
                MachineName::try_new(value).map_err(|error| invalid_value("--name", error))?;
            set_once(&mut name, parsed, "--name")?;
            continue;
        }
        if let Some(value) = args.take_value("--operation")? {
            let parsed =
                OperationId::try_new(value).map_err(|error| invalid_value("--operation", error))?;
            set_once(&mut operation_id, parsed, "--operation")?;
            continue;
        }
        if let Some(value) = args.take_value("--idempotency-key")? {
            let parsed = OperationIdempotencyKey::try_new(value)
                .map_err(|error| invalid_value("--idempotency-key", error))?;
            set_once(&mut idempotency_key, parsed, "--idempotency-key")?;
            continue;
        }
        return Err(args.unexpected());
    }

    Ok(MachineAddCommand {
        operation_id: required(operation_id, "--operation")?,
        idempotency_key: required(idempotency_key, "--idempotency-key")?,
        node_id: required(node_id, "--node")?,
        name: required(name, "--name")?,
        gateway,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineListCommand;

impl MachineListCommand {
    #[must_use]
    pub const fn into_request(self) -> MachineListRequest {
        MachineListRequest {}
    }
}

pub fn parse_machine_list_command(args: &[String]) -> Result<MachineListCommand, PloyzctlCliError> {
    match args {
        [] => Ok(MachineListCommand),
        [unexpected, ..] => Err(PloyzctlCliError::UnexpectedArgument {
            value: unexpected.clone(),
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineInspectCommand {
    pub node_id: NodeId,
}

impl MachineInspectCommand {
    #[must_use]
    pub fn into_request(self) -> MachineInspectRequest {
        MachineInspectRequest {
            node_id: self.node_id,
        }
    }
}

pub fn parse_machine_inspect_command(
    args: &[String],
) -> Result<MachineInspectCommand, PloyzctlCliError> {
    let node_id = match args {
        [] => {
            return Err(PloyzctlCliError::MissingRequiredArgument { flag: "<node_id>" });
        }
        [node_id] => node_id,
        [_, unexpected, ..] => {
            return Err(PloyzctlCliError::UnexpectedArgument {
                value: unexpected.clone(),
            });
        }
    };
    let node_id =
        NodeId::try_new(node_id.clone()).map_err(|error| invalid_value("<node_id>", error))?;

    Ok(MachineInspectCommand { node_id })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineListOutput {
    pub machines: Vec<MachineSnapshot>,
}

impl MachineListOutput {
    #[must_use]
    pub fn from_result(result: MachineListResult) -> Self {
        Self {
            machines: result.machines,
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        let rendered = self
            .machines
            .iter()
            .map(render_machine_summary)
            .collect::<Vec<_>>()
            .join("\n");

        if rendered.is_empty() {
            rendered
        } else {
            rendered + "\n"
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineInspectOutput {
    pub machine: MachineSnapshot,
}

impl MachineInspectOutput {
    #[must_use]
    pub fn new(machine: MachineSnapshot) -> Self {
        Self { machine }
    }

    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "node {}\nname {}\nactivated-by {}\npublic-ip {}\ngateway {}\ncontainers {}\n",
            self.machine.active.node_id.as_str(),
            self.machine.active.name.as_str(),
            self.machine.active.activated_by.as_str(),
            render_public_ip(&self.machine),
            render_gateway(&self.machine),
            self.machine.observed_container_count,
        )
    }
}

fn render_machine_summary(machine: &MachineSnapshot) -> String {
    format!(
        "{} {} public-ip {} gateway {} containers {}",
        machine.active.node_id.as_str(),
        machine.active.name.as_str(),
        render_public_ip(machine),
        render_gateway(machine),
        machine.observed_container_count,
    )
}

fn render_public_ip(machine: &MachineSnapshot) -> String {
    match &machine.public_ip {
        Some(observation) => observation.public_ip.to_string(),
        None => "unknown".to_owned(),
    }
}

fn render_gateway(machine: &MachineSnapshot) -> String {
    match &machine.gateway {
        Some(observation) => format!(
            "{} {} routes {}",
            render_gateway_serving(observation.serving),
            observation.listen_addr,
            observation.route_count
        ),
        None => "none".to_owned(),
    }
}

const fn render_gateway_serving(serving: GatewayServingStatus) -> &'static str {
    match serving {
        GatewayServingStatus::Current => "current",
        GatewayServingStatus::LastKnownGood => "last-known-good",
        GatewayServingStatus::Unavailable => "unavailable",
    }
}
