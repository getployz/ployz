use std::fmt;

use clap::Parser;
use ployz_core::ids::{NodeId, OperationId};
use ployz_core::install::MachineJoinBundle;
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

use crate::commands::{PloyzctlCliError, clap_error, invalid_value};
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
    pub join_bundle: MachineJoinBundle,
    pub join_token: MachineJoinToken,
}

impl MachineAddOutput {
    #[must_use]
    pub fn from_accepted(accepted: MachineAddAccepted) -> Self {
        Self {
            node_id: accepted.node_id,
            accepted: accepted.accepted,
            bootstrap_url: accepted.bootstrap_url,
            join_bundle: accepted.join_bundle,
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
                shell_quote(self.runtime_nats_url().as_str())
            ),
            shell_quote(self.join_token.as_str())
        )
    }

    #[must_use]
    fn runtime_nats_url(&self) -> &MachineJoinRuntimeNatsUrl {
        &self.join_bundle.material.runtime_nats_url
    }
}

impl fmt::Debug for MachineAddOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MachineAddOutput")
            .field("node_id", &self.node_id)
            .field("accepted", &self.accepted)
            .field("bootstrap_url", &self.bootstrap_url)
            .field("join_bundle", &self.join_bundle)
            .field("join_token", &self.join_token)
            .finish()
    }
}

pub fn parse_machine_add_command(args: &[String]) -> Result<MachineAddCommand, PloyzctlCliError> {
    let parsed = MachineAddCli::try_parse_from(
        std::iter::once("machine add".to_owned()).chain(args.iter().cloned()),
    )
    .map_err(clap_error)?;

    Ok(MachineAddCommand {
        operation_id: OperationId::try_new(parsed.operation)
            .map_err(|error| invalid_value("--operation", error))?,
        idempotency_key: OperationIdempotencyKey::try_new(parsed.idempotency_key)
            .map_err(|error| invalid_value("--idempotency-key", error))?,
        node_id: NodeId::try_new(parsed.node).map_err(|error| invalid_value("--node", error))?,
        name: MachineName::try_new(parsed.name).map_err(|error| invalid_value("--name", error))?,
        gateway: if parsed.gateway {
            MachineAddGateway::Install
        } else {
            MachineAddGateway::Skip
        },
    })
}

#[derive(Debug, Parser)]
#[command(name = "machine add")]
struct MachineAddCli {
    #[arg(long)]
    node: String,
    #[arg(long)]
    name: String,
    #[arg(long)]
    operation: String,
    #[arg(long)]
    idempotency_key: String,
    #[arg(long)]
    gateway: bool,
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
    EmptyCli::try_parse_from(
        std::iter::once("machine list".to_owned()).chain(args.iter().cloned()),
    )
    .map_err(clap_error)?;
    Ok(MachineListCommand)
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
    let parsed = MachineInspectCli::try_parse_from(
        std::iter::once("machine inspect".to_owned()).chain(args.iter().cloned()),
    )
    .map_err(clap_error)?;
    let node_id =
        NodeId::try_new(parsed.node_id).map_err(|error| invalid_value("<node_id>", error))?;

    Ok(MachineInspectCommand { node_id })
}

#[derive(Debug, Parser)]
#[command(name = "machine inspect")]
struct MachineInspectCli {
    node_id: String,
}

#[derive(Debug, Parser)]
struct EmptyCli {}

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
