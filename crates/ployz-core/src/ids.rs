//! Typed identifiers used in storage keys, subjects, operations, and routing.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubjectTokenError {
    Empty,
    InvalidCharacter { value: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationId(SubjectToken);

impl OperationId {
    pub fn try_new(value: impl Into<String>) -> Result<Self, SubjectTokenError> {
        Ok(Self(SubjectToken::try_new(value)?))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeId(SubjectToken);

impl NodeId {
    pub fn try_new(value: impl Into<String>) -> Result<Self, SubjectTokenError> {
        Ok(Self(SubjectToken::try_new(value)?))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubjectToken(String);

impl SubjectToken {
    pub fn try_new(value: impl Into<String>) -> Result<Self, SubjectTokenError> {
        let value = value.into();
        if value.is_empty() {
            return Err(SubjectTokenError::Empty);
        }

        if value
            .chars()
            .any(|character| matches!(character, '.' | '*' | '>' | ' ' | '\t' | '\n' | '\r'))
        {
            return Err(SubjectTokenError::InvalidCharacter { value });
        }

        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
