use super::fixture::{
    CorrosionAccess, RosterMachine, corrosion_transaction, machine_subnet, wait_for_machine_roster,
};
use super::{WAIT_BUDGET, WAIT_DELAY};
use bollard::Docker;
use ployz_core::corrosion::{MachineDocument, MachineTransport, Principal};
use ployz_core::ids::MachineRowId;
use ployz_core::network::MachineEndpointSubnet;
use ployz_e2e::dind::{DindMachine, exec_ok, require};
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
                let docker_subnet = docker_network_subnet(store.docker, joiner).await?;
                if docker_subnet == replacement {
                    let winner = roster
                        .get(&founder_row.id)
                        .ok_or_else(|| "collision winner disappeared".to_owned())?;
                    return require(
                        machine_subnet(&winner.document)? == collided,
                        "lower-ULID collision winner changed its subnet",
                    );
                }
                last = format!(
                    "row repaired to {replacement}, but Docker network remains {docker_subnet}"
                );
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

async fn docker_network_subnet(docker: &Docker, machine: &DindMachine) -> Result<String, String> {
    let outcome = exec_ok(
        docker,
        machine,
        &[
            "docker",
            "network",
            "inspect",
            "ployz",
            "--format",
            "{{(index .IPAM.Config 0).Subnet}}",
        ],
    )
    .await?;
    Ok(outcome.stdout.trim().to_owned())
}
