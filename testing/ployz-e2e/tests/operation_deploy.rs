#[path = "operation_deploy/support.rs"]
mod support;

use bollard::Docker;
use ployz_core::corrosion::fingerprint_env_value;
use ployz_core::deploy::EnvValue;
use ployz_e2e::dind::{
    DindCluster, DindClusterSpec, MachineSpec, artifact_dir, assert_cluster_wide_operation_replay,
    assert_dns_and_http, assert_driver_local_evidence_is_secret_free,
    assert_first_revision_container_is_gone, assert_gateway_http, connect_docker,
    create_namespace_and_deploy, e2e_enabled, found_and_join_with_service_urls, keep_requested,
    machine_image, parse_deploy_operation, push_second_revision, require, spawn_deploy,
    start_mutable_registry,
};

use support::assert_public_rows_are_digest_pinned_and_secret_free;

const NAMESPACE: &str = "production";
const SERVICE: &str = "web";
const SECRET_NAME: &str = "OPERATION_E2E_SECRET";
const SECRET_VALUE: &str = "sentinel-operation-e2e-secret";
const FIRST_BODY: &str = "Welcome to nginx";
const SECOND_BODY: &str = "ployz-operation-second-revision";
const PUBLIC_HOSTNAME: &str = "web.apps.example.test";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_deploy_is_observable_and_reachable_from_a_joined_machine() {
    if !e2e_enabled() {
        eprintln!("skipping operation-deploy DinD proof; set PLOYZ_DIND_E2E=1 to enable it");
        return;
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    panic!("the pinned operation-deploy proof supports only Linux x86_64");

    let docker = connect_docker().expect("connect to Docker for operation-deploy proof");
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
            ],
        },
    )
    .await
    .expect("provision operation-deploy machines");

    let result = exercise_operation_deploy(&docker, &cluster).await;
    if let Err(error) = &result {
        match cluster.capture_evidence().await {
            Ok(path) => eprintln!(
                "operation-deploy evidence captured under {}",
                path.display()
            ),
            Err(capture_error) => {
                eprintln!("operation-deploy evidence capture failed: {capture_error}")
            }
        }
        eprintln!("operation-deploy proof failed: {error}");
    }
    if keep_requested() {
        eprintln!(
            "retaining DinD run {} because PLOYZ_DIND_KEEP=1",
            cluster.run_id()
        );
    } else {
        cluster
            .teardown()
            .await
            .expect("tear down operation-deploy run");
    }
    result.unwrap_or_else(|error| panic!("operation-deploy proof failed: {error}"));
}

async fn exercise_operation_deploy(docker: &Docker, cluster: &DindCluster) -> Result<(), String> {
    let [founder, joiner] = cluster.machines() else {
        return Err("operation-deploy proof requires exactly two machines".to_owned());
    };
    let operator =
        found_and_join_with_service_urls(docker, founder, &[joiner], "custom:apps.example.test")
            .await?;
    let [joined] = operator.joiners.as_slice() else {
        return Err("operation-deploy proof expects exactly one joined machine".to_owned());
    };
    let image = start_mutable_registry(docker, founder, &[joiner]).await?;
    require(
        image.ends_with(":latest") && !image.contains("@sha256:"),
        format!("registry fixture was not a mutable image reference: {image}"),
    )?;

    let operation_id = create_namespace_and_deploy(
        &operator,
        NAMESPACE,
        SERVICE,
        &image,
        SECRET_NAME,
        SECRET_VALUE,
    )?;
    assert_cluster_wide_operation_replay(&operator, &operation_id, SECRET_VALUE)?;
    assert_driver_local_evidence_is_secret_free(docker, founder, &operation_id, SECRET_VALUE)
        .await?;

    let rows = support::wait_for_public_deploy_rows(
        docker,
        founder,
        &joined.api_address,
        SERVICE,
        &operation_id,
    )
    .await?;
    let expected_fingerprint =
        fingerprint_env_value(&EnvValue::try_new(SECRET_VALUE).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    assert_public_rows_are_digest_pinned_and_secret_free(
        &rows,
        &image,
        SECRET_NAME,
        SECRET_VALUE,
        &expected_fingerprint,
    )?;
    let hostname = format!("{SERVICE}.{NAMESPACE}.internal");
    assert_dns_and_http(
        docker,
        joiner,
        founder,
        joined.dns_address,
        &hostname,
        rows.container_ip,
        FIRST_BODY,
    )
    .await?;
    assert_gateway_http(docker, joiner, PUBLIC_HOSTNAME, FIRST_BODY).await?;

    // Second deploy of the same incumbent: revision-gated blue/green cutover.
    push_second_revision(docker, founder, &image, SECOND_BODY).await?;
    let deploy = spawn_deploy(
        &operator,
        NAMESPACE,
        SERVICE,
        &image,
        SECRET_NAME,
        SECRET_VALUE,
    )?;
    let deploy_output = support::watch_gateway_cutover_traffic(
        docker,
        joiner,
        support::GatewayCutover {
            api_address: &joined.api_address,
            service_name: SERVICE,
            first_operation: &operation_id,
            hostname: PUBLIC_HOSTNAME,
            bodies: support::RevisionBodies {
                first: FIRST_BODY,
                second: SECOND_BODY,
            },
        },
        deploy,
    )
    .await?;
    let second_operation_id =
        parse_deploy_operation(&deploy_output, "second deploy", SECRET_VALUE)?;
    let replay =
        assert_cluster_wide_operation_replay(&operator, &second_operation_id, SECRET_VALUE)?;
    support::assert_replay_orders_cutover_evidence(&replay)?;
    assert_driver_local_evidence_is_secret_free(
        docker,
        founder,
        &second_operation_id,
        SECRET_VALUE,
    )
    .await?;

    let second_rows = support::wait_for_public_deploy_rows(
        docker,
        founder,
        &joined.api_address,
        SERVICE,
        &second_operation_id,
    )
    .await?;
    assert_public_rows_are_digest_pinned_and_secret_free(
        &second_rows,
        &image,
        SECRET_NAME,
        SECRET_VALUE,
        &expected_fingerprint,
    )?;
    support::assert_cutover_rows(&second_rows, &rows, &second_operation_id)?;
    assert_dns_and_http(
        docker,
        joiner,
        founder,
        joined.dns_address,
        &hostname,
        second_rows.container_ip,
        SECOND_BODY,
    )
    .await?;
    assert_first_revision_container_is_gone(docker, &[founder, joiner], &operation_id).await?;
    Ok(())
}
