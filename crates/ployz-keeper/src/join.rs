//! Keeper bootstrap join material handling.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::steps::{JoinMaterialError, JoinToken, RedactedJoinMaterial};

pub const JOIN_MATERIAL_FILE: &str = "join-material";

pub fn consume_join_token_file(path: &Path) -> Result<JoinToken, JoinTokenFileError> {
    let contents = fs::read_to_string(path).map_err(|error| JoinTokenFileError::ReadFailed {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    let token =
        JoinToken::try_new(contents.trim_end_matches(['\r', '\n'])).map_err(
            |error| match error {
                JoinMaterialError::EmptyJoinToken => JoinTokenFileError::EmptyToken,
                JoinMaterialError::EmptyClusterName => {
                    unreachable!("token validation cannot inspect cluster names")
                }
                JoinMaterialError::InvalidClusterName { .. } => {
                    unreachable!("token validation cannot inspect cluster names")
                }
            },
        )?;

    fs::remove_file(path).map_err(|error| JoinTokenFileError::ConsumeFailed {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;

    Ok(token)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinTokenFileError {
    ReadFailed { path: PathBuf, message: String },
    EmptyToken,
    ConsumeFailed { path: PathBuf, message: String },
}

impl fmt::Display for JoinTokenFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadFailed { path, message } => {
                write!(
                    formatter,
                    "failed to read join token file {}: {message}",
                    path.display()
                )
            }
            Self::EmptyToken => formatter.write_str("join token file is empty"),
            Self::ConsumeFailed { path, message } => {
                write!(
                    formatter,
                    "failed to consume join token file {}: {message}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for JoinTokenFileError {}

#[must_use]
pub fn render_redacted_join_material(material: &RedactedJoinMaterial) -> Vec<u8> {
    format!(
        "node_id={}\ncluster_name={}\n",
        material.node_id.as_str(),
        material.cluster_name
    )
    .into_bytes()
}
