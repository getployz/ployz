use bollard::Docker;
use ployz::JoinDoorClient;
use ployz::commands::SshTarget;
use ployz::init::ssh::{SshPeerKey, default_config_home};
use ployz::mesh::context::{OperatorContextStore, SSH_CONTEXT_HANDOFF_PREFIX, SshContextHandoff};
use ployz_core::corrosion::{
    CorrosionTimestamp, MachineDocument, MachineTransport, PeerDocument, Principal, SqliteValue,
    StorageMode, TokenDocument,
};
use ployz_core::ids::{MachineRowId, TokenId};
use ployz_core::join::{
    JoinBlob, JoinDoorCertFingerprint, JoinDoorRefusal, JoinStorageChoice, JoinStorageFacts,
    MachineEndpointSetReply, MachineEndpointSetRequest, MachineJoinRequest, PeerJoinRequest,
    TokenCreateRefusal,
};
use ployz_core::machine::MachineName;
use ployz_core::network::{DEFAULT_WIREGUARD_LISTEN_PORT, MachineEndpointSubnet};
use ployz_core::{MACHINE_ENDPOINT_ROUTE_PREFIX, TOKEN_CREATE_ROUTE};
use ployz_e2e::dind::{
    DindCluster, DindClusterSpec, DindMachine, ExecOutcome, MachineSpec, RELEASE_MANIFEST,
    artifact_dir, connect_docker, corrosion_access, corrosion_query, e2e_enabled, env_value,
    exec_in_container, exec_ok, install_local_release_channel, keep_requested, machine_image,
    require,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const CLUSTER_NAME: &str = "dind-token-door";
const FOUNDER_NAME: &str = "machine-one";
const WAIT_BUDGET: Duration = Duration::from_secs(60);
const WAIT_DELAY: Duration = Duration::from_millis(250);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_token_door_grows_the_cluster_and_repairs_a_surviving_collision() {
    if !e2e_enabled() {
        eprintln!("skipping token-door DinD proof; set PLOYZ_DIND_E2E=1 to enable it");
        return;
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    panic!("the pinned token-door proof supports only Linux x86_64");

    let docker = connect_docker().expect("connect to Docker for token-door proof");
    let cluster = DindCluster::provision(
        &docker,
        DindClusterSpec {
            artifact_dir: artifact_dir(),
            machines: vec![
                MachineSpec {
                    image: machine_image(),
                },
                MachineSpec {
                    image: machine_image(),
                },
                MachineSpec {
                    image: machine_image(),
                },
            ],
        },
    )
    .await
    .expect("provision token-door machines");

    let result = exercise_token_door(&docker, &cluster).await;
    if let Err(error) = &result {
        match cluster.capture_evidence().await {
            Ok(path) => eprintln!("token-door evidence captured under {}", path.display()),
            Err(capture_error) => eprintln!("token-door evidence capture failed: {capture_error}"),
        }
        eprintln!("token-door proof failed: {error}");
    }
    if keep_requested() {
        eprintln!(
            "retaining DinD run {} because PLOYZ_DIND_KEEP=1",
            cluster.run_id()
        );
    } else {
        cluster.teardown().await.expect("tear down token-door run");
    }
    result.unwrap_or_else(|error| panic!("token-door proof failed: {error}"));
}

async fn exercise_token_door(docker: &Docker, cluster: &DindCluster) -> Result<(), String> {
    let [founder, joiner, foreign] = cluster.machines() else {
        return Err("token-door proof requires exactly three isolated machines".to_owned());
    };
    install_local_release_channel(docker, founder).await?;

    let temporary_home = tempfile::tempdir().map_err(|error| error.to_string())?;
    let config_home = default_config_home(temporary_home.path());
    let target: SshTarget = format!("root@{}", founder.bridge_ip).parse()?;
    let operator =
        SshPeerKey::generate("dind operator".to_owned()).map_err(|error| error.to_string())?;
    operator
        .persist_new(&config_home, &target)
        .map_err(|error| error.to_string())?;

    let initial_handoff = run_founding(docker, founder, &operator).await?;
    let (corrosion_address, corrosion_token) = corrosion_access(docker, founder).await?;
    let api_address = founding_api_address(docker, founder).await?;
    assert_missing_endpoint_refuses_without_a_token(
        docker,
        founder,
        &api_address,
        &corrosion_address,
        &corrosion_token,
    )
    .await?;

    let founder_endpoint = SocketAddr::new(founder.bridge_ip, DEFAULT_WIREGUARD_LISTEN_PORT);
    // Normal SSH init infers this endpoint before it persists a laptop context.
    // This hidden driver-peer fixture deliberately omits it to exercise the
    // refusal, so it uses the same public HTTP mutation once to make that
    // context dialable; the shipped CLI call below remains the user-path proof.
    let endpoint_reply =
        set_endpoint_on_local_public_api(docker, founder, &api_address, founder_endpoint).await?;
    let handoff = SshContextHandoff {
        cluster_id: initial_handoff.cluster_id,
        provider: initial_handoff.provider,
        machine_transport: endpoint_reply.machine.transport,
    };
    OperatorContextStore::new(&config_home)
        .persist(&target, handoff, &operator)
        .map_err(|error| error.to_string())?;

    let cli = artifact_dir().join("ployz");
    let endpoint_set = run_cli(
        &cli,
        temporary_home.path(),
        [
            "machine".to_owned(),
            "endpoint".to_owned(),
            "set".to_owned(),
            FOUNDER_NAME.to_owned(),
            founder_endpoint.to_string(),
        ],
    )?;
    require_success(&endpoint_set, "machine endpoint set")?;

    let created = run_cli(
        &cli,
        temporary_home.path(),
        ["token", "create", "--ttl", "1h"].map(str::to_owned),
    )?;
    require_success(&created, "token create")?;
    let created_stdout = String::from_utf8_lossy(&created.stdout);
    let blob = extract_join_blob(&created_stdout)?;
    let token_id = extract_token_id(&created_stdout)?;
    let store = CorrosionAccess {
        docker,
        machine: founder,
        address: &corrosion_address,
        token: &corrosion_token,
    };
    require(
        created_stdout.matches(blob.expose()).count() == 1,
        "token create printed the join blob more than once",
    )?;
    assert_token_row_is_hash_only(
        docker,
        founder,
        &corrosion_address,
        &corrosion_token,
        &token_id,
        &blob,
    )
    .await?;
    let listed = run_cli(
        &cli,
        temporary_home.path(),
        ["token", "list"].map(str::to_owned),
    )?;
    require_success(&listed, "token list")?;
    require(
        String::from_utf8_lossy(&listed.stdout).contains(token_id.as_str()),
        "live token list omitted the created token",
    )?;

    assert_wrong_door_fingerprint_is_rejected(&blob).await?;
    assert_foreign_machine_refuses(docker, foreign, &blob).await?;
    join_fresh_machine(docker, joiner, &blob).await?;
    let roster =
        wait_for_machine_roster(docker, founder, &corrosion_address, &corrosion_token, 2).await?;
    let founder_row = roster
        .values()
        .find(|row| row.document.name.as_str() == FOUNDER_NAME)
        .ok_or_else(|| "joined roster omitted machine one".to_owned())?;
    let joiner_row = roster
        .values()
        .find(|row| row.document.name.as_str() != FOUNDER_NAME)
        .ok_or_else(|| "joined roster omitted the fresh machine".to_owned())?;
    require(
        machine_subnet(&founder_row.document)? != machine_subnet(&joiner_row.document)?,
        "fresh join reused machine one's endpoint subnet",
    )?;
    wait_for_joined_reachability(docker, founder, &joiner_row.document).await?;

    admit_roaming_peer_and_assert_no_subnet(
        docker,
        founder,
        &corrosion_address,
        &corrosion_token,
        &blob,
    )
    .await?;
    admit_concurrent_machines_with_distinct_subnets(&blob).await?;
    assert_revoked_and_expired_refusals(store, &cli, temporary_home.path(), blob, token_id).await?;

    force_collision_and_wait_for_higher_ulid_repair(store, joiner, founder_row, joiner_row).await
}

async fn assert_wrong_door_fingerprint_is_rejected(blob: &JoinBlob) -> Result<(), String> {
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

#[derive(Clone)]
struct RosterMachine {
    id: MachineRowId,
    document: MachineDocument,
}

#[derive(Clone, Copy)]
struct CorrosionAccess<'a> {
    docker: &'a Docker,
    machine: &'a DindMachine,
    address: &'a str,
    token: &'a str,
}

async fn run_founding(
    docker: &Docker,
    machine: &DindMachine,
    operator: &SshPeerKey,
) -> Result<SshContextHandoff, String> {
    let manifest = format!("file://{RELEASE_MANIFEST}");
    let command = [
        "env".to_owned(),
        format!("PLOYZ_RELEASE_MANIFEST_URL={manifest}"),
        "/opt/ployz/artifacts/ployz".to_owned(),
        "init".to_owned(),
        "--storage".to_owned(),
        "plain".to_owned(),
        "--container-network".to_owned(),
        "10.210.0.0/16".to_owned(),
        "--service-urls".to_owned(),
        "disabled".to_owned(),
        "--cluster-name".to_owned(),
        CLUSTER_NAME.to_owned(),
        "--machine-name".to_owned(),
        FOUNDER_NAME.to_owned(),
        "--driver-peer-id".to_owned(),
        operator.peer_id.to_string(),
        "--driver-peer-name".to_owned(),
        operator.peer_name.clone(),
        "--driver-peer-public-key".to_owned(),
        operator.public_key.as_str().to_owned(),
    ];
    let refs = command.iter().map(String::as_str).collect::<Vec<_>>();
    let outcome = exec_in_container(docker, &machine.container_id, &refs)
        .await
        .map_err(|error| error.to_string())?;
    require(
        outcome.success(),
        format!("founding command failed: {outcome:?}"),
    )?;
    parse_handoff(&outcome.stdout)
}

fn parse_handoff(stdout: &str) -> Result<SshContextHandoff, String> {
    let mut handoffs = stdout
        .lines()
        .filter(|line| line.starts_with(SSH_CONTEXT_HANDOFF_PREFIX));
    let Some(line) = handoffs.next() else {
        return Err(format!(
            "founding output omitted SSH context handoff: {stdout}"
        ));
    };
    require(
        handoffs.next().is_none(),
        "founding emitted more than one SSH context handoff",
    )?;
    SshContextHandoff::decode_handoff(line).map_err(|error| error.to_string())
}

async fn founding_api_address(docker: &Docker, machine: &DindMachine) -> Result<String, String> {
    let env = exec_ok(docker, machine, &["cat", "/var/lib/ployz/ployzd.env"])
        .await?
        .stdout;
    env_value(&env, "PLOYZ_API_LISTEN_ADDR")
}

async fn assert_missing_endpoint_refuses_without_a_token(
    docker: &Docker,
    machine: &DindMachine,
    api_address: &str,
    corrosion_address: &str,
    corrosion_token: &str,
) -> Result<(), String> {
    let refusal: TokenCreateRefusal = local_api_json(
        docker,
        machine,
        api_address,
        TOKEN_CREATE_ROUTE,
        &json!({"ttl_seconds": 3600}),
    )
    .await?;
    let TokenCreateRefusal::NoAdvertisedDoorEndpoint { repair_command } = refusal else {
        return Err(format!(
            "missing endpoint returned the wrong refusal: {refusal:?}"
        ));
    };
    require(
        repair_command == "ployz machine endpoint set <machine> <ip:wireguard-port>",
        format!("token refusal named the wrong repair command: {repair_command}"),
    )?;
    let rows = corrosion_query(
        docker,
        machine,
        corrosion_address,
        corrosion_token,
        "SELECT COUNT(*) FROM tokens",
    )
    .await?;
    require(
        rows == vec![vec![SqliteValue::Integer(0)]],
        format!("refused token create wrote a row: {rows:?}"),
    )
}

async fn set_endpoint_on_local_public_api(
    docker: &Docker,
    machine: &DindMachine,
    api_address: &str,
    endpoint: SocketAddr,
) -> Result<MachineEndpointSetReply, String> {
    let reply: MachineEndpointSetReply = local_api_json(
        docker,
        machine,
        api_address,
        MACHINE_ENDPOINT_ROUTE_PREFIX,
        &MachineEndpointSetRequest {
            machine_name: MachineName::try_new(FOUNDER_NAME).map_err(|error| error.to_string())?,
            endpoint,
        },
    )
    .await?;
    require(
        matches!(&reply.machine.transport, MachineTransport::Wireguard { endpoint: Some(found), .. } if *found == endpoint),
        "endpoint-set reply did not carry the requested endpoint",
    )?;
    Ok(reply)
}

async fn local_api_json<Request, Reply>(
    docker: &Docker,
    machine: &DindMachine,
    api_address: &str,
    route: &str,
    request: &Request,
) -> Result<Reply, String>
where
    Request: Serialize + ?Sized,
    Reply: DeserializeOwned,
{
    let body = serde_json::to_string(request).map_err(|error| error.to_string())?;
    let url = format!("http://{api_address}{route}");
    let outcome = exec_ok(
        docker,
        machine,
        &[
            "curl",
            "--noproxy",
            "*",
            "--silent",
            "--show-error",
            "--max-time",
            "5",
            "--request",
            "POST",
            "--header",
            "Content-Type: application/json",
            "--data-binary",
            &body,
            &url,
        ],
    )
    .await?;
    serde_json::from_str(outcome.stdout.trim()).map_err(|error| {
        format!(
            "{route} returned invalid JSON ({error}): {}",
            outcome.stdout
        )
    })
}

fn run_cli(
    cli: &Path,
    home: &Path,
    args: impl IntoIterator<Item = String>,
) -> Result<Output, String> {
    Command::new(cli)
        .args(args)
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("could not run shipped CLI {}: {error}", cli.display()))
}

fn require_success(output: &Output, operation: &str) -> Result<(), String> {
    require(
        output.status.success(),
        format!(
            "{operation} failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

fn extract_join_blob(stdout: &str) -> Result<JoinBlob, String> {
    let value = stdout
        .lines()
        .find_map(|line| {
            line.strip_prefix("JOIN_BLOB='")
                .and_then(|value| value.strip_suffix('\''))
        })
        .ok_or_else(|| format!("token create output omitted JOIN_BLOB: {stdout}"))?;
    JoinBlob::try_parse(value).map_err(|error| error.to_string())
}

fn extract_token_id(stdout: &str) -> Result<TokenId, String> {
    let value = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("token  "))
        .and_then(|line| line.split_whitespace().next())
        .ok_or_else(|| format!("token create output omitted token id: {stdout}"))?;
    TokenId::try_new(value).map_err(|error| error.to_string())
}

async fn assert_token_row_is_hash_only(
    docker: &Docker,
    machine: &DindMachine,
    corrosion_address: &str,
    corrosion_token: &str,
    token_id: &TokenId,
    blob: &JoinBlob,
) -> Result<(), String> {
    let rows = corrosion_query(
        docker,
        machine,
        corrosion_address,
        corrosion_token,
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

async fn assert_foreign_machine_refuses(
    docker: &Docker,
    machine: &DindMachine,
    blob: &JoinBlob,
) -> Result<(), String> {
    exec_ok(docker, machine, &["mkdir", "-p", "/var/lib/ployz"]).await?;
    exec_ok(
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

async fn join_fresh_machine(
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

async fn wait_for_machine_roster(
    docker: &Docker,
    machine: &DindMachine,
    corrosion_address: &str,
    corrosion_token: &str,
    minimum: usize,
) -> Result<BTreeMap<MachineRowId, RosterMachine>, String> {
    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last = String::from("roster was not queried");
    while Instant::now() < deadline {
        match corrosion_query(
            docker,
            machine,
            corrosion_address,
            corrosion_token,
            "SELECT id, document FROM machines",
        )
        .await
        {
            Ok(rows) => match parse_machine_rows(rows) {
                Ok(roster) if roster.len() >= minimum => return Ok(roster),
                Ok(roster) => last = format!("only {} machine rows", roster.len()),
                Err(error) => last = error,
            },
            Err(error) => last = error,
        }
        tokio::time::sleep(WAIT_DELAY).await;
    }
    Err(format!("machine roster did not converge: {last}"))
}

fn parse_machine_rows(
    rows: Vec<Vec<SqliteValue>>,
) -> Result<BTreeMap<MachineRowId, RosterMachine>, String> {
    let mut roster = BTreeMap::new();
    for row in rows {
        let [SqliteValue::Text(id), SqliteValue::Text(document)] = row.as_slice() else {
            return Err(format!("machine query returned an invalid row: {row:?}"));
        };
        let id = MachineRowId::try_new(id.clone()).map_err(|error| error.to_string())?;
        let document: MachineDocument = serde_json::from_str(document)
            .map_err(|error| format!("machine row was invalid: {error}"))?;
        roster.insert(id.clone(), RosterMachine { id, document });
    }
    Ok(roster)
}

fn machine_subnet(document: &MachineDocument) -> Result<String, String> {
    match &document.transport {
        MachineTransport::Wireguard { subnet_v4, .. }
        | MachineTransport::Tailscale { subnet_v4, .. } => Ok(subnet_v4.as_string()),
    }
}

async fn wait_for_joined_reachability(
    docker: &Docker,
    founder: &DindMachine,
    joined: &MachineDocument,
) -> Result<(), String> {
    let MachineTransport::Wireguard { addr_v6, .. } = joined.transport else {
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

async fn admit_roaming_peer_and_assert_no_subnet(
    docker: &Docker,
    founder: &DindMachine,
    corrosion_address: &str,
    corrosion_token: &str,
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
        docker,
        founder,
        corrosion_address,
        corrosion_token,
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

async fn admit_concurrent_machines_with_distinct_subnets(blob: &JoinBlob) -> Result<(), String> {
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

async fn assert_revoked_and_expired_refusals(
    store: CorrosionAccess<'_>,
    cli: &Path,
    home: &Path,
    live_blob: JoinBlob,
    live_token_id: TokenId,
) -> Result<(), String> {
    expire_token_row(
        store.docker,
        store.machine,
        store.address,
        store.token,
        &live_token_id,
    )
    .await?;
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

async fn expire_token_row(
    docker: &Docker,
    machine: &DindMachine,
    corrosion_address: &str,
    corrosion_token: &str,
    token_id: &TokenId,
) -> Result<(), String> {
    let rows = corrosion_query(
        docker,
        machine,
        corrosion_address,
        corrosion_token,
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
        docker,
        machine,
        corrosion_address,
        corrosion_token,
        &json!([[
            "UPDATE tokens SET document = ? WHERE id = ?",
            [document, token_id.as_str().to_owned()]
        ]]),
    )
    .await
}

async fn force_collision_and_wait_for_higher_ulid_repair(
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
        store.docker,
        store.machine,
        store.address,
        store.token,
        &json!([[
            "UPDATE machines SET document = ? WHERE id = ?",
            [forced, joiner_row.id.as_str().to_owned()]
        ]]),
    )
    .await?;

    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last = String::from("repair row was not observed");
    while Instant::now() < deadline {
        let roster =
            wait_for_machine_roster(store.docker, store.machine, store.address, store.token, 2)
                .await?;
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

async fn corrosion_transaction(
    docker: &Docker,
    machine: &DindMachine,
    address: &str,
    token: &str,
    statements: &Value,
) -> Result<(), String> {
    let body = statements.to_string();
    let url = format!("http://{address}/v1/transactions");
    let authorization = format!("Authorization: Bearer {token}");
    let outcome = exec_ok(
        docker,
        machine,
        &[
            "curl",
            "--noproxy",
            "*",
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "5",
            "--header",
            &authorization,
            "--header",
            "Accept: application/json",
            "--header",
            "Content-Type: application/json",
            "--data-binary",
            &body,
            &url,
        ],
    )
    .await?;
    require(
        !outcome.stdout.contains("\"error\""),
        format!("Corrosion transaction failed: {}", outcome.stdout),
    )
}

async fn wait_for_command(
    docker: &Docker,
    machine: &DindMachine,
    command: &[&str],
    predicate: impl Fn(&ExecOutcome) -> bool,
    description: &str,
) -> Result<(), String> {
    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last = String::from("command was not attempted");
    while Instant::now() < deadline {
        match exec_in_container(docker, &machine.container_id, command).await {
            Ok(outcome) if predicate(&outcome) => return Ok(()),
            Ok(outcome) => last = format!("{outcome:?}"),
            Err(error) => last = error.to_string(),
        }
        tokio::time::sleep(WAIT_DELAY).await;
    }
    Err(format!("timed out waiting for {description}: {last}"))
}
