//! Gated Docker-in-Docker harness tests.
//!
//! Run with: `PLOYZ_DIND_E2E=1 cargo test -p ployz-e2e --test dind_cluster
//! -- --test-threads=1`. Requires the machine image from
//! `scripts/build-dind-machine-image.sh` and Docker with `--privileged`
//! support. `PLOYZ_DIND_KEEP=1` keeps the cluster running for debugging;
//! `scripts/dind-clean.sh` sweeps leftovers.

use ployz_e2e::bollard::query_parameters::{
    ListContainersOptionsBuilder, ListNetworksOptionsBuilder,
};
use ployz_e2e::dind::{
    self, DindCluster, DindClusterSpec, DindMachineRole, MachineSpec, exec_in_container,
};
use std::collections::HashMap;

/// Smoke test: one machine boots to systemd + inner-docker readiness with the
/// artifact mount in place, and teardown leaves nothing labeled behind.
#[tokio::test]
async fn boots_machine_image() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    let spec = DindClusterSpec {
        artifact_dir: dind::artifact_dir(),
        machines: vec![MachineSpec {
            role: DindMachineRole::Core,
            image: dind::machine_image(),
        }],
    };
    let cluster = DindCluster::provision(&docker, spec)
        .await
        .expect("provision one-machine DinD cluster");

    // Provisioning already waited for readiness; assert it holds from the
    // outside through the same exec surface scenarios will use.
    let system_state = exec_in_container(
        &docker,
        &cluster.core().container_id,
        &["systemctl", "is-system-running"],
    )
    .await;
    let system_ready = matches!(
        &system_state,
        Ok(outcome) if matches!(outcome.stdout.trim(), "running" | "degraded")
    );
    if !system_ready {
        fail_with_evidence(
            &cluster,
            &format!("core systemd not ready: {system_state:?}"),
        )
        .await;
    }

    let inner_docker =
        exec_in_container(&docker, &cluster.core().container_id, &["docker", "info"]).await;
    let inner_docker_ready = matches!(&inner_docker, Ok(outcome) if outcome.success());
    if !inner_docker_ready {
        fail_with_evidence(
            &cluster,
            &format!("inner docker not ready: {inner_docker:?}"),
        )
        .await;
    }

    let artifacts = exec_in_container(
        &docker,
        &cluster.core().container_id,
        &["test", "-x", "/opt/ployz/artifacts/ployzd"],
    )
    .await;
    let artifacts_mounted = matches!(&artifacts, Ok(outcome) if outcome.success());
    if !artifacts_mounted {
        fail_with_evidence(
            &cluster,
            &format!("artifact mount missing executable ployzd: {artifacts:?}"),
        )
        .await;
    }

    if dind::keep_requested() {
        eprintln!(
            "PLOYZ_DIND_KEEP=1: keeping run {} (network {}, core container {})",
            cluster.run_id(),
            cluster.network_name(),
            cluster.core().container_id,
        );
        return;
    }

    let run_label = format!("{}={}", dind::RUN_LABEL, cluster.run_id());
    cluster.teardown().await.expect("teardown DinD cluster");

    let filters = HashMap::from([("label".to_owned(), vec![run_label])]);
    let leftover_containers = docker
        .list_containers(Some(
            ListContainersOptionsBuilder::new()
                .all(true)
                .filters(&filters)
                .build(),
        ))
        .await
        .expect("list containers after teardown");
    assert!(
        leftover_containers.is_empty(),
        "teardown left labeled containers behind: {leftover_containers:?}"
    );
    let leftover_networks = docker
        .list_networks(Some(
            ListNetworksOptionsBuilder::new().filters(&filters).build(),
        ))
        .await
        .expect("list networks after teardown");
    assert!(
        leftover_networks.is_empty(),
        "teardown left labeled networks behind: {leftover_networks:?}"
    );
}

/// Captures evidence for the whole cluster, then panics with the message and
/// the evidence location.
async fn fail_with_evidence(cluster: &DindCluster, message: &str) -> ! {
    match cluster.capture_evidence().await {
        Ok(dir) => panic!("{message}; evidence: {}", dir.display()),
        Err(error) => panic!("{message}; evidence capture also failed: {error}"),
    }
}
