use std::time::{SystemTime, UNIX_EPOCH};

use crate::operation::authority::AuthorityContext;
use crate::operation::claims::SubmittedFenceToken;
use crate::operation::identity::{
    IdempotencyKey, OperationId, PrincipalId, ScopeId, parse_non_empty,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommandKind(String);

impl CommandKind {
    pub fn parse(value: impl Into<String>) -> Result<Self, crate::error::PrimitiveFailure> {
        parse_non_empty(value, Self)
    }

    pub(crate) fn into_polis(self) -> polis::Result<polis::CommandKind> {
        polis::CommandKind::parse(self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FingerprintedResource(String);

impl FingerprintedResource {
    pub fn parse(value: impl Into<String>) -> Result<Self, crate::error::PrimitiveFailure> {
        parse_non_empty(value, Self)
    }

    pub(crate) fn into_polis(self) -> polis::Result<polis::FingerprintedResource> {
        polis::FingerprintedResource::parse(self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandIssue {
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
        command: CommandIssue,
        authority_epoch: crate::operation::authority::AuthorityEpoch,
        submitted_fence: Option<SubmittedFenceToken>,
    ) -> Self {
        Self::new(
            command.operation,
            command.idempotency,
            AuthorityContext::new(command.principal, command.scope, authority_epoch),
            submitted_fence,
            command.deadline,
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
pub(crate) struct MutationIntent {
    pub operation: OperationId,
    pub idempotency: IdempotencyKey,
    pub principal: PrincipalId,
    pub scope: ScopeId,
    pub command: CommandKind,
    pub payload_hash: Vec<u8>,
    pub resources: Vec<FingerprintedResource>,
    pub submitted_fence: Option<SubmittedFenceToken>,
    pub deadline: SystemTime,
}

pub(crate) struct CommandPayload {
    bytes: Vec<u8>,
}

impl CommandPayload {
    #[must_use]
    pub(crate) fn new(version: &'static str) -> Self {
        let mut payload = Self { bytes: Vec::new() };
        payload.field("version", version);
        payload
    }

    pub(crate) fn field(&mut self, key: &'static str, value: impl AsRef<str>) {
        let value = value.as_ref();
        self.bytes.extend_from_slice(key.as_bytes());
        self.bytes.push(b'=');
        self.bytes
            .extend_from_slice(value.len().to_string().as_bytes());
        self.bytes.push(b':');
        self.bytes.extend_from_slice(value.as_bytes());
        self.bytes.push(b';');
    }

    pub(crate) fn field_u64(&mut self, key: &'static str, value: u64) {
        self.field(key, value.to_string());
    }

    pub(crate) fn field_time(&mut self, key: &'static str, value: SystemTime) {
        match value.duration_since(UNIX_EPOCH) {
            Ok(duration) => self.field(
                key,
                format!("after:{}:{}", duration.as_secs(), duration.subsec_nanos()),
            ),
            Err(error) => {
                let duration = error.duration();
                self.field(
                    key,
                    format!("before:{}:{}", duration.as_secs(), duration.subsec_nanos()),
                );
            }
        }
    }

    #[must_use]
    pub(crate) fn finish_hash(self) -> Vec<u8> {
        const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const PRIME: u64 = 0x0000_0100_0000_01b3;

        let mut hash = OFFSET;
        for byte in self.bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(PRIME);
        }
        hash.to_be_bytes().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::PrimitiveFailure;

    #[test]
    fn empty_command_kind_is_malformed_payload() {
        assert_eq!(
            CommandKind::parse(" "),
            Err(PrimitiveFailure::MalformedPayload)
        );
    }

    #[test]
    fn empty_fingerprinted_resource_is_malformed_payload() {
        assert_eq!(
            FingerprintedResource::parse(" "),
            Err(PrimitiveFailure::MalformedPayload)
        );
    }

    #[test]
    fn command_payload_hash_is_length_prefixed_and_stable() {
        let mut first = CommandPayload::new("test.v1");
        first.field("field", "a;b");

        let mut second = CommandPayload::new("test.v1");
        second.field("field", "a");
        second.field("b", "");

        assert_ne!(first.finish_hash(), second.finish_hash());
    }
}
