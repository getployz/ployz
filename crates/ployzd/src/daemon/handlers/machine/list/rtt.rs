use crate::daemon::DaemonState;
use ployz_api::{DaemonPayload, DaemonResponse, MachineRttPayload, MachineRttRow};
use ployz_model::{MachineId, MachineMembership};
use ployz_store_api::{MachineMembershipStore, PeerRttObservation, PeerRttStore, StoreDriver};
use std::collections::HashMap;
use std::net::IpAddr;

impl DaemonState {
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

fn render_machine_rtt_report(payload: &MachineRttPayload, warnings: &[String]) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_model::{OverlayIp, PublicKey};
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
