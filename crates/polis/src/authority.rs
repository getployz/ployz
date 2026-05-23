//! Product-neutral authority proof metadata.

use std::marker::PhantomData;

use crate::identity::{PrincipalId, ScopeId};
use crate::{Error, Result};

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
    principal: PrincipalId,
    scope: ScopeId,
    epoch: GrantEpoch,
}

impl AuthorityContext {
    #[must_use]
    pub(crate) fn new(principal: PrincipalId, scope: ScopeId, epoch: GrantEpoch) -> Self {
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
    pub fn epoch(&self) -> GrantEpoch {
        self.epoch
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authorized<A = ()> {
    context: AuthorityContext,
    _actor: PhantomData<fn() -> A>,
}

impl<A> Authorized<A> {
    #[must_use]
    pub(crate) fn new(context: AuthorityContext) -> Self {
        Self {
            context,
            _actor: PhantomData,
        }
    }

    #[must_use]
    pub fn context(&self) -> &AuthorityContext {
        &self.context
    }

    #[must_use]
    pub fn into_context(self) -> AuthorityContext {
        self.context
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AuthorityOutcome {
    Allowed(GrantEpoch),
    Denied,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityDecision {
    outcome: AuthorityOutcome,
}

impl AuthorityDecision {
    #[must_use]
    pub const fn allowed(epoch: GrantEpoch) -> Self {
        Self {
            outcome: AuthorityOutcome::Allowed(epoch),
        }
    }

    #[must_use]
    pub const fn denied() -> Self {
        Self {
            outcome: AuthorityOutcome::Denied,
        }
    }

    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            outcome: AuthorityOutcome::Unknown,
        }
    }

    #[must_use]
    pub fn epoch(&self) -> Option<GrantEpoch> {
        let AuthorityOutcome::Allowed(epoch) = self.outcome else {
            return None;
        };
        Some(epoch)
    }
}

pub trait Authority {
    fn decide(&self, principal: &PrincipalId, scope: &ScopeId) -> AuthorityDecision;
}

pub struct AuthorityService<A> {
    authority: A,
}

impl<A> AuthorityService<A> {
    #[must_use]
    pub fn new(authority: A) -> Self {
        Self { authority }
    }
}

impl<A> AuthorityService<A>
where
    A: Authority,
{
    pub fn authorize<T>(&self, principal: PrincipalId, scope: ScopeId) -> Result<Authorized<T>> {
        match self.authority.decide(&principal, &scope).epoch() {
            Some(epoch) => Ok(Authorized::new(AuthorityContext::new(
                principal, scope, epoch,
            ))),
            None => Err(Error::Unauthorized),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denied_authority_has_no_context() {
        assert_eq!(AuthorityDecision::denied().epoch(), None);
    }

    #[test]
    fn denied_authority_cannot_produce_authorized_value() {
        enum DeployActor {}
        struct DeniedAuthority;

        impl Authority for DeniedAuthority {
            fn decide(&self, _principal: &PrincipalId, _scope: &ScopeId) -> AuthorityDecision {
                AuthorityDecision::denied()
            }
        }

        assert!(
            AuthorityService::new(DeniedAuthority)
                .authorize::<DeployActor>(
                    PrincipalId::parse("node-a").expect("principal"),
                    ScopeId::parse("cluster").expect("scope"),
                )
                .is_err()
        );
    }

    #[test]
    fn authority_service_issues_authorized_value_from_allowed_decision() {
        enum DeployActor {}
        struct AllowAuthority;

        impl Authority for AllowAuthority {
            fn decide(&self, _principal: &PrincipalId, _scope: &ScopeId) -> AuthorityDecision {
                AuthorityDecision::allowed(GrantEpoch::new(7))
            }
        }

        let authorized = AuthorityService::new(AllowAuthority)
            .authorize::<DeployActor>(
                PrincipalId::parse("node-a").expect("principal"),
                ScopeId::parse("cluster").expect("scope"),
            )
            .expect("authorized");

        assert_eq!(authorized.context().epoch(), GrantEpoch::new(7));
    }
}
