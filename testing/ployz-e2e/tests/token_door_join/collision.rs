use super::fixture::{
    CorrosionAccess, RosterMachine, corrosion_transaction, machine_subnet, wait_for_machine_roster,
};
use super::{WAIT_BUDGET, WAIT_DELAY};
use bollard::Docker;
use ployz_core::corrosion::{MachineDocument, MachineTransport, Principal};
use ployz_core::ids::MachineRowId;
use ployz_core::network::MachineEndpointSubnet;
use ployz_e2e::dind::{DindMachine, ExecOutcome, exec_in_container, require};
use serde_json::json;
use std::time::Instant;

pub(super) async fn force_collision_and_wait_for_higher_ulid_repair(
    store: CorrosionAccess<'_>,
    joiner: &DindMachine,
    founder_row: &RosterMachine,
    joiner_row: &RosterMachine,
) -> Result<(), String> {
    require(
        founder_row.id < joiner_row.id,
        "machine one must be the lowest ULID in the forced-collision fixture",
    )?;
    let collided = machine_subnet(&founder_row.document)?;
    let mut forced = joiner_row.document.clone();
    let replacement =
        MachineEndpointSubnet::try_new(&collided).map_err(|error| error.to_string())?;
    match &mut forced.transport {
        MachineTransport::Wireguard { subnet_v4, .. }
        | MachineTransport::Tailscale { subnet_v4, .. } => *subnet_v4 = replacement,
    }
    let forced = serde_json::to_string(&forced).map_err(|error| error.to_string())?;
    corrosion_transaction(
        store,
        &json!([[
            "UPDATE machines SET document = ? WHERE id = ?",
            [forced, joiner_row.id.as_str().to_owned()]
        ]]),
    )
    .await?;

    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last = String::from("repair row was not observed");
    while Instant::now() < deadline {
        let roster = wait_for_machine_roster(store, 2).await?;
        if let Some(repaired) = roster.get(&joiner_row.id) {
            let replacement = machine_subnet(&repaired.document)?;
            if replacement != collided {
                assert_only_local_subnet_was_repaired(
                    &joiner_row.id,
                    &joiner_row.document,
                    &repaired.document,
                )?;
                match docker_network_subnet(store.docker, joiner).await? {
                    DockerNetworkObservation::Present(docker_subnet)
                        if docker_subnet == replacement =>
                    {
                        let winner = roster
                            .get(&founder_row.id)
                            .ok_or_else(|| "collision winner disappeared".to_owned())?;
                        return require(
                            machine_subnet(&winner.document)? == collided,
                            "lower-ULID collision winner changed its subnet",
                        );
                    }
                    DockerNetworkObservation::Present(docker_subnet) => {
                        last = format!(
                            "row repaired to {replacement}, but Docker network remains {docker_subnet}"
                        );
                    }
                    DockerNetworkObservation::Absent => {
                        last = format!(
                            "row repaired to {replacement}, but Docker network is temporarily absent"
                        );
                    }
                }
            } else {
                last = "higher-ULID row still carries the duplicated subnet".to_owned();
            }
        }
        tokio::time::sleep(WAIT_DELAY).await;
    }
    Err(format!(
        "higher-ULID collision loser did not repair row and Docker network: {last}"
    ))
}

fn assert_only_local_subnet_was_repaired(
    machine_id: &MachineRowId,
    before: &MachineDocument,
    repaired: &MachineDocument,
) -> Result<(), String> {
    let original_subnet = MachineEndpointSubnet::try_new(machine_subnet(before)?)
        .map_err(|error| error.to_string())?;
    let mut normalized = repaired.clone();
    match &mut normalized.transport {
        MachineTransport::Wireguard { subnet_v4, .. }
        | MachineTransport::Tailscale { subnet_v4, .. } => *subnet_v4 = original_subnet,
    }
    // Provenance necessarily records the machine-owned exception write. Once
    // that envelope and the repaired field are normalized, the entire typed
    // document must be byte-for-byte-equivalent at the data-model boundary.
    normalized.provenance = before.provenance.clone();
    require(
        normalized == *before,
        format!(
            "subnet self-heal changed another machine field: before={before:?} repaired={repaired:?}"
        ),
    )?;
    require(
        matches!(
            &repaired.provenance.written_by,
            Principal::Machine { machine_id: writer } if writer == machine_id
        ),
        "subnet self-heal was not authored by the losing machine",
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DockerNetworkObservation {
    Present(String),
    Absent,
}

async fn docker_network_subnet(
    docker: &Docker,
    machine: &DindMachine,
) -> Result<DockerNetworkObservation, String> {
    let outcome = exec_in_container(
        docker,
        &machine.container_id,
        &[
            "docker",
            "network",
            "inspect",
            "ployz",
            "--format",
            "{{(index .IPAM.Config 0).Subnet}}",
        ],
    )
    .await
    .map_err(|error| error.to_string())?;
    classify_docker_network_inspect(&outcome)
}

fn classify_docker_network_inspect(
    outcome: &ExecOutcome,
) -> Result<DockerNetworkObservation, String> {
    if outcome.success() {
        return Ok(DockerNetworkObservation::Present(
            outcome.stdout.trim().to_owned(),
        ));
    }
    if outcome.stderr.trim() == "Error response from daemon: network ployz not found" {
        return Ok(DockerNetworkObservation::Absent);
    }
    Err(format!(
        "Docker network inspect failed: exit {} stdout={:?} stderr={:?}",
        outcome.exit_code,
        outcome.stdout.trim(),
        outcome.stderr.trim()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_missing_network_is_a_transient_absent_observation() {
        let outcome = ExecOutcome {
            exit_code: 1,
            stdout: String::new(),
            stderr: "Error response from daemon: network ployz not found\n".to_owned(),
        };

        assert_eq!(
            classify_docker_network_inspect(&outcome),
            Ok(DockerNetworkObservation::Absent)
        );
    }

    #[test]
    fn successful_inspect_preserves_the_exact_observed_subnet() {
        let outcome = ExecOutcome {
            exit_code: 0,
            stdout: "10.210.42.0/24\n".to_owned(),
            stderr: String::new(),
        };

        assert_eq!(
            classify_docker_network_inspect(&outcome),
            Ok(DockerNetworkObservation::Present(
                "10.210.42.0/24".to_owned()
            ))
        );
    }

    #[test]
    fn every_other_inspect_failure_remains_fatal() {
        let outcome = ExecOutcome {
            exit_code: 1,
            stdout: String::new(),
            stderr: "Error response from daemon: permission denied\n".to_owned(),
        };

        let error = classify_docker_network_inspect(&outcome)
            .expect_err("a non-transient Docker failure must remain fatal");
        assert!(error.contains("permission denied"));
    }
}
