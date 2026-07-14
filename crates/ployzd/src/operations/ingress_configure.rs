use crate::intent::ingress_intent::{
    IngressConfigurationWrite, IngressIntentStore, IngressIntentStoreError,
};
use crate::operation_api::admission::OperationControllers;
use crate::operations::log::AcceptedIngressConfigureSubmission;
use crate::tasks::TaskRegistry;
use ployz_core::ops::{FailureMessage, IngressConfigureFailure, IngressConfigureTransition};
use ployz_core::subjects::INTENT_CHANGED;

#[derive(Debug, Clone)]
pub struct IngressConfigureOperation {
    controllers: OperationControllers,
    intent: IngressIntentStore,
    client: async_nats::Client,
    task_registry: TaskRegistry,
}

impl IngressConfigureOperation {
    #[must_use]
    pub const fn new(
        controllers: OperationControllers,
        intent: IngressIntentStore,
        client: async_nats::Client,
        task_registry: TaskRegistry,
    ) -> Self {
        Self {
            controllers,
            intent,
            client,
            task_registry,
        }
    }

    pub fn start(&self, accepted: AcceptedIngressConfigureSubmission) {
        if !accepted.should_start_execution {
            return;
        }
        let worker = self.clone();
        self.task_registry.spawn(async move {
            worker.run(accepted).await;
        });
    }

    async fn run(self, accepted: AcceptedIngressConfigureSubmission) {
        let operation_id = accepted.operation_id;
        let transition = match self.intent.replace(accepted.configuration).await {
            Ok(write) => {
                if matches!(write, IngressConfigurationWrite::Stored) {
                    let _ = self.client.publish(INTENT_CHANGED, Vec::new().into()).await;
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
            eprintln!(
                "ingress configuration operation {} could not record terminal evidence: {error}",
                operation_id.as_str()
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
