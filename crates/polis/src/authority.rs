//! Product-neutral authority proof metadata.

use crate::identity::{PrincipalId, ScopeId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GrantEpoch(u64);

impl GrantEpoch {
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
    pub principal: PrincipalId,
    pub scope: ScopeId,
    pub epoch: GrantEpoch,
}

impl AuthorityContext {
    #[must_use]
    pub fn new(principal: PrincipalId, scope: ScopeId, epoch: GrantEpoch) -> Self {
        Self {
            principal,
            scope,
            epoch,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorityDecision {
    Allowed(AuthorityContext),
    Denied,
    Unknown,
}

impl AuthorityDecision {
    #[must_use]
    pub fn context(&self) -> Option<&AuthorityContext> {
        let Self::Allowed(context) = self else {
            return None;
        };
        Some(context)
    }
}

pub trait Authority {
    fn decide(&self, principal: &PrincipalId, scope: &ScopeId) -> AuthorityDecision;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denied_authority_has_no_context() {
        assert_eq!(AuthorityDecision::Denied.context(), None);
    }
}
