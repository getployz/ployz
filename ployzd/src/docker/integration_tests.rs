use std::{
    collections::BTreeMap,
    fs,
    future::Future,
    net::TcpListener,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use bollard::{
    exec::StartExecResults,
    models::{
        ContainerCreateBody, ExecConfig as DockerExecConfig, MountType, NetworkCreateRequest,
    },
    query_parameters::{
        CreateContainerOptionsBuilder, RemoveContainerOptionsBuilder,
        RenameContainerOptionsBuilder, StartContainerOptions,
    },
};
use futures_util::TryStreamExt;
use ployz_core::{
    AdvertisedEndpoint, CreateVolumeRequest, DockerVolumeId, MachineGateway, MachineName,
    PullPolicy, ResolvedServiceSpec,
};

use super::*;

// ponytail: serialize tests sharing the fixed Ployz network; use unique networks if parallelism matters.
static DOCKER_NETWORK_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const TEST_GATEWAY: MachineGateway = MachineGateway(Ipv4Addr::new(10, 210, 0, 1));

mod stream;

#[tokio::test]
#[ignore = "requires Docker and alpine:3.23.3"]
async fn l3_061_default_spec_creates_and_removes_from_docker_and_machine_db() {
    let _lock = DOCKER_NETWORK_LOCK.lock().await;
    let root = TestRoot::new();
    let specs = MachineSpecStore::open(root.0.join("machine.db"))
        .await
        .unwrap();
    let docker = LocalDocker::connect().unwrap();
    let runtime = ContainerRuntime::new(docker.clone(), specs.clone());
    let created_network = ensure_ployz_network(&docker.client).await;
    let machine_id = MachineId::random();
    let service_id = ServiceId::random();
    let service_name = ServiceName::parse("default-api").unwrap();
    let spec = fixture_spec(&service_id, &service_name);

    let created = runtime
        .create(
            &machine_id,
            TEST_GATEWAY,
            ContainerKind::ServiceContainer,
            &spec,
        )
        .await
        .unwrap();
    let inspected = runtime
        .inspect_managed(&created.container_id, &machine_id)
        .await
        .unwrap();
    assert_eq!(inspected.resolved_spec, spec);
    assert_eq!(inspected.kind, ContainerKind::ServiceContainer);
    assert_eq!(
        specs.get(&created.container_id).await.unwrap(),
        Some(spec.clone())
    );

    let malformed = create_managed_container(
        &docker.client,
        &service_id,
        &service_name,
        ContainerKind::ServiceContainer,
    )
    .await;
    let listed = runtime.list_managed(&machine_id).await.unwrap();
    assert!(
        listed
            .iter()
            .any(|container| container.container_id == created.container_id)
    );
    assert!(
        listed
            .iter()
            .all(|container| container.container_id != malformed)
    );
    remove_container(&docker.client, &malformed).await;

    runtime
        .remove(&created.container_id, true, true)
        .await
        .unwrap();
    assert!(specs.get(&created.container_id).await.unwrap().is_none());
    assert!(matches!(
        runtime
            .inspect_managed(&created.container_id, &machine_id)
            .await,
        Err(Error::ContainerNotFound(_))
    ));
    let orphan = ContainerId::parse("f".repeat(64)).unwrap();
    specs
        .config_operation()
        .await
        .put(&orphan, &fixture_spec(&service_id, &service_name))
        .await
        .unwrap();
    runtime.remove(&orphan, true, true).await.unwrap();
    assert!(specs.get(&orphan).await.unwrap().is_none());
    assert!(matches!(
        runtime.remove(&orphan, true, true).await,
        Err(Error::ContainerNotFound(_))
    ));

    let reset_target = runtime
        .create(
            &machine_id,
            TEST_GATEWAY,
            ContainerKind::ServiceContainer,
            &spec,
        )
        .await
        .unwrap();
    let malformed_reset_target = create_managed_container(
        &docker.client,
        &service_id,
        &service_name,
        ContainerKind::ServiceContainer,
    )
    .await;
    runtime.remove_all_managed().await.unwrap();
    assert!(
        specs
            .get(&reset_target.container_id)
            .await
            .unwrap()
            .is_none()
    );
    for container_id in [&reset_target.container_id, &malformed_reset_target] {
        assert!(matches!(
            docker
                .client
                .inspect_container(container_id.as_str(), None)
                .await,
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404,
                ..
            })
        ));
    }

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
        runtime.start(&unmanaged).await,
        Err(Error::NotManaged)
    ));
    assert!(matches!(
        runtime.stop(&unmanaged, None, None).await,
        Err(Error::NotManaged)
    ));
    assert!(matches!(
        runtime.remove(&unmanaged, true, true).await,
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
    let runtime = ContainerRuntime::new(docker.clone(), specs.clone());
    let created_network = ensure_ployz_network(&docker.client).await;
    let machine_id = MachineId::random();
    let metadata = fs::metadata(&root.0).unwrap();
    let uid = metadata.uid();
    let gid = metadata.gid();
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
                "uid": uid,
                "gid": gid,
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

    let created = runtime
        .create(
            &machine_id,
            TEST_GATEWAY,
            ContainerKind::ServiceContainer,
            &spec,
        )
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
    assert_eq!(host.dns, Some(vec![TEST_GATEWAY.0.to_string()]));
    assert_eq!(host.dns_search, Some(vec!["internal".into()]));
    assert_eq!(host.dns_options, Some(vec!["ndots:1".into()]));
    assert!(host.port_bindings.unwrap().contains_key("8080/tcp"));
    let mounts = host.mounts.unwrap();
    assert_eq!(mounts.first().unwrap().target.as_deref(), Some("/scratch"));
    assert_eq!(
        mounts
            .iter()
            .find(|mount| mount.target.as_deref() == Some("/etc/settings.conf"))
            .unwrap()
            .read_only,
        Some(true)
    );
    assert_eq!(
        specs.get(&created.container_id).await.unwrap(),
        Some(spec.clone())
    );

    runtime.start(&created.container_id).await.unwrap();
    runtime.start(&created.container_id).await.unwrap();
    runtime
        .stop(&created.container_id, None, Some(0))
        .await
        .unwrap();

    runtime
        .remove(&created.container_id, true, true)
        .await
        .unwrap();
    let mut failed_spec = spec.clone();
    failed_spec.container.image = format!("ployz-missing-{machine_id}");
    failed_spec.container.pull_policy = PullPolicy::Never;
    assert!(
        runtime
            .create(
                &machine_id,
                TEST_GATEWAY,
                ContainerKind::ServiceContainer,
                &failed_spec,
            )
            .await
            .is_err()
    );
    assert_eq!(
        fs::read_dir(root.0.join("configs")).unwrap().count(),
        0,
        "failed creation leaked a materialized config"
    );
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
#[ignore = "requires Docker"]
async fn machine_local_volume_lifecycle_preserves_identity_and_labels() {
    let root = TestRoot::new();
    let runtime = ContainerRuntime::open(root.0.join("machine.db"))
        .await
        .unwrap();
    let machine_id = MachineId::random();
    let name =
        ployz_core::DockerVolumeName::parse(format!("ployz-volume-test-{machine_id}")).unwrap();
    let labels = BTreeMap::from([("purpose".into(), "machine-local-test".into())]);

    let created = runtime
        .create_volume(
            &machine_id,
            CreateVolumeRequest {
                name: name.clone(),
                driver: "local".into(),
                options: BTreeMap::new(),
                labels: labels.clone(),
            },
        )
        .await
        .unwrap();
    let listed = runtime.list_volumes(&machine_id).await.unwrap();
    let inspected = runtime.inspect_volume(&machine_id, &name).await.unwrap();
    runtime.remove_volume(&name, false).await.unwrap();

    assert_eq!(created.id, DockerVolumeId { machine_id, name });
    assert_eq!(created.driver, "local");
    assert_eq!(created.labels, labels);
    assert!(listed.contains(&created));
    assert_eq!(inspected, created);
}

#[tokio::test]
#[ignore = "requires Docker and alpine:3.23.3"]
async fn container_creation_uses_bind_named_and_tmpfs_mounts() {
    let root = TestRoot::new();
    fs::create_dir_all(root.0.join("bind")).unwrap();
    let specs = MachineSpecStore::open(root.0.join("machine.db"))
        .await
        .unwrap();
    let docker = LocalDocker::connect().unwrap();
    let runtime = ContainerRuntime::new(docker.clone(), specs);
    let created_network = match docker
        .client
        .inspect_network(crate::network::DOCKER_NETWORK_NAME, None)
        .await
    {
        Ok(_) => false,
        Err(error) if crate::docker_image::is_not_found(&error) => {
            docker
                .client
                .create_network(NetworkCreateRequest {
                    name: crate::network::DOCKER_NETWORK_NAME.into(),
                    ..Default::default()
                })
                .await
                .unwrap();
            true
        }
        Err(error) => panic!("inspect test network: {error}"),
    };
    let published_port = unused_address().port();
    let machine_id = MachineId::random();
    let name =
        ployz_core::DockerVolumeName::parse(format!("ployz-mount-test-{machine_id}")).unwrap();
    runtime
        .create_volume(
            &machine_id,
            CreateVolumeRequest {
                name: name.clone(),
                driver: "local".into(),
                options: BTreeMap::new(),
                labels: BTreeMap::new(),
            },
        )
        .await
        .unwrap();
    let spec: ResolvedServiceSpec = serde_json::from_value(json!({
        "service_id": ServiceId::random(),
        "name": "mount-test",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": {
            "image": "alpine:3.23.3",
            "command": ["sleep", "60"],
            "pull_policy": "missing",
            "healthcheck": {"test":["CMD","true"],"interval_millis":1000,"timeout_millis":2000,"retries":3},
            "log_driver": {"name":"local","options":{}},
            "config_mounts": [{"config_name":"settings","target":"/etc/ployz/settings","uid":1000,"gid":1001,"mode":288}]
        },
        "ports": [{
            "mode":"host",
            "bind":{"kind":"address","address":"127.0.0.1"},
            "published_port":published_port,
            "container_port":8080,
            "transport_protocol":"tcp"
        }],
        "volumes": [
            {"reference":"host","source":{"kind":"bind","machine_path":root.0.join("bind")}},
            {"reference":"data","source":{"kind":"named","name":name}},
            {"reference":"memory","source":{"kind":"tmpfs","size_bytes":4096,"mode":448}}
        ],
        "mounts": [
            {"volume":"host","target":"/host"},
            {"volume":"data","target":"/data","read_only":true},
            {"volume":"memory","target":"/run/cache"}
        ],
        "configs": [{"name":"settings","content":[112,111,114,116,61,56,48,56,48]}]
    }))
    .unwrap();

    let container_id = runtime
        .create(
            &machine_id,
            TEST_GATEWAY,
            ContainerKind::ServiceContainer,
            &spec,
        )
        .await
        .unwrap()
        .container_id;
    let inspected = docker
        .client
        .inspect_container(container_id.as_str(), None)
        .await
        .unwrap();
    let host_config = inspected.host_config.as_ref().unwrap();
    let mounts = host_config.mounts.clone().unwrap();
    let container_config = inspected.config.as_ref().unwrap();
    assert_eq!(
        host_config.network_mode.as_deref(),
        Some(crate::network::DOCKER_NETWORK_NAME)
    );
    let binding = host_config
        .port_bindings
        .as_ref()
        .and_then(|bindings| bindings.get("8080/tcp"))
        .and_then(Option::as_ref)
        .and_then(|bindings| bindings.first())
        .unwrap();
    assert_eq!(
        binding.host_port.as_deref(),
        Some(published_port.to_string().as_str())
    );
    assert_eq!(
        host_config
            .log_config
            .as_ref()
            .and_then(|config| config.typ.as_deref()),
        Some("local")
    );
    assert_eq!(
        container_config.exposed_ports.as_ref().unwrap(),
        &["8080/tcp".to_owned()]
    );
    let healthcheck = container_config.healthcheck.as_ref().unwrap();
    assert_eq!(
        healthcheck.test.as_ref().unwrap(),
        &["CMD".to_owned(), "true".to_owned()]
    );
    assert_eq!(healthcheck.interval, Some(1_000_000_000));
    assert_eq!(healthcheck.timeout, Some(2_000_000_000));
    assert_eq!(healthcheck.retries, Some(3));
    assert!(
        inspected
            .network_settings
            .as_ref()
            .and_then(|settings| settings.networks.as_ref())
            .is_some_and(|networks| networks.contains_key(crate::network::DOCKER_NETWORK_NAME))
    );
    let config_mount = mounts
        .iter()
        .find(|mount| mount.target.as_deref() == Some("/etc/ployz/settings"))
        .unwrap();
    let config_path = config_mount.source.as_ref().unwrap();
    let config = fs::read_to_string(config_path).unwrap();
    let metadata = fs::metadata(config_path).unwrap();
    docker
        .client
        .start_container(container_id.as_str(), None::<StartContainerOptions>)
        .await
        .unwrap();
    let write = docker
        .client
        .create_exec(
            container_id.as_str(),
            DockerExecConfig {
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                cmd: Some(vec![
                    "sh".into(),
                    "-c".into(),
                    "printf changed >/etc/ployz/settings".into(),
                ]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    if let StartExecResults::Attached { output, .. } =
        docker.client.start_exec(&write.id, None).await.unwrap()
    {
        output.try_collect::<Vec<_>>().await.unwrap();
    }
    let write_exit_code = docker
        .client
        .inspect_exec(&write.id)
        .await
        .unwrap()
        .exit_code;
    docker
        .client
        .remove_container(
            container_id.as_str(),
            Some(RemoveContainerOptionsBuilder::default().force(true).build()),
        )
        .await
        .unwrap();
    runtime.remove_volume(&name, false).await.unwrap();
    if created_network {
        docker
            .client
            .remove_network(crate::network::DOCKER_NETWORK_NAME)
            .await
            .unwrap();
    }

    assert!(
        mounts
            .iter()
            .any(|mount| mount.typ == Some(MountType::BIND))
    );
    assert!(mounts.iter().any(|mount| {
        mount.typ == Some(MountType::VOLUME) && mount.source.as_deref() == Some(name.as_str())
    }));
    assert!(
        mounts
            .iter()
            .any(|mount| mount.typ == Some(MountType::TMPFS))
    );
    assert_eq!(config, "port=8080");
    assert_eq!(config_mount.read_only, Some(true));
    assert_ne!(write_exit_code, Some(0));
    assert_eq!(metadata.permissions().mode() & 0o777, 0o440);
    assert_eq!(metadata.uid(), 1000);
    assert_eq!(metadata.gid(), 1001);
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
    let runtime = ContainerRuntime::new(docker.clone(), specs.clone());
    let mut local = crate::machine::LocalMachineStore::open(root.0.join("machine")).unwrap();
    let machine_id = local
        .initialize(
            MachineName::parse("observer").unwrap(),
            "10.210.0.0/16".parse().unwrap(),
            None,
            vec![AdvertisedEndpoint("127.0.0.1:51820".parse().unwrap())],
            None,
        )
        .unwrap()
        .id;
    let local = Arc::new(Mutex::new(local));
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
    {
        let mut operation = specs.config_operation().await;
        operation.put(&service, &spec).await.unwrap();
        operation.put(&hook, &spec).await.unwrap();
    }

    let observer = ContainerObserver::new(
        runtime,
        replicated.clone(),
        Arc::clone(&local),
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

    specs
        .config_operation()
        .await
        .put(&fallback, &spec)
        .await
        .unwrap();
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
    let failed_observer = ContainerObserver::new(
        ContainerRuntime::new(failed_docker, specs),
        replicated.clone(),
        local,
        machine_id,
    );
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
        created_at_unix_nanos: 0,
        machine_id,
        service_id: service_id.clone(),
        service_name: service_name.clone(),
        kind: ContainerKind::ServiceContainer,
        runtime: ContainerRuntimeObservation::Created,
        effective_healthcheck: None,
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
