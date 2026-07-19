//! NATS service handlers exposed by the daemon.

use ployz_core::ids::MachineId;
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata,
    ServiceMetadataEntry, ServiceVersion,
};
#[cfg(test)]
use ployz_nats::services::{ServiceDiscoveryQuery, ServicePing, discover_services};
use ployz_nats::subjects::{
    INGRESS_ENDPOINT_GET, INTENT_GET, MachineServiceEndpoint, OperationApiEndpoint,
    OperationApiEndpointExecution, RUNTIME_SNAPSHOT_SEED, machine_service,
};

pub const API_SERVICE_NAME: &str = "plz-api";
pub const API_SERVICE_ID: &str = "plz-api.core";
pub const API_SERVICE_DESCRIPTION: &str = "Ployz operator-facing command service";
pub const MACHINE_SERVICE_NAME: &str = "plz-machine";
pub const MACHINE_SERVICE_DESCRIPTION: &str = "Ployz machine-local runtime service";
pub const GATEWAY_MACHINE_SERVICE_NAME: &str = "plz-gateway-machine";
pub const GATEWAY_MACHINE_SERVICE_DESCRIPTION: &str =
    "Ployz gateway machine-local certificate service";
pub const DNS_SERVICE_NAME: &str = "plz-dns";
pub const DNS_SERVICE_DESCRIPTION: &str = "Ployz machine-local DNS role service";
pub const INTENT_SERVICE_NAME: &str = "plz-intent";
pub const INTENT_SERVICE_ID: &str = "plz-intent.core";
pub const INTENT_SERVICE_DESCRIPTION: &str = "Ployz operator intent service";
pub const INGRESS_ENDPOINT_SERVICE_NAME: &str = "plz-ingress-endpoint";
pub const INGRESS_ENDPOINT_SERVICE_ID: &str = "plz-ingress-endpoint.core";
pub const INGRESS_ENDPOINT_SERVICE_DESCRIPTION: &str =
    "Ployz canonical ingress endpoint projection";
pub const RUNTIME_PROJECTION_SERVICE_NAME: &str = "plz-runtime-projection";
pub const RUNTIME_PROJECTION_SERVICE_ID: &str = "plz-runtime-projection.core";
pub const RUNTIME_PROJECTION_SERVICE_DESCRIPTION: &str = "Ployz passive runtime projection";
pub const SERVICE_VERSION: ServiceVersion = ServiceVersion::new(0, 1, 0);
pub const IMPLEMENTED_OPERATION_API_ENDPOINTS: &[OperationApiEndpoint] = &[
    OperationApiEndpoint::BuildSubmit,
    OperationApiEndpoint::BuildCancel,
    OperationApiEndpoint::CredentialAdd,
    OperationApiEndpoint::CredentialList,
    OperationApiEndpoint::CredentialRemove,
    OperationApiEndpoint::IngressConfigure,
    OperationApiEndpoint::DeployReserve,
    OperationApiEndpoint::DeployPreview,
    OperationApiEndpoint::DeploySubmit,
    OperationApiEndpoint::SystemDeploy,
    OperationApiEndpoint::ServiceRestart,
    OperationApiEndpoint::NamespaceRemove,
    OperationApiEndpoint::VolumeRemove,
    OperationApiEndpoint::VolumeCreate,
    OperationApiEndpoint::InitFirstMachineActivate,
    OperationApiEndpoint::MachineAdd,
    OperationApiEndpoint::MachineBuildCachePrune,
    OperationApiEndpoint::MachineUpdate,
    OperationApiEndpoint::MachineStoragePrepare,
    OperationApiEndpoint::MachineDrain,
    OperationApiEndpoint::MachineResume,
    OperationApiEndpoint::CoreReplace,
    OperationApiEndpoint::CoreReplaceReport,
    OperationApiEndpoint::MachineList,
    OperationApiEndpoint::MachineInspect,
    OperationApiEndpoint::NetworkStatus,
    OperationApiEndpoint::NetworkResolve,
    OperationApiEndpoint::NetworkRepair,
    OperationApiEndpoint::MachineJoinRedeem,
    OperationApiEndpoint::MachineJoinReport,
    OperationApiEndpoint::ServiceList,
    OperationApiEndpoint::VolumeList,
    OperationApiEndpoint::ServiceInspect,
    OperationApiEndpoint::RuntimeSnapshot,
    OperationApiEndpoint::LogsTail,
    OperationApiEndpoint::OpsList,
    OperationApiEndpoint::OpsStatus,
    OperationApiEndpoint::OpsWatch,
];

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
pub struct DaemonServiceCatalog {
    services: Vec<NatsServiceSpec>,
}

#[cfg(test)]
impl DaemonServiceCatalog {
    #[must_use]
    pub fn for_control() -> Self {
        Self {
            services: vec![
                api_service(),
                intent_service(),
                ingress_endpoint_service(),
                runtime_projection_service(),
            ],
        }
    }

    #[must_use]
    pub fn for_machine(machine_id: &MachineId) -> Self {
        Self {
            services: vec![machine_role_service(machine_id)],
        }
    }

    #[must_use]
    pub fn for_gateway(machine_id: &MachineId) -> Self {
        Self {
            services: vec![gateway_role_service(machine_id)],
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
pub fn intent_service() -> NatsServiceSpec {
    NatsServiceSpec::new(
        INTENT_SERVICE_ID,
        INTENT_SERVICE_NAME,
        SERVICE_VERSION,
        INTENT_SERVICE_DESCRIPTION,
        ServiceMetadata::empty(),
        vec![intent_get_endpoint_spec()],
    )
}

#[must_use]
pub fn intent_get_endpoint_spec() -> NatsServiceEndpointSpec {
    NatsServiceEndpointSpec::new("intent.get", INTENT_GET, EndpointExecution::Query)
}

#[must_use]
pub fn ingress_endpoint_service() -> NatsServiceSpec {
    NatsServiceSpec::new(
        INGRESS_ENDPOINT_SERVICE_ID,
        INGRESS_ENDPOINT_SERVICE_NAME,
        SERVICE_VERSION,
        INGRESS_ENDPOINT_SERVICE_DESCRIPTION,
        ServiceMetadata::empty(),
        vec![ingress_endpoint_get_spec()],
    )
}

#[must_use]
pub fn ingress_endpoint_get_spec() -> NatsServiceEndpointSpec {
    NatsServiceEndpointSpec::new(
        "ingress.endpoint.get",
        INGRESS_ENDPOINT_GET,
        EndpointExecution::Query,
    )
}

#[must_use]
pub fn runtime_projection_service() -> NatsServiceSpec {
    NatsServiceSpec::new(
        RUNTIME_PROJECTION_SERVICE_ID,
        RUNTIME_PROJECTION_SERVICE_NAME,
        SERVICE_VERSION,
        RUNTIME_PROJECTION_SERVICE_DESCRIPTION,
        ServiceMetadata::empty(),
        vec![runtime_snapshot_seed_endpoint_spec()],
    )
}

#[must_use]
pub fn runtime_snapshot_seed_endpoint_spec() -> NatsServiceEndpointSpec {
    NatsServiceEndpointSpec::new(
        "runtime.snapshot.seed",
        RUNTIME_SNAPSHOT_SEED,
        EndpointExecution::Query,
    )
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
#[cfg(test)]
pub fn machine_role_service(machine_id: &MachineId) -> NatsServiceSpec {
    machine_role_service_spec(
        machine_id,
        vec![
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::Inspect),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::FactsGet),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::FactsRefresh),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::ContainerInspect),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::ContainerResolveImage),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::ContainerRun),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::ContainerRunHook),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::ContainerRestart),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::ContainerStop),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::ContainerRemove),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::VolumeRemove),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::VolumeEnsure),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::DataplanePublicKey),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::DataplaneStatus),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::SubstrateUpdate),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::SubstrateReport),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::LogsTail),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::ImageBlobCheck),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::ImageBlobPush),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::ImageManifestPush),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::ImageEnsure),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::ImageRemove),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::BuildStart),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::BuildCancel),
        ],
    )
}

#[must_use]
pub fn machine_role_service_base(machine_id: &MachineId) -> NatsServiceSpec {
    machine_role_service_spec(machine_id, Vec::new())
}

#[must_use]
pub fn gateway_role_service(machine_id: &MachineId) -> NatsServiceSpec {
    NatsServiceSpec::new(
        format!("{GATEWAY_MACHINE_SERVICE_NAME}.{}", machine_id.as_str()),
        GATEWAY_MACHINE_SERVICE_NAME,
        SERVICE_VERSION,
        GATEWAY_MACHINE_SERVICE_DESCRIPTION,
        ServiceMetadata::from_entries(vec![ServiceMetadataEntry::new(
            "machine_id",
            machine_id.as_str(),
        )]),
        vec![
            machine_endpoint_spec(
                machine_id,
                MachineServiceEndpoint::CertificateArtifactStatus,
            ),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::CertificateArtifactPush),
            machine_endpoint_spec(
                machine_id,
                MachineServiceEndpoint::CertificateArtifactRemove,
            ),
            machine_endpoint_spec(
                machine_id,
                MachineServiceEndpoint::CertificateChallengeApply,
            ),
            machine_endpoint_spec(
                machine_id,
                MachineServiceEndpoint::CertificateChallengeRemove,
            ),
            machine_endpoint_spec(
                machine_id,
                MachineServiceEndpoint::CertificateChallengeStatus,
            ),
            machine_endpoint_spec(machine_id, MachineServiceEndpoint::GatewayStatusGet),
        ],
    )
}

#[must_use]
pub fn dns_role_service_base(machine_id: &MachineId) -> NatsServiceSpec {
    NatsServiceSpec::new(
        format!("{DNS_SERVICE_NAME}.{}", machine_id.as_str()),
        DNS_SERVICE_NAME,
        SERVICE_VERSION,
        DNS_SERVICE_DESCRIPTION,
        ServiceMetadata::from_entries(vec![ServiceMetadataEntry::new(
            "machine_id",
            machine_id.as_str(),
        )]),
        Vec::new(),
    )
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
        MachineServiceEndpoint::FactsGet => "machine.facts.get",
        MachineServiceEndpoint::FactsRefresh => "machine.facts.refresh",
        MachineServiceEndpoint::DnsResolve => "machine.dns.resolve",
        MachineServiceEndpoint::DnsStatus => "machine.dns.status",
        MachineServiceEndpoint::ContainerInspect => "machine.container.inspect",
        MachineServiceEndpoint::ContainerResolveImage => "machine.container.resolve_image",
        MachineServiceEndpoint::ContainerRun => "machine.container.run",
        MachineServiceEndpoint::ContainerRunHook => "machine.container.run_hook",
        MachineServiceEndpoint::ContainerRestart => "machine.container.restart",
        MachineServiceEndpoint::ContainerStop => "machine.container.stop",
        MachineServiceEndpoint::ContainerRemove => "machine.container.remove",
        MachineServiceEndpoint::VolumeEnsure => "machine.volume.ensure",
        MachineServiceEndpoint::VolumeRemove => "machine.volume.remove",
        MachineServiceEndpoint::VolumeTestimony => "machine.volume.testimony",
        MachineServiceEndpoint::DataplanePublicKey => "machine.dataplane.public_key",
        MachineServiceEndpoint::DataplaneStatus => "machine.dataplane.status",
        MachineServiceEndpoint::SubstrateUpdate => "machine.substrate.update",
        MachineServiceEndpoint::SubstrateReport => "machine.substrate.report",
        MachineServiceEndpoint::StoragePrepare => "machine.storage.prepare",
        MachineServiceEndpoint::StoragePrepareReport => "machine.storage.prepare.report",
        MachineServiceEndpoint::LogsTail => "machine.logs.tail",
        MachineServiceEndpoint::ImageBlobCheck => "machine.image.blob.check",
        MachineServiceEndpoint::ImageBlobPush => "machine.image.blob.push",
        MachineServiceEndpoint::ImageManifestPush => "machine.image.manifest.push",
        MachineServiceEndpoint::ImageEnsure => "machine.image.ensure",
        MachineServiceEndpoint::ImageRemove => "machine.image.remove",
        MachineServiceEndpoint::BuildStart => "machine.build.start",
        MachineServiceEndpoint::BuildCancel => "machine.build.cancel",
        MachineServiceEndpoint::BuildCachePrune => "machine.build.cache.prune",
        MachineServiceEndpoint::CertificateArtifactStatus => "machine.certificate.artifact.status",
        MachineServiceEndpoint::CertificateArtifactPush => "machine.certificate.artifact.push",
        MachineServiceEndpoint::CertificateArtifactRemove => "machine.certificate.artifact.remove",
        MachineServiceEndpoint::CertificateChallengeApply => "machine.certificate.challenge.apply",
        MachineServiceEndpoint::CertificateChallengeRemove => {
            "machine.certificate.challenge.remove"
        }
        MachineServiceEndpoint::CertificateChallengeStatus => {
            "machine.certificate.challenge.status"
        }
        MachineServiceEndpoint::GatewayStatusGet => "machine.gateway.status.get",
    }
}

fn machine_role_service_spec(
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
