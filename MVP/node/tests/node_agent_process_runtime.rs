use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use mvp_bus::{Grant, IslandId, PrincipalId, Subject, SubjectPattern};
use mvp_deploy::{
    DeployId, InstanceCommandReply, InstanceCommandRequest, InstanceId, InstanceStartOutcome,
    RevisionId, StopInstanceRequest, decode, encode,
};
use mvp_node::{
    InitOptions, init_node, node_agent_grant, register_node_agent_services_with_process,
};
use mvp_projection::{BackendEndpoint, ServiceName};
use mvp_runtime::ProcessRuntime;

#[tokio::test]
async fn product_node_agent_starts_http_process_with_shipped_binary_role() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state_dir = temp.path().join("node-a");
    let state = init_node(
        InitOptions::new(&state_dir)
            .with_island("prod")
            .with_node_id("node-a"),
    )
    .expect("init node");
    let runtime =
        ProcessRuntime::managed_http(temp.path().join("runtime"), env!("CARGO_BIN_EXE_mvp-node"));
    let (bus, authority, _raw) = mvp_bus::harness::actor_with_authority();
    let agent = authority.grant_in(
        state.island(),
        PrincipalId::new("node-agent"),
        node_agent_grant(state.node_id_str()).expect("node-agent grant"),
    );
    let requester = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("operator"),
        Grant::empty().with_publish(SubjectPattern::parse("node.*.>").expect("request grant")),
    );
    let (_services, _report) =
        register_node_agent_services_with_process(&bus, &agent, &state, Some(runtime.clone()))
            .await
            .expect("register node agent");
    let instance = InstanceCommandRequest {
        deploy_id: DeployId::new("deploy-1"),
        instance_id: InstanceId::new("instance-a"),
        service: ServiceName::new("web"),
        revision: RevisionId::new("rev-1"),
    };

    request_unit(
        &bus,
        &requester,
        "node.node-a.rpc.prepare_instance",
        &instance,
    )
    .await;
    let reply: InstanceCommandReply = request_json(
        &bus,
        &requester,
        "node.node-a.rpc.start_instance",
        &instance,
    )
    .await;
    assert_eq!(reply.outcome, InstanceStartOutcome::Ready);

    let instances = runtime.list().expect("runtime instances");
    assert_eq!(instances.len(), 1);
    assert_response_contains(&instances[0].address, "instance=instance-a");

    let (restart_bus, restart_authority, _raw) = mvp_bus::harness::actor_with_authority();
    let restarted_agent = restart_authority.grant_in(
        state.island(),
        PrincipalId::new("node-agent-restarted"),
        node_agent_grant(state.node_id_str()).expect("node-agent grant"),
    );
    let (restarted_services, _report) = register_node_agent_services_with_process(
        &restart_bus,
        &restarted_agent,
        &state,
        Some(runtime.clone()),
    )
    .await
    .expect("register restarted node agent");
    assert_eq!(
        restarted_services.running_instances(),
        vec![InstanceId::new("instance-a")]
    );

    request_unit(
        &bus,
        &requester,
        "node.node-a.rpc.stop_instance",
        &StopInstanceRequest {
            deploy_id: DeployId::new("deploy-1"),
            cleanup_target: BackendEndpoint {
                node_id: state.node_id(),
                address: "instance-a".to_string(),
            },
        },
    )
    .await;
    let stopped = runtime.list().expect("runtime instances after stop");
    assert_eq!(stopped[0].pid, None);
}

async fn request_unit<T: serde::Serialize>(
    bus: &mvp_bus::BusActorHandle,
    session: &mvp_bus::BusSession,
    subject: &str,
    request: &T,
) {
    bus.request(
        session,
        Subject::parse(subject).expect("subject"),
        encode(request, "request").expect("request"),
        Duration::from_secs(2),
    )
    .await
    .expect("request");
}

async fn request_json<T: serde::Serialize, R: serde::de::DeserializeOwned>(
    bus: &mvp_bus::BusActorHandle,
    session: &mvp_bus::BusSession,
    subject: &str,
    request: &T,
) -> R {
    let response = bus
        .request(
            session,
            Subject::parse(subject).expect("subject"),
            encode(request, "request").expect("request"),
            Duration::from_secs(2),
        )
        .await
        .expect("request");
    decode(response.payload(), "reply").expect("reply")
}

fn assert_response_contains(address: &str, expected: &str) {
    let mut stream = TcpStream::connect(address).expect("connect service");
    stream
        .write_all(b"GET / HTTP/1.1\r\nhost: local\r\n\r\n")
        .expect("request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("response");
    assert!(
        response.contains(expected),
        "response {response:?} did not contain {expected:?}"
    );
}
