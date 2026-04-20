use crate::error::{Error, Result};
use crate::runner::scenario_run::ScenarioRun;
use crate::support::{
    daemon_machine_list, daemon_node_status, daemon_peer_health, daemon_wait_node_phase, wait_until,
};
use ployz_api::{MachineListPayload, PeerHealthPayload, PeerProbeStatus};
use ployz_types::model::{DrainState, Phase};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::Duration;

use super::environment::{CONTAINER_WAIT_TIMEOUT, READY_WAIT_TIMEOUT, STATE_WAIT_TIMEOUT};
use super::nodes::Node;

impl ScenarioRun {
    pub(crate) fn wait_node_ready_name(&self, node_name: &str) -> Result<()> {
        Self::wait_node_ready_default(self.node(node_name)?)
    }

    pub(crate) fn wait_node_ready_default(node: &Node) -> Result<()> {
        Self::wait_node_ready(node, READY_WAIT_TIMEOUT)
    }

    fn wait_node_ready(node: &Node, timeout: Duration) -> Result<()> {
        daemon_wait_node_phase(node.rpc_port, Phase::Running, timeout)
            .map(|_| ())
            .map_err(|error| {
                Error::Message(format!(
                    "mesh did not become ready on {}: {error}",
                    node.name
                ))
            })
    }

    pub(crate) fn wait_peer_health_name(
        &self,
        observer_name: &str,
        peer_name: &str,
        expected_healthy: bool,
        expected_probe: PeerProbeStatus,
        timeout: Duration,
    ) -> Result<()> {
        let node = self.node(observer_name)?;
        let mut last_payload: Option<PeerHealthPayload> = None;
        let mut last_error: Option<String> = None;
        wait_until(timeout, || {
            let payload = match daemon_peer_health(node.rpc_port, peer_name) {
                Ok(payload) => payload,
                Err(error) => {
                    last_error = Some(error.to_string());
                    return Ok(false);
                }
            };
            last_error = None;
            last_payload = Some(payload.clone());
            Ok(payload.healthy == expected_healthy && payload.probe == expected_probe)
        })
        .map_err(|error| {
            let observed = match (&last_payload, &last_error) {
                (
                    Some(PeerHealthPayload {
                        healthy,
                        probe,
                        control_plane_ready,
                        ..
                    }),
                    _,
                ) => format!(
                    " last observed: healthy={} probe={} control_plane_ready={}",
                    healthy,
                    match probe {
                        PeerProbeStatus::Reachable => "reachable",
                        PeerProbeStatus::Unreachable => "unreachable",
                    },
                    control_plane_ready
                ),
                (None, Some(last_error)) => format!(" last error: {last_error}"),
                (None, None) => String::from(" no peer-health payload observed"),
            };
            Error::Message(format!(
                "peer '{peer_name}' on {} did not reach healthy={} probe={}: {error}.{}",
                node.name,
                expected_healthy,
                match expected_probe {
                    PeerProbeStatus::Reachable => "reachable",
                    PeerProbeStatus::Unreachable => "unreachable",
                },
                observed
            ))
        })
    }

    pub(crate) fn wait_all_machine_states(
        &self,
        node_name: &str,
        machine_ids: &[&str],
        expected_state: &str,
    ) -> Result<()> {
        let node = self.node(node_name)?;
        let joined_ids = machine_ids.join(", ");

        wait_until(STATE_WAIT_TIMEOUT, || {
            let Ok(payload) = daemon_machine_list(node.rpc_port) else {
                return Ok(false);
            };
            Ok(machine_ids.iter().all(|machine_id| {
                machine_state(&payload, machine_id).as_deref() == Some(expected_state)
            }))
        })
        .map_err(|error| {
            Error::Message(format!(
                "machines '{joined_ids}' did not reach state '{expected_state}' on {}: {error}",
                node.name
            ))
        })
    }

    pub(crate) fn wait_for_settled_machine_states(
        &self,
        node_name: &str,
        expected_states: &[(&str, &str)],
    ) -> Result<()> {
        let node = self.node(node_name)?;
        let expected_count = expected_states.len();
        let expected_labels = expected_states
            .iter()
            .map(|(machine_id, state)| format!("{machine_id}:{state}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut settle_snapshot: Option<Vec<MachineRow>> = None;
        let mut latest_observed: Option<Vec<MachineRow>> = None;
        let mut consecutive_matches: u8 = 0;

        let wait_result = wait_until(STATE_WAIT_TIMEOUT, || {
            let Ok(payload) = daemon_machine_list(node.rpc_port) else {
                return Ok(false);
            };
            let snapshot = machine_rows(&payload);
            latest_observed = Some(snapshot.clone());
            if snapshot.len() != expected_count {
                consecutive_matches = 0;
                settle_snapshot = None;
                return Ok(false);
            }
            if !expected_states.iter().all(|(machine_id, expected_state)| {
                snapshot.iter().any(|row| {
                    row.id == *machine_id
                        && row_matches_state(row, expected_state)
                        && row.subnet != "—"
                })
            }) {
                consecutive_matches = 0;
                settle_snapshot = None;
                return Ok(false);
            }

            if settle_snapshot.as_ref() == Some(&snapshot) {
                consecutive_matches = consecutive_matches.saturating_add(1);
            } else {
                consecutive_matches = 1;
                settle_snapshot = Some(snapshot);
            }

            Ok(consecutive_matches >= 3)
        })
        .map_err(|error| {
            Error::Message(self.settle_timeout_report(
                &node.name,
                expected_states,
                latest_observed.as_deref(),
                &error.to_string(),
            ))
        });

        wait_result?;
        let _ = expected_labels;
        Ok(())
    }

    pub(crate) fn wait_for_settled_machine_states_with_ticks(
        &self,
        node_name: &str,
        expected_states: &[(&str, &str)],
        tick_nodes: &[&str],
        repeat: u32,
    ) -> Result<()> {
        let node = self.node(node_name)?;
        let expected_count = expected_states.len();
        let expected_labels = expected_states
            .iter()
            .map(|(machine_id, state)| format!("{machine_id}:{state}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut settle_snapshot: Option<Vec<MachineRow>> = None;
        let mut latest_observed: Option<Vec<MachineRow>> = None;
        let mut consecutive_matches: u8 = 0;

        let wait_result = wait_until(STATE_WAIT_TIMEOUT, || {
            self.tick_nodes(tick_nodes, repeat)?;

            let Ok(payload) = daemon_machine_list(node.rpc_port) else {
                return Ok(false);
            };
            let snapshot = machine_rows(&payload);
            latest_observed = Some(snapshot.clone());
            if snapshot.len() != expected_count {
                consecutive_matches = 0;
                settle_snapshot = None;
                return Ok(false);
            }
            if !expected_states.iter().all(|(machine_id, expected_state)| {
                snapshot.iter().any(|row| {
                    row.id == *machine_id
                        && row_matches_state(row, expected_state)
                        && row.subnet != "—"
                })
            }) {
                consecutive_matches = 0;
                settle_snapshot = None;
                return Ok(false);
            }

            if settle_snapshot.as_ref() == Some(&snapshot) {
                consecutive_matches = consecutive_matches.saturating_add(1);
            } else {
                consecutive_matches = 1;
                settle_snapshot = Some(snapshot);
            }

            Ok(consecutive_matches >= 3)
        })
        .map_err(|error| {
            Error::Message(self.settle_timeout_report(
                &node.name,
                expected_states,
                latest_observed.as_deref(),
                &error.to_string(),
            ))
        });

        wait_result?;
        let _ = expected_labels;
        Ok(())
    }

    fn settle_timeout_report(
        &self,
        observer: &str,
        expected_states: &[(&str, &str)],
        snapshot: Option<&[MachineRow]>,
        error: &str,
    ) -> String {
        let expected_labels = expected_states
            .iter()
            .map(|(machine_id, state)| format!("{machine_id}:{state}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut report =
            format!("machine state did not settle on {observer} for [{expected_labels}]: {error}");

        let Some(snapshot) = snapshot else {
            let _ = write!(report, "\n  (no machine list snapshot was ever observed)");
            return report;
        };

        let _ = write!(report, "\n  observed snapshot ({} rows):", snapshot.len());
        if snapshot.is_empty() {
            let _ = write!(report, " (empty)");
        }
        for row in snapshot {
            let _ = write!(
                report,
                "\n    - {} drain_state={} subnet={}",
                row.id,
                format_optional_drain_state(row.drain_state),
                row.subnet,
            );
        }

        let expected_ids: BTreeMap<&str, &str> = expected_states
            .iter()
            .map(|(id, state)| (*id, *state))
            .collect();
        for row in snapshot {
            let expected_state = expected_ids.get(row.id.as_str()).copied();
            let matches_expected =
                expected_state.is_some_and(|state| row_matches_state(row, state));
            if matches_expected {
                continue;
            }
            let Ok(peer_node) = self.node(&row.id) else {
                let _ = write!(
                    report,
                    "\n  {}: no e2e-managed container — cannot probe node status",
                    row.id
                );
                continue;
            };
            append_readiness_diagnostics(&mut report, &row.id, peer_node);
        }

        report
    }

    pub(crate) fn assert_unique_machine_subnets(&self, node_name: &str) -> Result<()> {
        let node = self.node(node_name)?;
        let payload = daemon_machine_list(node.rpc_port)?;
        let mut seen: BTreeMap<String, String> = BTreeMap::new();

        for prefix in machine_rows(&payload) {
            if !prefix.subnet.contains('/') {
                continue;
            }
            if let Some(existing) = seen.insert(prefix.subnet.clone(), prefix.id.clone()) {
                return Err(Error::Message(format!(
                    "duplicate subnet '{}' reported by {} for machines '{}' and '{}'",
                    prefix.subnet, node_name, existing, prefix.id
                )));
            }
        }

        Ok(())
    }

    pub(crate) fn wait_service_container_name(
        &self,
        node_name: &str,
        namespace: &str,
        service: &str,
    ) -> Result<()> {
        self.wait_service_container(self.node(node_name)?, namespace, service)
    }

    fn wait_service_container(&self, node: &Node, namespace: &str, service: &str) -> Result<()> {
        wait_until(CONTAINER_WAIT_TIMEOUT, || {
            let output = self.ssh_run(
                node,
                &format!(
                    "docker ps -a --filter label=dev.ployz.namespace={namespace} --filter label=dev.ployz.service={service} --format '{{{{.Names}}}}'"
                ),
            )?;
            let names: Vec<&str> = output
                .stdout
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect();
            Ok(!names.is_empty())
        })
        .map_err(|error| {
            Error::Message(format!(
                "service '{service}' in namespace '{namespace}' did not create a workload: {error}"
            ))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MachineRow {
    id: String,
    admitted: bool,
    drain_state: Option<DrainState>,
    subnet: String,
}

impl MachineRow {
    fn from_payload(row: &ployz_api::MachineListRow) -> Self {
        Self {
            id: row.id.clone(),
            admitted: row.admitted,
            drain_state: row.drain_state,
            subnet: row.subnet.clone().unwrap_or_else(|| "—".into()),
        }
    }
}

fn machine_rows(payload: &MachineListPayload) -> Vec<MachineRow> {
    payload.rows.iter().map(MachineRow::from_payload).collect()
}

fn append_readiness_diagnostics(report: &mut String, row_id: &str, peer_node: &Node) {
    match daemon_node_status(peer_node.rpc_port) {
        Ok(payload) => {
            let _ = write!(
                report,
                "\n  {row_id} self-report: phase={} ready={} drain_state={} boot_id={} subnet_claim={} slots={}",
                payload.phase,
                payload.ready,
                payload.drain_state,
                payload.boot_id,
                payload.subnet_claim.as_deref().unwrap_or("none"),
                payload.workloads.slots,
            );
        }
        Err(error) => {
            let _ = write!(report, "\n  {row_id} self-report: unavailable ({error})");
        }
    }
}

fn machine_state(payload: &MachineListPayload, machine_id: &str) -> Option<String> {
    payload.rows.iter().find_map(|row| {
        if row.id == machine_id {
            return Some(if !row.admitted {
                String::from("pending")
            } else if row.drain_state.is_some_and(DrainState::is_drained) {
                String::from("drained")
            } else {
                String::from("active")
            });
        }
        None
    })
}

fn row_matches_state(row: &MachineRow, expected_state: &str) -> bool {
    match expected_state {
        "pending" => !row.admitted,
        "drained" => row.drain_state.is_some_and(DrainState::is_drained),
        "active" => row.admitted && !row.drain_state.is_some_and(DrainState::is_drained),
        _ => false,
    }
}

fn format_optional_drain_state(drain_state: Option<DrainState>) -> &'static str {
    match drain_state {
        Some(DrainState::Active) => "active",
        Some(DrainState::Drained) => "drained",
        None => "unknown",
    }
}
