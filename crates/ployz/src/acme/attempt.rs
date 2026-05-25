//! ACME certificate issuance attempt state.

use std::future::Future;
use std::time::SystemTime;

use crate::error::{CertificateFailure, PrimitiveFailure};
use crate::operation::{IdempotencyKey, MutationContext, OperationId, ScopeId};

use super::{CertificateIssueRequest, Hostname};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateAttemptRequest {
    scope: ScopeId,
    operation: OperationId,
    idempotency: IdempotencyKey,
    hostname: Hostname,
    certificate_deadline: SystemTime,
    owner_deadline: SystemTime,
}

impl CertificateAttemptRequest {
    pub(crate) fn for_issue(context: &MutationContext, request: &CertificateIssueRequest) -> Self {
        Self {
            scope: context.authority().scope().clone(),
            operation: context.operation().clone(),
            idempotency: context.idempotency().clone(),
            hostname: request.binding.hostname.clone(),
            certificate_deadline: request.deadline.expires_at,
            owner_deadline: context.deadline(),
        }
    }

    #[must_use]
    pub fn scope(&self) -> &ScopeId {
        &self.scope
    }

    #[must_use]
    pub fn operation(&self) -> &OperationId {
        &self.operation
    }

    #[must_use]
    pub fn idempotency(&self) -> &IdempotencyKey {
        &self.idempotency
    }

    #[must_use]
    pub fn hostname(&self) -> &Hostname {
        &self.hostname
    }

    #[must_use]
    pub fn certificate_deadline(&self) -> SystemTime {
        self.certificate_deadline
    }

    #[must_use]
    pub fn owner_deadline(&self) -> SystemTime {
        self.owner_deadline
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertificateAttemptStart {
    Started,
    Replayed(CertificateAttemptReplay),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertificateAttemptReplay {
    Open {
        operation: OperationId,
        owner_deadline: SystemTime,
    },
    Succeeded {
        operation: OperationId,
    },
    Failed {
        operation: OperationId,
        failure: CertificateFailure,
    },
    Interrupted {
        operation: OperationId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertificateAttemptTerminal {
    Succeeded,
    Failed(CertificateFailure),
    Interrupted,
}

pub trait CertificateAttemptStore {
    fn begin<'a>(
        &'a self,
        request: &'a CertificateAttemptRequest,
    ) -> impl Future<Output = Result<CertificateAttemptStart, PrimitiveFailure>> + 'a;

    fn finish<'a>(
        &'a self,
        request: &'a CertificateAttemptRequest,
        terminal: CertificateAttemptTerminal,
    ) -> impl Future<Output = Result<(), PrimitiveFailure>> + 'a;
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;
    use crate::acme::{CertificateDeadline, Hostname, HttpsBinding};
    use crate::operation::{
        AuthorityContext, AuthorityEpoch, IdempotencyKey, OperationId, PrincipalId, ScopeId,
    };

    #[test]
    fn issue_attempt_identity_preserves_parent_and_certificate_dimensions() {
        let request = CertificateAttemptRequest::for_issue(
            &context("deploy:parent", "idem:parent"),
            &issue_request("app.example.com"),
        );

        assert_eq!(request.operation().as_str(), "deploy:parent");
        assert_eq!(request.idempotency().as_str(), "idem:parent");
        assert_eq!(request.hostname().as_str(), "app.example.com");
        assert_eq!(request.certificate_deadline(), issue_deadline());
    }

    fn context(operation: &str, idempotency: &str) -> MutationContext {
        MutationContext::new(
            OperationId::parse(operation).expect("operation"),
            IdempotencyKey::parse(idempotency).expect("idempotency"),
            AuthorityContext::new(
                PrincipalId::parse("node-a").expect("principal"),
                ScopeId::parse("cluster").expect("scope"),
                AuthorityEpoch::new(7),
            ),
            None,
            UNIX_EPOCH + Duration::from_secs(60),
        )
    }

    fn issue_request(hostname: &str) -> CertificateIssueRequest {
        CertificateIssueRequest::new(
            HttpsBinding::new(Hostname::parse(hostname).expect("hostname")),
            CertificateDeadline {
                expires_at: issue_deadline(),
            },
        )
    }

    fn issue_deadline() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(120)
    }
}
