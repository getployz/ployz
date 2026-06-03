use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;

use crate::error::PrimitiveFailure;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrincipalId(String);

impl PrincipalId {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeId(String);

impl ScopeId {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationId(String);

impl OperationId {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationKind(String);

impl OperationKind {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationPayloadFingerprint(String);

impl OperationPayloadFingerprint {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationOwner(String);

impl OperationOwner {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceId(String);

impl ResourceId {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug)]
pub struct TypedResourceId<R> {
    value: String,
    _resource: PhantomData<fn() -> R>,
}

impl<R> TypedResourceId<R> {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(PrimitiveFailure::MalformedPayload);
        }
        Ok(Self {
            value,
            _resource: PhantomData,
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

impl<R> Clone for TypedResourceId<R> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            _resource: PhantomData,
        }
    }
}

impl<R> PartialEq for TypedResourceId<R> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<R> Eq for TypedResourceId<R> {}

impl<R> PartialOrd for TypedResourceId<R> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<R> Ord for TypedResourceId<R> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.value.cmp(&other.value)
    }
}

impl<R> Hash for TypedResourceId<R> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.value.hash(state);
    }
}

pub(crate) fn parse_non_empty<T>(
    value: impl Into<String>,
    build: impl FnOnce(String) -> T,
) -> Result<T, PrimitiveFailure> {
    let value = value.into();
    if value.trim().is_empty() {
        return Err(PrimitiveFailure::MalformedPayload);
    }
    Ok(build(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_operation_id_is_malformed_payload() {
        assert_eq!(
            OperationId::parse(""),
            Err(PrimitiveFailure::MalformedPayload)
        );
    }
}
