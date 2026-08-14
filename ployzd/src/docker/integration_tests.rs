use std::{collections::BTreeMap, fs, future::Future, net::TcpListener, path::PathBuf};

use bollard::{
    models::{ContainerCreateBody, NetworkCreateRequest},
    query_parameters::{
        CreateContainerOptionsBuilder, RemoveContainerOptionsBuilder, RenameContainerOptionsBuilder,
    },
};
use ployz_core::ResolvedServiceSpec;

use super::*;

// ponytail: serialize tests sharing the fixed Ployz network; use unique networks if parallelism matters.
static DOCKER_NETWORK_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
#[ignore = "requires Docker and alpine:3.23.3"]
async fn l3_061_default_spec_creates_and_removes_from_docker_and_machine_db() {
    let _lock = DOCKER_NETWORK_LOCK.lock().await;
    let root = TestRoot::new();
    let specs = MachineSpecStore::open(root.0.join("machine.db"))
        .await
        .unwrap();
    let docker = LocalDocker::connect().unwrap();
    let created_network = ensure_ployz_network(&docker.client).await;
    let machine_id = MachineId::random();
    let service_id = ServiceId::random();
    let service_name = ServiceName::parse("default-api").unwrap();
    let spec = fixture_spec(&service_id, &service_name);

    let created = docker
        .create(&machine_id, &specs, ContainerKind::ServiceContainer, &spec)
        .await
        .unwrap();
    let inspected = docker
        .inspect_managed(&created.container_id, &machine_id, &specs)
        .await
        .unwrap();
    assert_eq!(inspected.resolved_spec, spec);
    assert_eq!(inspected.kind, ContainerKind::ServiceContainer);
    assert_eq!(specs.get(&created.container_id).await.unwrap(), Some(spec));

    docker
        .remove(&specs, &created.container_id, true, true)
        .await
        .unwrap();
    assert!(specs.get(&created.container_id).await.unwrap().is_none());
    assert!(matches!(
        docker
            .inspect_managed(&created.container_id, &machine_id, &specs)
            .await,
        Err(Error::ContainerNotFound(_))
    ));
    let orphan = ContainerId::parse("f".repeat(64)).unwrap();
    specs
        .put(&orphan, &fixture_spec(&service_id, &service_name))
        .await
        .unwrap();
    docker.remove(&specs, &orphan, true, true).await.unwrap();
    assert!(specs.get(&orphan).await.unwrap().is_none());
    assert!(matches!(
        docker.remove(&specs, &orphan, true, true).await,
        Err(Error::ContainerNotFound(_))
    ));

    let unmanaged = docker
        .client
        .create_container(
            None::<bollard::query_parameters::CreateContainerOptions>,
            ContainerCreateBody {
                image: Some("alpine:3.23.3".into()),
                cmd: Some(vec!["sleep".into(), "30".into()]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let unmanaged = ContainerId::parse(unmanaged.id).unwrap();
    assert!(matches!(
        docker.start(&specs, &unmanaged).await,
        Err(Error::NotManaged)
    ));
    assert!(matches!(
        docker.stop(&specs, &unmanaged, None, None).await,
        Err(Error::NotManaged)
    ));
    assert!(matches!(
        docker.remove(&specs, &unmanaged, true, true).await,
        Err(Error::NotManaged)
    ));
    docker
        .client
        .inspect_container(unmanaged.as_str(), None)
        .await
        .unwrap();
    remove_container(&docker.client, &unmanaged).await;
    cleanup_ployz_network(&docker.client, created_network).await;
}

#[tokio::test]
#[ignore = "requires Docker and alpine:3.23.3"]
async fn l3_062_full_spec_reaches_docker_and_machine_db() {
    let _lock = DOCKER_NETWORK_LOCK.lock().await;
    let root = TestRoot::new();
    let specs = MachineSpecStore::open(root.0.join("machine.db"))
        .await
        .unwrap();
    let docker = LocalDocker::connect().unwrap();
    let created_network = ensure_ployz_network(&docker.client).await;
    let machine_id = MachineId::random();
    let spec: ResolvedServiceSpec = serde_json::from_value(json!({
        "service_id": "b".repeat(32),
        "name": "full-api",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": {
            "image": "alpine:3.23.3",
            "command": ["sleep", "30"],
            "environment": { "TOKEN": "secret" },
            "cap_add": ["CHOWN"],
            "cap_drop": ["NET_RAW"],
            "healthcheck": { "test": ["CMD", "true"], "interval_millis": 1000 },
            "pull_policy": "always",
            "init": true,
            "user": "0",
            "working_directory": "/tmp",
            "resources": {
                "memory_bytes": 67108864,
                "memory_reservation_bytes": 33554432,
                "shared_memory_bytes": 33554432,
                "ulimits": { "nofile": { "soft": 1024, "hard": 2048 } }
            },
            "config_mounts": [{
                "config_name": "settings",
                "target": "/etc/settings.conf",
                "mode": 288
            }]
        },
        "ports": [{
            "mode": "host",
            "bind": { "kind": "address", "address": "127.0.0.1" },
            "published_port": 49111,
            "container_port": 8080,
            "transport_protocol": "tcp"
        }],
        "volumes": [{
            "reference": "scratch",
            "source": { "kind": "tmpfs", "size_bytes": 1048576, "mode": 448 }
        }],
        "mounts": [{ "volume": "scratch", "target": "/scratch" }],
        "configs": [{ "name": "settings", "content": [107, 61, 118, 10] }]
    }))
    .unwrap();

    let created = docker
        .create(&machine_id, &specs, ContainerKind::ServiceContainer, &spec)
        .await
        .unwrap();
    let raw = docker
        .client
        .inspect_container(created.container_id.as_str(), None)
        .await
        .unwrap();
    let config = raw.config.unwrap();
    let host = raw.host_config.unwrap();
    assert!(
        config
            .env
            .unwrap()
            .contains(&format!("PLOYZ_MACHINE_ID={machine_id}"))
    );
    assert_eq!(host.memory, Some(67_108_864));
    assert_eq!(host.memory_reservation, Some(33_554_432));
    assert!(host.port_bindings.unwrap().contains_key("8080/tcp"));
    assert_eq!(
        host.mounts.unwrap().first().unwrap().target.as_deref(),
        Some("/scratch")
    );
    assert_eq!(specs.get(&created.container_id).await.unwrap(), Some(spec));

    docker
        .remove(&specs, &created.container_id, true, true)
        .await
        .unwrap();
    cleanup_ployz_network(&docker.client, created_network).await;
}

async fn ensure_ployz_network(docker: &Docker) -> bool {
    match docker
        .inspect_network(crate::network::DOCKER_NETWORK_NAME, None)
        .await
    {
        Ok(_) => false,
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => {
            docker
                .create_network(NetworkCreateRequest {
                    name: crate::network::DOCKER_NETWORK_NAME.into(),
                    ..Default::default()
                })
                .await
                .unwrap();
            true
        }
        Err(error) => panic!("failed to inspect Ployz Docker network: {error}"),
    }
}

async fn cleanup_ployz_network(docker: &Docker, created: bool) {
    if created {
        docker
            .remove_network(crate::network::DOCKER_NETWORK_NAME)
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "requires Docker, alpine:3.23.3, and the pinned Corrosion image"]
async fn docker_events_and_rescans_publish_redacted_local_observations() {
    let root = TestRoot::new();
    let mut corrosion = crate::corrosion::CorrosionConfig::new(
        root.0.join("corrosion"),
        root.0.join("run"),
        unused_address(),
        unused_address(),
        format!("ployz-corrosion-{}", MachineId::random()),
    )
    .start()
    .await
    .unwrap();
    let replicated = corrosion.store().clone();
    let specs = MachineSpecStore::open(root.0.join("machine.db"))
        .await
        .unwrap();
    let docker = LocalDocker::connect().unwrap();
    let machine_id = MachineId::random();
    let foreign_machine_id = MachineId::random();
    let service_id = ServiceId::parse("a".repeat(32)).unwrap();
    let service_name = ServiceName::parse("api").unwrap();

    let stale_local = fixture_observation(
        ContainerId::parse("c".repeat(64)).unwrap(),
        machine_id.clone(),
        service_id.clone(),
        service_name.clone(),
    );
    let stale_foreign = fixture_observation(
        ContainerId::parse("d".repeat(64)).unwrap(),
        foreign_machine_id,
        service_id.clone(),
        service_name.clone(),
    );
    replicated.publish_container(&stale_local).await.unwrap();
    replicated.publish_container(&stale_foreign).await.unwrap();

    let service = create_managed_container(
        &docker.client,
        &service_id,
        &service_name,
        ContainerKind::ServiceContainer,
    )
    .await;
    let hook = create_managed_container(
        &docker.client,
        &service_id,
        &service_name,
        ContainerKind::PreDeployHook,
    )
    .await;
    let fallback = create_managed_container(
        &docker.client,
        &service_id,
        &service_name,
        ContainerKind::ServiceContainer,
    )
    .await;
    let spec = fixture_spec(&service_id, &service_name);
    specs.put(&service, &spec).await.unwrap();
    specs.put(&hook, &spec).await.unwrap();

    let observer = ContainerObserver::new(
        docker.clone(),
        specs.clone(),
        replicated.clone(),
        machine_id.clone(),
    )
    .with_rescan_interval(Duration::from_secs(3));
    let (shutdown, shutdown_rx) = watch::channel(false);
    let task = tokio::spawn(async move { observer.run(shutdown_rx).await });

    wait_for(Duration::from_secs(5), || async {
        let observations = replicated.containers().await.unwrap().observations;
        observations.iter().any(|item| item.container_id == service)
            && observations.iter().any(|item| item.container_id == hook)
    })
    .await;
    let observations = replicated.containers().await.unwrap().observations;
    let service_observation = observations
        .iter()
        .find(|item| item.container_id == service)
        .unwrap();
    let hook_observation = observations
        .iter()
        .find(|item| item.container_id == hook)
        .unwrap();
    assert_eq!(service_observation.kind, ContainerKind::ServiceContainer);
    assert_eq!(hook_observation.kind, ContainerKind::PreDeployHook);
    assert_eq!(
        service_observation.runtime,
        ContainerRuntimeObservation::Created
    );
    assert_eq!(
        hook_observation.runtime,
        ContainerRuntimeObservation::Created
    );
    assert!(replicated.container(&fallback).await.unwrap().is_none());
    assert!(
        service_observation
            .resolved_spec
            .container
            .environment
            .is_empty()
    );
    assert!(
        service_observation
            .resolved_spec
            .pre_deploy
            .as_ref()
            .unwrap()
            .environment
            .is_empty()
    );
    assert!(
        !observations
            .iter()
            .any(|item| item.container_id == stale_local.container_id)
    );
    assert!(
        observations
            .iter()
            .any(|item| item.container_id == stale_foreign.container_id)
    );
    assert!(
        !serde_json::to_string(&observations)
            .unwrap()
            .contains("DOCKER_SECRET")
    );

    docker
        .client
        .start_container(service.as_str(), None)
        .await
        .unwrap();
    wait_for(Duration::from_secs(2), || async {
        replicated
            .container(&service)
            .await
            .unwrap()
            .is_some_and(|item| {
                item.runtime
                    == ContainerRuntimeObservation::Running {
                        health: HealthObservation::NotConfigured,
                    }
            })
    })
    .await;

    specs.put(&fallback, &spec).await.unwrap();
    wait_for(Duration::from_secs(5), || async {
        replicated.container(&fallback).await.unwrap().is_some()
    })
    .await;
    let renamed = format!("ployz-fallback-{}", MachineId::random());
    docker
        .client
        .rename_container(
            fallback.as_str(),
            RenameContainerOptionsBuilder::default()
                .name(&renamed)
                .build(),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_ne!(
        replicated
            .container(&fallback)
            .await
            .unwrap()
            .unwrap()
            .display_name,
        renamed
    );
    wait_for(Duration::from_secs(5), || async {
        replicated
            .container(&fallback)
            .await
            .unwrap()
            .is_some_and(|item| item.display_name == renamed)
    })
    .await;

    remove_container(&docker.client, &hook).await;
    wait_for(Duration::from_secs(2), || async {
        replicated.container(&hook).await.unwrap().is_none()
    })
    .await;
    assert!(replicated.container(&service).await.unwrap().is_some());

    shutdown.send_replace(true);
    task.await.unwrap().unwrap();
    let before_failure = replicated.raw_container(&service).await.unwrap();
    let invalid_socket = root.0.join("not-docker.sock");
    fs::write(&invalid_socket, []).unwrap();
    let failed_docker = LocalDocker::connect_socket(invalid_socket.to_str().unwrap()).unwrap();
    let failed_observer =
        ContainerObserver::new(failed_docker, specs, replicated.clone(), machine_id);
    assert!(failed_observer.sync_once().await.is_err());
    assert_eq!(
        replicated.raw_container(&service).await.unwrap(),
        before_failure
    );
    assert!(
        !serde_json::from_str::<serde_json::Value>(&before_failure.unwrap())
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("sync_status")
    );
    remove_container(&docker.client, &service).await;
    remove_container(&docker.client, &fallback).await;
    corrosion.cleanup().await.unwrap();
}

async fn create_managed_container(
    docker: &Docker,
    service_id: &ServiceId,
    service_name: &ServiceName,
    kind: ContainerKind,
) -> ContainerId {
    let mut labels = HashMap::from([
        (LABEL_MANAGED.to_owned(), String::new()),
        (LABEL_SERVICE_ID.to_owned(), service_id.to_string()),
        (LABEL_SERVICE_NAME.to_owned(), service_name.to_string()),
    ]);
    if kind == ContainerKind::PreDeployHook {
        labels.insert(LABEL_HOOK.to_owned(), LABEL_HOOK_PRE_DEPLOY.to_owned());
    }
    let name = format!("ployz-observer-test-{}", MachineId::random());
    let response = docker
        .create_container(
            Some(CreateContainerOptionsBuilder::default().name(&name).build()),
            ContainerCreateBody {
                image: Some("alpine:3.23.3".into()),
                cmd: Some(vec!["sleep".into(), "30".into()]),
                env: Some(vec!["DOCKER_SECRET=local-only".into()]),
                labels: Some(labels),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    ContainerId::parse(response.id).unwrap()
}

async fn remove_container(docker: &Docker, container_id: &ContainerId) {
    docker
        .remove_container(
            container_id.as_str(),
            Some(RemoveContainerOptionsBuilder::default().force(true).build()),
        )
        .await
        .unwrap();
}

fn fixture_spec(service_id: &ServiceId, service_name: &ServiceName) -> ResolvedServiceSpec {
    serde_json::from_value(json!({
        "service_id": service_id,
        "name": service_name,
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": {
            "image": "alpine:3.23.3",
            "environment": { "TOKEN": "service-secret" },
            "pull_policy": "missing"
        },
        "pre_deploy": {
            "command": ["true"],
            "environment": { "TOKEN": "hook-secret" }
        }
    }))
    .unwrap()
}

fn fixture_observation(
    container_id: ContainerId,
    machine_id: MachineId,
    service_id: ServiceId,
    service_name: ServiceName,
) -> ContainerObservation {
    ContainerObservation {
        display_name: format!("{service_name}-stale"),
        machine_id,
        service_id: service_id.clone(),
        service_name: service_name.clone(),
        kind: ContainerKind::ServiceContainer,
        runtime: ContainerRuntimeObservation::Created,
        resolved_spec: fixture_spec(&service_id, &service_name),
        address: None,
        labels: BTreeMap::new(),
        container_id,
    }
}

async fn wait_for<F, Fut>(timeout: Duration, mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    tokio::time::timeout(timeout, async {
        while !condition().await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

fn unused_address() -> std::net::SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("ployzd-docker-observer-{}", MachineId::random())))
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
