use clap::Args;
use ployz_core::ids::{NamespaceId, OperationId, ServiceId};
use ployz_core::machine_runtime::{ContainerRuntimeState, ManagedContainerHealthStatus};
use ployz_sdk_types::{
    ServiceContainerMembership, ServiceInspectRequest, ServiceListRequest, ServiceListResult,
    ServiceMachineTestimony, ServiceRestartRequest, ServiceSnapshot,
};

use crate::commands::{PloyzctlCliError, invalid_value};
use crate::execution_support::generate_client_service_restart_id;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceListCommand;

impl ServiceListCommand {
    #[must_use]
    pub const fn into_request(self) -> ServiceListRequest {
        ServiceListRequest {}
    }
}

pub(crate) fn service_list_command(_: EmptyCli) -> ServiceListCommand {
    ServiceListCommand
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInspectCommand {
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
}

impl ServiceInspectCommand {
    #[must_use]
    pub fn into_request(self) -> ServiceInspectRequest {
        ServiceInspectRequest {
            namespace_id: self.namespace_id,
            service_id: self.service_id,
        }
    }
}

pub(crate) fn service_inspect_command(
    parsed: ServiceInspectCli,
) -> Result<ServiceInspectCommand, PloyzctlCliError> {
    let namespace_id = parsed
        .namespace
        .map(NamespaceId::try_new)
        .transpose()
        .map_err(|error| invalid_value("--namespace", error))?
        .unwrap_or_else(|| NamespaceId::try_new("default").expect("default namespace is valid"));
    let service_id = ServiceId::try_new(parsed.service_id)
        .map_err(|error| invalid_value("<service_id>", error))?;

    Ok(ServiceInspectCommand {
        namespace_id,
        service_id,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRestartCommand {
    pub operation_id: OperationId,
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
    pub detach: bool,
}

impl ServiceRestartCommand {
    #[must_use]
    pub fn into_request(self) -> ServiceRestartRequest {
        ServiceRestartRequest {
            operation_id: self.operation_id,
            namespace_id: self.namespace_id,
            service_id: self.service_id,
        }
    }
}

pub(crate) fn service_restart_command(
    parsed: ServiceRestartCli,
) -> Result<ServiceRestartCommand, PloyzctlCliError> {
    let namespace_id = parsed
        .namespace
        .map(NamespaceId::try_new)
        .transpose()
        .map_err(|error| invalid_value("--namespace", error))?
        .unwrap_or_else(|| NamespaceId::try_new("default").expect("default namespace is valid"));
    let service_id = ServiceId::try_new(parsed.service_id)
        .map_err(|error| invalid_value("<service_id>", error))?;
    let operation_id = generate_client_service_restart_id(&service_id)
        .map_err(|error| invalid_value("<service_id>", error))?
        .operation_id;

    Ok(ServiceRestartCommand {
        operation_id,
        namespace_id,
        service_id,
        detach: parsed.detach,
    })
}

#[derive(Debug, Args)]
pub(crate) struct EmptyCli {}

#[derive(Debug, Args)]
pub(crate) struct ServiceInspectCli {
    #[arg(short = 'n', long = "namespace")]
    namespace: Option<String>,
    service_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct ServiceRestartCli {
    #[arg(short = 'n', long = "namespace")]
    namespace: Option<String>,
    #[arg(long)]
    detach: bool,
    service_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceListOutput {
    pub services: Vec<ServiceSnapshot>,
}

impl ServiceListOutput {
    #[must_use]
    pub fn from_result(result: ServiceListResult) -> Self {
        Self {
            services: result.services,
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        let rendered = self
            .services
            .iter()
            .map(render_service_summary)
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
pub struct ServiceInspectOutput {
    pub service: ServiceSnapshot,
}

impl ServiceInspectOutput {
    #[must_use]
    pub fn new(service: ServiceSnapshot) -> Self {
        Self { service }
    }

    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "service {}\nintent namespace-revision-entry {}\nintent image {}\nintent desired-replicas {}\nintent routes {}\n{}",
            self.service.active.service_id.as_str(),
            self.service.active.namespace_revision_entry_id.as_str(),
            self.service.active.image.as_str(),
            self.service.active.desired_replicas.get(),
            render_routes(&self.service),
            render_service_container_rows(&self.service),
        )
    }
}

fn render_service_summary(service: &ServiceSnapshot) -> String {
    format!(
        "{} image {} testimony ready-replicas {} intent desired-replicas {} machines {} routes {}",
        service.active.service_id.as_str(),
        service.active.image.as_str(),
        service.testimony.ready_container_count,
        service.active.desired_replicas.get(),
        render_machine_counts(service, "", ", "),
        render_routes(service),
    )
}

fn render_routes(service: &ServiceSnapshot) -> String {
    if service.route_bindings.is_empty() {
        return "none".to_owned();
    }
    service
        .route_bindings
        .iter()
        .map(|route| route.target.hostname.as_str().to_owned())
        .collect::<Vec<_>>()
        .join(",")
}

fn render_machine_counts(service: &ServiceSnapshot, prefix: &str, separator: &str) -> String {
    if service.testimony.machines.is_empty() {
        return "none".to_owned();
    }
    service
        .testimony
        .machines
        .iter()
        .map(|machine| match machine {
            ServiceMachineTestimony::Answered {
                machine_id,
                containers,
            } => format!(
                "{}{}: {} containers",
                prefix,
                machine_id.as_str(),
                containers.len()
            ),
            ServiceMachineTestimony::NoAnswer { machine_id } => {
                format!("{}{}: no answer", prefix, machine_id.as_str())
            }
        })
        .collect::<Vec<_>>()
        .join(separator)
}

fn render_service_container_rows(service: &ServiceSnapshot) -> String {
    let rows = service
        .testimony
        .machines
        .iter()
        .flat_map(|machine| match machine {
            ServiceMachineTestimony::Answered {
                machine_id,
                containers,
            } => containers
                .iter()
                .map(|container| {
                    let observation = &container.observation;
                    format!(
                        "container {} machine {} docker-state {} health {} resolved-image {} created {} operation {} {}",
                        observation.container_id.as_str(),
                        machine_id.as_str(),
                        render_container_state(&observation.state),
                        observation
                            .health_status
                            .map(render_health_status)
                            .unwrap_or("absent"),
                        observation.resolved_image_identity.as_deref().unwrap_or("absent"),
                        observation
                            .created_at_unix_seconds
                            .map(|created| created.to_string())
                            .unwrap_or_else(|| "absent".to_owned()),
                        observation.identity.operation_id.as_str(),
                        render_container_membership(container.membership),
                    )
                })
                .collect::<Vec<_>>(),
            ServiceMachineTestimony::NoAnswer { machine_id } => {
                vec![format!("machine {}: no answer", machine_id.as_str())]
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    if rows.is_empty() {
        "containers none\n".to_owned()
    } else {
        format!("{rows}\n")
    }
}

fn render_container_state(state: &ContainerRuntimeState) -> &'static str {
    match state {
        ContainerRuntimeState::Running { .. } => "running",
        ContainerRuntimeState::Exited => "exited",
    }
}

fn render_health_status(health_status: ManagedContainerHealthStatus) -> &'static str {
    match health_status {
        ManagedContainerHealthStatus::Starting => "starting",
        ManagedContainerHealthStatus::Healthy => "healthy",
        ManagedContainerHealthStatus::Unhealthy => "unhealthy",
    }
}

fn render_container_membership(membership: ServiceContainerMembership) -> &'static str {
    match membership {
        ServiceContainerMembership::ServingTargetMember => "serving-target-member",
        ServiceContainerMembership::RetainedEvidence => "retained-evidence",
    }
}
