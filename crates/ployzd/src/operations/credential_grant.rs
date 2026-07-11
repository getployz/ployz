use crate::adapters::nats_authorization::{
    CredentialMutationChange, CredentialMutationFailure, CredentialMutationRejection,
    NatsAuthorizationHandle, RenderFailure, RenderPrepareFailure,
};
use crate::operation_api::admission::OperationControllers;
use crate::operations::log::AcceptedCredentialGrantSubmission;
use crate::tasks::TaskRegistry;
use ployz_core::ops::{
    CredentialGrantAction, CredentialGrantFailure, CredentialGrantTransition, FailureMessage,
};
use ployz_core::subjects::INTENT_CHANGED;

#[derive(Debug, Clone)]
pub struct CredentialGrantOperation {
    controllers: OperationControllers,
    authorization: NatsAuthorizationHandle,
    client: async_nats::Client,
    task_registry: TaskRegistry,
}

impl CredentialGrantOperation {
    #[must_use]
    pub const fn new(
        controllers: OperationControllers,
        authorization: NatsAuthorizationHandle,
        client: async_nats::Client,
        task_registry: TaskRegistry,
    ) -> Self {
        Self {
            controllers,
            authorization,
            client,
            task_registry,
        }
    }

    pub fn start(&self, accepted: AcceptedCredentialGrantSubmission) {
        if !accepted.should_start_execution {
            return;
        }
        let worker = self.clone();
        self.task_registry.spawn(async move {
            worker.run(accepted).await;
        });
    }

    pub async fn list_credentials(
        &self,
    ) -> Result<Vec<ployz_core::nats_config::CredentialGrant>, RenderFailure> {
        self.authorization.list_credentials().await
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
