//! Machine-local join-token file handling.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, PartialEq, Eq)]
pub struct JoinToken(String);

impl JoinToken {
    pub fn try_new(value: impl Into<String>) -> Result<Self, JoinTokenError> {
        let value = value.into();
        if value.is_empty() {
            return Err(JoinTokenError::Empty);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for JoinToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("JoinToken")
            .field(&"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum JoinTokenError {
    #[error("join token is empty")]
    Empty,
}

pub fn read_join_token_file(path: &Path) -> Result<JoinToken, JoinTokenFileError> {
    let contents = fs::read_to_string(path).map_err(|error| JoinTokenFileError::ReadFailed {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    JoinToken::try_new(contents.trim_end_matches(['\r', '\n']))
        .map_err(|JoinTokenError::Empty| JoinTokenFileError::EmptyToken)
}

pub fn remove_join_token_file(path: &Path) -> Result<(), JoinTokenFileError> {
    fs::remove_file(path).map_err(|error| JoinTokenFileError::ConsumeFailed {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JoinTokenFileError {
    #[error("failed to read join token file {}: {message}", path.display())]
    ReadFailed { path: PathBuf, message: String },
    #[error("join token file is empty")]
    EmptyToken,
    #[error("failed to consume join token file {}: {message}", path.display())]
    ConsumeFailed { path: PathBuf, message: String },
}

#[cfg(test)]
mod tests {
    use super::{JoinToken, JoinTokenFileError, read_join_token_file, remove_join_token_file};

    #[test]
    fn join_token_debug_is_redacted() {
        let token = JoinToken::try_new("join_secret_123").expect("token");

        let debug = format!("{token:?}");
        assert!(debug.contains("[redacted]"));
        assert!(!debug.contains(token.as_str()));
    }

    #[test]
    fn token_file_trims_only_line_endings_and_can_be_consumed() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("join-token");
        std::fs::write(&path, " join_secret_123 \r\n").expect("token writes");

        let token = read_join_token_file(&path).expect("token reads");
        assert_eq!(token.as_str(), " join_secret_123 ");

        remove_join_token_file(&path).expect("token consumes");
        assert!(!path.exists());
    }

    #[test]
    fn empty_token_file_is_rejected() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("join-token");
        std::fs::write(&path, "\n").expect("token writes");

        assert_eq!(
            read_join_token_file(&path),
            Err(JoinTokenFileError::EmptyToken)
        );
    }
}
