use std::time::SystemTime;

use crate::operation::authority::AuthorityContext;
use crate::operation::claims::SubmittedFenceToken;
use crate::operation::identity::{IdempotencyKey, OperationId, PrincipalId, ScopeId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptIssue {
    pub operation: OperationId,
    pub idempotency: IdempotencyKey,
    pub principal: PrincipalId,
    pub scope: ScopeId,
    pub deadline: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationContext {
    operation: OperationId,
    idempotency: IdempotencyKey,
    authority: AuthorityContext,
    submitted_fence: Option<SubmittedFenceToken>,
    deadline: SystemTime,
}

impl MutationContext {
    #[must_use]
    pub(crate) fn new(
        operation: OperationId,
        idempotency: IdempotencyKey,
        authority: AuthorityContext,
        submitted_fence: Option<SubmittedFenceToken>,
        deadline: SystemTime,
    ) -> Self {
        Self {
            operation,
            idempotency,
            authority,
            submitted_fence,
            deadline,
        }
    }

    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn test_new(
        command: AttemptIssue,
        authority_epoch: crate::operation::authority::AuthorityEpoch,
        submitted_fence: Option<SubmittedFenceToken>,
    ) -> Self {
        Self::test_authorized(
            command.operation,
            command.idempotency,
            command.principal,
            command.scope,
            authority_epoch,
            submitted_fence,
            command.deadline,
        )
    }

    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn test_authorized(
        operation: OperationId,
        idempotency: IdempotencyKey,
        principal: PrincipalId,
        scope: ScopeId,
        authority_epoch: crate::operation::authority::AuthorityEpoch,
        submitted_fence: Option<SubmittedFenceToken>,
        deadline: SystemTime,
    ) -> Self {
        Self::new(
            operation,
            idempotency,
            AuthorityContext::new(principal, scope, authority_epoch),
            submitted_fence,
            deadline,
        )
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
    pub fn authority(&self) -> &AuthorityContext {
        &self.authority
    }

    #[must_use]
    pub fn submitted_fence(&self) -> Option<&SubmittedFenceToken> {
        self.submitted_fence.as_ref()
    }

    #[must_use]
    pub fn deadline(&self) -> SystemTime {
        self.deadline
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
pub(crate) struct AttemptSpec {
    command: &'static str,
    schema: &'static str,
    fields: Vec<AttemptField>,
    resources: Vec<String>,
    submitted_fence: Option<SubmittedFenceToken>,
}

#[cfg(test)]
impl AttemptSpec {
    #[must_use]
    pub(crate) fn new(command: &'static str, schema: &'static str) -> Self {
        Self {
            command,
            schema,
            fields: Vec::new(),
            resources: Vec::new(),
            submitted_fence: None,
        }
    }

    #[must_use]
    pub(crate) fn field(mut self, key: &'static str, value: impl AsRef<str>) -> Self {
        self.fields.push(AttemptField {
            key,
            value: AttemptFieldValue::String(value.as_ref().to_owned()),
        });
        self
    }

    #[must_use]
    pub(crate) fn field_u64(mut self, key: &'static str, value: u64) -> Self {
        self.fields.push(AttemptField {
            key,
            value: AttemptFieldValue::U64(value),
        });
        self
    }

    #[must_use]
    pub(crate) fn resource(mut self, resource: impl Into<String>) -> Self {
        self.resources.push(resource.into());
        self
    }

    #[must_use]
    pub(crate) fn submitted_fence(mut self, fence: SubmittedFenceToken) -> Self {
        self.submitted_fence = Some(fence);
        self
    }

    pub(crate) fn into_fingerprint_builder(
        self,
        actor: polis::PrincipalId,
        scope: polis::ScopeId,
        authority_epoch: polis::GrantEpoch,
    ) -> polis::Result<(
        polis::RequestFingerprintBuilder,
        Option<SubmittedFenceToken>,
    )> {
        let command = polis::CommandKind::parse(self.command)?;
        let mut builder =
            polis::RequestFingerprint::builder(actor, scope, command, self.schema, authority_epoch);
        for field in self.fields {
            builder = match field.value {
                AttemptFieldValue::String(value) => builder.field(field.key, value),
                AttemptFieldValue::U64(value) => builder.field_u64(field.key, value),
            };
        }
        for resource in self.resources {
            builder = builder.resource(polis::FingerprintedResource::parse(resource)?);
        }
        if let Some(fence) = &self.submitted_fence {
            builder =
                builder.submitted_fence(fence.fingerprint().map_err(|_| polis::Error::StaleFence)?);
        }
        Ok((builder, self.submitted_fence))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
struct AttemptField {
    key: &'static str,
    value: AttemptFieldValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
enum AttemptFieldValue {
    String(String),
    U64(u64),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attempt_spec_rejects_empty_command_at_fingerprint_boundary() {
        let result = AttemptSpec::new(" ", "test.v1")
            .field("field", "value")
            .resource("resource:test")
            .into_fingerprint_builder(
                polis::PrincipalId::parse("node-a").expect("principal"),
                polis::ScopeId::parse("cluster").expect("scope"),
                polis::GrantEpoch::new(7),
            );

        assert!(matches!(result, Err(polis::Error::MalformedPayload)));
    }
}
