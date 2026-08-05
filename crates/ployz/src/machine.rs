//! Machine command execution, mesh HTTP, and human presentation.

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use ployz_core::corrosion::MachineTransport;
use ployz_core::ids::ClusterId;
use ployz_core::{LensCollection, LensSnapshot};

use crate::commands::{MachineCommand, MachineListCommand, SshTarget};
use crate::init::ssh::default_config_home;
use crate::mesh::context::{
    LoadedOperatorContext, OperatorContextError, OperatorContextStore, UnsupportedMeshProvider,
};
use crate::mesh::http::{MeshApiClient, MeshApiClientError};
use crate::mesh::{
    BuiltinWireguardDial, BuiltinWireguardPeer, MeshConnectError, MeshConnector, MeshDialTimeouts,
};

const API_PORT: u16 = 2_020;

pub async fn execute(command: MachineCommand) -> Result<String, MachineExecutionError> {
    match command {
        MachineCommand::List(command) => list(command).await,
    }
}

async fn list(command: MachineListCommand) -> Result<String, MachineExecutionError> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(MachineExecutionError::MissingHome)?;
    let contexts = OperatorContextStore::new(default_config_home(&home)).load_all()?;
    let targets = contexts
        .iter()
        .map(|loaded| loaded.target().clone())
        .collect::<Vec<_>>();
    let selected = select_context_index(&targets, command.target.as_ref())?;
    let Some(loaded) = contexts.into_iter().nth(selected) else {
        unreachable!("selected operator context index comes from the same collection")
    };
    let (connector, api_target) = connector_for_context(&loaded)?;
    let stream = connector.connect(api_target).await?;
    let snapshot = MeshApiClient::default()
        .lens(stream, LensCollection::Machines)
        .await?;
    validate_snapshot_cluster(&snapshot, loaded.cluster_id())?;
    render_machines(&snapshot).map_err(MachineExecutionError::from)
}

fn connector_for_context(
    loaded: &LoadedOperatorContext,
) -> Result<(MeshConnector, SocketAddr), MachineExecutionError> {
    match loaded {
        LoadedOperatorContext::BuiltinWireguard(context) => {
            let dial = BuiltinWireguardDial::new(
                context.private_key.bytes(),
                context.source_address,
                BuiltinWireguardPeer {
                    public_key: context.machine_public_key,
                    endpoint: context.machine_endpoint,
                    allowed_subnet: context.machine_allowed_subnet,
                },
                context.target.as_str(),
            );
            Ok((
                MeshConnector::builtin_wireguard(dial, MeshDialTimeouts::default()),
                SocketAddr::new(IpAddr::V6(context.machine_address), API_PORT),
            ))
        }
        LoadedOperatorContext::UnsupportedProvider(context) => {
            Err(MachineExecutionError::UnsupportedProvider {
                provider: context.provider,
            })
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MachineExecutionError {
    #[error("HOME is not set; cannot load operator contexts")]
    MissingHome,
    #[error(transparent)]
    Context(#[from] OperatorContextError),
    #[error(transparent)]
    Selection(#[from] ContextSelectionError),
    #[error(
        "mesh provider {provider:?} is not shipped; builtin WireGuard is the only supported provider"
    )]
    UnsupportedProvider { provider: UnsupportedMeshProvider },
    #[error(transparent)]
    Connect(#[from] MeshConnectError),
    #[error(transparent)]
    Api(#[from] MeshApiClientError),
    #[error(transparent)]
    Snapshot(#[from] MachineSnapshotError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextSelectionError {
    NotConfigured,
    Ambiguous {
        targets: Vec<String>,
    },
    UnknownTarget {
        target: String,
        configured: Vec<String>,
    },
}

impl fmt::Display for ContextSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotConfigured => formatter
                .write_str("no operator context is configured; run `ployz init root@<host>` first"),
            Self::Ambiguous { targets } => {
                formatter.write_str("multiple operator contexts are configured; choose one:")?;
                for target in targets {
                    write!(
                        formatter,
                        "\n  {target}: ployz machine ls --target {target}"
                    )?;
                }
                Ok(())
            }
            Self::UnknownTarget { target, configured } => {
                write!(
                    formatter,
                    "no operator context is configured for {target}; configured targets:"
                )?;
                for target in configured {
                    write!(
                        formatter,
                        "\n  {target}: ployz machine ls --target {target}"
                    )?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ContextSelectionError {}

pub(crate) fn select_context_index(
    configured: &[SshTarget],
    requested: Option<&SshTarget>,
) -> Result<usize, ContextSelectionError> {
    match requested {
        Some(requested) => configured
            .iter()
            .position(|candidate| candidate == requested)
            .ok_or_else(|| ContextSelectionError::UnknownTarget {
                target: requested.as_str().to_owned(),
                configured: sorted_target_names(configured),
            }),
        None => match configured {
            [] => Err(ContextSelectionError::NotConfigured),
            [_] => Ok(0),
            [_, _, ..] => Err(ContextSelectionError::Ambiguous {
                targets: sorted_target_names(configured),
            }),
        },
    }
}

fn sorted_target_names(targets: &[SshTarget]) -> Vec<String> {
    let mut targets = targets
        .iter()
        .map(|target| target.as_str().to_owned())
        .collect::<Vec<_>>();
    targets.sort();
    targets
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const MACHINE_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
    const MACHINE_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
    const PEER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";

    fn target(value: &str) -> SshTarget {
        value.parse().expect("valid SSH target")
    }

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
    fn context_selection_has_exact_zero_one_multiple_and_unknown_copy() {
        assert_eq!(
            select_context_index(&[], None).expect_err("no context"),
            ContextSelectionError::NotConfigured
        );
        assert_eq!(
            ContextSelectionError::NotConfigured.to_string(),
            "no operator context is configured; run `ployz init root@<host>` first"
        );

        let one = vec![target("root@one.example")];
        assert_eq!(select_context_index(&one, None).expect("one context"), 0);

        let multiple = vec![target("root@z.example"), target("root@a.example")];
        let ambiguous = select_context_index(&multiple, None).expect_err("selector required");
        assert_eq!(
            ambiguous.to_string(),
            "multiple operator contexts are configured; choose one:\n  root@a.example: ployz machine ls --target root@a.example\n  root@z.example: ployz machine ls --target root@z.example"
        );
        assert_eq!(
            select_context_index(&multiple, Some(&target("root@z.example"))).expect("exact target"),
            0
        );

        let unknown = select_context_index(&multiple, Some(&target("root@missing.example")))
            .expect_err("unknown target");
        assert_eq!(
            unknown.to_string(),
            "no operator context is configured for root@missing.example; configured targets:\n  root@a.example: ployz machine ls --target root@a.example\n  root@z.example: ployz machine ls --target root@z.example"
        );
    }

    #[test]
    fn machines_render_as_stable_human_first_table() {
        assert_eq!(
            render_machines(&machines_snapshot()).expect("machine table"),
            "NAME\tCONTROL ADDRESS\nalpha\tfd00::20\nzeta\t100.64.0.20\n"
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
        let error = MachineExecutionError::Connect(MeshConnectError::UdpDialTimedOut {
            endpoint,
            target: api_target,
            founding_target: "root@machine.example".into(),
            docs_anchor: crate::mesh::UDP_BLOCKED_DOCS_ANCHOR,
            ssh_fallback: crate::mesh::render_ssh_fallback_command(
                "root@machine.example",
                api_target,
            )
            .into(),
        });
        assert_eq!(
            error.to_string(),
            "WireGuard dial to [fd00::20]:2020 through UDP endpoint 203.0.113.7:51820 timed out; UDP may be blocked. Fallback: ssh -N -L 127.0.0.1:2020:[fd00::20]:2020 root@machine.example. See docs/operations/cli-mesh-access.md#udp-blocked-networks"
        );
    }

    #[test]
    fn unsupported_context_refuses_before_attempting_a_connection() {
        let loaded = LoadedOperatorContext::UnsupportedProvider(
            crate::mesh::context::UnsupportedOperatorContext {
                target: target("root@machine.example"),
                cluster_id: ClusterId::try_new(CLUSTER).expect("cluster id"),
                provider: UnsupportedMeshProvider::Tailscale,
            },
        );

        let error = connector_for_context(&loaded).expect_err("unsupported provider");
        assert!(matches!(
            error,
            MachineExecutionError::UnsupportedProvider {
                provider: UnsupportedMeshProvider::Tailscale
            }
        ));
        assert_eq!(
            error.to_string(),
            "mesh provider Tailscale is not shipped; builtin WireGuard is the only supported provider"
        );
    }
}
