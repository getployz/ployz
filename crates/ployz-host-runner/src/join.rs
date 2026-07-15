//! Host Runner bootstrap join material handling.

use std::fs;
use std::path::{Path, PathBuf};

use crate::plan::{JoinMaterialError, JoinToken, RedactedJoinMaterial};
use ployz_core::dataplane::MachineEndpointSupernet;
use ployz_core::ids::MachineId;

pub const JOIN_MATERIAL_FILE: &str = "join-material";
pub const JOIN_NATS_CREDENTIALS_FILE: &str = "nats.creds";
pub const JOIN_TRUSTED_CA_FILE: &str = "ca.pem";
pub const JOIN_RECOVERY_KEY_FILE: &str = "ca-recovery.key";
pub const JOIN_CORE_SEEDS_FILE: &str = "core-seeds.key";
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
        "machine_id={}\ncluster_name={}\nnats_credentials={}\ntrusted_nats_ca_sha256={}\ndataplane_endpoint_supernet={}\n",
        material.machine_id.as_str(),
        material.cluster_name,
        "[redacted]",
        material.trusted_nats_ca_sha256,
        material.dataplane_endpoint_supernet.as_string(),
    )
    .into_bytes()
}

/// Recover this machine's id from the redacted join-material file written at join
/// (`machine_id=<id>` on the first line, see [`render_redacted_join_material`]).
/// Lets `core-promote` identify the machine it runs on without an operator flag.
#[must_use]
pub fn parse_machine_id_from_join_material(contents: &str) -> Option<MachineId> {
    let value = contents
        .lines()
        .find_map(|line| line.strip_prefix("machine_id="))?;
    MachineId::try_new(value).ok()
}

#[must_use]
pub fn parse_dataplane_endpoint_supernet_from_join_material(
    contents: &str,
) -> Option<MachineEndpointSupernet> {
    let value = contents
        .lines()
        .find_map(|line| line.strip_prefix("dataplane_endpoint_supernet="))?;
    MachineEndpointSupernet::try_new(value).ok()
}

#[cfg(test)]
mod tests {
    use super::parse_machine_id_from_join_material;

    #[test]
    fn parses_machine_id_from_redacted_material() {
        let contents = "machine_id=machine_7\ncluster_name=prod\nnats_credentials=[redacted]\ntrusted_nats_ca_sha256=abc\n";
        assert_eq!(
            parse_machine_id_from_join_material(contents).map(|id| id.as_str().to_owned()),
            Some("machine_7".to_owned())
        );
        assert!(parse_machine_id_from_join_material("cluster_name=prod\n").is_none());
    }
}
