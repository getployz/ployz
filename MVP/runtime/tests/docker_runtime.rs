#![cfg(feature = "docker")]

use std::time::Duration;

use mvp_deploy::{InstanceId, RevisionId};
use mvp_identity::NodeId;
use mvp_projection::ServiceName;
use mvp_runtime::{
    DockerRuntime, DockerRuntimeConfig, RuntimeBackend, RuntimeInstanceSpec, RuntimeInstanceState,
};

#[test]
fn docker_runtime_starts_lists_adopts_drains_and_stops_container() {
    let temp = tempfile::tempdir().expect("tempdir");
    let config = DockerRuntimeConfig::new(NodeId::new("node-a"), temp.path(), "busybox:latest")
        .with_command([
            "sh",
            "-c",
            "mkdir -p /www && echo ok >/www/index.html && httpd -f -p 8080 -h /www",
        ])
        .with_service_port(8080)
        .with_readiness_timeout(Duration::from_secs(10))
        .with_stop_timeout(Duration::from_secs(1));
    let runtime = match DockerRuntime::connect(config.clone()) {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("skipping Docker integration test: {error}");
            return;
        }
    };
    let spec = RuntimeInstanceSpec::new(
        InstanceId::new(format!("docker-test-{}", std::process::id())),
        ServiceName::new("web"),
        RevisionId::new("rev-1"),
    );

    let started = runtime.start(&spec).expect("start container");
    assert_eq!(started.state, RuntimeInstanceState::Running);
    assert_eq!(started.service, ServiceName::new("web"));
    assert_eq!(started.revision, RevisionId::new("rev-1"));
    assert!(started.backend_id.is_some());
    assert!(started.backend_name.is_some());

    let listed = runtime.list().expect("list containers");
    assert!(
        listed
            .iter()
            .any(|instance| instance.instance_id == spec.instance_id)
    );

    let adopted = DockerRuntime::connect(config)
        .expect("reconnect docker runtime")
        .adopt()
        .expect("adopt containers");
    assert!(
        adopted
            .iter()
            .any(|instance| instance.instance_id == spec.instance_id)
    );

    let drained = runtime
        .drain(&spec.instance_id)
        .expect("drain container")
        .expect("drained instance");
    assert_eq!(drained.state, RuntimeInstanceState::Draining);

    let stopped = runtime
        .stop(&spec.instance_id)
        .expect("stop container")
        .expect("stopped instance");
    assert_eq!(stopped.state, RuntimeInstanceState::Stopped);
}
