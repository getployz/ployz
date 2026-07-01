//! NATS service handlers exposed by the daemon.

use ployz_core::ids::MachineId;
use ployz_core::subjects::{
    MachineServiceEndpoint, OperationApiEndpoint, OperationApiEndpointExecution, machine_service,
};
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceDiscoveryQuery,
    ServiceMetadata, ServiceMetadataEntry, ServicePing, ServiceVersion, discover_services,
};

pub const API_SERVICE_NAME: &str = "plz-api";
pub const API_SERVICE_ID: &str = "plz-api.core";
pub const API_SERVICE_DESCRIPTION: &str = "Ployz user-facing command service";
pub const MACHINE_SERVICE_NAME: &str = "plz-machine";
pub const MACHINE_SERVICE_DESCRIPTION: &str = "Ployz machine-local runtime service";
pub const SERVICE_VERSION: ServiceVersion = ServiceVersion::new(0, 1, 0);
pub const IMPLEMENTED_OPERATION_API_ENDPOINTS: [OperationApiEndpoint; 14] = [
    OperationApiEndpoint::DeploySubmit,
    OperationApiEndpoint::InitFirstMachineActivate,
    OperationApiEndpoint::MachineAdd,
    OperationApiEndpoint::MachineList,
    OperationApiEndpoint::MachineInspect,
    OperationApiEndpoint::MachineJoinRedeem,
    OperationApiEndpoint::MachineJoinReport,
    OperationApiEndpoint::ServiceList,
    OperationApiEndpoint::ServiceInspect,
    OperationApiEndpoint::RuntimeSnapshot,
    OperationApiEndpoint::LogsTail,
    OperationApiEndpoint::OpsStatus,
    OperationApiEndpoint::OpsWatch,
    OperationApiEndpoint::BackupCreate,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonServiceCatalog {
    services: Vec<NatsServiceSpec>,
}

impl DaemonServiceCatalog {
    #[must_use]
    pub fn for_control() -> Self {
        Self {
            services: vec![api_service()],
        }
    }

    #[must_use]
    pub fn for_machine(machine_id: &MachineId) -> Self {
        Self {
            services: vec![machine_runtime_service(machine_id)],
        }
    }

    #[must_use]
    pub fn services(&self) -> &[NatsServiceSpec] {
        &self.services
    }

    #[must_use]
    pub fn discover(&self, query: ServiceDiscoveryQuery<'_>) -> Vec<ServicePing> {
        discover_services(&self.services, query)
    }

    #[must_use]
    pub fn has_endpoint_subject(&self, subject: &str) -> bool {
        self.services
            .iter()
            .any(|service| service.has_endpoint_subject(subject))
    }
}

#[must_use]
pub fn api_service() -> NatsServiceSpec {
    NatsServiceSpec::new(
        API_SERVICE_ID,
        API_SERVICE_NAME,
        SERVICE_VERSION,
        API_SERVICE_DESCRIPTION,
        ServiceMetadata::empty(),
        api_endpoints(),
    )
}

#[must_use]
pub fn api_endpoints() -> Vec<NatsServiceEndpointSpec> {
    IMPLEMENTED_OPERATION_API_ENDPOINTS
        .iter()
        .copied()
        .map(api_endpoint_spec)
        .collect()
}

#[must_use]
pub fn api_endpoint_spec(endpoint: OperationApiEndpoint) -> NatsServiceEndpointSpec {
    NatsServiceEndpointSpec::new(
        endpoint.name(),
        endpoint.subject(),
        api_endpoint_execution(endpoint.execution()),
    )
}

#[must_use]
pub const fn api_endpoint_execution(execution: OperationApiEndpointExecution) -> EndpointExecution {
    match execution {
        OperationApiEndpointExecution::AcceptsOperation => EndpointExecution::AcceptsOperation,
        OperationApiEndpointExecution::MutatesOperation => EndpointExecution::MutatesOperation,
        OperationApiEndpointExecution::Query => EndpointExecution::Query,
    }
}

#[must_use]
pub fn machine_runtime_service(machine_id: &MachineId) -> NatsServiceSpec {
    machine_runtime_service_spec(
        machine_id,
        vec![
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::Inspect),
            machine_endpoint_spec(
                machine_id,
                MachineServiceEndpoint::ContainerEnsureEndpointNetwork,
            ),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::ContainerRun),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::ContainerStop),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::ContainerRemove),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::DataplanePrepare),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::LogsTail),
        ],
    )
}

#[must_use]
pub fn machine_runtime_service_base(machine_id: &MachineId) -> NatsServiceSpec {
    machine_runtime_service_spec(machine_id, Vec::new())
}

#[must_use]
pub fn machine_endpoint_spec(
    machine_id: &MachineId,
    endpoint: MachineServiceEndpoint,
) -> NatsServiceEndpointSpec {
    NatsServiceEndpointSpec::new(
        machine_endpoint_name(endpoint),
        machine_service(machine_id, endpoint),
        EndpointExecution::MachineRpc,
    )
}

#[must_use]
pub const fn machine_endpoint_name(endpoint: MachineServiceEndpoint) -> &'static str {
    match endpoint {
        MachineServiceEndpoint::Inspect => "machine.inspect",
        MachineServiceEndpoint::ContainerEnsureEndpointNetwork => {
            "machine.container.ensure_endpoint_network"
        }
        MachineServiceEndpoint::ContainerRun => "machine.container.run",
        MachineServiceEndpoint::ContainerStop => "machine.container.stop",
        MachineServiceEndpoint::ContainerRemove => "machine.container.remove",
        MachineServiceEndpoint::DataplanePrepare => "machine.dataplane.prepare",
        MachineServiceEndpoint::LogsTail => "machine.logs.tail",
    }
}

fn machine_runtime_service_spec(
    machine_id: &MachineId,
    endpoints: Vec<NatsServiceEndpointSpec>,
) -> NatsServiceSpec {
    NatsServiceSpec::new(
        format!("{MACHINE_SERVICE_NAME}.{}", machine_id.as_str()),
        MACHINE_SERVICE_NAME,
        SERVICE_VERSION,
        MACHINE_SERVICE_DESCRIPTION,
        ServiceMetadata::from_entries(vec![ServiceMetadataEntry::new(
            "machine_id",
            machine_id.as_str(),
        )]),
        endpoints,
    )
}
