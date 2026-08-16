use std::{process, time::Duration};

use ployz_core::{
    ContainerAction, ContainerId, ContainerKind, ContainerRuntimeObservation,
    InspectContainerRequest, Machine, MachineId, MachineSelector, RemoveContainerRequest,
    ResolvedServiceSpec, ServiceId, StopContainerRequest, op, select_service,
};
use ployz_testkit::{Cluster, ClusterPlan};

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn service_observations_and_lifecycle_remain_partial_in_a_real_cluster() {
    let plan = ClusterPlan::new(&format!("l3-service-{}", process::id()), 2).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    let machines = cluster.initialize_two().await.unwrap();
    let direct = cluster.api_address(0).unwrap();
    let mut client = ployz::connect::connect(
        std::path::Path::new("/missing-ployz-test-config"),
        Some(&direct),
        None,
    )
    .await
    .unwrap();
    assert_l3_061_default_spec(&cluster, &mut client, &machines[0]).await;
    assert_l3_062_full_spec(&cluster, &mut client, &machines[0]).await;

    let service_id = ServiceId::random();
    let first = spec(&service_id, "shared", "v1");
    let second = spec(&service_id, "shared", "v2");
    client
        .create_container(
            machines[0].id,
            ContainerKind::ServiceContainer,
            first.clone(),
        )
        .await
        .unwrap();
    client
        .create_container(
            machines[1].id,
            ContainerKind::ServiceContainer,
            second.clone(),
        )
        .await
        .unwrap();
    client
        .create_container(machines[0].id, ContainerKind::PreDeployHook, first.clone())
        .await
        .unwrap();
    client
        .create_container(machines[1].id, ContainerKind::PreDeployHook, second.clone())
        .await
        .unwrap();

    let live = wait_for_services(&mut client, 1, 4).await;
    let observed = select_service(&live.services, service_id.as_str())
        .unwrap()
        .clone();
    assert_eq!(observed.containers.len(), 2);
    assert_eq!(observed.hook_containers.len(), 2);
    assert_ne!(
        observed.containers.first().unwrap().resolved_spec,
        observed.containers.get(1).unwrap().resolved_spec
    );

    let collision_id = ServiceId::random();
    client
        .create_container(
            machines[1].id,
            ContainerKind::ServiceContainer,
            spec(&collision_id, "shared", "collision"),
        )
        .await
        .unwrap();
    let live = wait_for_services(&mut client, 2, 5).await;
    assert!(select_service(&live.services, "shared").is_err());
    assert_eq!(
        select_service(&live.services, collision_id.as_str())
            .unwrap()
            .service_id,
        collision_id
    );

    cluster.remote_machine_api_rule(0, "--insert").unwrap();
    let partial = client.live_services().await.unwrap();
    assert_eq!(partial.containers.successes.len(), 1);
    assert_eq!(partial.containers.failures.len(), 1);
    assert!(!partial.complete);

    let started = client
        .change_observed_service(&observed, ContainerAction::Start, None, None)
        .await;
    assert_eq!(started.successes.len(), 1);
    assert_eq!(started.failures.len(), 1);
    assert!(started.successes.iter().all(|success| {
        observed
            .hook_containers
            .iter()
            .all(|hook| hook.container_id != success.value)
    }));
    let local_after_start = client.live_services().await.unwrap();
    let local_service = select_service(&local_after_start.services, service_id.as_str()).unwrap();
    assert!(matches!(
        local_service.hook_containers.first().unwrap().runtime,
        ContainerRuntimeObservation::Created
    ));

    let stopped = client
        .change_observed_service(
            &observed,
            ContainerAction::Stop,
            Some("SIGTERM".into()),
            Some(10),
        )
        .await;
    assert_eq!(stopped.successes.len(), 2);
    assert_eq!(stopped.failures.len(), 2);
    assert!(
        stopped
            .failures
            .iter()
            .all(|failure| failure.machine_id == machines[1].id)
    );
    assert_ne!(
        stopped.failures.first().unwrap().error.container_id,
        stopped.failures.get(1).unwrap().error.container_id
    );
    let removed = client
        .change_observed_service(&observed, ContainerAction::Remove, None, Some(10))
        .await;
    assert_eq!(removed.successes.len(), 2);
    assert_eq!(removed.failures.len(), 2);

    cluster.remote_machine_api_rule(0, "--delete").unwrap();
    let live = client.live_services().await.unwrap();
    let service = select_service(&live.services, service_id.as_str()).unwrap();
    client
        .change_observed_service(service, ContainerAction::Remove, None, Some(10))
        .await;
    let live = client.live_services().await.unwrap();
    let service = select_service(&live.services, collision_id.as_str()).unwrap();
    client
        .change_observed_service(service, ContainerAction::Remove, None, Some(10))
        .await;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if client
                .live_services()
                .await
                .is_ok_and(|live| live.services.is_empty())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
}

async fn assert_l3_061_default_spec(
    cluster: &Cluster,
    client: &mut ployz::connect::Client,
    machine: &Machine,
) {
    let service_id = ServiceId::random();
    let spec: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
        "service_id": service_id,
        "name": "l3-default",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "alpine:3.23.3", "pull_policy": "missing" }
    }))
    .unwrap();
    let created = client
        .create_container(machine.id, ContainerKind::ServiceContainer, spec.clone())
        .await
        .unwrap();

    let observed = inspect_container(client, machine.id, created.container_id).await;
    assert_eq!(observed.resolved_spec, spec);
    let docker = docker_inspect(cluster, 0, &created.container_id);
    assert_eq!(
        docker
            .pointer("/Config/Labels/ployz.service.id")
            .and_then(serde_json::Value::as_str),
        Some(service_id.as_str())
    );
    assert_eq!(
        docker
            .pointer("/HostConfig/RestartPolicy/Name")
            .and_then(serde_json::Value::as_str),
        Some("unless-stopped")
    );
    assert!(
        docker
            .pointer("/Config/Env")
            .and_then(serde_json::Value::as_array)
            .unwrap()
            .iter()
            .any(|value| value == &format!("PLOYZ_MACHINE_ID={}", machine.id))
    );
    assert_eq!(stored_spec(cluster, 0, &created.container_id), spec);

    remove_container(client, machine.id, created.container_id).await;
    assert_removed(cluster, 0, &created.container_id);
}

async fn assert_l3_062_full_spec(
    cluster: &Cluster,
    client: &mut ployz::connect::Client,
    machine: &Machine,
) {
    let spec: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
        "service_id": ServiceId::random(),
        "name": "l3-full",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": {
            "image": "alpine:3.23.3",
            "command": ["sleep", "30"],
            "environment": { "TOKEN": "secret" },
            "cap_add": ["CHOWN"],
            "cap_drop": ["NET_RAW"],
            "healthcheck": { "state": "configured", "test": ["CMD", "true"], "interval_millis": 1000 },
            "pull_policy": "missing",
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
    let created = client
        .create_container(machine.id, ContainerKind::ServiceContainer, spec.clone())
        .await
        .unwrap();

    let observed = inspect_container(client, machine.id, created.container_id).await;
    assert_eq!(observed.resolved_spec, spec);
    let docker = docker_inspect(cluster, 0, &created.container_id);
    assert_eq!(
        docker
            .pointer("/HostConfig/Memory")
            .and_then(serde_json::Value::as_i64),
        Some(67_108_864)
    );
    assert_eq!(
        docker
            .pointer("/HostConfig/MemoryReservation")
            .and_then(serde_json::Value::as_i64),
        Some(33_554_432)
    );
    assert!(
        docker
            .pointer("/HostConfig/PortBindings/8080~1tcp")
            .is_some_and(serde_json::Value::is_array)
    );
    assert!(
        docker
            .pointer("/Config/Env")
            .and_then(serde_json::Value::as_array)
            .unwrap()
            .iter()
            .any(|value| value == "TOKEN=secret")
    );
    assert_eq!(stored_spec(cluster, 0, &created.container_id), spec);

    remove_container(client, machine.id, created.container_id).await;
    assert_removed(cluster, 0, &created.container_id);
}

async fn inspect_container(
    client: &mut ployz::connect::Client,
    machine_id: MachineId,
    container_id: ContainerId,
) -> ployz_core::ContainerObservation {
    client
        .call::<op::InspectContainer>(
            InspectContainerRequest { container_id },
            Some(&MachineSelector::from(&machine_id)),
        )
        .await
        .unwrap()
        .container
}

async fn remove_container(
    client: &mut ployz::connect::Client,
    machine_id: MachineId,
    container_id: ContainerId,
) {
    let target = MachineSelector::from(&machine_id);
    let _ = client
        .call::<op::StopContainer>(
            StopContainerRequest {
                container_id,
                signal: None,
                grace_period_seconds: None,
            },
            Some(&target),
        )
        .await;
    client
        .call::<op::RemoveContainer>(
            RemoveContainerRequest {
                container_id,
                remove_volumes: true,
                force: false,
            },
            Some(&target),
        )
        .await
        .unwrap();
}

fn docker_inspect(
    cluster: &Cluster,
    machine_index: usize,
    container_id: &ContainerId,
) -> serde_json::Value {
    let output = cluster
        .machine_shell(machine_index, &format!("docker inspect {container_id}"))
        .unwrap();
    serde_json::from_str::<Vec<serde_json::Value>>(&output)
        .unwrap()
        .pop()
        .unwrap()
}

fn stored_spec(
    cluster: &Cluster,
    machine_index: usize,
    container_id: &ContainerId,
) -> ResolvedServiceSpec {
    let output = cluster
        .machine_shell(
            machine_index,
            &format!(
                "sqlite3 /var/lib/ployz/machine.db \"SELECT service_spec FROM containers WHERE id = '{container_id}';\""
            ),
        )
        .unwrap();
    serde_json::from_str(output.trim()).unwrap()
}

fn assert_removed(cluster: &Cluster, machine_index: usize, container_id: &ContainerId) {
    assert!(
        cluster
            .machine_shell(machine_index, &format!("docker inspect {container_id}"))
            .is_err()
    );
    let stored = cluster
        .machine_shell(
            machine_index,
            &format!(
                "sqlite3 /var/lib/ployz/machine.db \"SELECT service_spec FROM containers WHERE id = '{container_id}';\""
            ),
        )
        .unwrap();
    assert!(stored.trim().is_empty());
}

async fn wait_for_services(
    client: &mut ployz::connect::Client,
    service_count: usize,
    container_count: usize,
) -> ployz_core::LiveServices<ployz_core::RpcError> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(live) = client.live_services().await
                && live.services.len() == service_count
                && live
                    .containers
                    .successes
                    .iter()
                    .map(|success| success.value.len())
                    .sum::<usize>()
                    == container_count
            {
                return live;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap()
}

fn spec(service_id: &ServiceId, name: &str, version: &str) -> ResolvedServiceSpec {
    serde_json::from_value(serde_json::json!({
        "service_id": service_id,
        "name": name,
        "mode": { "mode": "replicated", "replicas": 2 },
        "container": {
            "image": "alpine:3.23.3",
            "command": ["sleep", "30"],
            "environment": { "VERSION": version },
            "pull_policy": "missing"
        },
        "pre_deploy": { "command": ["true"] }
    }))
    .unwrap()
}
