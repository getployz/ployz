//! The create body Docker receives, pinned for an app, a hook and the two Ingress doors.

use super::*;
use crate::docker::test_support::*;
use ployz_core::{MachineGateway, QualifiedService};

fn machine_id() -> MachineId {
    MachineId::parse("1".repeat(32)).unwrap()
}

fn spec(name: &str, container: serde_json::Value) -> ResolvedServiceSpec {
    serde_json::from_value(serde_json::json!({
        "service_id": "a".repeat(32),
        "name": name,
        "mode": { "mode": "global" },
        "container": container,
        "pre_deploy": { "command": ["migrate"] },
    }))
    .unwrap()
}

fn app_spec() -> ResolvedServiceSpec {
    spec(
        "api",
        serde_json::json!({
            "image": "example.test/api",
            "pull_policy": "missing",
            "healthcheck": {
                "state": "configured",
                "test": ["CMD", "true"],
                "interval_millis": 1500,
                "retries": 3,
            },
        }),
    )
}

/// The body `container_create_body` and `ingress_sysctls` make, as Docker reads it.
fn body_of(
    kind: ContainerKind,
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
    network: &create::CreationNetwork,
) -> serde_json::Value {
    let mut body = create::container_create_body(
        &machine_id(),
        MachineGateway("10.210.0.1".parse().unwrap()),
        kind,
        namespace,
        spec,
        network,
        None,
    )
    .unwrap();
    create::ingress_sysctls(&mut body, namespace, spec, true);
    serde_json::to_value(body).unwrap()
}

/// What Docker is sent for one keyed create, from the fake that keeps the config it was given.
async fn keyed_config(
    kind: ContainerKind,
    creation_key: &'static str,
) -> (String, serde_json::Value) {
    let containers = std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
    let (runtime, _fake) = fake_runtime_with(FakeDocker {
        named_containers: Some(containers.clone()),
        ..Default::default()
    })
    .await;
    let machine = test_machine(machine_id(), MachineGateway("10.210.0.1".parse().unwrap()));
    let namespace = Namespace::parse("app").unwrap();
    let app = app_spec();
    let mut request = container_request(kind, &namespace, &app, std::future::ready(None));
    request.creation_key = creation_key;
    let created = runtime
        .create_with_admission(&machine, request)
        .await
        .unwrap();
    let config = containers
        .lock()
        .unwrap()
        .get(&created.display_name)
        .and_then(|container| container.get("Config"))
        .cloned()
        .unwrap();
    (created.display_name, config)
}

const SERVICE_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const MACHINE: &str = "PLOYZ_MACHINE_ID=11111111111111111111111111111111";

fn log_config() -> serde_json::Value {
    serde_json::json!({
        "Config": {"compress": "false", "max-file": "3", "max-size": "10m"},
        "Type": "local",
    })
}

#[test]
fn an_app_body_is_pinned() {
    let body = body_of(
        ContainerKind::ServiceContainer,
        &Namespace::parse("app").unwrap(),
        &app_spec(),
        &create::CreationNetwork::Bridge,
    );
    assert_eq!(
        body,
        serde_json::json!({
            "Cmd": [],
            "Env": [MACHINE],
            "Healthcheck": {"Interval": 1_500_000_000_i64, "Retries": 3, "Test": ["CMD", "true"]},
            "HostConfig": {
                "Dns": ["10.210.0.1"],
                "DnsOptions": ["ndots:1"],
                "DnsSearch": ["app.internal"],
                "LogConfig": log_config(),
                "NetworkMode": "ployz",
                "Privileged": false,
                "RestartPolicy": {"Name": "unless-stopped"},
            },
            "Image": "example.test/api",
            "Labels": {
                "ployz.managed": "",
                "ployz.namespace": "app",
                "ployz.network.ipv6": "true",
                "ployz.service.id": SERVICE_ID,
                "ployz.service.name": "api",
            },
            "OpenStdin": false,
            "Tty": false,
        })
    );
}

#[test]
fn a_hook_body_disables_the_healthcheck_and_never_restarts() {
    let body = body_of(
        ContainerKind::PreDeployHook,
        &Namespace::parse("app").unwrap(),
        &app_spec(),
        &create::CreationNetwork::Bridge,
    );
    assert_eq!(
        body,
        serde_json::json!({
            "Cmd": ["migrate"],
            "Entrypoint": [],
            "Env": ["PLOYZ_HOOK_PRE_DEPLOY=true", MACHINE],
            "Healthcheck": {"Test": ["NONE"]},
            "HostConfig": {
                "Dns": ["10.210.0.1"],
                "DnsOptions": ["ndots:1"],
                "DnsSearch": ["app.internal"],
                "LogConfig": log_config(),
                "NetworkMode": "ployz",
                "Privileged": false,
                "RestartPolicy": {"Name": "no"},
            },
            "Image": "example.test/api",
            "Labels": {
                "ployz.managed": "",
                "ployz.namespace": "app",
                "ployz.network.ipv6": "true",
                "ployz.service.hook": "pre-deploy",
                "ployz.service.id": SERVICE_ID,
                "ployz.service.name": "api",
            },
            "OpenStdin": false,
            "Tty": false,
        })
    );
}

#[test]
fn the_holder_gets_its_sysctls_and_the_proxy_joins_its_network() {
    let holder_identity = QualifiedService::system_ingress_holder();
    let holder = spec(
        &holder_identity.name.to_string(),
        serde_json::json!({ "image": "example.test/holder", "pull_policy": "missing" }),
    );
    assert_eq!(
        body_of(
            ContainerKind::ServiceContainer,
            &holder_identity.namespace,
            &holder,
            &create::CreationNetwork::Bridge,
        ),
        serde_json::json!({
            "Cmd": [],
            "Env": [MACHINE],
            "HostConfig": {
                "Dns": ["10.210.0.1"],
                "DnsOptions": ["ndots:1"],
                "DnsSearch": ["ployz-system.internal"],
                "LogConfig": log_config(),
                "NetworkMode": "ployz",
                "Privileged": false,
                "RestartPolicy": {"Name": "unless-stopped"},
                "Sysctls": {"net.ipv4.tcp_migrate_req": "1", "net.ipv4.tcp_retries2": "5"},
            },
            "Image": "example.test/holder",
            "Labels": {
                "ployz.managed": "",
                "ployz.namespace": "ployz-system",
                "ployz.network.ipv6": "true",
                "ployz.service.id": SERVICE_ID,
                "ployz.service.name": "ingress-holder",
            },
            "OpenStdin": false,
            "Tty": false,
        })
    );

    let proxy_identity = QualifiedService::system_ingress();
    let proxy = spec(
        &proxy_identity.name.to_string(),
        serde_json::json!({
            "image": "example.test/caddy",
            "pull_policy": "missing",
            "network": "ingress_holder",
        }),
    );
    assert_eq!(
        body_of(
            ContainerKind::ServiceContainer,
            &proxy_identity.namespace,
            &proxy,
            &create::CreationNetwork::Holder(ContainerId::parse("b".repeat(64)).unwrap()),
        ),
        serde_json::json!({
            "Cmd": [],
            "Env": [MACHINE],
            "HostConfig": {
                "LogConfig": log_config(),
                "NetworkMode": format!("container:{}", "b".repeat(64)),
                "Privileged": false,
                "RestartPolicy": {"Name": "unless-stopped"},
            },
            "Image": "example.test/caddy",
            "Labels": {
                "ployz.managed": "",
                "ployz.namespace": "ployz-system",
                "ployz.network.ipv6": "true",
                "ployz.service.id": SERVICE_ID,
                "ployz.service.name": "ingress",
            },
            "OpenStdin": false,
            "Tty": false,
        })
    );
}

#[tokio::test]
async fn a_keyed_service_create_sends_docker_its_name_and_run_labels() {
    let (name, config) = keyed_config(ContainerKind::ServiceContainer, "golden-key").await;
    assert_eq!(name, "api-1890ced8dd79");
    assert_eq!(
        config,
        serde_json::json!({
            "Cmd": [],
            "Env": [MACHINE],
            "Healthcheck": {"Interval": 1_500_000_000_i64, "Retries": 3, "Test": ["CMD", "true"]},
            "HostConfig": {
                "Dns": ["10.210.0.1"],
                "DnsOptions": ["ndots:1"],
                "DnsSearch": ["app.internal"],
                "LogConfig": log_config(),
                "Mounts": [],
                "NetworkMode": "ployz",
                "Privileged": false,
                "RestartPolicy": {"Name": "unless-stopped"},
            },
            "Image": "example.test/api",
            "Labels": {
                "ployz.creation.key": "golden-key",
                "ployz.gen": "1",
                "ployz.managed": "",
                "ployz.namespace": "app",
                "ployz.network.ipv6": "true",
                "ployz.service.id": SERVICE_ID,
                "ployz.service.name": "api",
            },
            "NetworkingConfig": {"EndpointsConfig": {"ployz": {"IPAMConfig": {"IPv4Address": "10.210.0.2"}}}},
            "OpenStdin": false,
            "Tty": false,
        })
    );
}

#[tokio::test]
async fn a_keyed_hook_create_sends_docker_its_name_and_run_labels() {
    let (name, config) = keyed_config(ContainerKind::PreDeployHook, "golden-hook").await;
    assert_eq!(name, "api-2246de3c85a0");
    assert_eq!(
        config,
        serde_json::json!({
            "Cmd": ["migrate"],
            "Entrypoint": [],
            "Env": ["PLOYZ_HOOK_PRE_DEPLOY=true", MACHINE],
            "Healthcheck": {"Test": ["NONE"]},
            "HostConfig": {
                "Dns": ["10.210.0.1"],
                "DnsOptions": ["ndots:1"],
                "DnsSearch": ["app.internal"],
                "LogConfig": log_config(),
                "Mounts": [],
                "NetworkMode": "ployz",
                "Privileged": false,
                "RestartPolicy": {"Name": "no"},
            },
            "Image": "example.test/api",
            "Labels": {
                "ployz.creation.key": "golden-hook",
                "ployz.gen": "1",
                "ployz.managed": "",
                "ployz.namespace": "app",
                "ployz.network.ipv6": "true",
                "ployz.service.hook": "pre-deploy",
                "ployz.service.id": SERVICE_ID,
                "ployz.service.name": "api",
            },
            "NetworkingConfig": {"EndpointsConfig": {"ployz": {"IPAMConfig": {"IPv4Address": "10.210.0.2"}}}},
            "OpenStdin": false,
            "Tty": false,
        })
    );
}
