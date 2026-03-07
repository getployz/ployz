use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Error {
    #[error("{operation}: {message}")]
    Operation {
        operation: &'static str,
        message: String,
    },
}

impl Error {
    #[must_use]
    pub fn operation(operation: &'static str, message: impl Into<String>) -> Self {
        Self::Operation {
            operation,
            message: message.into(),
        }
    }
}
