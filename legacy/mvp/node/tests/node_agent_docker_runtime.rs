#![cfg(feature = "docker-runtime")]

use std::sync::Arc;
use std::time::Duration;

use mvp_bus::{Grant, IslandId, PrincipalId, Subject};
use mvp_deploy::{
    DeployId, DrainInstanceRequest, InstanceCommandReply, InstanceCommandRequest, InstanceId,
    InstanceStartOutcome, RevisionId, StopInstanceRequest, encode,
};
use mvp_node::{
    InitOptions, init_node, node_agent_grant, register_node_agent_services_with_runtime,
};
use mvp_projection::ServiceName;
use mvp_runtime::{
    ContainerNetworkBackend, DockerBridgeNetwork, DockerBridgeNetworkConfig, DockerRuntime,
    DockerRuntimeConfig, RuntimeBackend, RuntimeInstanceState,
};

#[tokio::test]
async fn docker_node_agent_starts_and_stops_container_through_runtime_contract() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state_dir = temp.path().join("node-a");
    let state = init_node(
        InitOptions::new(&state_dir)
            .with_island("prod")
            .with_node_id("node-a"),
    )
    .expect("init node");
    let bridge = match DockerBridgeNetwork::connect(DockerBridgeNetworkConfig::new(
        format!("ployz-mvp-node-agent-test-{}", std::process::id()),
        state.container_subnet(),
    )) {
        Ok(network) => Arc::new(network),
        Err(error) => {
            eprintln!("skipping Docker node-agent integration test: {error}");
            return;
        }
    };
    let config = DockerRuntimeConfig::new(
        state.node_id(),
        state.paths().runtime_dir.clone(),
        "busybox:latest",
    )
    .with_command([
        "sh",
        "-c",
        "mkdir -p /www && echo ok >/www/index.html && httpd -f -p 8080 -h /www",
    ])
    .with_readiness_timeout(Duration::from_secs(10))
    .with_stop_timeout(Duration::from_secs(1));
    let docker_runtime =
        match DockerRuntime::connect_with_container_network(config.clone(), bridge.clone()) {
            Ok(runtime) => Arc::new(runtime),
            Err(error) => {
                eprintln!("skipping Docker node-agent integration test: {error}");
                return;
            }
        };
    let runtime_backend = Arc::clone(&docker_runtime) as Arc<dyn RuntimeBackend>;
    let (bus, authority, _raw) = mvp_bus::harness::actor_with_authority();
    let agent = authority.grant_in(
        state.island(),
        PrincipalId::new("node-agent"),
        node_agent_grant(state.node_id_str()).expect("agent grant"),
    );
    let requester = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("operator"),
        Grant::allow_all(),
    );
    register_node_agent_services_with_runtime(&bus, &agent, &state, Some(runtime_backend))
        .await
        .expect("register node agent");
    let instance_id = InstanceId::new(format!("docker-agent-test-{}", std::process::id()));
    let request = InstanceCommandRequest {
        deploy_id: DeployId::new("deploy-1"),
        instance_id: instance_id.clone(),
        service: ServiceName::new("web"),
        revision: RevisionId::new("rev-1"),
    };

    let response = bus
        .request(
            &requester,
            Subject::parse("node.node-a.rpc.start_instance").expect("subject"),
            encode(&request, "start request").expect("request"),
            Duration::from_secs(15),
        )
        .await
        .expect("start request");
    let reply: InstanceCommandReply =
        mvp_deploy::decode(response.payload(), "start reply").expect("start reply");
    assert_eq!(reply.outcome, InstanceStartOutcome::Ready);
    let backend = reply.backend.expect("backend endpoint");
    assert!(backend.address.starts_with("10.210."));

    let restarted_runtime =
        DockerRuntime::connect_with_container_network(config.clone(), bridge.clone())
            .expect("reconnect docker runtime");
    let (restart_bus, restart_authority, _raw) = mvp_bus::harness::actor_with_authority();
    let restarted_agent = restart_authority.grant_in(
        state.island(),
        PrincipalId::new("node-agent-restarted"),
        node_agent_grant(state.node_id_str()).expect("agent grant"),
    );
    let (restarted_services, _report) = register_node_agent_services_with_runtime(
        &restart_bus,
        &restarted_agent,
        &state,
        Some(Arc::new(restarted_runtime) as Arc<dyn RuntimeBackend>),
    )
    .await
    .expect("register restarted node agent");
    assert_eq!(
        restarted_services.running_instances(),
        vec![instance_id.clone()]
    );

    bus.request(
        &requester,
        Subject::parse("node.node-a.rpc.drain_instance").expect("subject"),
        encode(
            &DrainInstanceRequest {
                deploy_id: DeployId::new("deploy-1"),
                cleanup_target: backend.clone(),
            },
            "drain request",
        )
        .expect("request"),
        Duration::from_secs(15),
    )
    .await
    .expect("drain request");
    assert_eq!(
        docker_runtime
            .list()
            .expect("list containers")
            .iter()
            .find(|instance| instance.instance_id == instance_id)
            .map(|instance| instance.state.clone()),
        Some(RuntimeInstanceState::Draining)
    );

    bus.request(
        &requester,
        Subject::parse("node.node-a.rpc.stop_instance").expect("subject"),
        encode(
            &StopInstanceRequest {
                deploy_id: DeployId::new("deploy-1"),
                cleanup_target: backend,
            },
            "stop request",
        )
        .expect("request"),
        Duration::from_secs(15),
    )
    .await
    .expect("stop request");
    assert!(
        docker_runtime
            .list()
            .expect("list containers")
            .iter()
            .all(|instance| instance.instance_id != instance_id)
    );
    bridge.remove().expect("remove bridge network");
}
