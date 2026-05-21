#![cfg(feature = "docker")]

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mvp_deploy::{InstanceId, RevisionId};
use mvp_identity::NodeId;
use mvp_mesh::ContainerSubnet;
use mvp_projection::ServiceName;
use mvp_runtime::{
    ContainerNetworkBackend, DockerBridgeNetwork, DockerBridgeNetworkConfig, DockerRuntime,
    DockerRuntimeConfig, RuntimeBackend, RuntimeError, RuntimeInstanceSpec, RuntimeInstanceState,
};

#[test]
fn docker_runtime_starts_lists_adopts_drains_and_stops_container() {
    let temp = tempfile::tempdir().expect("tempdir");
    let node_id = format!("node-runtime-lifecycle-{}", unique_suffix());
    let config = runtime_config(temp.path(), &node_id);
    let network = match docker_network("runtime-lifecycle", "10.210.90.0/24") {
        Ok(network) => Arc::new(network),
        Err(error) => {
            eprintln!("skipping Docker integration test: {error}");
            return;
        }
    };
    let runtime =
        match DockerRuntime::connect_with_container_network(config.clone(), network.clone()) {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("skipping Docker integration test: {error}");
                return;
            }
        };
    let spec = runtime_spec("runtime-lifecycle", "rev-1");

    let started = runtime.start(&spec).expect("start container");
    assert_eq!(started.state, RuntimeInstanceState::Running);
    assert_eq!(started.service, ServiceName::new("web"));
    assert_eq!(started.revision, RevisionId::new("rev-1"));
    assert!(started.backend_id.is_some());
    assert!(started.backend_name.is_some());
    assert!(started.address.starts_with("10.210.90."));

    let listed = runtime.list().expect("list containers");
    assert!(
        listed
            .iter()
            .any(|instance| instance.instance_id == spec.instance_id)
    );

    let restarted_runtime =
        DockerRuntime::connect_with_container_network(config.clone(), network.clone())
            .expect("reconnect docker runtime")
            .adopt()
            .expect("adopt containers");
    assert!(
        restarted_runtime
            .iter()
            .any(|instance| instance.instance_id == spec.instance_id)
    );

    let drained = runtime
        .drain(&spec.instance_id)
        .expect("drain container")
        .expect("drained instance");
    assert_eq!(drained.state, RuntimeInstanceState::Draining);
    let adopted_after_drain =
        DockerRuntime::connect_with_container_network(config.clone(), network.clone())
            .expect("reconnect docker runtime")
            .adopt()
            .expect("adopt drained container");
    assert_eq!(
        adopted_after_drain
            .iter()
            .find(|instance| instance.instance_id == spec.instance_id)
            .map(|instance| instance.state.clone()),
        Some(RuntimeInstanceState::Draining)
    );

    let stopped = runtime
        .stop(&spec.instance_id)
        .expect("stop container")
        .expect("stopped instance");
    assert_eq!(stopped.state, RuntimeInstanceState::Stopped);
    network.remove().expect("remove bridge network");
}

#[test]
fn docker_runtime_adopts_same_spec_and_recreates_changed_revision() {
    let temp = tempfile::tempdir().expect("tempdir");
    let node_id = format!("node-runtime-recreate-{}", unique_suffix());
    let network = match docker_network("runtime-recreate", "10.210.91.0/24") {
        Ok(network) => Arc::new(network),
        Err(error) => {
            eprintln!("skipping Docker integration test: {error}");
            return;
        }
    };
    let runtime = match DockerRuntime::connect_with_container_network(
        runtime_config(temp.path(), &node_id),
        network.clone(),
    ) {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("skipping Docker integration test: {error}");
            return;
        }
    };
    let rev_1 = runtime_spec("runtime-recreate", "rev-1");
    let rev_2 = RuntimeInstanceSpec::new(
        rev_1.instance_id.clone(),
        rev_1.service.clone(),
        RevisionId::new("rev-2"),
    );

    let first = runtime.start(&rev_1).expect("start first revision");
    let first_id = first.backend_id.clone().expect("first backend id");
    let adopted = runtime.start(&rev_1).expect("start same revision");
    assert_eq!(adopted.backend_id.as_deref(), Some(first_id.as_str()));
    assert_eq!(adopted.address, first.address);

    let replaced = runtime.start(&rev_2).expect("start changed revision");
    assert_eq!(replaced.revision, RevisionId::new("rev-2"));
    assert_ne!(replaced.backend_id.as_deref(), Some(first_id.as_str()));
    assert_eq!(replaced.address, first.address);

    runtime
        .stop(&rev_1.instance_id)
        .expect("stop replacement")
        .expect("stopped replacement");
    network.remove().expect("remove bridge network");
}

#[test]
fn docker_runtime_removes_new_container_when_readiness_fails() {
    let temp = tempfile::tempdir().expect("tempdir");
    let node_id = format!("node-runtime-readiness-{}", unique_suffix());
    let network = match docker_network("runtime-readiness", "10.210.93.0/24") {
        Ok(network) => Arc::new(network),
        Err(error) => {
            eprintln!("skipping Docker integration test: {error}");
            return;
        }
    };
    let config = DockerRuntimeConfig::new(NodeId::new(&node_id), temp.path(), "busybox:latest")
        .with_command(["sh", "-c", "sleep 60"])
        .with_service_port(8080)
        .with_readiness_timeout(Duration::from_millis(250))
        .with_stop_timeout(Duration::from_secs(1));
    let runtime = match DockerRuntime::connect_with_container_network(config, network.clone()) {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("skipping Docker integration test: {error}");
            return;
        }
    };
    let spec = runtime_spec("runtime-readiness", "rev-1");

    let error = runtime
        .start(&spec)
        .expect_err("container without a listener should fail readiness");
    assert!(
        matches!(error, RuntimeError::ReadinessTimeout { .. }),
        "expected readiness timeout, got {error:?}"
    );
    assert!(
        runtime
            .list()
            .expect("list containers")
            .iter()
            .all(|instance| instance.instance_id != spec.instance_id),
        "failed readiness containers must not remain adoptable"
    );
    network.remove().expect("remove bridge network");
}

fn runtime_config(root: &std::path::Path, node_id: &str) -> DockerRuntimeConfig {
    DockerRuntimeConfig::new(NodeId::new(node_id), root, "busybox:latest")
        .with_command([
            "sh",
            "-c",
            "mkdir -p /www && echo ok >/www/index.html && httpd -f -p 8080 -h /www",
        ])
        .with_service_port(8080)
        .with_readiness_timeout(Duration::from_secs(10))
        .with_stop_timeout(Duration::from_secs(1))
}

fn runtime_spec(label: &str, revision: &str) -> RuntimeInstanceSpec {
    RuntimeInstanceSpec::new(
        InstanceId::new(format!(
            "{label}-{}-{}",
            std::process::id(),
            unique_suffix()
        )),
        ServiceName::new("web"),
        RevisionId::new(revision),
    )
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_nanos()
}

fn docker_network(label: &str, subnet: &str) -> mvp_runtime::RuntimeResult<DockerBridgeNetwork> {
    let subnet =
        ContainerSubnet::new(subnet.parse().expect("valid test subnet")).expect("test subnet");
    DockerBridgeNetwork::connect(DockerBridgeNetworkConfig::new(
        format!("ployz-mvp-runtime-test-{label}-{}", std::process::id()),
        subnet,
    ))
}
