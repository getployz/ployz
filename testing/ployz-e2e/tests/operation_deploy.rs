#[path = "operation_deploy/support.rs"]
mod support;

use bollard::Docker;
use ployz_core::corrosion::fingerprint_env_value;
use ployz_core::deploy::EnvValue;
use ployz_e2e::dind::{
    DindCluster, DindClusterSpec, MachineSpec, artifact_dir, connect_docker, e2e_enabled,
    keep_requested, machine_image, require,
};

use support::{
    assert_cluster_wide_operation_replay, assert_dns_and_http,
    assert_driver_local_evidence_is_secret_free,
    assert_public_rows_are_digest_pinned_and_secret_free, create_namespace_and_deploy,
    found_and_join, start_mutable_registry,
};

const NAMESPACE: &str = "production";
const SERVICE: &str = "web";
const SECRET_NAME: &str = "OPERATION_E2E_SECRET";
const SECRET_VALUE: &str = "sentinel-operation-e2e-secret";

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
    let operator = found_and_join(docker, founder, joiner).await?;
    let image = start_mutable_registry(docker, founder, joiner).await?;
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
        &operator.joiner_api_address,
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
    assert_dns_and_http(
        docker,
        joiner,
        founder,
        operator.joiner_dns_address,
        SERVICE,
        NAMESPACE,
        rows.container_ip,
    )
    .await?;
    Ok(())
}
