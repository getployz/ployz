//! Durable machine-local substrate guidance owned by this machine's keeper.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use ployz_core::install::HostPortAssurance;
use serde::{Deserialize, Serialize};

use crate::execution::{FileMode, write_durable_file};

pub const ASSIGNED_SUBSTRATE_FILE: &str = "assigned-substrate.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssignedSubstrateState {
    pub assignments: BTreeSet<SubstrateAssignment>,
    pub host_ports: HostPortAssurance,
}

impl AssignedSubstrateState {
    #[must_use]
    pub fn required_host_ports(&self) -> Vec<AssignedHostPort> {
        let mut ports = Vec::new();
        if self.assignments.contains(&SubstrateAssignment::NatsServer) {
            ports.push(AssignedHostPort::tcp(4222));
        }
        if self.assignments.contains(&SubstrateAssignment::Dataplane) {
            ports.push(AssignedHostPort::udp(51820));
        }
        if self.assignments.contains(&SubstrateAssignment::Gateway) {
            ports.extend([AssignedHostPort::tcp(80), AssignedHostPort::tcp(443)]);
        }
        ports.sort_by_key(|port| (port.port, port.protocol));
        ports
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubstrateAssignment {
    NatsServer,
    Dataplane,
    Gateway,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssignedHostPort {
    pub port: u16,
    pub protocol: HostPortProtocol,
}

impl AssignedHostPort {
    const fn tcp(port: u16) -> Self {
        Self {
            port,
            protocol: HostPortProtocol::Tcp,
        }
    }

    const fn udp(port: u16) -> Self {
        Self {
            port,
            protocol: HostPortProtocol::Udp,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPortProtocol {
    Tcp,
    Udp,
}

#[derive(Debug, thiserror::Error)]
pub enum AssignedSubstrateStateError {
    #[error("assigned substrate state is missing at {path}")]
    Missing { path: PathBuf },
    #[error("failed to read assigned substrate state {path}: {message}")]
    Read { path: PathBuf, message: String },
    #[error("failed to parse assigned substrate state {path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("failed to serialize assigned substrate state: {message}")]
    Serialize { message: String },
    #[error("failed to write assigned substrate state {path}: {message}")]
    Write { path: PathBuf, message: String },
}

pub fn load_assigned_substrate_state(
    state_dir: &Path,
) -> Result<AssignedSubstrateState, AssignedSubstrateStateError> {
    let path = state_dir.join(ASSIGNED_SUBSTRATE_FILE);
    let bytes = fs::read(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            AssignedSubstrateStateError::Missing { path: path.clone() }
        } else {
            AssignedSubstrateStateError::Read {
                path: path.clone(),
                message: error.to_string(),
            }
        }
    })?;
    serde_json::from_slice(&bytes).map_err(|error| AssignedSubstrateStateError::Parse {
        path,
        message: error.to_string(),
    })
}

pub fn write_assigned_substrate_state(
    state_dir: &Path,
    state: &AssignedSubstrateState,
) -> Result<(), AssignedSubstrateStateError> {
    let bytes = serde_json::to_vec_pretty(state).map_err(|error| {
        AssignedSubstrateStateError::Serialize {
            message: error.to_string(),
        }
    })?;
    write_durable_file(
        state_dir,
        ASSIGNED_SUBSTRATE_FILE,
        FileMode::Secret0600,
        &bytes,
    )
    .map_err(|error| AssignedSubstrateStateError::Write {
        path: state_dir.join(ASSIGNED_SUBSTRATE_FILE),
        message: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{
        AssignedHostPort, AssignedSubstrateState, HostPortProtocol, SubstrateAssignment,
        load_assigned_substrate_state, write_assigned_substrate_state,
    };

    fn assigned_state() -> AssignedSubstrateState {
        AssignedSubstrateState {
            assignments: [
                SubstrateAssignment::NatsServer,
                SubstrateAssignment::Dataplane,
            ]
            .into(),
            host_ports: ployz_core::install::HostPortAssurance::External,
        }
    }

    #[test]
    fn assigned_substrate_state_round_trips_through_durable_store() {
        let state_dir = tempdir().expect("state dir creates");
        let expected = assigned_state();

        write_assigned_substrate_state(state_dir.path(), &expected).expect("state writes");

        assert_eq!(
            load_assigned_substrate_state(state_dir.path()).expect("state loads"),
            expected
        );
    }

    #[test]
    fn assigned_substrate_state_uses_public_json_contract() {
        let json = serde_json::to_value(assigned_state()).expect("state serializes");

        assert_eq!(
            json,
            serde_json::json!({
                "assignments": ["nats_server", "dataplane"],
                "host_ports": "external"
            })
        );
    }

    #[test]
    fn assigned_substrate_state_reports_missing_file() {
        let state_dir = tempdir().expect("state dir creates");

        let error = load_assigned_substrate_state(state_dir.path()).expect_err("state is missing");

        assert!(matches!(
            error,
            super::AssignedSubstrateStateError::Missing { .. }
        ));
    }

    #[test]
    fn assigned_substrate_state_rejects_corrupt_file() {
        let state_dir = tempdir().expect("state dir creates");
        fs::write(
            state_dir.path().join("assigned-substrate.json"),
            br#"{"assignments":[],"host_ports":"somehow"}"#,
        )
        .expect("corrupt state writes");

        let error = load_assigned_substrate_state(state_dir.path()).expect_err("state is corrupt");

        assert!(matches!(
            error,
            super::AssignedSubstrateStateError::Parse { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn assigned_substrate_state_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let state_dir = tempdir().expect("state dir creates");
        write_assigned_substrate_state(state_dir.path(), &assigned_state()).expect("state writes");

        let mode = fs::metadata(state_dir.path().join("assigned-substrate.json"))
            .expect("state metadata loads")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn assignments_project_exact_sorted_host_ports() {
        let state = AssignedSubstrateState {
            assignments: [
                SubstrateAssignment::Gateway,
                SubstrateAssignment::Dataplane,
                SubstrateAssignment::NatsServer,
            ]
            .into(),
            host_ports: ployz_core::install::HostPortAssurance::Keeper,
        };

        assert_eq!(
            state.required_host_ports(),
            vec![
                AssignedHostPort {
                    port: 80,
                    protocol: HostPortProtocol::Tcp
                },
                AssignedHostPort {
                    port: 443,
                    protocol: HostPortProtocol::Tcp
                },
                AssignedHostPort {
                    port: 4222,
                    protocol: HostPortProtocol::Tcp
                },
                AssignedHostPort {
                    port: 51820,
                    protocol: HostPortProtocol::Udp
                },
            ]
        );
    }
}
