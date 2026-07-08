//! Core-owned operator intent service.

use crate::core_store::CoreStore;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentReadError {
    Unavailable { message: String },
}

impl std::fmt::Display for IntentReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable { message } => write!(formatter, "{message}"),
        }
    }
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
    let mut service = start_nats_service(client.clone(), &intent_service()).await?;
    // The grant set is a projection of the same store; a thin wrapper over it reads
    // the grants the authorization writer persists there.
    let nats_authorizations = NatsAuthorizationStore::new(core_store.clone());
    let operation_repository = OperationRepository::open(core_store.clone(), client.clone());
    let service_core_machine_id = core_machine_id.clone();
    let publisher_core_machine_id = core_machine_id;
    let service_machine_roster = machine_roster.clone();
    let service_namespace_intent = namespace_intent.clone();
    let service_core_store = core_store.clone();
    let service_authorizations = nats_authorizations.clone();
    service
        .bind_endpoint(&intent_get_endpoint_spec(), move |request| {
            let machine_roster = service_machine_roster.clone();
            let namespace_intent = service_namespace_intent.clone();
            let core_store = service_core_store.clone();
            let nats_authorizations = service_authorizations.clone();
            let core_machine_id = service_core_machine_id.clone();
            async move {
                intent_get_response(
                    request,
                    &core_machine_id,
                    &machine_roster,
                    &namespace_intent,
                    &core_store,
                    &nats_authorizations,
                )
                .await
            }
        })
        .await?;

    let publisher = tokio::spawn(async move {
        let mut interval = tokio::time::interval(publish_interval);
        loop {
            interval.tick().await;
            let Ok(intent) = load_intent(
                &publisher_core_machine_id,
                &machine_roster,
                &namespace_intent,
                &core_store,
                &nats_authorizations,
            )
            .await
            else {
                continue;
            };
            let Ok(payload) = serde_json::to_vec(&intent) else {
                continue;
            };
            let _ = client.publish(INTENT_CHANGED, payload.into()).await;
            let _ =
                publish_pending_machine_joins(&client, &operation_repository, &core_store).await;
        }
    });

    Ok(RunningIntentService { service, publisher })
}

async fn intent_get_response(
    request: NatsServiceRequest,
    core_machine_id: &MachineId,
    machine_roster: &MachineRosterStore,
    namespace_intent: &NamespaceIntentStore,
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

    Ok(IntentSnapshot {
        epoch,
        core_machine_id: core_machine_id.clone(),
        active_machines,
        route_bindings: namespace_intent.route_bindings,
        serving_target_entries: namespace_intent.serving_target_entries,
        authorized_users,
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
