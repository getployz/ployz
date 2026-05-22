use crate::error::PrimitiveFailure;
use crate::operation::identity::{PrincipalId, ScopeId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuthorityEpoch(u64);

impl AuthorityEpoch {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityContext {
    principal: PrincipalId,
    scope: ScopeId,
    epoch: AuthorityEpoch,
}

impl AuthorityContext {
    #[must_use]
    pub(crate) fn new(principal: PrincipalId, scope: ScopeId, epoch: AuthorityEpoch) -> Self {
        Self {
            principal,
            scope,
            epoch,
        }
    }

    #[must_use]
    pub fn principal(&self) -> &PrincipalId {
        &self.principal
    }

    #[must_use]
    pub fn scope(&self) -> &ScopeId {
        &self.scope
    }

    #[must_use]
    pub fn epoch(&self) -> AuthorityEpoch {
        self.epoch
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorityDecision {
    Allowed(AuthorityEpoch),
    Denied,
    Unknown,
}

impl AuthorityDecision {
    #[must_use]
    pub fn epoch(&self) -> Option<AuthorityEpoch> {
        let Self::Allowed(epoch) = self else {
            return None;
        };
        Some(*epoch)
    }
}

pub trait AuthorityPort {
    fn decide(
        &self,
        principal: &PrincipalId,
        scope: &ScopeId,
    ) -> Result<AuthorityDecision, PrimitiveFailure>;
}
