//! Core-owned operator intent service.

use crate::control::intent::ingress_intent::{ActiveCertificateMetadataStore, IngressIntentStore};
use crate::control::intent::namespace_intent::NamespaceIntentStore;
use crate::control::intent::nats_authorizations::NatsAuthorizationStore;
use crate::control::operation_evidence::OperationRepository;
use crate::control::store::CoreStore;
use crate::service_catalog::{intent_get_endpoint_spec, intent_service};
use ployz_core::ids::MachineId;
use ployz_core::intent::IntentSnapshot;
use ployz_core::intent::recovery::PendingMachineJoinRecoverySnapshot;
use ployz_core::network::{DataplaneProjection, DataplaneProjectionMember};

use ployz_nats::service_protocol::NatsServiceError;
use ployz_nats::service_runtime::{
    NatsJsonServiceRequestError, NatsServiceRequest, NatsServiceResponse, NatsServiceRuntimeError,
    NatsServiceShutdownError, RunningNatsService, decode_json_request, request_json,
    start_nats_service,
};
use ployz_nats::subjects::{INTENT_CHANGED, INTENT_GET, PENDING_MACHINE_JOINS_CHANGED};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::task::JoinHandle;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentGetRequest {}

#[derive(Debug)]
pub struct RunningIntentService {
    service: RunningNatsService,
    publisher: JoinHandle<()>,
}

struct IntentSnapshotSources<'a> {
    namespace_intent: &'a NamespaceIntentStore,
    ingress_intent: &'a IngressIntentStore,
    certificate_metadata: &'a ActiveCertificateMetadataStore,
    core_store: &'a CoreStore,
    nats_authorizations: &'a NatsAuthorizationStore,
    operation_repository: &'a OperationRepository,
}

impl RunningIntentService {
    pub async fn shutdown(self) -> Result<(), NatsServiceShutdownError> {
        self.publisher.abort();
        self.service.shutdown().await
    }
}

#[derive(Debug, Clone)]
pub struct NatsIntentReader {
    client: async_nats::Client,
    request_timeout: Duration,
}

impl NatsIntentReader {
    #[must_use]
    pub fn new(client: async_nats::Client) -> Self {
        Self {
            client,
            request_timeout: Duration::from_secs(30),
        }
    }

    #[must_use]
    pub const fn with_request_timeout(mut self, request_timeout: Duration) -> Self {
        self.request_timeout = request_timeout;
        self
    }

    pub async fn intent(&self) -> Result<IntentSnapshot, IntentReadError> {
        request_json(
            &self.client,
            INTENT_GET.to_owned(),
            &IntentGetRequest {},
            self.request_timeout,
        )
        .await
        .map_err(IntentReadError::from)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IntentReadError {
    #[error("{message}")]
    Unavailable { message: String },
}

impl From<NatsJsonServiceRequestError> for IntentReadError {
    fn from(error: NatsJsonServiceRequestError) -> Self {
        Self::Unavailable {
            message: format!("{error}"),
        }
    }
}

pub async fn start_intent_service(
    client: async_nats::Client,
    core_machine_id: MachineId,
    namespace_intent: NamespaceIntentStore,
    core_store: CoreStore,
    publish_interval: Duration,
) -> Result<RunningIntentService, NatsServiceRuntimeError> {
    let ingress_intent = IngressIntentStore::new(core_store.clone());
    let certificate_metadata = ActiveCertificateMetadataStore::new(core_store.clone());
    let mut service = start_nats_service(client.clone(), &intent_service()).await?;
    // The grant set is a projection of the same store; a thin wrapper over it reads
    // the grants the authorization writer persists there.
    let nats_authorizations = NatsAuthorizationStore::new(core_store.clone());
    let operation_repository = OperationRepository::open(core_store.clone(), client.clone());
    let service_core_machine_id = core_machine_id.clone();
    let publisher_core_machine_id = core_machine_id;
    let service_namespace_intent = namespace_intent.clone();
    let service_ingress_intent = ingress_intent.clone();
    let service_certificate_metadata = certificate_metadata.clone();
    let service_core_store = core_store.clone();
    let service_authorizations = nats_authorizations.clone();
    let service_operation_repository = operation_repository.clone();
    service
        .bind_endpoint(&intent_get_endpoint_spec(), move |request| {
            let namespace_intent = service_namespace_intent.clone();
            let ingress_intent = service_ingress_intent.clone();
            let certificate_metadata = service_certificate_metadata.clone();
            let core_store = service_core_store.clone();
            let nats_authorizations = service_authorizations.clone();
            let operation_repository = service_operation_repository.clone();
            let core_machine_id = service_core_machine_id.clone();
            async move {
                let sources = IntentSnapshotSources {
                    namespace_intent: &namespace_intent,
                    ingress_intent: &ingress_intent,
                    certificate_metadata: &certificate_metadata,
                    core_store: &core_store,
                    nats_authorizations: &nats_authorizations,
                    operation_repository: &operation_repository,
                };
                intent_get_response(request, &core_machine_id, &sources).await
            }
        })
        .await?;
    let publisher = tokio::spawn(async move {
        let sources = IntentSnapshotSources {
            namespace_intent: &namespace_intent,
            ingress_intent: &ingress_intent,
            certificate_metadata: &certificate_metadata,
            core_store: &core_store,
            nats_authorizations: &nats_authorizations,
            operation_repository: &operation_repository,
        };
        let mut interval = tokio::time::interval(publish_interval);
        let mut consecutive_failures: u64 = 0;
        loop {
            interval.tick().await;
            let intent = match load_intent(&publisher_core_machine_id, &sources).await {
                Ok(intent) => intent,
                Err(error) => {
                    warn_publisher_failure(&mut consecutive_failures, "load-intent", error);
                    continue;
                }
            };
            let payload = match serde_json::to_vec(&intent) {
                Ok(payload) => payload,
                Err(error) => {
                    warn_publisher_failure(&mut consecutive_failures, "encode-intent", error);
                    continue;
                }
            };
            if let Err(error) = client.publish(INTENT_CHANGED, payload.into()).await {
                warn_publisher_failure(&mut consecutive_failures, "publish-intent", error);
                continue;
            }
            if let Err(error) =
                publish_pending_machine_joins(&client, &operation_repository, &core_store).await
            {
                warn_publisher_failure(&mut consecutive_failures, "publish-pending-joins", error);
                continue;
            }
            consecutive_failures = 0;
        }
    });

    Ok(RunningIntentService { service, publisher })
}

fn dataplane_projection(
    active_machines: &[ployz_core::intent::ActiveMachineState],
    staged: Option<ployz_core::intent::StagedMachineDataplaneState>,
) -> Result<DataplaneProjection, String> {
    let declared = active_machines
        .iter()
        .cloned()
        .map(|machine| DataplaneProjectionMember {
            machine_id: machine.machine_id,
            endpoint_subnet: machine.endpoint_subnet,
            mesh_endpoints: machine.mesh_endpoints,
            wireguard_public_key: machine.wireguard_public_key,
        })
        .collect();
    let staged = staged.map(|machine| DataplaneProjectionMember {
        machine_id: machine.machine_id,
        endpoint_subnet: machine.endpoint_subnet,
        mesh_endpoints: machine.mesh_endpoints,
        wireguard_public_key: machine.wireguard_public_key,
    });
    DataplaneProjection::try_new(declared, staged).map_err(|error| error.to_string())
}

async fn intent_get_response(
    request: NatsServiceRequest,
    core_machine_id: &MachineId,
    sources: &IntentSnapshotSources<'_>,
) -> NatsServiceResponse {
    if let Err(response) = decode_json_request::<IntentGetRequest>(&request) {
        return response;
    }

    match load_intent(core_machine_id, sources).await {
        Ok(intent) => NatsServiceResponse::json_ok(&intent),
        Err(message) => NatsServiceResponse::transport_error(NatsServiceError::internal(message)),
    }
}

async fn load_intent(
    core_machine_id: &MachineId,
    sources: &IntentSnapshotSources<'_>,
) -> Result<IntentSnapshot, String> {
    let epoch = sources
        .core_store
        .control_plane_epoch()
        .await
        .map_err(|error| error.to_string())?;
    let (active_machines, staged_machine) = sources
        .operation_repository
        .intent_machine_sources()
        .await
        .map_err(|error| error.to_string())?;
    let dataplane_projection = dataplane_projection(&active_machines, staged_machine)?;
    let namespace_intent = sources
        .namespace_intent
        .load()
        .await
        .map_err(|error| error.to_string())?;
    let nats_authorizations = sources
        .nats_authorizations
        .list()
        .await
        .map_err(|error| error.to_string())?;
    let ingress = sources
        .ingress_intent
        .load()
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "ingress intent is unconfigured".to_owned())?;
    let active_certificates = sources
        .certificate_metadata
        .active_certificates()
        .await
        .map_err(|error| error.to_string())?;

    Ok(IntentSnapshot {
        epoch,
        core_machine_id: core_machine_id.clone(),
        active_machines,
        dataplane_projection,
        route_bindings: namespace_intent.route_bindings,
        serving_target_entries: namespace_intent.serving_target_entries,
        volume_pins: namespace_intent.volume_pins,
        nats_authorizations,
        automatic_hostname_configuration: ingress.automatic_hostnames().clone(),
        ployz_dns_target: ingress.ployz_dns_target(),
        active_certificates,
    })
}

pub async fn publish_pending_machine_joins(
    client: &async_nats::Client,
    operation_repository: &OperationRepository,
    core_store: &CoreStore,
) -> Result<(), String> {
    let pending = operation_repository
        .pending_machine_adds_for_mirror()
        .await
        .map_err(|error| error.to_string())?;
    let epoch = core_store
        .control_plane_epoch()
        .await
        .map_err(|error| error.to_string())?;
    let payload = serde_json::to_vec(&PendingMachineJoinRecoverySnapshot { epoch, pending })
        .map_err(|error| error.to_string())?;
    client
        .publish(PENDING_MACHINE_JOINS_CHANGED, payload.into())
        .await
        .map_err(|error| error.to_string())
}

fn warn_publisher_failure(
    consecutive_failures: &mut u64,
    phase: &str,
    error: impl std::fmt::Display,
) {
    *consecutive_failures += 1;
    eprintln!(
        "ployzd intent publisher warning: phase={phase} consecutive_failures={} error={error}",
        *consecutive_failures
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::intent::ingress_intent::{IngressConfiguration, PloyzDnsTargetStore};
    use ployz_core::ingress::{AutomaticHostnameConfiguration, PloyzDnsTargetIntent};

    #[tokio::test]
    async fn private_ingress_evidence_is_not_projected() {
        let nats = ployz_test_support::nats::TestNats::start().await;
        let store = CoreStore::open_in_memory().await.expect("store");
        let namespace_intent = NamespaceIntentStore::new(store.clone());
        let ingress_intent = IngressIntentStore::new(store.clone());
        ingress_intent
            .replace(
                IngressConfiguration::try_new(
                    AutomaticHostnameConfiguration::Ployz,
                    PloyzDnsTargetIntent::Enabled,
                )
                .expect("valid ingress configuration"),
            )
            .await
            .expect("configure ingress");
        assert!(
            PloyzDnsTargetStore::new(store.clone())
                .ensure_acquisition()
                .await
                .expect("create private allocation")
                .is_some()
        );
        let certificate_metadata = ActiveCertificateMetadataStore::new(store.clone());
        let nats_authorizations = NatsAuthorizationStore::new(store.clone());
        let operation_repository =
            OperationRepository::open(store.clone(), nats.controller.clone());
        let sources = IntentSnapshotSources {
            namespace_intent: &namespace_intent,
            ingress_intent: &ingress_intent,
            certificate_metadata: &certificate_metadata,
            core_store: &store,
            nats_authorizations: &nats_authorizations,
            operation_repository: &operation_repository,
        };

        let snapshot = load_intent(
            &MachineId::try_new("machine_a").expect("machine id"),
            &sources,
        )
        .await
        .expect("project intent");
        assert_eq!(
            snapshot.automatic_hostname_configuration,
            AutomaticHostnameConfiguration::Ployz
        );
        assert_eq!(snapshot.ployz_dns_target, PloyzDnsTargetIntent::Enabled);
        let json = serde_json::to_string(&snapshot).expect("serialize snapshot");

        for forbidden in [
            "\"token\"",
            "bearer_token",
            "private_key_pem",
            "managed_dns_checkpoint",
            "ingress_endpoint_projection",
            "acquisition_id",
        ] {
            assert!(!json.contains(forbidden));
        }
    }
}
