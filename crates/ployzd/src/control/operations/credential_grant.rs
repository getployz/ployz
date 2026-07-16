use crate::control::authorization::{
    CredentialMutationChange, CredentialMutationFailure, CredentialMutationRejection,
    NatsAuthorizationHandle, RenderFailure, RenderPrepareFailure,
};
use crate::control::operation_evidence::AcceptedCredentialGrantSubmission;
use crate::control::sequencer::OperationControllers;
use crate::tasks::TaskSpawner;
use ployz_core::operation::{
    CredentialGrantAction, CredentialGrantFailure, CredentialGrantTransition, FailureMessage,
};
use ployz_nats::subjects::INTENT_CHANGED;

#[derive(Debug, Clone)]
pub struct CredentialGrantOperation {
    controllers: OperationControllers,
    authorization: NatsAuthorizationHandle,
    client: async_nats::Client,
    task_registry: TaskSpawner,
}

impl CredentialGrantOperation {
    #[must_use]
    pub const fn new(
        controllers: OperationControllers,
        authorization: NatsAuthorizationHandle,
        client: async_nats::Client,
        task_registry: TaskSpawner,
    ) -> Self {
        Self {
            controllers,
            authorization,
            client,
            task_registry,
        }
    }

    pub async fn start(&self, accepted: AcceptedCredentialGrantSubmission) {
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

    async fn run(self, accepted: AcceptedCredentialGrantSubmission) {
        let operation_id = accepted.operation_id;
        let result = match accepted.action {
            CredentialGrantAction::Add { grant } => self.authorization.add_credential(grant).await,
            CredentialGrantAction::Remove { public_key } => {
                self.authorization.remove_credential(public_key).await
            }
        };
        let transition = match result {
            Ok(result) => {
                if !matches!(result.change, CredentialMutationChange::Unchanged) {
                    let _ = self.client.publish(INTENT_CHANGED, Vec::new().into()).await;
                }
                CredentialGrantTransition::Completed
            }
            Err(error) => CredentialGrantTransition::Failed {
                failure: mutation_failure(error),
            },
        };
        if let Err(error) = self
            .controllers
            .repository()
            .record_credential_grant_transition(&operation_id, transition)
            .await
        {
            eprintln!(
                "credential grant operation {} could not record terminal evidence: {error}",
                operation_id.as_str()
            );
        }
    }
}

fn mutation_failure(error: CredentialMutationFailure) -> CredentialGrantFailure {
    match error {
        CredentialMutationFailure::Rejected { reason } => match reason {
            CredentialMutationRejection::RoleMismatch {
                existing,
                requested,
            } => CredentialGrantFailure::RoleChangeRequiresExplicitOperation {
                current: existing,
                requested,
            },
            CredentialMutationRejection::LastOperator => CredentialGrantFailure::LastOperator,
        },
        CredentialMutationFailure::NotCommitted { failure } => render_failure(failure, false),
        CredentialMutationFailure::Committed { failure } => render_failure(failure, true),
    }
}

fn render_failure(error: RenderFailure, intent_committed: bool) -> CredentialGrantFailure {
    let message = failure_message(error.to_string());
    match error {
        RenderFailure::Prepare {
            failure: RenderPrepareFailure::Store { .. },
        } => CredentialGrantFailure::IntentStoreFailed {
            message,
            intent_committed,
        },
        RenderFailure::Prepare { .. } => CredentialGrantFailure::AuthorizationRenderFailed {
            message,
            intent_committed,
        },
        RenderFailure::Reload { .. } => CredentialGrantFailure::NatsReloadFailed {
            message,
            intent_committed,
        },
        RenderFailure::Verify { .. } => unreachable!("credential mutations do not verify a seed"),
    }
}

fn failure_message(message: String) -> FailureMessage {
    FailureMessage::try_new(message).expect("rendered errors are non-empty")
}
