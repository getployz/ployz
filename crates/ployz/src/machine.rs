//! Machine command execution, mesh HTTP, and human presentation.

use ployz_core::MACHINE_ENDPOINT_ROUTE_PREFIX;
use ployz_core::corrosion::MachineTransport;
use ployz_core::founding::InitStorageChoice;
use ployz_core::ids::{ClusterId, MachineRowId};
use ployz_core::join::{
    JoinStorageChoice, MachineEndpointSetRefusal, MachineEndpointSetReply,
    MachineEndpointSetRequest,
};
use ployz_core::machine::MachineName;
use ployz_core::{LensCollection, LensSnapshot};
use ployz_host_runner::lifecycle::machine_join::{
    MachineJoinFailure, MachineJoinOutcomeKind, run_linux_machine_join,
};

use crate::JoinDoorClient;
use crate::commands::{
    MachineCommand, MachineEndpointSetCommand, MachineJoinCommand, MachineListCommand,
};
use crate::mesh::http::JsonReply;
use crate::remote::{OperatorRemote, OperatorRemoteError};

pub async fn execute(command: MachineCommand) -> Result<String, MachineExecutionError> {
    match command {
        MachineCommand::List(command) => list(command).await,
        MachineCommand::EndpointSet(command) => endpoint_set(command).await,
        MachineCommand::Join(command) => join(command).await,
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
    let outcome = run_linux_machine_join(
        blob.into_blob(),
        storage_choice,
        wireguard_endpoint,
        &mut door,
    )
    .await?;
    let presentation = match outcome.kind {
        MachineJoinOutcomeKind::Joined => MachineJoinOutcome::Joined,
        MachineJoinOutcomeKind::Resumed => MachineJoinOutcome::Resumed,
        MachineJoinOutcomeKind::NoOp => MachineJoinOutcome::NoOp,
    };
    Ok(render_machine_join(
        presentation,
        &outcome.machine_name,
        &outcome.machine_id,
    ))
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
    #[error(transparent)]
    Join(#[from] MachineJoinFailure),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineJoinOutcome {
    Joined,
    Resumed,
    NoOp,
}

#[must_use]
pub fn render_machine_join(
    outcome: MachineJoinOutcome,
    machine_name: &MachineName,
    machine_id: &MachineRowId,
) -> String {
    let verb = match outcome {
        MachineJoinOutcome::Joined => "Joined",
        MachineJoinOutcome::Resumed => "Resumed",
        MachineJoinOutcome::NoOp => "No-op",
    };
    format!(
        "{verb} machine {} ({}).\n",
        machine_name.as_str(),
        machine_id.as_str()
    )
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
    fn join_outcomes_name_the_accepted_machine_identity() {
        let machine_name = MachineName::try_new("edge-b").expect("machine name");
        let machine_id = MachineRowId::try_new(MACHINE_A).expect("machine id");
        for (outcome, expected) in [
            (MachineJoinOutcome::Joined, "Joined"),
            (MachineJoinOutcome::Resumed, "Resumed"),
            (MachineJoinOutcome::NoOp, "No-op"),
        ] {
            assert_eq!(
                render_machine_join(outcome, &machine_name, &machine_id),
                format!("{expected} machine edge-b ({MACHINE_A}).\n")
            );
        }
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
