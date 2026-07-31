use crate::control::intent::ingress_intent::{
    IngressConfigurationWrite, IngressIntentStore, IngressIntentStoreError,
};
use crate::control::operation_evidence::AcceptedIngressConfigureSubmission;
use crate::control::sequencer::OperationControllers;
use crate::tasks::TaskSpawner;
use ployz_core::operation::{
    FailureMessage, IngressConfigureFailure, IngressConfigureTransition, OperationKind,
};

#[derive(Debug, Clone)]
pub struct IngressConfigureOperation {
    controllers: OperationControllers,
    intent: IngressIntentStore,
    client: async_nats::Client,
    task_registry: TaskSpawner,
}

impl IngressConfigureOperation {
    #[must_use]
    pub const fn new(
        controllers: OperationControllers,
        intent: IngressIntentStore,
        client: async_nats::Client,
        task_registry: TaskSpawner,
    ) -> Self {
        Self {
            controllers,
            intent,
            client,
            task_registry,
        }
    }

    pub async fn start(&self, accepted: AcceptedIngressConfigureSubmission) {
        if !accepted.should_start_execution {
            return;
        }
        let operation_id = accepted.operation_id.clone();
        let worker = self.clone();
        super::finish_rejected_task_admission(
            &self.controllers,
            &operation_id,
            self.task_registry.spawn(|| async move {
                worker.run(accepted).await;
            }),
        )
        .await;
    }

    #[tracing::instrument(name = "operation", skip_all, fields(kind = OperationKind::IngressConfigure.as_str(), operation_id = accepted.operation_id.as_str()))]
    async fn run(self, accepted: AcceptedIngressConfigureSubmission) {
        let operation_id = accepted.operation_id;
        let transition = match self.intent.replace(accepted.configuration).await {
            Ok(write) => {
                if matches!(write, IngressConfigurationWrite::Stored) {
                    super::publish_intent_changed(&self.client, "ingress-configure-commit").await;
                }
                IngressConfigureTransition::Completed
            }
            Err(error) => IngressConfigureTransition::Failed {
                failure: operation_failure(error),
            },
        };
        if let Err(error) = self
            .controllers
            .repository()
            .record_ingress_configure_transition(&operation_id, transition)
            .await
        {
            tracing::error!(
                operation_id = operation_id.as_str(),
                error = %error,
                "ingress configuration terminal transition could not be recorded"
            );
        }
        self.controllers.release_ingress(&operation_id).await;
    }
}

fn operation_failure(error: IngressIntentStoreError) -> IngressConfigureFailure {
    let message = FailureMessage::try_new(error.to_string()).expect("store errors are non-empty");
    match error {
        IngressIntentStoreError::InvalidConfiguration { .. } => {
            IngressConfigureFailure::InvalidConfiguration { message }
        }
        IngressIntentStoreError::Store(_) => IngressConfigureFailure::IntentStoreFailed { message },
    }
}
