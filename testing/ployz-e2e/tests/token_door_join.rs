#[path = "token_door_join/admission.rs"]
mod admission;
#[path = "token_door_join/fixture.rs"]
mod fixture;
#[path = "token_door_join/repair.rs"]
mod repair;

use admission::{
    admit_concurrent_machines_with_distinct_subnets, admit_roaming_peer_and_assert_no_subnet,
    assert_foreign_machine_refuses, assert_revoked_and_expired_refusals,
    assert_token_row_is_hash_only, assert_wrong_door_fingerprint_is_rejected, join_fresh_machine,
    wait_for_joined_reachability,
};
use bollard::Docker;
use fixture::{
    CorrosionAccess, assert_missing_endpoint_refuses_without_a_token, extract_join_blob,
    extract_token_id, handoff_with_known_endpoint, machine_subnet, require_success, run_cli,
    run_founding, wait_for_machine_roster,
};
use ployz::commands::SshTarget;
use ployz::init::ssh::{SshPeerKey, default_config_home};
use ployz::mesh::context::OperatorContextStore;
use ployz_core::network::DEFAULT_WIREGUARD_LISTEN_PORT;
use ployz_e2e::dind::{
    DindCluster, DindClusterSpec, MachineSpec, artifact_dir, connect_docker, corrosion_access,
    e2e_enabled, install_local_release_channel, keep_requested, machine_image, require,
};
use repair::{
    RefoundContext, RepairContext, refound_cluster, repair_contaminated_and_wiped_machine,
};
use std::net::SocketAddr;
use std::time::Duration;

const CLUSTER_NAME: &str = "dind-token-door";
const FOUNDER_NAME: &str = "machine-one";
const OPERATOR_PEER_NAME: &str = "dind-operator";
const WAIT_BUDGET: Duration = Duration::from_secs(60);
const WAIT_DELAY: Duration = Duration::from_millis(250);

#[test]
fn peer_fixture_names_are_valid() {
    for name in [OPERATOR_PEER_NAME, admission::ROAMING_PEER_NAME] {
        SshPeerKey::generate(name.to_owned()).expect("DinD peer fixture name must be valid");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_token_door_grows_and_repairs_the_cluster() {
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
        SshPeerKey::generate(OPERATOR_PEER_NAME.to_owned()).map_err(|error| error.to_string())?;
    operator
        .persist_new(&config_home, &target)
        .map_err(|error| error.to_string())?;

    let initial_handoff = run_founding(docker, founder, &operator).await?;
    let founder_endpoint = SocketAddr::new(founder.bridge_ip, DEFAULT_WIREGUARD_LISTEN_PORT);
    let handoff = handoff_with_known_endpoint(initial_handoff, founder_endpoint)?;
    OperatorContextStore::new(&config_home)
        .persist(&target, handoff, &operator)
        .map_err(|error| error.to_string())?;

    let cli = artifact_dir().join("ployz");
    let (corrosion_address, corrosion_token) = corrosion_access(docker, founder).await?;
    let store = CorrosionAccess {
        docker,
        machine: founder,
        address: &corrosion_address,
        token: &corrosion_token,
    };
    assert_missing_endpoint_refuses_without_a_token(store, &cli, temporary_home.path()).await?;

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
        ["token", "create", "bootstrap", "--ttl", "1h"].map(str::to_owned),
    )?;
    require_success(&created, "token create")?;
    let created_stdout = String::from_utf8_lossy(&created.stdout);
    let blob = extract_join_blob(&created_stdout)?;
    let token_id = extract_token_id(&created_stdout)?;
    require(
        created_stdout.matches(blob.expose()).count() == 1,
        "token create printed the join blob more than once",
    )?;
    require(
        created_stdout.contains("sudo ployz machine join \"$JOIN_BLOB\""),
        "machine join line does not reference the show-once variable",
    )?;
    require(
        created_stdout.contains("curl -fsSL https://ployz.sh | sh -s -- join \"$JOIN_BLOB\""),
        "cloud-init line does not reference the show-once variable",
    )?;
    assert_token_row_is_hash_only(store, &token_id, &blob).await?;
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

    assert_wrong_door_fingerprint_is_rejected(&blob)
        .await
        .map_err(|error| format!("wrong-fingerprint refusal failed: {error}"))?;
    assert_foreign_machine_refuses(docker, foreign, &blob)
        .await
        .map_err(|error| format!("foreign-machine refusal failed: {error}"))?;
    join_fresh_machine(docker, joiner, &blob)
        .await
        .map_err(|error| format!("initial machine join failed: {error}"))?;
    let roster = wait_for_machine_roster(store, 2).await?;
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

    let repaired_joiner = repair_contaminated_and_wiped_machine(
        RepairContext {
            docker,
            store,
            cli: &cli,
            home: temporary_home.path(),
            founder,
            joiner,
        },
        &blob,
        joiner_row,
    )
    .await
    .map_err(|error| format!("machine repair flow failed: {error}"))?;

    let post_removal = run_cli(
        &cli,
        temporary_home.path(),
        ["token", "create", "post-removal", "--ttl", "1h"].map(str::to_owned),
    )?;
    require_success(&post_removal, "post-removal token create")?;
    let post_removal_stdout = String::from_utf8_lossy(&post_removal.stdout);
    let post_removal_blob = extract_join_blob(&post_removal_stdout)?;
    let post_removal_token_id = extract_token_id(&post_removal_stdout)?;

    admit_roaming_peer_and_assert_no_subnet(store, &post_removal_blob)
        .await
        .map_err(|error| format!("roaming-peer admission failed: {error}"))?;
    admit_concurrent_machines_with_distinct_subnets(&post_removal_blob)
        .await
        .map_err(|error| format!("concurrent-machine admission failed: {error}"))?;
    assert_revoked_and_expired_refusals(
        store,
        &cli,
        temporary_home.path(),
        post_removal_blob,
        post_removal_token_id,
    )
    .await
    .map_err(|error| format!("revoked/expired token checks failed: {error}"))?;

    refound_cluster(
        RefoundContext {
            docker,
            cli: &cli,
            home: temporary_home.path(),
            config_home: &config_home,
            target: &target,
            operator: &operator,
            founder,
            joiner,
        },
        &repaired_joiner,
    )
    .await
    .map_err(|error| format!("cluster refounding failed: {error}"))
}
