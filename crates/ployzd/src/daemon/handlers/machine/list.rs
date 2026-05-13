use crate::daemon::DaemonState;
use ployz_api::{
    DaemonPayload, DaemonResponse, MachineRemovePayload, MachineRttPayload, MachineRttRow,
};
use ployz_store_api::{MachineMembershipStore, PeerRttObservation, PeerRttStore, StoreDriver};
use ployz_types::model::{MachineId, MachineMembership};
use std::collections::HashMap;
use std::net::IpAddr;

use super::render::{format_lifecycle, format_timestamp, render_machine_list_report};
use super::types::{MachineListReport, MachineListReportRow};
use crate::mesh_state::bootstrap::refresh_bootstrap_peer_records_from_store;
use ployz_nats::{NodeCommandSubject, RpcPolicy};
use ployz_node_api::NodeRequest;
use std::time::Duration;

const MACHINE_REMOVE_RPC_TIMEOUT: Duration = Duration::from_secs(120);

impl DaemonState {
    pub(crate) async fn handle_machine_list(&self) -> DaemonResponse {
        let active = match self.require_active("NO_RUNNING_NETWORK", "no mesh running") {
            Ok(active) => active,
            Err(response) => return *response,
        };

        let report = match machine_list_report(active.mesh.store.clone()).await {
            Ok(report) => report,
            Err(err) => return self.err("LIST_FAILED", err),
        };
        if report.rows.is_empty() {
            return self.ok_with_payload(
                "no machines",
                Some(DaemonPayload::MachineList(report.payload())),
            );
        }

        self.ok_with_payload(
            render_machine_list_report(&report),
            Some(DaemonPayload::MachineList(report.payload())),
        )
    }

    pub(crate) async fn handle_machine_rtt(&self) -> DaemonResponse {
        let active = match self.require_active("NO_RUNNING_NETWORK", "no mesh running") {
            Ok(active) => active,
            Err(response) => return *response,
        };
        let machines = match active.mesh.store.list_machines().await {
            Ok(machines) => machines,
            Err(err) => return self.err("LIST_FAILED", format!("failed to list machines: {err}")),
        };
        let mut rows = match machine_rtt_rows_for(
            &active.mesh.store,
            &self.identity.machine_id,
            machines.as_slice(),
        )
        .await
        {
            Ok(rows) => rows,
            Err(err) => return self.err("RTT_READ_FAILED", err),
        };

        rows.sort_by(|left, right| {
            left.machine
                .cmp(&right.machine)
                .then_with(|| left.peer.cmp(&right.peer))
        });
        let payload = MachineRttPayload {
            rows,
            warnings: Vec::new(),
        };
        self.ok_with_payload(
            render_machine_rtt_report(&payload, payload.warnings.as_slice()),
            Some(DaemonPayload::MachineRtt(payload)),
        )
    }

    pub(crate) async fn handle_mesh_peer_rtt_snapshot(&self) -> DaemonResponse {
        let active = match self.require_active("NO_RUNNING_NETWORK", "no mesh running") {
            Ok(active) => active,
            Err(response) => return *response,
        };
        let machines = match active.mesh.store.list_machines().await {
            Ok(machines) => machines,
            Err(err) => return self.err("LIST_FAILED", format!("failed to list machines: {err}")),
        };
        let mut rows = match machine_rtt_rows_for(
            &active.mesh.store,
            &self.identity.machine_id,
            machines.as_slice(),
        )
        .await
        {
            Ok(rows) => rows,
            Err(err) => return self.err("RTT_READ_FAILED", err),
        };
        rows.sort_by(|left, right| {
            left.machine
                .cmp(&right.machine)
                .then_with(|| left.peer.cmp(&right.peer))
        });
        let payload = MachineRttPayload {
            rows,
            warnings: Vec::new(),
        };
        self.ok_with_payload(
            render_machine_rtt_report(&payload, &[]),
            Some(DaemonPayload::MachineRtt(payload)),
        )
    }

    pub(crate) async fn handle_machine_remove(&self, id: &str, force: bool) -> DaemonResponse {
        let active = match self.require_active("NO_RUNNING_NETWORK", "no mesh running") {
            Ok(active) => active,
            Err(response) => return *response,
        };

        let machine_id = match MachineId::try_new(id) {
            Ok(machine_id) => machine_id,
            Err(error) => return self.err("MACHINE_INVALID_TARGET", error),
        };
        let record = match find_machine_record(&active.mesh.store, &machine_id).await {
            Ok(Some(record)) => record,
            Ok(None) => {
                return self.err("MACHINE_NOT_FOUND", format!("machine '{id}' not found"));
            }
            Err(err) => {
                return self.err("LIST_FAILED", format!("failed to read machines: {err}"));
            }
        };

        if record.id == self.identity.machine_id {
            return self.err(
                "CANNOT_REMOVE_SELF",
                "cannot remove the local machine with `machine rm`; use `mesh destroy` for whole-mesh teardown",
            );
        }

        if !force {
            let rpc_client = match self.nats_node_rpc_client().await {
                Ok(client) => client.with_policy(RpcPolicy {
                    timeout: MACHINE_REMOVE_RPC_TIMEOUT,
                }),
                Err(error) => {
                    return self.err(
                        "MACHINE_REMOVE_PEER_UNREACHABLE",
                        format!(
                            "machine '{id}' did not confirm online removal; rerun with --force for membership-record-only removal: {error}"
                        ),
                    );
                }
            };
            let operation_id = format!("machine-rm-{}", ployz_types::model::NetworkId::random());
            let response = rpc_client
                .request(
                    NodeCommandSubject::mesh_remove_machine(&record.id),
                    &NodeRequest::MeshPeerRemoveMachine {
                        operation_id,
                        network_id: active.config.id.clone(),
                        machine_id: record.id.clone(),
                    },
                )
                .await;
            match response {
                Ok(response) if response.is_ok() => {}
                Ok(response) => {
                    return self.err(
                        "MACHINE_REMOVE_PEER_REJECTED",
                        format!(
                            "machine '{id}' rejected coordinated removal [{}]: {}; resolve the remote failure or rerun with --force only if you intend membership-record-only removal",
                            response.code(), response.message()
                        ),
                    );
                }
                Err(error) => {
                    return self.err(
                        "MACHINE_REMOVE_PEER_UNREACHABLE",
                        format!(
                            "machine '{id}' did not confirm online removal; rerun with --force for membership-record-only removal: {error}"
                        ),
                    );
                }
            }
        }

        match active.mesh.store.delete_machine(&machine_id).await {
            Ok(()) => {
                let network_dir = self.network_dir(&active.config.name.0);
                if let Err(error) = refresh_bootstrap_peer_records_from_store(
                    &network_dir,
                    &active.mesh.store,
                    &self.identity.machine_id,
                )
                .await
                {
                    tracing::warn!(
                        %machine_id,
                        %error,
                        "failed to refresh bootstrap peer seed after machine remove"
                    );
                }
                self.ok_with_payload(
                    format!("machine '{id}' removed"),
                    Some(DaemonPayload::MachineRemove(MachineRemovePayload {
                        id: id.to_string(),
                        force,
                    })),
                )
            }
            Err(err) => self.err("DELETE_FAILED", format!("failed to remove machine: {err}")),
        }
    }
}

async fn machine_rtt_rows_for(
    store: &StoreDriver,
    source_machine_id: &MachineId,
    machines: &[MachineMembership],
) -> Result<Vec<MachineRttRow>, String> {
    let observations = store
        .peer_rtt_observations()
        .await
        .map_err(|err| format!("failed to read peer RTT observations: {err}"))?;
    Ok(rows_from_rtt_observations(
        source_machine_id,
        machines,
        observations.as_slice(),
    ))
}

fn rows_from_rtt_observations(
    source_machine_id: &MachineId,
    machines: &[MachineMembership],
    observations: &[PeerRttObservation],
) -> Vec<MachineRttRow> {
    let machine_by_ip: HashMap<IpAddr, &MachineMembership> = machines
        .iter()
        .map(|machine| (IpAddr::V6(machine.overlay_ip.0), machine))
        .collect();
    let mut rows = Vec::new();
    for observation in observations {
        if observation.rtts_ms.is_empty() {
            continue;
        }
        let Some(peer) = machine_by_ip.get(&observation.addr.ip()) else {
            continue;
        };
        if peer.id == *source_machine_id {
            continue;
        }
        let Some((median_ms, stddev_ms)) = rtt_stats(observation.rtts_ms.as_slice()) else {
            continue;
        };
        rows.push(MachineRttRow {
            machine: source_machine_id.as_str().to_string(),
            peer: peer.id.as_str().to_string(),
            median_ms,
            stddev_ms,
        });
    }
    rows
}

fn rtt_stats(samples: &[u64]) -> Option<(f64, f64)> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    let median = if sorted.len() % 2 == 0 {
        (sorted[mid - 1] as f64 + sorted[mid] as f64) / 2.0
    } else {
        sorted[mid] as f64
    };
    let mean = samples.iter().map(|sample| *sample as f64).sum::<f64>() / samples.len() as f64;
    let variance = samples
        .iter()
        .map(|sample| {
            let delta = *sample as f64 - mean;
            delta * delta
        })
        .sum::<f64>()
        / samples.len() as f64;
    Some((median, variance.sqrt()))
}

pub(super) fn render_machine_rtt_report(
    payload: &MachineRttPayload,
    warnings: &[String],
) -> String {
    let mut lines = Vec::new();
    if payload.rows.is_empty() {
        lines.push(String::from("no rtt samples"));
    } else {
        let w_machine = payload
            .rows
            .iter()
            .map(|row| row.machine.len())
            .max()
            .unwrap_or(0)
            .max("MACHINE".len());
        let w_peer = payload
            .rows
            .iter()
            .map(|row| row.peer.len())
            .max()
            .unwrap_or(0)
            .max("PEER".len());
        let w_median = payload
            .rows
            .iter()
            .map(|row| format_ms(row.median_ms).len())
            .max()
            .unwrap_or(0)
            .max("MEDIAN".len());
        lines.push(format!(
            "{:<w_machine$}  {:<w_peer$}  {:<w_median$}  {}",
            "MACHINE", "PEER", "MEDIAN", "STDDEV"
        ));
        for row in &payload.rows {
            lines.push(format!(
                "{:<w_machine$}  {:<w_peer$}  {:<w_median$}  ±{}",
                row.machine,
                row.peer,
                format_ms(row.median_ms),
                format_ms_one_decimal(row.stddev_ms),
            ));
        }
    }
    if !warnings.is_empty() {
        lines.push(String::new());
        lines.push(String::from("warnings:"));
        lines.extend(warnings.iter().map(|warning| format!("  {warning}")));
    }
    lines.join("\n")
}

fn format_ms(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}ms")
    } else {
        format!("{value:.1}ms")
    }
}

fn format_ms_one_decimal(value: f64) -> String {
    format!("{value:.1}ms")
}

pub(super) async fn find_machine_record(
    store: &StoreDriver,
    machine_id: &MachineId,
) -> Result<Option<MachineMembership>, String> {
    let machines = store
        .list_machines()
        .await
        .map_err(|err| format!("{err}"))?;
    Ok(machines
        .into_iter()
        .find(|machine| machine.id == *machine_id))
}

pub(super) async fn machine_list_report(store: StoreDriver) -> Result<MachineListReport, String> {
    let machines = store
        .list_machines()
        .await
        .map_err(|err| format!("failed to list machines: {err}"))?;

    Ok(MachineListReport {
        rows: machines
            .iter()
            .map(|machine| MachineListReportRow {
                id: machine.id.as_str().to_string(),
                lifecycle: format_lifecycle(machine),
                authority: ployz_types::model::AuthorityNodePosture::from_machine_membership(
                    machine,
                ),
                region: machine.topology.region.0.clone(),
                region_role: machine.region_role.to_string(),
                availability_zone: machine
                    .topology
                    .availability_zone
                    .as_ref()
                    .map(|zone| zone.0.clone()),
                availability_zone_display: machine
                    .topology
                    .availability_zone
                    .as_ref()
                    .map(|zone| zone.0.clone())
                    .unwrap_or_else(|| "—".into()),
                overlay: machine.overlay_ip.0.to_string(),
                subnet: machine.subnet,
                subnet_display: machine
                    .subnet
                    .map(|subnet| subnet.to_string())
                    .unwrap_or_else(|| "—".into()),
                created_at: machine.created_at,
                created_display: format_timestamp(machine.created_at),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::{OverlayIp, PublicKey};
    use std::net::{Ipv6Addr, SocketAddr};

    #[test]
    fn machine_rtt_rows_map_observations_and_skip_empty_unknown_and_self() {
        let source_id = MachineId::new("machine-1");
        let peer = machine_record("machine-2", Ipv6Addr::LOCALHOST, 2);
        let machines = vec![
            machine_record("machine-1", Ipv6Addr::UNSPECIFIED, 1),
            peer.clone(),
        ];
        let observations = vec![
            PeerRttObservation {
                addr: SocketAddr::new(IpAddr::V6(peer.overlay_ip.0), 51001),
                rtts_ms: vec![120, 140, 160],
            },
            PeerRttObservation {
                addr: SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 51001),
                rtts_ms: vec![1],
            },
            PeerRttObservation {
                addr: "[fd00::99]:51001".parse().expect("valid addr"),
                rtts_ms: vec![1],
            },
            PeerRttObservation {
                addr: SocketAddr::new(IpAddr::V6(peer.overlay_ip.0), 51001),
                rtts_ms: Vec::new(),
            },
        ];

        let rows =
            rows_from_rtt_observations(&source_id, machines.as_slice(), observations.as_slice());

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].machine, "machine-1");
        assert_eq!(rows[0].peer, "machine-2");
        assert_eq!(rows[0].median_ms, 140.0);
        assert!((rows[0].stddev_ms - 16.3299).abs() < 0.001);
    }

    #[test]
    fn rtt_stats_handles_even_odd_and_empty_samples() {
        assert_eq!(rtt_stats(&[]), None);
        assert_eq!(rtt_stats(&[39, 40, 41]).expect("stats").0, 40.0);
        assert_eq!(rtt_stats(&[100, 200]).expect("stats").0, 150.0);
    }

    #[test]
    fn render_machine_rtt_report_matches_table_shape() {
        let payload = MachineRttPayload {
            rows: vec![MachineRttRow {
                machine: "machine-1".into(),
                peer: "machine-2".into(),
                median_ms: 140.0,
                stddev_ms: 19.4,
            }],
            warnings: Vec::new(),
        };

        let rendered = render_machine_rtt_report(&payload, &[]);

        assert!(rendered.contains("MACHINE    PEER"));
        assert!(rendered.contains("MEDIAN"));
        assert!(rendered.contains("STDDEV"));
        assert!(rendered.contains("machine-1"));
        assert!(rendered.contains("machine-2"));
        assert!(rendered.contains("140ms"));
        assert!(rendered.contains("±19.4ms"));
    }

    fn machine_record(id: &str, overlay_ip: Ipv6Addr, key_byte: u8) -> MachineMembership {
        MachineMembership::seed(
            MachineId::new(id),
            PublicKey([key_byte; 32]),
            OverlayIp(overlay_ip),
            None,
            Vec::new(),
        )
    }
}
