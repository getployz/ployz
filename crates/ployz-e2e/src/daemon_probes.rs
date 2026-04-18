use crate::error::{Error, Result};
use crate::runner::scenario_run::ScenarioRun;
use crate::support::{
    daemon_machine_list_in_container, daemon_mesh_ready_in_container, wait_until,
};
use ployz_api::MachineListPayload;
use std::collections::BTreeMap;
use std::time::Duration;

use super::environment::{CONTAINER_WAIT_TIMEOUT, READY_WAIT_TIMEOUT, STATE_WAIT_TIMEOUT};
use super::nodes::Node;

impl ScenarioRun {
    pub(crate) fn wait_mesh_ready_name(&self, node_name: &str) -> Result<()> {
        Self::wait_mesh_ready_default(self.node(node_name)?)
    }

    pub(crate) fn wait_mesh_ready_default(node: &Node) -> Result<()> {
        Self::wait_mesh_ready(node, READY_WAIT_TIMEOUT)
    }

    fn wait_mesh_ready(node: &Node, timeout: Duration) -> Result<()> {
        wait_until(timeout, || {
            let Ok(payload) = daemon_mesh_ready_in_container(&node.container_name) else {
                return Ok(false);
            };
            Ok(payload.ready)
        })
        .map_err(|error| {
            Error::Message(format!(
                "mesh did not become ready on {}: {error}",
                node.name
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
            let Ok(payload) = daemon_machine_list_in_container(&node.container_name) else {
                return Ok(false);
            };
            Ok(machine_ids.iter().all(|machine_id| {
                payload
                    .rows
                    .iter()
                    .any(|row| row.id == *machine_id && row_matches_state(&MachineRow::from_payload(row), expected_state))
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
        let mut last_snapshot: Option<Vec<MachineRow>> = None;
        let mut consecutive_matches: u8 = 0;

        wait_until(STATE_WAIT_TIMEOUT, || {
            let Ok(payload) = daemon_machine_list_in_container(&node.container_name) else {
                return Ok(false);
            };
            let snapshot = machine_rows(&payload);
            if snapshot.len() != expected_count {
                consecutive_matches = 0;
                last_snapshot = None;
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
                last_snapshot = None;
                return Ok(false);
            }

            if last_snapshot.as_ref() == Some(&snapshot) {
                consecutive_matches = consecutive_matches.saturating_add(1);
            } else {
                consecutive_matches = 1;
                last_snapshot = Some(snapshot);
            }

            Ok(consecutive_matches >= 3)
        })
        .map_err(|error| {
            Error::Message(format!(
                "machine state did not settle on {} for [{}]: {error}",
                node.name, expected_labels
            ))
        })
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
        let mut last_snapshot: Option<Vec<MachineRow>> = None;
        let mut consecutive_matches: u8 = 0;

        wait_until(STATE_WAIT_TIMEOUT, || {
            self.tick_nodes(tick_nodes, repeat)?;

            let Ok(payload) = daemon_machine_list_in_container(&node.container_name) else {
                return Ok(false);
            };
            let snapshot = machine_rows(&payload);
            if snapshot.len() != expected_count {
                consecutive_matches = 0;
                last_snapshot = None;
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
                last_snapshot = None;
                return Ok(false);
            }

            if last_snapshot.as_ref() == Some(&snapshot) {
                consecutive_matches = consecutive_matches.saturating_add(1);
            } else {
                consecutive_matches = 1;
                last_snapshot = Some(snapshot);
            }

            Ok(consecutive_matches >= 3)
        })
        .map_err(|error| {
            Error::Message(format!(
                "machine state did not settle on {} for [{}]: {error}",
                node.name, expected_labels
            ))
        })
    }

    pub(crate) fn assert_unique_machine_subnets(&self, node_name: &str) -> Result<()> {
        let node = self.node(node_name)?;
        let payload = daemon_machine_list_in_container(&node.container_name)?;
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

    pub(crate) fn wait_for_unique_machine_subnets_with_ticks(
        &self,
        node_name: &str,
        tick_nodes: &[&str],
        repeat: u32,
    ) -> Result<()> {
        wait_until(STATE_WAIT_TIMEOUT, || {
            self.tick_nodes(tick_nodes, repeat)?;
            match self.assert_unique_machine_subnets(node_name) {
                Ok(()) => Ok(true),
                Err(_) => Ok(false),
            }
        })
        .map_err(|error| {
            Error::Message(format!(
                "machine subnets did not become unique on {node_name}: {error}"
            ))
        })
    }

    pub(crate) fn wait_for_machine_ids_with_subnets(
        &self,
        node_name: &str,
        machine_ids: &[&str],
    ) -> Result<()> {
        let node = self.node(node_name)?;
        let joined_ids = machine_ids.join(", ");

        wait_until(STATE_WAIT_TIMEOUT, || {
            let Ok(payload) = daemon_machine_list_in_container(&node.container_name) else {
                return Ok(false);
            };
            let snapshot = machine_rows(&payload);
            Ok(machine_ids.iter().all(|machine_id| {
                snapshot
                    .iter()
                    .any(|row| row.id == *machine_id && row.subnet != "—")
            }))
        })
        .map_err(|error| {
            Error::Message(format!(
                "machines '{joined_ids}' did not appear with subnets on {}: {error}",
                node.name
            ))
        })
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
    drain: bool,
    liveness: String,
    subnet: String,
}

impl MachineRow {
    fn from_payload(row: &ployz_api::MachineListRow) -> Self {
        Self {
            id: row.id.clone(),
            drain: row.drain,
            liveness: row.liveness.clone(),
            subnet: row.subnet.clone().unwrap_or_else(|| "—".into()),
        }
    }
}

fn machine_rows(payload: &MachineListPayload) -> Vec<MachineRow> {
    payload.rows.iter().map(MachineRow::from_payload).collect()
}

fn row_matches_state(row: &MachineRow, expected_state: &str) -> bool {
    match expected_state {
        "enabled" => !row.drain && row.liveness == "fresh",
        "draining" => row.drain,
        "down" => row.liveness == "down",
        "fresh" | "stale" | "mismatch" => row.liveness == expected_state,
        other => other == row.liveness,
    }
}
