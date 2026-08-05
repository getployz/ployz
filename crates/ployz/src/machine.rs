//! Machine command execution, mesh HTTP, and human presentation.

use std::fmt;

use ployz_core::corrosion::MachineTransport;
use ployz_core::founding::InitStorageChoice;
use ployz_core::ids::{ClusterId, MachineRowId};
use ployz_core::join::{
    JoinStorageChoice, MachineEndpointSetRefusal, MachineEndpointSetReply,
    MachineEndpointSetRequest,
};
use ployz_core::machine::MachineName;
use ployz_core::operation::FailureMessage;
use ployz_core::{LensCollection, LensSnapshot};
use ployz_core::{
    MACHINE_ENDPOINT_ROUTE_PREFIX, MACHINE_REMOVE_ROUTE, MachineRemoveRefusal, MachineRemoveReply,
    MachineRemoveRequest,
};
use ployz_host_runner::lifecycle::machine_join::{
    MachineJoinFailure, MachineJoinOutcomeKind, run_linux_machine_join,
};
use ployz_host_runner::lifecycle::machine_reset::run_linux_machine_reset;

use crate::JoinDoorClient;
use crate::commands::{
    MachineCommand, MachineEndpointSetCommand, MachineJoinCommand, MachineListCommand,
    MachineRemoveCommand,
};
use crate::mesh::http::JsonReply;
use crate::remote::{OperatorRemote, OperatorRemoteError};

pub async fn execute(command: MachineCommand) -> Result<String, MachineExecutionError> {
    match command {
        MachineCommand::List(command) => list(command).await,
        MachineCommand::Remove(command) => remove(command).await,
        MachineCommand::EndpointSet(command) => endpoint_set(command).await,
        MachineCommand::Join(command) => join(command).await,
        MachineCommand::Reset => reset(),
    }
}

async fn endpoint_set(command: MachineEndpointSetCommand) -> Result<String, MachineExecutionError> {
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let request = MachineEndpointSetRequest {
        machine_name: command.machine.clone(),
        endpoint: command.endpoint,
    };
    let reply = remote
        .request_json_with_refusal::<_, MachineEndpointSetReply, MachineEndpointSetRefusal>(
            hyper::Method::POST,
            MACHINE_ENDPOINT_ROUTE_PREFIX,
            Some(&request),
        )
        .await?;
    let reply = match reply {
        JsonReply::Success(reply) => reply,
        JsonReply::Refused(MachineEndpointSetRefusal::NotFound { machine_name }) => {
            return Err(MachineExecutionError::MachineNotFound {
                machine: machine_name.as_str().to_owned(),
            });
        }
        JsonReply::Refused(MachineEndpointSetRefusal::ProviderDoesNotUseWireguard {
            machine_id,
        }) => return Err(MachineExecutionError::EndpointProviderMismatch { machine_id }),
        JsonReply::Refused(MachineEndpointSetRefusal::EndpointPortZero { machine_name }) => {
            return Err(MachineExecutionError::EndpointPortZero {
                machine: machine_name.as_str().to_owned(),
            });
        }
    };
    if reply.machine.name != command.machine
        || !matches!(
            &reply.machine.transport,
            MachineTransport::Wireguard {
                endpoint: Some(endpoint),
                ..
            } if *endpoint == command.endpoint
        )
    {
        return Err(MachineExecutionError::EndpointReplyMismatch);
    }
    Ok(render_endpoint_set(&command.machine, command.endpoint))
}

async fn join(command: MachineJoinCommand) -> Result<String, MachineExecutionError> {
    let MachineJoinCommand {
        blob,
        storage,
        wireguard_endpoint,
    } = command;
    let storage_choice = match storage {
        InitStorageChoice::Automatic => JoinStorageChoice::Automatic,
        InitStorageChoice::Flag { mode } => JoinStorageChoice::Flag { mode },
    };
    let mut door = JoinDoorClient::default();
    let outcome =
        run_linux_machine_join(blob, storage_choice, wireguard_endpoint, &mut door).await?;
    Ok(render_machine_join(
        outcome.kind,
        &outcome.machine_name,
        &outcome.machine_id,
    ))
}

async fn remove(command: MachineRemoveCommand) -> Result<String, MachineExecutionError> {
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let request = MachineRemoveRequest {
        machine_name: command.machine.clone(),
        machine_id: command.machine_id.clone(),
    };
    let reply = remote
        .request_json_with_refusal::<_, MachineRemoveReply, MachineRemoveRefusal>(
            hyper::Method::POST,
            MACHINE_REMOVE_ROUTE,
            Some(&request),
        )
        .await?;
    let reply = match reply {
        JsonReply::Success(reply) => reply,
        JsonReply::Refused(MachineRemoveRefusal::NotFound { machine_name }) => {
            return Err(MachineExecutionError::MachineRemovalNotFound { machine_name });
        }
        JsonReply::Refused(MachineRemoveRefusal::Ambiguous {
            machine_name,
            machine_ids,
        }) => {
            return Err(MachineExecutionError::MachineRemovalAmbiguous(
                MachineRemovalAmbiguity {
                    machine_name,
                    machine_ids,
                },
            ));
        }
        JsonReply::Refused(MachineRemoveRefusal::IdMismatch {
            machine_name,
            machine_id,
        }) => {
            return Err(MachineExecutionError::MachineRemovalIdMismatch {
                machine_name,
                machine_id,
            });
        }
    };
    if !machine_removal_reply_matches_requested_identity(&reply, command.machine_id.as_ref()) {
        return Err(MachineExecutionError::MachineRemovalReplyMismatch);
    }
    Ok(render_machine_removal(&command.machine, &reply))
}

fn reset() -> Result<String, MachineExecutionError> {
    run_linux_machine_reset().map_err(|message| MachineExecutionError::Reset { message })?;
    Ok(render_machine_reset().to_owned())
}

async fn list(command: MachineListCommand) -> Result<String, MachineExecutionError> {
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let snapshot = remote.lens(LensCollection::Machines).await?;
    validate_snapshot_cluster(&snapshot, remote.cluster_id())?;
    render_machines(&snapshot).map_err(MachineExecutionError::from)
}
#[derive(Debug, thiserror::Error)]
pub enum MachineExecutionError {
    #[error(transparent)]
    Remote(#[from] OperatorRemoteError),
    #[error(transparent)]
    Snapshot(#[from] MachineSnapshotError),
    #[error("machine {machine} was not found")]
    MachineNotFound { machine: String },
    #[error("machine {machine_id} does not use builtin WireGuard; its endpoint cannot be set")]
    EndpointProviderMismatch { machine_id: MachineRowId },
    #[error("machine {machine} endpoint must use a nonzero WireGuard port")]
    EndpointPortZero { machine: String },
    #[error("cluster API endpoint-set reply did not match the requested machine and endpoint")]
    EndpointReplyMismatch,
    #[error(
        "machine {} is not in the roster; run `ployz machine ls` to inspect current machines",
        machine_name.as_str()
    )]
    MachineRemovalNotFound { machine_name: MachineName },
    #[error(transparent)]
    MachineRemovalAmbiguous(#[from] MachineRemovalAmbiguity),
    #[error(
        "machine {} does not match identity {machine_id}; omit `--id` to resolve the name again, or retry with a matching identity",
        machine_name.as_str()
    )]
    MachineRemovalIdMismatch {
        machine_name: MachineName,
        machine_id: MachineRowId,
    },
    #[error("cluster API machine-removal reply did not match the requested identity")]
    MachineRemovalReplyMismatch,
    #[error(transparent)]
    Join(#[from] MachineJoinFailure),
    #[error("{message}")]
    Reset { message: FailureMessage },
}

#[derive(Debug, thiserror::Error)]
pub enum MachineSnapshotError {
    #[error("cluster API returned {actual:?} instead of the machines lens")]
    WrongLens { actual: LensCollection },
    #[error("cluster API answered for cluster {actual}, expected {expected}")]
    WrongCluster {
        expected: ClusterId,
        actual: ClusterId,
    },
}

/// The actionable choices returned when a roster name has multiple identities.
#[derive(Debug)]
pub struct MachineRemovalAmbiguity {
    machine_name: MachineName,
    machine_ids: Vec<MachineRowId>,
}

impl fmt::Display for MachineRemovalAmbiguity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "machine {} is ambiguous; retry with one of:",
            self.machine_name.as_str()
        )?;
        for machine_id in &self.machine_ids {
            write!(
                formatter,
                "\n  ployz machine rm {} --id {}",
                self.machine_name.as_str(),
                machine_id.as_str()
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for MachineRemovalAmbiguity {}

pub(crate) fn render_machines(snapshot: &LensSnapshot) -> Result<String, MachineSnapshotError> {
    let LensSnapshot::Machines { rows, .. } = snapshot else {
        return Err(MachineSnapshotError::WrongLens {
            actual: snapshot_collection(snapshot),
        });
    };
    let mut rows = rows.iter().collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.document
            .name
            .as_str()
            .cmp(right.document.name.as_str())
            .then_with(|| left.id.as_str().cmp(right.id.as_str()))
    });

    let mut output = String::from("NAME\tCONTROL ADDRESS\n");
    for row in rows {
        let address = match &row.document.transport {
            MachineTransport::Wireguard {
                pubkey: _,
                addr_v6,
                endpoint: _,
                subnet_v4: _,
            } => addr_v6.to_string(),
            MachineTransport::Tailscale { ip, subnet_v4: _ } => ip.to_string(),
        };
        output.push_str(row.document.name.as_str());
        output.push('\t');
        output.push_str(&address);
        output.push('\n');
    }
    Ok(output)
}

fn validate_snapshot_cluster(
    snapshot: &LensSnapshot,
    expected: &ClusterId,
) -> Result<(), MachineSnapshotError> {
    let LensSnapshot::Machines { cluster, .. } = snapshot else {
        return Err(MachineSnapshotError::WrongLens {
            actual: snapshot_collection(snapshot),
        });
    };
    if cluster.cluster_id != *expected {
        return Err(MachineSnapshotError::WrongCluster {
            expected: expected.clone(),
            actual: cluster.cluster_id.clone(),
        });
    }
    Ok(())
}

const fn snapshot_collection(snapshot: &LensSnapshot) -> LensCollection {
    match snapshot {
        LensSnapshot::Machines { .. } => LensCollection::Machines,
        LensSnapshot::Services { .. } => LensCollection::Services,
        LensSnapshot::Containers { .. } => LensCollection::Containers,
        LensSnapshot::MachineStatus { .. } => LensCollection::MachineStatus,
        LensSnapshot::Operations { .. } => LensCollection::Operations,
    }
}

#[must_use]
pub fn render_endpoint_set(machine: &MachineName, endpoint: std::net::SocketAddr) -> String {
    format!("{} endpoint {}\n", machine.as_str(), endpoint)
}

#[must_use]
pub fn render_machine_removal(machine_name: &MachineName, reply: &MachineRemoveReply) -> String {
    match reply {
        MachineRemoveReply::Removed { machine_id } => format!(
            "Removed machine {} ({}).\n",
            machine_name.as_str(),
            machine_id.as_str()
        ),
        MachineRemoveReply::AlreadyAbsent { machine_id } => format!(
            "Machine {} ({}) was already absent.\n",
            machine_name.as_str(),
            machine_id.as_str()
        ),
    }
}

fn machine_removal_reply_matches_requested_identity(
    reply: &MachineRemoveReply,
    requested_machine_id: Option<&MachineRowId>,
) -> bool {
    match (reply, requested_machine_id) {
        (MachineRemoveReply::Removed { .. }, None) => true,
        (MachineRemoveReply::Removed { machine_id }, Some(requested_machine_id)) => {
            machine_id == requested_machine_id
        }
        (MachineRemoveReply::AlreadyAbsent { machine_id }, Some(requested_machine_id)) => {
            machine_id == requested_machine_id
        }
        (MachineRemoveReply::AlreadyAbsent { .. }, None) => false,
    }
}

#[must_use]
pub fn render_machine_join(
    outcome: MachineJoinOutcomeKind,
    machine_name: &MachineName,
    machine_id: &MachineRowId,
) -> String {
    let verb = match outcome {
        MachineJoinOutcomeKind::Joined => "Joined",
        MachineJoinOutcomeKind::Resumed => "Resumed",
        MachineJoinOutcomeKind::NoOp => "No-op",
    };
    format!(
        "{verb} machine {} ({}).\n",
        machine_name.as_str(),
        machine_id.as_str()
    )
}

#[must_use]
pub const fn render_machine_reset() -> &'static str {
    "Ployz state reset. Join with a fresh token.\n"
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const MACHINE_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
    const MACHINE_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
    const PEER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";

    fn machines_snapshot() -> LensSnapshot {
        serde_json::from_value(json!({
            "collection": "machines",
            "cluster": {
                "v": 1,
                "cluster_id": CLUSTER,
                "written_by": { "kind": "peer", "peer_id": PEER },
                "written_at": "2026-08-04T10:00:00Z",
                "name": "acme",
                "storage_default": "plain",
                "hostname_mode": { "mode": "disabled" },
                "prefix": "10.210.0.0/16",
                "provider": "builtin_wireguard",
                "acme_directory_url": "https://acme.example/directory",
                "acme_contact": null
            },
            "rows": [
                {
                    "id": MACHINE_B,
                    "document": {
                        "v": 1,
                        "cluster_id": CLUSTER,
                        "written_by": { "kind": "peer", "peer_id": PEER },
                        "written_at": "2026-08-04T10:00:00Z",
                        "name": "zeta",
                        "lifecycle": "active",
                        "transport": {
                            "kind": "tailscale",
                            "ip": "100.64.0.20",
                            "subnet_v4": "10.210.20.0/24"
                        },
                        "storage": { "mode": "plain", "reason": { "kind": "default" } }
                    }
                },
                {
                    "id": MACHINE_A,
                    "document": {
                        "v": 1,
                        "cluster_id": CLUSTER,
                        "written_by": { "kind": "peer", "peer_id": PEER },
                        "written_at": "2026-08-04T10:00:00Z",
                        "name": "alpha",
                        "lifecycle": "active",
                        "transport": {
                            "kind": "wireguard",
                            "pubkey": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
                            "addr_v6": "fd00::20",
                            "endpoint": null,
                            "subnet_v4": "10.210.21.0/24"
                        },
                        "storage": { "mode": "plain", "reason": { "kind": "default" } }
                    }
                }
            ]
        }))
        .expect("machines snapshot fixture")
    }

    #[test]
    fn machines_render_as_stable_human_first_table() {
        assert_eq!(
            render_machines(&machines_snapshot()).expect("machine table"),
            "NAME\tCONTROL ADDRESS\nalpha\tfd00::20\nzeta\t100.64.0.20\n"
        );
    }

    #[test]
    fn endpoint_set_confirmation_is_terse() {
        assert_eq!(
            render_endpoint_set(
                &MachineName::try_new("edge-b").expect("machine name"),
                "203.0.113.7:51820".parse().expect("endpoint"),
            ),
            "edge-b endpoint 203.0.113.7:51820\n"
        );
    }

    #[test]
    fn endpoint_set_client_uses_the_typed_machine_route() {
        let request = MachineEndpointSetRequest {
            machine_name: MachineName::try_new("edge-b").expect("machine name"),
            endpoint: "203.0.113.7:51820".parse().expect("endpoint"),
        };
        assert_eq!(MACHINE_ENDPOINT_ROUTE_PREFIX, "/machines/endpoint");
        assert_eq!(request.machine_name.as_str(), "edge-b");
        assert_eq!(request.endpoint.to_string(), "203.0.113.7:51820");
    }

    #[test]
    fn machine_removal_uses_the_typed_route_and_renders_terminal_outcomes() {
        let machine_name = MachineName::try_new("edge-b").expect("machine name");
        let machine_id = MachineRowId::try_new(MACHINE_A).expect("machine id");
        let request = MachineRemoveRequest {
            machine_name: machine_name.clone(),
            machine_id: Some(machine_id.clone()),
        };
        assert_eq!(MACHINE_REMOVE_ROUTE, "/machines/remove");
        assert_eq!(request.machine_name, machine_name);
        assert_eq!(request.machine_id, Some(machine_id.clone()));
        assert_eq!(
            render_machine_removal(
                &machine_name,
                &MachineRemoveReply::Removed {
                    machine_id: machine_id.clone(),
                },
            ),
            format!("Removed machine edge-b ({MACHINE_A}).\n")
        );
        assert_eq!(
            render_machine_removal(
                &machine_name,
                &MachineRemoveReply::AlreadyAbsent {
                    machine_id: machine_id.clone(),
                },
            ),
            format!("Machine edge-b ({MACHINE_A}) was already absent.\n")
        );
        assert!(machine_removal_reply_matches_requested_identity(
            &MachineRemoveReply::AlreadyAbsent {
                machine_id: machine_id.clone(),
            },
            Some(&machine_id),
        ));
        assert!(!machine_removal_reply_matches_requested_identity(
            &MachineRemoveReply::AlreadyAbsent { machine_id },
            None,
        ));
    }

    #[test]
    fn machine_removal_refusals_tell_the_operator_how_to_continue() {
        let machine_name = MachineName::try_new("edge-b").expect("machine name");
        let lower = MachineRowId::try_new(MACHINE_A).expect("machine id");
        let higher = MachineRowId::try_new(MACHINE_B).expect("machine id");
        let ambiguity = MachineExecutionError::MachineRemovalAmbiguous(MachineRemovalAmbiguity {
            machine_name: machine_name.clone(),
            machine_ids: vec![lower.clone(), higher.clone()],
        });
        assert_eq!(
            ambiguity.to_string(),
            format!(
                "machine edge-b is ambiguous; retry with one of:\n  ployz machine rm edge-b --id {MACHINE_A}\n  ployz machine rm edge-b --id {MACHINE_B}"
            )
        );
        assert_eq!(
            MachineExecutionError::MachineRemovalNotFound {
                machine_name: machine_name.clone(),
            }
            .to_string(),
            "machine edge-b is not in the roster; run `ployz machine ls` to inspect current machines"
        );
        assert_eq!(
            MachineExecutionError::MachineRemovalIdMismatch {
                machine_name,
                machine_id: lower,
            }
            .to_string(),
            format!(
                "machine edge-b does not match identity {MACHINE_A}; omit `--id` to resolve the name again, or retry with a matching identity"
            )
        );
    }

    #[test]
    fn join_outcomes_name_the_accepted_machine_identity() {
        let machine_name = MachineName::try_new("edge-b").expect("machine name");
        let machine_id = MachineRowId::try_new(MACHINE_A).expect("machine id");
        for (outcome, expected) in [
            (MachineJoinOutcomeKind::Joined, "Joined"),
            (MachineJoinOutcomeKind::Resumed, "Resumed"),
            (MachineJoinOutcomeKind::NoOp, "No-op"),
        ] {
            assert_eq!(
                render_machine_join(outcome, &machine_name, &machine_id),
                format!("{expected} machine edge-b ({MACHINE_A}).\n")
            );
        }
    }

    #[test]
    fn reset_confirmation_names_the_next_primitive() {
        assert_eq!(
            render_machine_reset(),
            "Ployz state reset. Join with a fresh token.\n"
        );
    }

    #[test]
    fn machine_snapshot_must_belong_to_the_selected_cluster() {
        let wrong = ClusterId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAZ").expect("cluster id");
        assert!(matches!(
            validate_snapshot_cluster(&machines_snapshot(), &wrong),
            Err(MachineSnapshotError::WrongCluster { expected, actual })
                if expected == wrong && actual.as_str() == CLUSTER
        ));
    }

    #[test]
    fn udp_timeout_presents_the_documented_ssh_fallback_without_running_it() {
        let endpoint = "203.0.113.7:51820".parse().expect("UDP endpoint");
        let api_target = "[fd00::20]:2020".parse().expect("API target");
        let error = MachineExecutionError::Remote(OperatorRemoteError::Connect(
            crate::mesh::MeshConnectError::UdpDialTimedOut {
                endpoint,
                target: api_target,
                founding_target: "root@machine.example".into(),
                docs_anchor: crate::mesh::UDP_BLOCKED_DOCS_ANCHOR,
                ssh_fallback: crate::mesh::render_ssh_fallback_command(
                    "root@machine.example",
                    api_target,
                )
                .into(),
            },
        ));
        assert_eq!(
            error.to_string(),
            "WireGuard dial to [fd00::20]:2020 through UDP endpoint 203.0.113.7:51820 timed out; UDP may be blocked. Fallback: ssh -N -L 127.0.0.1:2020:[fd00::20]:2020 root@machine.example. See docs/operations/cli-mesh-access.md#udp-blocked-networks"
        );
    }
}
