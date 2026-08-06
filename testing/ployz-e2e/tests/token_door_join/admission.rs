use super::fixture::{
    CorrosionAccess, corrosion_transaction, extract_join_blob, extract_token_id, machine_subnet,
    require_success, run_cli, wait_for_command,
};
use super::{WAIT_BUDGET, WAIT_DELAY};
use bollard::Docker;
use ployz::JoinDoorClient;
use ployz::init::ssh::SshPeerKey;
use ployz_core::corrosion::{
    CorrosionTimestamp, MachineDocument, PeerDocument, SqliteValue, StorageMode, TokenDocument,
};
use ployz_core::ids::{MachineRowId, TokenId};
use ployz_core::join::{
    JoinBlob, JoinDoorCertFingerprint, JoinDoorRefusal, JoinStorageChoice, JoinStorageFacts,
    MachineJoinRequest, PeerJoinRequest, ValidatedPeerJoinAccepted,
};
use ployz_core::machine::MachineName;
use ployz_core::network::DEFAULT_WIREGUARD_LISTEN_PORT;
use ployz_e2e::dind::{
    DindMachine, ExecOutcome, assert_keeper_isolation_root, corrosion_query, exec_in_container,
    require,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Instant;
use tokio::task::JoinSet;

pub(super) async fn assert_wrong_door_fingerprint_is_rejected(
    blob: &JoinBlob,
) -> Result<(), String> {
    let wrong =
        JoinDoorCertFingerprint::try_new("0".repeat(64)).map_err(|error| error.to_string())?;
    require(
        &wrong != blob.door_cert_fingerprint(),
        "fixture fingerprint unexpectedly matched the real door",
    )?;
    let altered = JoinBlob::try_new(
        blob.token_id().clone(),
        blob.secret().clone(),
        wrong,
        blob.endpoints().to_vec(),
    )
    .map_err(|error| error.to_string())?;
    let error = JoinDoorClient::default()
        .admit_peer(&altered, peer_request("wrong-fingerprint")?)
        .await
        .expect_err("a join blob with the wrong fingerprint must not reach HTTP");
    let ployz::JoinDoorClientError::AllEndpointsFailed { attempts } = error else {
        return Err(format!(
            "wrong fingerprint returned the wrong client failure: {error:?}"
        ));
    };
    require(
        !attempts.is_empty()
            && attempts.iter().all(|attempt| {
                matches!(
                    &attempt.failure,
                    ployz::join_client::JoinDoorAttemptFailure::FingerprintMismatch
                )
            }),
        format!("wrong fingerprint was not rejected by every door: {attempts:?}"),
    )
}

pub(super) async fn assert_token_row_is_hash_only(
    store: CorrosionAccess<'_>,
    token_id: &TokenId,
    blob: &JoinBlob,
) -> Result<(), String> {
    let rows = corrosion_query(
        store.docker,
        store.machine,
        store.address,
        store.token,
        &format!(
            "SELECT document FROM tokens WHERE id = '{}'",
            token_id.as_str()
        ),
    )
    .await?;
    let [row] = rows.as_slice() else {
        return Err(format!("expected one token row, found {rows:?}"));
    };
    let [SqliteValue::Text(document)] = row.as_slice() else {
        return Err(format!("token query returned an invalid row: {row:?}"));
    };
    let parsed: TokenDocument = serde_json::from_str(document)
        .map_err(|error| format!("token row was invalid: {error}"))?;
    require(
        parsed.secret_sha256.as_str().len() == 64
            && !document.contains(&blob.secret().expose_base64()),
        "token row retained plaintext join material",
    )
}

pub(super) async fn assert_foreign_machine_refuses(
    docker: &Docker,
    machine: &DindMachine,
    blob: &JoinBlob,
) -> Result<(), String> {
    ployz_e2e::dind::exec_ok(docker, machine, &["mkdir", "-p", "/var/lib/ployz"]).await?;
    ployz_e2e::dind::exec_ok(
        docker,
        machine,
        &["touch", "/var/lib/ployz/founding-complete"],
    )
    .await?;
    let endpoint = SocketAddr::new(machine.bridge_ip, DEFAULT_WIREGUARD_LISTEN_PORT);
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
        format!("foreign machine did not return the typed reset refusal: {outcome:?}"),
    )?;
    let residue = exec_in_container(
        docker,
        &machine.container_id,
        &[
            "sh",
            "-c",
            "test ! -e /var/lib/ployz/join-identity.json && test ! -e /var/lib/ployz/join-acceptance.json",
        ],
    )
    .await
    .map_err(|error| error.to_string())?;
    require(residue.success(), "foreign refusal persisted join identity")
}

pub(super) async fn join_fresh_machine(
    docker: &Docker,
    machine: &DindMachine,
    blob: &JoinBlob,
) -> Result<(), String> {
    let endpoint = SocketAddr::new(machine.bridge_ip, DEFAULT_WIREGUARD_LISTEN_PORT);
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
        outcome.success() && outcome.stdout.contains("Joined machine"),
        format!("fresh machine join failed: {outcome:?}"),
    )?;
    wait_for_command(
        docker,
        machine,
        &[
            "systemctl",
            "is-active",
            "ployz-corrosion.service",
            "ployzd-keeper.service",
            "ployzd-api.service",
        ],
        ExecOutcome::success,
        "joined substrate services",
    )
    .await?;
    assert_keeper_isolation_root(docker, machine, "ployzd-keeper.service").await
}

pub(super) async fn wait_for_joined_reachability(
    docker: &Docker,
    founder: &DindMachine,
    joined: &MachineDocument,
) -> Result<(), String> {
    let ployz_core::corrosion::MachineTransport::Wireguard { addr_v6, .. } = joined.transport
    else {
        return Err("joined machine did not use builtin WireGuard".to_owned());
    };
    let url = format!("http://[{addr_v6}]:2020/version");
    wait_for_command(
        docker,
        founder,
        &[
            "curl",
            "--noproxy",
            "*",
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "3",
            &url,
        ],
        ExecOutcome::success,
        "joined API over the WireGuard mesh",
    )
    .await
}

pub(super) async fn admit_roaming_peer_and_assert_no_subnet(
    store: CorrosionAccess<'_>,
    blob: &JoinBlob,
) -> Result<(), String> {
    let peer =
        SshPeerKey::generate("roaming peer".to_owned()).map_err(|error| error.to_string())?;
    let request = PeerJoinRequest {
        peer_id: peer.peer_id.clone(),
        name: peer.peer_name.clone(),
        public_key: peer.public_key.clone(),
        endpoint: None,
    };
    let accepted = JoinDoorClient::default()
        .admit_peer(blob, request)
        .await
        .map_err(|error| error.to_string())?;
    let encoded = serde_json::to_string(&accepted.accepted().peer.document)
        .map_err(|error| error.to_string())?;
    require(
        !encoded.contains("subnet_v4"),
        "roaming peer acceptance carried a machine endpoint subnet",
    )?;
    let rows = corrosion_query(
        store.docker,
        store.machine,
        store.address,
        store.token,
        &format!(
            "SELECT document FROM peers WHERE id = '{}'",
            peer.peer_id.as_str()
        ),
    )
    .await?;
    let [row] = rows.as_slice() else {
        return Err(format!("roaming peer row was not written: {rows:?}"));
    };
    let [SqliteValue::Text(document)] = row.as_slice() else {
        return Err(format!(
            "roaming peer query returned an invalid row: {row:?}"
        ));
    };
    let _: PeerDocument = serde_json::from_str(document)
        .map_err(|error| format!("roaming peer row was invalid: {error}"))?;
    require(
        !document.contains("subnet_v4"),
        "roaming peer row carried a machine endpoint subnet",
    )
}

pub(super) async fn admit_concurrent_machines_with_distinct_subnets(
    blob: &JoinBlob,
) -> Result<(), String> {
    let left = machine_request("concurrent-a")?;
    let right = machine_request("concurrent-b")?;
    let client = JoinDoorClient::default();
    let (left_reply, right_reply) = tokio::join!(
        client.admit_machine(blob, left),
        client.admit_machine(blob, right)
    );
    let left = left_reply.map_err(|error| error.to_string())?;
    let right = right_reply.map_err(|error| error.to_string())?;
    require(
        machine_subnet(&left.accepted().machine.document)?
            != machine_subnet(&right.accepted().machine.document)?,
        "concurrent machine admissions allocated the same endpoint subnet",
    )
}

fn machine_request(name: &str) -> Result<MachineJoinRequest, String> {
    let key = SshPeerKey::generate(name.to_owned()).map_err(|error| error.to_string())?;
    Ok(MachineJoinRequest {
        machine_id: MachineRowId::generate(),
        name: MachineName::try_new(name).map_err(|error| error.to_string())?,
        public_key: key.public_key,
        endpoint: None,
        storage_choice: JoinStorageChoice::Flag {
            mode: StorageMode::Plain,
        },
        storage_facts: JoinStorageFacts {
            imported_zfs_pool: false,
            total_memory_bytes: 1024 * 1024 * 1024,
        },
    })
}

pub(super) async fn assert_revoked_and_expired_refusals(
    store: CorrosionAccess<'_>,
    cli: &Path,
    home: &Path,
    live_blob: JoinBlob,
    live_token_id: TokenId,
) -> Result<(), String> {
    expire_token_row(store, &live_token_id).await?;
    let expired = JoinDoorClient::default()
        .admit_peer(&live_blob, peer_request("expired-proof")?)
        .await
        .expect_err("expired token must be refused");
    require(
        matches!(
            expired,
            ployz::JoinDoorClientError::Refused {
                refusal: JoinDoorRefusal::TokenExpired { .. }
            }
        ),
        format!("expired token returned the wrong refusal: {expired:?}"),
    )?;
    let list = run_cli(cli, home, ["token", "list"].map(str::to_owned))?;
    require_success(&list, "live token list after expiry")?;
    require(
        !String::from_utf8_lossy(&list.stdout).contains(live_token_id.as_str()),
        "default token list retained an expired row",
    )?;
    let all = run_cli(cli, home, ["token", "list", "--all"].map(str::to_owned))?;
    require_success(&all, "all token list after expiry")?;
    require(
        String::from_utf8_lossy(&all.stdout).contains(live_token_id.as_str()),
        "token list --all omitted an expired row",
    )?;

    let created = run_cli(
        cli,
        home,
        ["token", "create", "--ttl", "1h"].map(str::to_owned),
    )?;
    require_success(&created, "second token create")?;
    let stdout = String::from_utf8_lossy(&created.stdout);
    let revoked_blob = extract_join_blob(&stdout)?;
    let revoked_id = extract_token_id(&stdout)?;
    let revoked = run_cli(
        cli,
        home,
        [
            "token".to_owned(),
            "revoke".to_owned(),
            revoked_id.to_string(),
        ],
    )?;
    require_success(&revoked, "token revoke")?;
    wait_for_revoked_token_refusals(&revoked_blob, peer_request("revoked-proof")?).await
}

struct RevokedDoorProbe {
    endpoint: SocketAddr,
    blob: JoinBlob,
    last_observation: Option<RevokedDoorObservation>,
}

#[derive(Debug)]
enum RevokedDoorObservation {
    Accepted(String),
    Refused(JoinDoorRefusal),
    ClientError(ployz::JoinDoorClientError),
    ProbeTaskError(tokio::task::JoinError),
    SharedDeadlineElapsed,
}

impl RevokedDoorObservation {
    fn describe(&self) -> String {
        match self {
            Self::Accepted(accepted) => format!("accepted: {accepted}"),
            Self::Refused(refusal) => format!("refused: {refusal:?}"),
            Self::ClientError(error) => format!("client error: {error:?}"),
            Self::ProbeTaskError(error) => format!("probe task error: {error:?}"),
            Self::SharedDeadlineElapsed => "shared deadline elapsed during request".to_owned(),
        }
    }
}

async fn wait_for_revoked_token_refusals(
    revoked_blob: &JoinBlob,
    request: PeerJoinRequest,
) -> Result<(), String> {
    if revoked_blob.endpoints().is_empty() {
        return Err("revoked join blob advertised no join doors".to_owned());
    }
    let mut doors = Vec::with_capacity(revoked_blob.endpoints().len());
    for endpoint in revoked_blob.endpoints() {
        let blob = JoinBlob::try_new(
            revoked_blob.token_id().clone(),
            revoked_blob.secret().clone(),
            revoked_blob.door_cert_fingerprint().clone(),
            vec![*endpoint],
        )
        .map_err(|error| {
            format!("could not isolate advertised join door {endpoint} for revocation: {error}")
        })?;
        doors.push(RevokedDoorProbe {
            endpoint: *endpoint,
            blob,
            last_observation: None,
        });
    }

    let deadline = Instant::now() + WAIT_BUDGET;
    let timeout_deadline = tokio::time::Instant::from_std(deadline);
    let client = JoinDoorClient::default();
    loop {
        if Instant::now() >= deadline {
            break;
        }
        let mut round = JoinSet::new();
        let mut task_doors = BTreeMap::new();
        for (index, door) in doors.iter().enumerate() {
            if door_converged(door) {
                continue;
            }
            let blob = door.blob.clone();
            let request = request.clone();
            let task = round.spawn(async move {
                tokio::time::timeout_at(timeout_deadline, client.admit_peer(&blob, request)).await
            });
            task_doors.insert(task.id(), index);
        }

        while let Some(result) = round.join_next_with_id().await {
            match result {
                Ok((task_id, result)) => {
                    let Some(index) = task_doors.remove(&task_id) else {
                        return Err(format!(
                            "revocation probe completed with unknown task id {task_id}"
                        ));
                    };
                    let Some(door) = doors.get_mut(index) else {
                        return Err(format!(
                            "revocation probe task {task_id} was assigned to missing door index {index}"
                        ));
                    };
                    record_revocation_probe(door, result);
                }
                Err(error) => {
                    let task_id = error.id();
                    let Some(index) = task_doors.remove(&task_id) else {
                        return Err(format!(
                            "revocation probe failed with unknown task id {task_id}: {error:?}"
                        ));
                    };
                    let Some(door) = doors.get_mut(index) else {
                        return Err(format!(
                            "failed revocation probe task {task_id} was assigned to missing door index {index}"
                        ));
                    };
                    door.last_observation = Some(RevokedDoorObservation::ProbeTaskError(error));
                }
            }
        }
        if doors.iter().all(door_converged) {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        tokio::time::sleep(WAIT_DELAY.min(remaining)).await;
    }

    let observations = doors
        .iter()
        .map(|door| match &door.last_observation {
            Some(observation) => format!("{}: {}", door.endpoint, observation.describe()),
            None => format!("{}: not probed before shared deadline", door.endpoint),
        })
        .collect::<Vec<_>>()
        .join("; ");
    Err(format!(
        "revoked token did not converge to TokenNotFound at every advertised join door within {WAIT_BUDGET:?}: {observations}"
    ))
}

fn door_converged(door: &RevokedDoorProbe) -> bool {
    matches!(
        &door.last_observation,
        Some(RevokedDoorObservation::Refused(
            JoinDoorRefusal::TokenNotFound { .. }
        ))
    )
}

fn record_revocation_probe(
    door: &mut RevokedDoorProbe,
    result: Result<
        Result<ValidatedPeerJoinAccepted, ployz::JoinDoorClientError>,
        tokio::time::error::Elapsed,
    >,
) {
    match result {
        Ok(Ok(accepted)) => {
            door.last_observation = Some(RevokedDoorObservation::Accepted(format!("{accepted:?}")));
        }
        Ok(Err(ployz::JoinDoorClientError::Refused {
            refusal: JoinDoorRefusal::TokenNotFound { token_id },
        })) => {
            door.last_observation = Some(RevokedDoorObservation::Refused(
                JoinDoorRefusal::TokenNotFound { token_id },
            ));
        }
        Ok(Err(ployz::JoinDoorClientError::Refused { refusal })) => {
            door.last_observation = Some(RevokedDoorObservation::Refused(refusal));
        }
        Ok(Err(error @ ployz::JoinDoorClientError::NoAdvertisedEndpoints))
        | Ok(Err(error @ ployz::JoinDoorClientError::OverallTimedOut))
        | Ok(Err(error @ ployz::JoinDoorClientError::AllEndpointsFailed { .. }))
        | Ok(Err(error @ ployz::JoinDoorClientError::WrongAcceptanceKind))
        | Ok(Err(error @ ployz::JoinDoorClientError::InvalidAcceptance(_))) => {
            door.last_observation = Some(RevokedDoorObservation::ClientError(error));
        }
        Err(_) => {
            door.last_observation = Some(RevokedDoorObservation::SharedDeadlineElapsed);
        }
    }
}

fn peer_request(name: &str) -> Result<PeerJoinRequest, String> {
    let peer = SshPeerKey::generate(name.to_owned()).map_err(|error| error.to_string())?;
    Ok(PeerJoinRequest {
        peer_id: peer.peer_id,
        name: name.to_owned(),
        public_key: peer.public_key,
        endpoint: None,
    })
}

async fn expire_token_row(store: CorrosionAccess<'_>, token_id: &TokenId) -> Result<(), String> {
    let rows = corrosion_query(
        store.docker,
        store.machine,
        store.address,
        store.token,
        &format!(
            "SELECT document FROM tokens WHERE id = '{}'",
            token_id.as_str()
        ),
    )
    .await?;
    let [row] = rows.as_slice() else {
        return Err(format!("token to expire was missing: {rows:?}"));
    };
    let [SqliteValue::Text(document)] = row.as_slice() else {
        return Err(format!("token query returned an invalid row: {row:?}"));
    };
    let mut document: TokenDocument =
        serde_json::from_str(document).map_err(|error| error.to_string())?;
    document.expires_at =
        CorrosionTimestamp::try_new("2020-01-01T00:00:00Z").map_err(|error| error.to_string())?;
    let document = serde_json::to_string(&document).map_err(|error| error.to_string())?;
    corrosion_transaction(
        store,
        &json!([[
            "UPDATE tokens SET document = ? WHERE id = ?",
            [document, token_id.as_str().to_owned()]
        ]]),
    )
    .await
}
