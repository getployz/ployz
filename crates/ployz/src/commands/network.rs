use clap::Args;
use ployz_core::ids::OperationId;
use ployz_sdk_types::{
    MachineListRequest, MachineListResult, MachineSnapshot, MachineTestimony, NetworkRepairRequest,
    NetworkResolveMachineTestimony, NetworkResolveRequest, NetworkResolveResult,
};

use crate::client_ids::generate_client_network_repair_id;
use crate::commands::{PloyzctlCliError, invalid_value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkStatusCommand;

impl NetworkStatusCommand {
    #[must_use]
    pub const fn into_request(self) -> MachineListRequest {
        MachineListRequest {}
    }
}

pub(crate) const fn network_status_command(_: EmptyCli) -> NetworkStatusCommand {
    NetworkStatusCommand
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkResolveCommand {
    pub name: String,
}

impl NetworkResolveCommand {
    #[must_use]
    pub fn into_request(self) -> NetworkResolveRequest {
        NetworkResolveRequest { name: self.name }
    }
}

pub(crate) fn network_resolve_command(parsed: NetworkResolveCli) -> NetworkResolveCommand {
    NetworkResolveCommand { name: parsed.name }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkRepairCommand {
    pub operation_id: OperationId,
    pub detach: bool,
}

impl NetworkRepairCommand {
    #[must_use]
    pub fn into_request(self) -> NetworkRepairRequest {
        NetworkRepairRequest {
            operation_id: self.operation_id,
        }
    }
}

pub(crate) fn network_repair_command(
    parsed: NetworkRepairCli,
) -> Result<NetworkRepairCommand, PloyzctlCliError> {
    let operation_id = generate_client_network_repair_id()
        .map_err(|error| invalid_value("network repair", error))?
        .operation_id;
    Ok(NetworkRepairCommand {
        operation_id,
        detach: parsed.detach,
    })
}

#[derive(Debug, Args)]
pub(crate) struct EmptyCli {}

#[derive(Debug, Args)]
pub(crate) struct NetworkResolveCli {
    name: String,
}

#[derive(Debug, Args)]
pub(crate) struct NetworkRepairCli {
    #[arg(long)]
    detach: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkStatusOutput {
    pub machines: Vec<MachineSnapshot>,
}

impl NetworkStatusOutput {
    #[must_use]
    pub fn from_result(result: MachineListResult) -> Self {
        Self {
            machines: result.machines,
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        render_rows(self.machines.iter().map(render_status_row))
    }
}

fn render_status_row(machine: &MachineSnapshot) -> String {
    let (live_mesh, live_control) = match &machine.testimony {
        MachineTestimony::Answered { endpoints, .. } => match endpoints {
            Some(endpoints) => (
                render_addresses(&endpoints.mesh_endpoints),
                render_addresses(&endpoints.control_endpoints),
            ),
            None => ("unknown".to_owned(), "unknown".to_owned()),
        },
        MachineTestimony::NoAnswer => ("no answer".to_owned(), "no answer".to_owned()),
    };
    format!(
        "{} {} allocation {} intended-mesh-endpoints {} live-mesh-endpoints {} live-control-endpoints {}",
        machine.active.machine_id.as_str(),
        machine.active.name.as_str(),
        machine.active.endpoint_subnet.as_string(),
        render_addresses(&machine.active.mesh_endpoints),
        live_mesh,
        live_control,
    )
}

fn render_addresses<T: ToString>(addresses: &[T]) -> String {
    let rendered = addresses
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    if rendered.is_empty() {
        "none".to_owned()
    } else {
        rendered
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkResolveOutput {
    pub result: NetworkResolveResult,
}

impl NetworkResolveOutput {
    #[must_use]
    pub const fn new(result: NetworkResolveResult) -> Self {
        Self { result }
    }

    #[must_use]
    pub fn render(&self) -> String {
        let addresses = render_addresses(&self.result.addresses);
        let coverage = self.result.machines.iter().map(|machine| match machine {
            NetworkResolveMachineTestimony::Answered { machine_id } => {
                format!("machine {} answered", machine_id.as_str())
            }
            NetworkResolveMachineTestimony::NoAnswer { machine_id } => {
                format!("machine {} no answer", machine_id.as_str())
            }
        });
        let mut rows = vec![format!("{} A {}", self.result.name, addresses)];
        rows.extend(coverage);
        render_rows(rows)
    }
}

fn render_rows(rows: impl IntoIterator<Item = String>) -> String {
    let rendered = rows.into_iter().collect::<Vec<_>>().join("\n");
    if rendered.is_empty() {
        rendered
    } else {
        rendered + "\n"
    }
}
