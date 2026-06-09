//! NATS service handlers exposed by the daemon.

use ployz_core::ids::NodeId;
use ployz_core::subjects::{
    NodeServiceEndpoint, OperationApiEndpoint, OperationApiEndpointExecution, node_service,
};
use ployz_nats::services::{
    EndpointExecution, NatsRequestFailure, NatsServiceEndpointSpec, NatsServiceSpec,
    ServiceDiscoveryQuery, ServiceMetadata, ServiceMetadataEntry, ServicePing, ServiceVersion,
    discover_services,
};

pub const API_SERVICE_NAME: &str = "plz-api";
pub const API_SERVICE_ID: &str = "plz-api.core";
pub const API_SERVICE_DESCRIPTION: &str = "Ployz user-facing command service";
pub const NODE_SERVICE_NAME: &str = "plz-node";
pub const NODE_SERVICE_DESCRIPTION: &str = "Ployz node-local runtime service";
pub const SERVICE_VERSION: ServiceVersion = ServiceVersion::new(0, 1, 0);
pub const IMPLEMENTED_OPERATION_API_ENDPOINTS: [OperationApiEndpoint; 9] = [
    OperationApiEndpoint::DeploySubmit,
    OperationApiEndpoint::MachineAdd,
    OperationApiEndpoint::MachineList,
    OperationApiEndpoint::MachineInspect,
    OperationApiEndpoint::MachineJoinRedeem,
    OperationApiEndpoint::MachineJoinReport,
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
    pub fn for_node(node_id: &NodeId) -> Self {
        Self {
            services: vec![node_runtime_service(node_id)],
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
pub fn api_deploy_submit_endpoint() -> NatsServiceEndpointSpec {
    api_endpoint_spec(OperationApiEndpoint::DeploySubmit)
}

#[must_use]
pub fn api_ops_status_endpoint() -> NatsServiceEndpointSpec {
    api_endpoint_spec(OperationApiEndpoint::OpsStatus)
}

#[must_use]
pub fn api_ops_watch_endpoint() -> NatsServiceEndpointSpec {
    api_endpoint_spec(OperationApiEndpoint::OpsWatch)
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
pub fn node_runtime_service(node_id: &NodeId) -> NatsServiceSpec {
    node_runtime_service_spec(
        node_id,
        vec![
            node_endpoint_spec(node_id, NodeServiceEndpoint::Inspect),
            node_endpoint_spec(node_id, NodeServiceEndpoint::ContainerRun),
            node_endpoint_spec(node_id, NodeServiceEndpoint::ContainerRemove),
            node_endpoint_spec(node_id, NodeServiceEndpoint::WireGuardEbpfPrepare),
            node_endpoint_spec(node_id, NodeServiceEndpoint::LogsTail),
        ],
    )
}

#[must_use]
pub fn node_runtime_service_base(node_id: &NodeId) -> NatsServiceSpec {
    node_runtime_service_spec(node_id, Vec::new())
}

#[must_use]
pub fn node_endpoint_spec(
    node_id: &NodeId,
    endpoint: NodeServiceEndpoint,
) -> NatsServiceEndpointSpec {
    NatsServiceEndpointSpec::new(
        node_endpoint_name(endpoint),
        node_service(node_id, endpoint),
        EndpointExecution::NodeRpc,
    )
}

#[must_use]
pub const fn node_endpoint_name(endpoint: NodeServiceEndpoint) -> &'static str {
    match endpoint {
        NodeServiceEndpoint::Inspect => "node.inspect",
        NodeServiceEndpoint::ContainerRun => "node.container.run",
        NodeServiceEndpoint::ContainerRemove => "node.container.remove",
        NodeServiceEndpoint::WireGuardEbpfPrepare => "node.wireguard_ebpf.prepare",
        NodeServiceEndpoint::LogsTail => "node.logs.tail",
    }
}

fn node_runtime_service_spec(
    node_id: &NodeId,
    endpoints: Vec<NatsServiceEndpointSpec>,
) -> NatsServiceSpec {
    NatsServiceSpec::new(
        format!("{NODE_SERVICE_NAME}.{}", node_id.as_str()),
        NODE_SERVICE_NAME,
        SERVICE_VERSION,
        NODE_SERVICE_DESCRIPTION,
        ServiceMetadata::from_entries(vec![ServiceMetadataEntry::new("node_id", node_id.as_str())]),
        endpoints,
    )
}

#[must_use]
pub fn node_endpoint_subject(node_id: &NodeId, endpoint: NodeServiceEndpoint) -> String {
    node_service(node_id, endpoint)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeServiceCallError {
    NodeUnavailable { node_id: NodeId, subject: String },
    Timeout { node_id: NodeId, subject: String },
}

impl NodeServiceCallError {
    #[must_use]
    pub fn from_request_failure(node_id: &NodeId, failure: NatsRequestFailure) -> Self {
        match failure {
            NatsRequestFailure::NoResponders { subject } => Self::NodeUnavailable {
                node_id: node_id.clone(),
                subject,
            },
            NatsRequestFailure::Timeout { subject } => Self::Timeout {
                node_id: node_id.clone(),
                subject,
            },
        }
    }
}
