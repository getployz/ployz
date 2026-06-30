//! Keeper bootstrap join material handling.

use std::fs;
use std::path::{Path, PathBuf};

use crate::steps::{JoinMaterialError, JoinToken, RedactedJoinMaterial};

pub const JOIN_MATERIAL_FILE: &str = "join-material";
pub const JOIN_NATS_CREDENTIALS_FILE: &str = "nats.creds";
pub const JOIN_TRUSTED_CA_FILE: &str = "ca.pem";
pub const JOIN_MATERIAL_DIR: &str = "join-material.d";

pub fn read_join_token_file(path: &Path) -> Result<JoinToken, JoinTokenFileError> {
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
                JoinMaterialError::EmptyJoinMaterialValue { .. }
                | JoinMaterialError::InvalidJoinMaterialValue { .. } => {
                    unreachable!("token validation cannot inspect machine join material")
                }
            },
        )?;

    Ok(token)
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

#[must_use]
pub fn render_redacted_join_material(material: &RedactedJoinMaterial) -> Vec<u8> {
    format!(
        "machine_id={}\ncluster_name={}\nnats_credentials={}\ntrusted_nats_ca_sha256={}\n",
        material.machine_id.as_str(),
        material.cluster_name,
        "[redacted]",
        material.trusted_nats_ca_sha256,
    )
    .into_bytes()
}
