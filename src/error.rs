use thiserror::Error;

pub type PortResult<T> = std::result::Result<T, PortError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PortError {
    #[error("{operation}: {message}")]
    Operation {
        operation: &'static str,
        message: String,
    },
}

impl PortError {
    #[must_use]
    pub fn operation(operation: &'static str, message: impl Into<String>) -> Self {
        Self::Operation {
            operation,
            message: message.into(),
        }
    }
}
