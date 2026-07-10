//! Core-owned operator intent service.

use crate::core_store::CoreStore;
use crate::intent::certificate_intent::CertificateIntentStore;
use crate::intent::lease_intent::LeaseIntentStore;
use crate::intent::machine_roster::MachineRosterStore;
use crate::intent::namespace_intent::NamespaceIntentStore;
use crate::intent::nats_authorizations::NatsAuthorizationStore;
use crate::operations::log::OperationRepository;
use crate::service_catalog::{intent_get_endpoint_spec, intent_service};
use ployz_core::ids::MachineId;
use ployz_core::state::{IntentSnapshot, PendingMachineJoinRecoverySnapshot};
use ployz_core::subjects::{INTENT_CHANGED, INTENT_GET, PENDING_MACHINE_JOINS_CHANGED};
use ployz_nats::service_protocol::NatsServiceError;
use ployz_nats::service_runtime::{
    NatsJsonServiceRequestError, NatsServiceRequest, NatsServiceResponse, NatsServiceRuntimeError,
    NatsServiceShutdownError, RunningNatsService, decode_json_request, request_json,
    start_nats_service,
};
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
    machine_roster: MachineRosterStore,
    namespace_intent: NamespaceIntentStore,
    core_store: CoreStore,
    publish_interval: Duration,
) -> Result<RunningIntentService, NatsServiceRuntimeError> {
    let lease_intent = LeaseIntentStore::new(core_store.clone());
    let certificate_intent = CertificateIntentStore::reader(core_store.clone());
    let mut service = start_nats_service(client.clone(), &intent_service()).await?;
    // The grant set is a projection of the same store; a thin wrapper over it reads
    // the grants the authorization writer persists there.
    let nats_authorizations = NatsAuthorizationStore::new(core_store.clone());
    let operation_repository = OperationRepository::open(core_store.clone(), client.clone());
    let service_core_machine_id = core_machine_id.clone();
    let publisher_core_machine_id = core_machine_id;
    let service_machine_roster = machine_roster.clone();
    let service_namespace_intent = namespace_intent.clone();
    let service_lease_intent = lease_intent.clone();
    let service_certificate_intent = certificate_intent.clone();
    let service_core_store = core_store.clone();
    let service_authorizations = nats_authorizations.clone();
    service
        .bind_endpoint(&intent_get_endpoint_spec(), move |request| {
            let machine_roster = service_machine_roster.clone();
            let namespace_intent = service_namespace_intent.clone();
            let lease_intent = service_lease_intent.clone();
            let certificate_intent = service_certificate_intent.clone();
            let core_store = service_core_store.clone();
            let nats_authorizations = service_authorizations.clone();
            let core_machine_id = service_core_machine_id.clone();
            async move {
                intent_get_response(
                    request,
                    &core_machine_id,
                    &machine_roster,
                    &namespace_intent,
                    &lease_intent,
                    &certificate_intent,
                    &core_store,
                    &nats_authorizations,
                )
                .await
            }
        })
        .await?;

    let publisher = tokio::spawn(async move {
        let mut interval = tokio::time::interval(publish_interval);
        let mut consecutive_failures: u64 = 0;
        loop {
            interval.tick().await;
            let intent = match load_intent(
                &publisher_core_machine_id,
                &machine_roster,
                &namespace_intent,
                &lease_intent,
                &certificate_intent,
                &core_store,
                &nats_authorizations,
            )
            .await
            {
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

async fn intent_get_response(
    request: NatsServiceRequest,
    core_machine_id: &MachineId,
    machine_roster: &MachineRosterStore,
    namespace_intent: &NamespaceIntentStore,
    lease_intent: &LeaseIntentStore,
    certificate_intent: &CertificateIntentStore,
    core_store: &CoreStore,
    nats_authorizations: &NatsAuthorizationStore,
) -> NatsServiceResponse {
    if let Err(response) = decode_json_request::<IntentGetRequest>(&request) {
        return response;
    }

    match load_intent(
        core_machine_id,
        machine_roster,
        namespace_intent,
        lease_intent,
        certificate_intent,
        core_store,
        nats_authorizations,
    )
    .await
    {
        Ok(intent) => NatsServiceResponse::json_ok(&intent),
        Err(message) => NatsServiceResponse::transport_error(NatsServiceError::internal(message)),
    }
}

async fn load_intent(
    core_machine_id: &MachineId,
    machine_roster: &MachineRosterStore,
    namespace_intent: &NamespaceIntentStore,
    lease_intent: &LeaseIntentStore,
    certificate_intent: &CertificateIntentStore,
    core_store: &CoreStore,
    nats_authorizations: &NatsAuthorizationStore,
) -> Result<IntentSnapshot, String> {
    let epoch = core_store
        .control_plane_epoch()
        .await
        .map_err(|error| error.to_string())?;
    let active_machines = machine_roster
        .active_machines()
        .await
        .map_err(|error| error.to_string())?;
    let namespace_intent = namespace_intent
        .load()
        .await
        .map_err(|error| error.to_string())?;
    let authorized_users = nats_authorizations
        .list()
        .await
        .map_err(|error| error.to_string())?;
    let managed_lease = match lease_intent
        .load()
        .await
        .map_err(|error| error.to_string())?
    {
        ployz_core::cert::ManagedLeaseIntent::Auto { state } => match *state {
            ployz_core::cert::AutoLeaseState::RecordOnly { lease } => {
                ployz_core::state::ManagedLeaseProjection::RecordOnly { lease }
            }
            ployz_core::cert::AutoLeaseState::Ready { lease, bundle } => {
                ployz_core::state::ManagedLeaseProjection::Ready { lease, bundle }
            }
            ployz_core::cert::AutoLeaseState::Unacquired => {
                ployz_core::state::ManagedLeaseProjection::Unacquired
            }
        },
        ployz_core::cert::ManagedLeaseIntent::BringYourOwn
        | ployz_core::cert::ManagedLeaseIntent::None => {
            ployz_core::state::ManagedLeaseProjection::Unacquired
        }
    };
    let custom_certificates = certificate_intent
        .active_certificates()
        .await
        .map_err(|error| error.to_string())?;
    let acme_http01_challenges = certificate_intent
        .challenges()
        .await
        .map_err(|error| error.to_string())?;

    Ok(IntentSnapshot {
        epoch,
        core_machine_id: core_machine_id.clone(),
        active_machines,
        route_bindings: namespace_intent.route_bindings,
        serving_target_entries: namespace_intent.serving_target_entries,
        volume_pins: namespace_intent.volume_pins,
        authorized_users,
        managed_lease,
        custom_certificates,
        acme_http01_challenges,
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
