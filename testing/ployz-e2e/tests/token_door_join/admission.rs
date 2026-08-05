use super::fixture::{
    CorrosionAccess, corrosion_transaction, extract_join_blob, extract_token_id, machine_subnet,
    require_success, run_cli, wait_for_command,
};
use bollard::Docker;
use ployz::JoinDoorClient;
use ployz::init::ssh::SshPeerKey;
use ployz_core::corrosion::{
    CorrosionTimestamp, MachineDocument, PeerDocument, SqliteValue, StorageMode, TokenDocument,
};
use ployz_core::ids::{MachineRowId, TokenId};
use ployz_core::join::{
    JoinBlob, JoinDoorCertFingerprint, JoinDoorRefusal, JoinStorageChoice, JoinStorageFacts,
    MachineJoinRequest, PeerJoinRequest,
};
use ployz_core::machine::MachineName;
use ployz_core::network::DEFAULT_WIREGUARD_LISTEN_PORT;
use ployz_e2e::dind::{DindMachine, ExecOutcome, corrosion_query, exec_in_container, require};
use serde_json::json;
use std::net::SocketAddr;
use std::path::Path;

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
    .await
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
    let refused = JoinDoorClient::default()
        .admit_peer(&revoked_blob, peer_request("revoked-proof")?)
        .await
        .expect_err("revoked token must be refused");
    require(
        matches!(
            refused,
            ployz::JoinDoorClientError::Refused {
                refusal: JoinDoorRefusal::TokenNotFound { .. }
            }
        ),
        format!("revoked token returned the wrong refusal: {refused:?}"),
    )
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
