//! NATS service handlers exposed by the daemon.

use ployz_core::ids::NodeId;
use ployz_core::subjects::{
    API_DEPLOY_SUBMIT, API_OPS_STATUS, API_OPS_WATCH, NodeServiceEndpoint, node_service,
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
pub const API_ENDPOINTS: [ApiEndpoint; 3] = [
    ApiEndpoint::DeploySubmit,
    ApiEndpoint::OpsStatus,
    ApiEndpoint::OpsWatch,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiEndpoint {
    DeploySubmit,
    OpsStatus,
    OpsWatch,
}

impl ApiEndpoint {
    #[must_use]
    pub fn spec(self) -> NatsServiceEndpointSpec {
        match self {
            Self::DeploySubmit => NatsServiceEndpointSpec::new(
                "deploy.submit",
                API_DEPLOY_SUBMIT,
                EndpointExecution::AcceptsOperation,
            ),
            Self::OpsStatus => {
                NatsServiceEndpointSpec::new("ops.status", API_OPS_STATUS, EndpointExecution::Query)
            }
            Self::OpsWatch => {
                NatsServiceEndpointSpec::new("ops.watch", API_OPS_WATCH, EndpointExecution::Query)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonServiceCatalog {
    services: Vec<NatsServiceSpec>,
}

impl DaemonServiceCatalog {
    #[must_use]
    pub fn for_node(node_id: &NodeId) -> Self {
        Self {
            services: vec![api_service(), node_runtime_service(node_id)],
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
    API_ENDPOINTS
        .iter()
        .copied()
        .map(ApiEndpoint::spec)
        .collect()
}

#[must_use]
pub fn api_deploy_submit_endpoint() -> NatsServiceEndpointSpec {
    ApiEndpoint::DeploySubmit.spec()
}

#[must_use]
pub fn api_ops_status_endpoint() -> NatsServiceEndpointSpec {
    ApiEndpoint::OpsStatus.spec()
}

#[must_use]
pub fn api_ops_watch_endpoint() -> NatsServiceEndpointSpec {
    ApiEndpoint::OpsWatch.spec()
}

#[must_use]
pub fn node_runtime_service(node_id: &NodeId) -> NatsServiceSpec {
    NatsServiceSpec::new(
        format!("{NODE_SERVICE_NAME}.{}", node_id.as_str()),
        NODE_SERVICE_NAME,
        SERVICE_VERSION,
        NODE_SERVICE_DESCRIPTION,
        ServiceMetadata::from_entries(vec![ServiceMetadataEntry::new("node_id", node_id.as_str())]),
        vec![
            NatsServiceEndpointSpec::new(
                "node.inspect",
                node_service(node_id, NodeServiceEndpoint::Inspect),
                EndpointExecution::NodeRpc,
            ),
            NatsServiceEndpointSpec::new(
                "node.container.run",
                node_service(node_id, NodeServiceEndpoint::ContainerRun),
                EndpointExecution::NodeRpc,
            ),
            NatsServiceEndpointSpec::new(
                "node.logs.tail",
                node_service(node_id, NodeServiceEndpoint::LogsTail),
                EndpointExecution::NodeRpc,
            ),
        ],
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
