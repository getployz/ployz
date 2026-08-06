use super::admission::join_fresh_machine;
use super::fixture::{
    CorrosionAccess, RosterMachine, corrosion_transaction, extract_join_blob,
    handoff_with_known_endpoint, require_success, run_cli, run_founding, wait_for_command,
    wait_for_machine_roster,
};
use super::{FOUNDER_NAME, WAIT_BUDGET, WAIT_DELAY};
use bollard::Docker;
use ployz::commands::SshTarget;
use ployz::init::ssh::SshPeerKey;
use ployz::mesh::context::OperatorContextStore;
use ployz_core::corrosion::{
    AcmeHttp01Document, CertHoldingDocument, ContainerDocument, CorrosionDocumentVersion,
    CorrosionOperation, CorrosionOperationState, CorrosionTimestamp, MachineLoadBand,
    MachineStatusDocument, MachineTransport, OperationDocument, Principal,
};
use ployz_core::ids::{NamespaceRowId, OperationRowId, ServiceRowId};
use ployz_core::join::JoinBlob;
use ployz_core::operation::RouteHostname;
use ployz_e2e::dind::{
    DindMachine, ExecOutcome, corrosion_query, exec_in_container, install_local_release_channel,
    require,
};
use serde_json::json;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::time::Instant;

const WORKLOAD_CONTAINER: &str = "ployz-reset-proof";
const WORKLOAD_VOLUME: &str = "ployz-reset-proof";
const WORKLOAD_IMAGE: &str = "nginx:1.27-alpine";
const FOREIGN_FINGERPRINT: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

pub(super) struct RepairContext<'a> {
    pub(super) docker: &'a Docker,
    pub(super) store: CorrosionAccess<'a>,
    pub(super) cli: &'a Path,
    pub(super) home: &'a Path,
    pub(super) founder: &'a DindMachine,
    pub(super) joiner: &'a DindMachine,
}

pub(super) struct RefoundContext<'a> {
    pub(super) docker: &'a Docker,
    pub(super) cli: &'a Path,
    pub(super) home: &'a Path,
    pub(super) config_home: &'a Path,
    pub(super) target: &'a SshTarget,
    pub(super) operator: &'a SshPeerKey,
    pub(super) founder: &'a DindMachine,
    pub(super) joiner: &'a DindMachine,
}

/// Exercises the fence-first repair kit from the public CLI boundary.
pub(super) async fn repair_contaminated_and_wiped_machine(
    context: RepairContext<'_>,
    initial_blob: &JoinBlob,
    initial_row: &RosterMachine,
) -> Result<RosterMachine, String> {
    contaminate_join_identity(context.docker, context.joiner).await?;
    assert_foreign_identity_refuses(context.docker, context.joiner, initial_blob).await?;

    let preserved_operation = seed_machine_evidence(context.store, initial_row).await?;
    start_workload(context.docker, context.joiner).await?;
    remove_machine(context.cli, context.home, initial_row, false)?;
    wait_for_removal(
        context.store,
        context.founder,
        initial_row,
        Some(&preserved_operation),
    )
    .await?;

    reset_machine(context.docker, context.joiner).await?;
    assert_workload_survived(context.docker, context.joiner).await?;

    let fresh_blob = create_join_blob(context.cli, context.home)?;
    join_fresh_machine(context.docker, context.joiner, &fresh_blob).await?;
    let rejoined =
        wait_for_machine_named(context.store, initial_row.document.name.as_str()).await?;
    require(
        rejoined.id != initial_row.id,
        "machine reset rejoined the fenced identity instead of minting a new one",
    )?;

    wipe_ployz_state(context.docker, context.joiner).await?;
    let rejected_blob = create_join_blob(context.cli, context.home)?;
    assert_join_refuses_with_corpse(context.docker, context.joiner, &rejected_blob).await?;

    remove_machine(context.cli, context.home, &rejoined, true)?;
    wait_for_removal(context.store, context.founder, &rejoined, None).await?;

    let replacement_blob = create_join_blob(context.cli, context.home)?;
    join_fresh_machine(context.docker, context.joiner, &replacement_blob).await?;
    let replacement =
        wait_for_machine_named(context.store, rejoined.document.name.as_str()).await?;
    require(
        replacement.id != rejoined.id,
        "wiped host rejoined its corpse identity instead of receiving a new one",
    )?;
    Ok(replacement)
}

/// Rebuilds the cluster from the documented reset, init, token, and join primitives.
pub(super) async fn refound_cluster(
    context: RefoundContext<'_>,
    previous_joiner: &RosterMachine,
) -> Result<(), String> {
    let previous_cluster_id = previous_joiner.document.cluster_id.clone();
    let joiner_name = previous_joiner.document.name.as_str().to_owned();

    reset_machine(context.docker, context.joiner).await?;
    assert_workload_survived(context.docker, context.joiner).await?;
    reset_machine(context.docker, context.founder).await?;
    assert_workload_survived(context.docker, context.joiner).await?;

    install_local_release_channel(context.docker, context.founder).await?;
    let handoff = run_founding(context.docker, context.founder, context.operator).await?;
    let founder_endpoint = SocketAddr::new(
        context.founder.bridge_ip,
        ployz_core::network::DEFAULT_WIREGUARD_LISTEN_PORT,
    );
    let handoff = handoff_with_known_endpoint(handoff, founder_endpoint)?;
    OperatorContextStore::new(context.config_home)
        .persist(context.target, handoff, context.operator)
        .map_err(|error| error.to_string())?;
    let endpoint_set = run_cli(
        context.cli,
        context.home,
        [
            "machine".to_owned(),
            "endpoint".to_owned(),
            "set".to_owned(),
            FOUNDER_NAME.to_owned(),
            founder_endpoint.to_string(),
        ],
    )?;
    require_success(&endpoint_set, "refounded machine endpoint set")?;

    let blob = create_join_blob(context.cli, context.home)?;
    join_fresh_machine(context.docker, context.joiner, &blob).await?;
    let (corrosion_address, corrosion_token) =
        ployz_e2e::dind::corrosion_access(context.docker, context.founder).await?;
    let store = CorrosionAccess {
        docker: context.docker,
        machine: context.founder,
        address: &corrosion_address,
        token: &corrosion_token,
    };
    let roster = wait_for_machine_roster(store, 2).await?;
    let founder_row = roster
        .values()
        .find(|row| row.document.name.as_str() == FOUNDER_NAME)
        .ok_or_else(|| "refounded roster omitted machine one".to_owned())?;
    let joiner_row = roster
        .values()
        .find(|row| row.document.name.as_str() == joiner_name)
        .ok_or_else(|| "refounded roster omitted the reset joiner".to_owned())?;
    require(
        founder_row.document.cluster_id == joiner_row.document.cluster_id
            && founder_row.document.cluster_id != previous_cluster_id,
        "refound did not mint one new cluster identity for the rebuilt roster",
    )?;
    assert_workload_survived(context.docker, context.joiner).await
}

async fn contaminate_join_identity(docker: &Docker, machine: &DindMachine) -> Result<(), String> {
    let program = format!(
        "import json\nfrom pathlib import Path\npath = Path('/var/lib/ployz/join-identity.json')\nidentity = json.loads(path.read_text())\nidentity['door_fingerprint'] = '{FOREIGN_FINGERPRINT}'\npath.write_text(json.dumps(identity))\n"
    );
    let outcome = exec_in_container(docker, &machine.container_id, &["python3", "-c", &program])
        .await
        .map_err(|error| error.to_string())?;
    require(
        outcome.success(),
        format!("could not contaminate joined identity: {outcome:?}"),
    )
}

async fn assert_foreign_identity_refuses(
    docker: &Docker,
    machine: &DindMachine,
    blob: &JoinBlob,
) -> Result<(), String> {
    let endpoint = SocketAddr::new(
        machine.bridge_ip,
        ployz_core::network::DEFAULT_WIREGUARD_LISTEN_PORT,
    );
    let outcome = exec_in_container(
        docker,
        &machine.container_id,
        &[
            "/opt/ployz/artifacts/ployz",
            "machine",
            "join",
            blob.expose(),
            "--storage",
            "plain",
            "--wireguard-endpoint",
            &endpoint.to_string(),
        ],
    )
    .await
    .map_err(|error| error.to_string())?;
    require(
        !outcome.success() && outcome.stderr.contains("ployz machine reset"),
        format!("foreign identity did not name on-host reset: {outcome:?}"),
    )
}

fn create_join_blob(cli: &Path, home: &Path) -> Result<JoinBlob, String> {
    let created = run_cli(
        cli,
        home,
        ["token", "create", "--ttl", "1h"].map(str::to_owned),
    )?;
    require_success(&created, "repair token create")?;
    extract_join_blob(&String::from_utf8_lossy(&created.stdout))
}

fn remove_machine(
    cli: &Path,
    home: &Path,
    machine: &RosterMachine,
    include_id: bool,
) -> Result<(), String> {
    let mut arguments = vec![
        "machine".to_owned(),
        "rm".to_owned(),
        machine.document.name.as_str().to_owned(),
    ];
    if include_id {
        arguments.extend(["--id".to_owned(), machine.id.to_string()]);
    }
    let removed = run_cli(cli, home, arguments)?;
    require_success(&removed, "machine rm")
}

async fn seed_machine_evidence(
    store: CorrosionAccess<'_>,
    machine: &RosterMachine,
) -> Result<OperationRowId, String> {
    let timestamp =
        CorrosionTimestamp::try_new("2026-08-05T12:00:00Z").map_err(|error| error.to_string())?;
    let namespace_id = NamespaceRowId::generate();
    let service_id = ServiceRowId::generate();
    let operation_id = OperationRowId::generate();
    let status = MachineStatusDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: machine.document.cluster_id.clone(),
        machine_id: machine.id.clone(),
        ployz_version: "repair-proof".to_owned(),
        corrosion_version: "repair-proof".to_owned(),
        architecture: "x86_64".to_owned(),
        free_disk_bytes: 1,
        free_memory_bytes: 1,
        load: MachineLoadBand::Idle,
        observed_at: timestamp,
        mesh: None,
        container_isolation: None,
        wireguard_handshakes: None,
    };
    let container = ContainerDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: machine.document.cluster_id.clone(),
        machine_id: machine.id.clone(),
        service_id,
        namespace_id,
        ip: Ipv4Addr::new(10, 210, 250, 2),
        deploy: operation_id.clone(),
    };
    let hostname =
        RouteHostname::try_new("repair-proof.example.test").map_err(|error| error.to_string())?;
    let cert = CertHoldingDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: machine.document.cluster_id.clone(),
        machine_id: machine.id.clone(),
        hostname: hostname.clone(),
        fingerprint: ployz_core::corrosion::Sha256Hex::try_new("a".repeat(64))
            .map_err(|error| error.to_string())?,
        issued_at: timestamp,
        expires_at: CorrosionTimestamp::try_new("2026-11-05T12:00:00Z")
            .map_err(|error| error.to_string())?,
    };
    let acme = AcmeHttp01Document {
        v: CorrosionDocumentVersion::V1,
        cluster_id: machine.document.cluster_id.clone(),
        machine_id: machine.id.clone(),
        hostname,
        key_authorization: "repair-proof".to_owned(),
        created_at: timestamp,
    };
    let operation = OperationDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: machine.document.cluster_id.clone(),
        machine_id: machine.id.clone(),
        operation: CorrosionOperation::MachineRemove {
            target_machine_id: machine.id.clone(),
        },
        initiator: Principal::Machine {
            machine_id: machine.id.clone(),
        },
        status: CorrosionOperationState::Created {
            created_at: timestamp,
        },
    };
    let status = serde_json::to_string(&status).map_err(|error| error.to_string())?;
    let container = serde_json::to_string(&container).map_err(|error| error.to_string())?;
    let cert = serde_json::to_string(&cert).map_err(|error| error.to_string())?;
    let acme = serde_json::to_string(&acme).map_err(|error| error.to_string())?;
    let operation = serde_json::to_string(&operation).map_err(|error| error.to_string())?;
    let cert_id = format!("{}:repair-proof.example.test", machine.id.as_str());
    corrosion_transaction(
        store,
        &json!([
            [
                "INSERT INTO machine_status (machine_id, document) VALUES (?, ?) ON CONFLICT(machine_id) DO UPDATE SET document = excluded.document",
                [machine.id.as_str(), status]
            ],
            [
                "INSERT INTO containers (id, document) VALUES (?, ?)",
                ["repair-proof-container", container]
            ],
            [
                "INSERT INTO cert_holdings (id, document) VALUES (?, ?)",
                [cert_id, cert]
            ],
            [
                "INSERT INTO acme_http01 (id, document) VALUES (?, ?)",
                ["repair-proof-acme", acme]
            ],
            [
                "INSERT INTO operations (id, document) VALUES (?, ?)",
                [operation_id.as_str(), operation]
            ]
        ]),
    )
    .await?;
    let counts = evidence_counts(store, machine, &operation_id).await?;
    require(
        counts == [1, 1, 1, 1, 1, 1],
        format!("did not persist complete machine-removal evidence: {counts:?}"),
    )?;
    Ok(operation_id)
}

async fn start_workload(docker: &Docker, machine: &DindMachine) -> Result<(), String> {
    let command = format!(
        "set -eu; docker image inspect {WORKLOAD_IMAGE} >/dev/null; \
         docker volume create {WORKLOAD_VOLUME} >/dev/null; \
         docker run --detach --name {WORKLOAD_CONTAINER} \
         --mount type=volume,src={WORKLOAD_VOLUME},dst=/data {WORKLOAD_IMAGE} >/dev/null; \
         docker exec {WORKLOAD_CONTAINER} sh -c 'printf preserved > /data/proof'"
    );
    let outcome = exec_in_container(docker, &machine.container_id, &["sh", "-c", &command])
        .await
        .map_err(|error| error.to_string())?;
    require(
        outcome.success(),
        format!("could not start reset-proof workload: {outcome:?}"),
    )
}

async fn reset_machine(docker: &Docker, machine: &DindMachine) -> Result<(), String> {
    let outcome = exec_in_container(
        docker,
        &machine.container_id,
        &["/opt/ployz/artifacts/ployz", "machine", "reset"],
    )
    .await
    .map_err(|error| error.to_string())?;
    require(
        outcome.success() && outcome.stdout.contains("Join with a fresh token"),
        format!("on-host machine reset failed: {outcome:?}"),
    )?;
    let state = exec_in_container(
        docker,
        &machine.container_id,
        &[
            "sh",
            "-c",
            "test ! -e /var/lib/ployz/join-identity.json; test ! -e /var/lib/ployz/corrosion",
        ],
    )
    .await
    .map_err(|error| error.to_string())?;
    require(
        state.success(),
        format!("machine reset left Ployz state behind: {state:?}"),
    )
}

async fn assert_workload_survived(docker: &Docker, machine: &DindMachine) -> Result<(), String> {
    let command = format!(
        "set -eu; \
         test \"$(docker inspect --format '{{{{.State.Running}}}}' {WORKLOAD_CONTAINER})\" = true; \
         docker volume inspect {WORKLOAD_VOLUME} >/dev/null; \
         docker image inspect {WORKLOAD_IMAGE} >/dev/null; \
         test \"$(docker exec {WORKLOAD_CONTAINER} cat /data/proof)\" = preserved; \
         docker exec {WORKLOAD_CONTAINER} wget -qO- http://127.0.0.1 | grep -q 'Welcome to nginx'"
    );
    let outcome = exec_in_container(docker, &machine.container_id, &["sh", "-c", &command])
        .await
        .map_err(|error| error.to_string())?;
    require(
        outcome.success(),
        format!("machine reset disturbed the running Docker workload: {outcome:?}"),
    )
}

async fn wipe_ployz_state(docker: &Docker, machine: &DindMachine) -> Result<(), String> {
    let outcome = exec_in_container(
        docker,
        &machine.container_id,
        &[
            "sh",
            "-c",
            "set -eu; systemctl stop ployzd-keeper.service ployzd-api.service ployzd-gateway.service ployzd-dns.service ployz-corrosion.service; rm -rf /var/lib/ployz; test ! -e /var/lib/ployz",
        ],
    )
    .await
    .map_err(|error| error.to_string())?;
    require(
        outcome.success(),
        format!("could not simulate wiped Ployz disk: {outcome:?}"),
    )
}

async fn assert_join_refuses_with_corpse(
    docker: &Docker,
    machine: &DindMachine,
    blob: &JoinBlob,
) -> Result<(), String> {
    let endpoint = SocketAddr::new(
        machine.bridge_ip,
        ployz_core::network::DEFAULT_WIREGUARD_LISTEN_PORT,
    );
    let outcome = exec_in_container(
        docker,
        &machine.container_id,
        &[
            "/opt/ployz/artifacts/ployz",
            "machine",
            "join",
            blob.expose(),
            "--storage",
            "plain",
            "--wireguard-endpoint",
            &endpoint.to_string(),
        ],
    )
    .await
    .map_err(|error| error.to_string())?;
    require(
        !outcome.success() && outcome.stderr.contains("ployz machine rm"),
        format!("wiped host did not refuse its corpse with machine rm: {outcome:?}"),
    )
}

async fn wait_for_removal(
    store: CorrosionAccess<'_>,
    founder: &DindMachine,
    machine: &RosterMachine,
    preserved_operation: Option<&OperationRowId>,
) -> Result<(), String> {
    let MachineTransport::Wireguard { pubkey, .. } = &machine.document.transport else {
        return Err("repair proof requires builtin WireGuard machines".to_owned());
    };
    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last = String::from("removal state was not queried");
    while Instant::now() < deadline {
        match removal_evidence_counts(store, machine).await {
            Ok([0, 0, 0, 0, 0]) => {
                if let Some(operation_id) = preserved_operation {
                    let count = operation_count(store, operation_id).await?;
                    if count != 1 {
                        last = format!(
                            "machine removal swept operation {}: remaining count {count}",
                            operation_id.as_str()
                        );
                        tokio::time::sleep(WAIT_DELAY).await;
                        continue;
                    }
                }
                wait_for_command(
                    store.docker,
                    founder,
                    &["wg", "show", "ployz0", "dump"],
                    |outcome: &ExecOutcome| {
                        outcome.success() && !outcome.stdout.contains(pubkey.as_str())
                    },
                    "removed WireGuard peer",
                )
                .await?;
                return Ok(());
            }
            Ok(counts) => last = format!("remaining removal evidence: {counts:?}"),
            Err(error) => last = error,
        }
        tokio::time::sleep(WAIT_DELAY).await;
    }
    Err(format!("machine removal did not converge: {last}"))
}

async fn evidence_counts(
    store: CorrosionAccess<'_>,
    machine: &RosterMachine,
    operation_id: &OperationRowId,
) -> Result<[i64; 6], String> {
    let [machine, status, containers, cert_holdings, acme_http01] =
        removal_evidence_counts(store, machine).await?;
    let operation = operation_count(store, operation_id).await?;
    Ok([
        machine,
        status,
        containers,
        cert_holdings,
        acme_http01,
        operation,
    ])
}

async fn removal_evidence_counts(
    store: CorrosionAccess<'_>,
    machine: &RosterMachine,
) -> Result<[i64; 5], String> {
    let machine_id = machine.id.as_str();
    let statement = format!(
        "SELECT \
         (SELECT COUNT(*) FROM machines WHERE id = '{machine_id}'), \
         (SELECT COUNT(*) FROM machine_status WHERE machine_id = '{machine_id}'), \
         (SELECT COUNT(*) FROM containers WHERE machine_id = '{machine_id}'), \
         (SELECT COUNT(*) FROM cert_holdings WHERE machine_id = '{machine_id}'), \
         (SELECT COUNT(*) FROM acme_http01 WHERE machine_id = '{machine_id}')"
    );
    let rows = corrosion_query(
        store.docker,
        store.machine,
        store.address,
        store.token,
        &statement,
    )
    .await?;
    let [row] = rows.as_slice() else {
        return Err(format!("count query returned invalid rows: {rows:?}"));
    };
    let [
        ployz_core::corrosion::SqliteValue::Integer(machine),
        ployz_core::corrosion::SqliteValue::Integer(status),
        ployz_core::corrosion::SqliteValue::Integer(containers),
        ployz_core::corrosion::SqliteValue::Integer(cert_holdings),
        ployz_core::corrosion::SqliteValue::Integer(acme_http01),
    ] = row.as_slice()
    else {
        return Err(format!("count query returned invalid row: {row:?}"));
    };
    Ok([*machine, *status, *containers, *cert_holdings, *acme_http01])
}

async fn operation_count(
    store: CorrosionAccess<'_>,
    operation_id: &OperationRowId,
) -> Result<i64, String> {
    let rows = corrosion_query(
        store.docker,
        store.machine,
        store.address,
        store.token,
        &format!(
            "SELECT COUNT(*) FROM operations WHERE id = '{}'",
            operation_id.as_str()
        ),
    )
    .await?;
    let [row] = rows.as_slice() else {
        return Err(format!(
            "operation count query returned invalid rows: {rows:?}"
        ));
    };
    let [ployz_core::corrosion::SqliteValue::Integer(count)] = row.as_slice() else {
        return Err(format!(
            "operation count query returned invalid row: {row:?}"
        ));
    };
    Ok(*count)
}

async fn wait_for_machine_named(
    store: CorrosionAccess<'_>,
    name: &str,
) -> Result<RosterMachine, String> {
    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last = String::from("machine roster was not queried");
    while Instant::now() < deadline {
        match wait_for_machine_roster(store, 2).await {
            Ok(roster) => {
                if let Some(row) = roster
                    .into_values()
                    .find(|row| row.document.name.as_str() == name)
                {
                    return Ok(row);
                }
                last = format!("machine {name} was absent from the roster");
            }
            Err(error) => last = error,
        }
        tokio::time::sleep(WAIT_DELAY).await;
    }
    Err(format!("machine {name} did not rejoin: {last}"))
}
