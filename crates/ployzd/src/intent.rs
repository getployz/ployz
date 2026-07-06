//! Core-owned operator intent service.

use crate::machine_roster::MachineRosterStore;
use crate::namespace_intent::NamespaceIntentStore;
use crate::service_catalog::{intent_get_endpoint_spec, intent_service};
use ployz_core::state::IntentSnapshot;
use ployz_core::subjects::{INTENT_CHANGED, INTENT_GET};
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
pub struct RunningIntentRuntime {
    service: RunningNatsService,
    publisher: JoinHandle<()>,
}

impl RunningIntentRuntime {
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

pub async fn start_intent_runtime(
    client: async_nats::Client,
    machine_roster: MachineRosterStore,
    namespace_intent: NamespaceIntentStore,
    publish_interval: Duration,
) -> Result<RunningIntentRuntime, NatsServiceRuntimeError> {
    let mut service = start_nats_service(client.clone(), &intent_service()).await?;
    let service_machine_roster = machine_roster.clone();
    let service_namespace_intent = namespace_intent.clone();
    service
        .bind_endpoint(&intent_get_endpoint_spec(), move |request| {
            let machine_roster = service_machine_roster.clone();
            let namespace_intent = service_namespace_intent.clone();
            async move { intent_get_response(request, &machine_roster, &namespace_intent).await }
        })
        .await?;

    let publisher = tokio::spawn(async move {
        let mut interval = tokio::time::interval(publish_interval);
        loop {
            interval.tick().await;
            let Ok(intent) = load_intent(&machine_roster, &namespace_intent).await else {
                continue;
            };
            let Ok(payload) = serde_json::to_vec(&intent) else {
                continue;
            };
            let _ = client.publish(INTENT_CHANGED, payload.into()).await;
        }
    });

    Ok(RunningIntentRuntime { service, publisher })
}

async fn intent_get_response(
    request: NatsServiceRequest,
    machine_roster: &MachineRosterStore,
    namespace_intent: &NamespaceIntentStore,
) -> NatsServiceResponse {
    if let Err(response) = decode_json_request::<IntentGetRequest>(&request) {
        return response;
    }

    match load_intent(machine_roster, namespace_intent).await {
        Ok(intent) => NatsServiceResponse::json_ok(&intent),
        Err(message) => NatsServiceResponse::transport_error(NatsServiceError::internal(message)),
    }
}

async fn load_intent(
    machine_roster: &MachineRosterStore,
    namespace_intent: &NamespaceIntentStore,
) -> Result<IntentSnapshot, String> {
    let active_machines = machine_roster
        .active_machines()
        .await
        .map_err(|error| error.to_string())?;
    let namespace_intent = namespace_intent
        .load()
        .await
        .map_err(|error| error.to_string())?;

    Ok(IntentSnapshot {
        active_machines,
        route_bindings: namespace_intent.route_bindings,
        serving_target_entries: namespace_intent.serving_target_entries,
    })
}
