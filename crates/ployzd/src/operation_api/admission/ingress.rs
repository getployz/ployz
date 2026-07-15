use super::OperationControllers;
use crate::control::operation_evidence::{
    IngressConfigureOperationSubmission, SubmitOperationError,
};
use ployz_core::deploy::DeployRequest;
use ployz_core::ids::OperationId;
use ployz_core::ingress::IngressConfiguration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngressConfigureSubmitCommand {
    pub operation_id: OperationId,
    pub configuration: IngressConfiguration,
}

#[derive(Debug)]
pub enum IngressConfigureSubmitError {
    Busy { owner: OperationId },
    InvalidConfiguration { message: String },
    Unavailable { message: String },
    Submit(SubmitOperationError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum IngressClaim {
    Acquired,
    AlreadyOwned,
    Busy { owner: OperationId },
}

pub(super) fn deploy_requires_ingress(target: &DeployRequest) -> bool {
    target.services.iter().any(|service| {
        service.routes.iter().any(|route| {
            matches!(
                route.target,
                ployz_core::deploy::DeployRouteTarget::AutoHostname { .. }
            )
        })
    })
}

impl OperationControllers {
    pub async fn submit_ingress_configure(
        &self,
        command: IngressConfigureSubmitCommand,
        intent: &crate::control::intent::ingress_intent::IngressIntentStore,
    ) -> Result<
        crate::control::operation_evidence::AcceptedIngressConfigureSubmission,
        IngressConfigureSubmitError,
    > {
        let ingress_claim = self.claim_ingress(&command.operation_id).await;
        if let IngressClaim::Busy { owner } = &ingress_claim {
            return Err(IngressConfigureSubmitError::Busy {
                owner: owner.clone(),
            });
        }
        let acquired_ingress = matches!(ingress_claim, IngressClaim::Acquired);

        if let Err(error) = intent.validate_replace(&command.configuration).await {
            if acquired_ingress {
                self.release_ingress(&command.operation_id).await;
            }
            return Err(match error {
                crate::control::intent::ingress_intent::IngressIntentStoreError::InvalidConfiguration {
                    message,
                } => IngressConfigureSubmitError::InvalidConfiguration { message },
                crate::control::intent::ingress_intent::IngressIntentStoreError::Store(error) => {
                    IngressConfigureSubmitError::Unavailable {
                        message: error.to_string(),
                    }
                }
            });
        }

        let submitted = self
            .repository
            .submit_ingress_configure(IngressConfigureOperationSubmission {
                operation_id: command.operation_id.clone(),
                configuration: command.configuration,
            })
            .await;
        match submitted {
            Ok(accepted) => {
                if !accepted.should_start_execution && acquired_ingress {
                    self.release_ingress(&accepted.operation_id).await;
                }
                Ok(accepted)
            }
            Err(error) => {
                if acquired_ingress {
                    self.release_ingress(&command.operation_id).await;
                }
                Err(IngressConfigureSubmitError::Submit(error))
            }
        }
    }

    pub async fn release_ingress(&self, operation_id: &OperationId) {
        let mut lock = self.ingress_lock.lock().await;
        if lock.as_ref() == Some(operation_id) {
            *lock = None;
        }
    }

    pub(super) async fn claim_ingress(&self, operation_id: &OperationId) -> IngressClaim {
        let mut lock = self.ingress_lock.lock().await;
        match lock.as_ref() {
            Some(owner) if owner == operation_id => IngressClaim::AlreadyOwned,
            Some(owner) => IngressClaim::Busy {
                owner: owner.clone(),
            },
            None => {
                *lock = Some(operation_id.clone());
                IngressClaim::Acquired
            }
        }
    }
}
