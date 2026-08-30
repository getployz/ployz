//! Zentinel ingress lifecycle tracer against the privileged informing cluster.

use std::{path::Path, process, time::Duration};

use ployz_core::{
    ContainerAction, INGRESS_VERIFY_PATH, IngressProxyBackend, ResolvedServiceSpec, ServiceId,
};
use ployz_testkit::{Cluster, ClusterPlan};

use super::{
    change_container, cli, create_and_start, request, wait_config, wait_log_count, wait_running,
    wait_service,
};

#[tokio::test]
#[ignore = "informing: requires the privileged Ployz testkit image"]
async fn zentinel_recovers_after_its_sole_serving_container_returns() {
    let plan = ClusterPlan::new(&format!("l3-zentinel-{}", process::id()), 1).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    cluster.wait_ready(Duration::from_secs(30)).await.unwrap();
    let machine = cluster
        .initialize_first_with_backend(IngressProxyBackend::Zentinel)
        .await
        .unwrap();
    cluster.wait_ready(Duration::from_secs(60)).await.unwrap();
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if cluster
                .machines(0)
                .await
                .is_ok_and(|machines| machines.iter().any(|entry| entry.machine.id == machine.id))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .unwrap();
    let direct = cluster.api_address(0).unwrap();
    let mut client =
        ployz::connect::connect(Path::new("/missing-ployz-test-config"), Some(&direct), None)
            .await
            .unwrap();

    cli(&direct, &["ingress", "deploy"]);
    let ingress_id = wait_service(&mut client, "ingress", 1).await;
    let ingress = wait_running(&mut client, &ingress_id, 1).await.remove(0);
    assert_eq!(
        ingress.resolved_spec.container.image,
        ployz::ingress::ZENTINEL_IMAGE
    );
    cluster
        .machine_shell(
            0,
            &format!(
                "test \"$(docker inspect --format '{{{{.HostConfig.NetworkMode}}}}' {})\" = host",
                ingress.container_id
            ),
        )
        .unwrap();
    cluster
        .machine_shell(
            0,
            &format!(
                "test \"$(curl -fsS http://127.0.0.1{INGRESS_VERIFY_PATH})\" = '{}'",
                machine.id
            ),
        )
        .unwrap();

    let api_id = ServiceId::random();
    let api: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
        "service_id": api_id,
        "name": "api",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": {
            "image": "alpine:3.23.3",
            "command": ["sh", "-c", "while true; do printf 'HTTP/1.1 200 OK\\r\\nContent-Length: 3\\r\\n\\r\\nok\\n' | nc -l -p 8080; done"],
            "pull_policy": "missing"
        },
        "ports": [{
            "mode": "ingress",
            "hostname": { "kind": "explicit", "hostname": "zentinel.test" },
            "load_balancer_port": 80,
            "container_port": 8080,
            "http_protocol": "http"
        }]
    }))
    .unwrap();
    let api_container = create_and_start(&mut client, &machine, api).await;
    let observation = wait_running(&mut client, &api_id, 1).await.remove(0);
    let address = observation.address.unwrap();

    let serving = wait_config(&mut client, &machine, |config| {
        config.contains(&format!("target \"{}:8080\"", address.0))
    })
    .await;
    wait_private_digest(&cluster, &serving).await;
    assert_eq!(request(&cluster, "zentinel.test"), (200, "ok\n".into()));

    change_container(
        &mut client,
        machine.id,
        api_container,
        ContainerAction::Stop,
        Some(0),
    )
    .await;
    let empty = wait_config(&mut client, &machine, |config| {
        config.contains("target \"127.0.0.1:0\"")
            && !config.contains(&format!("{}:8080", address.0))
    })
    .await;
    assert_ne!(zentinel_digest(&serving), zentinel_digest(&empty));
    wait_private_digest(&cluster, &empty).await;
    assert_eq!(request(&cluster, "zentinel.test").0, 503);

    change_container(
        &mut client,
        machine.id,
        api_container,
        ContainerAction::Start,
        None,
    )
    .await;
    let restored = wait_config(&mut client, &machine, |config| {
        config.contains(&format!("target \"{}:8080\"", address.0))
    })
    .await;
    assert_ne!(zentinel_digest(&empty), zentinel_digest(&restored));
    wait_private_digest(&cluster, &restored).await;
    assert_eq!(request(&cluster, "zentinel.test"), (200, "ok\n".into()));

    for evidence in [
        "phase=\"validation\" outcome=\"accepted\"",
        "phase=\"reload\" outcome=\"requested\"",
        "phase=\"confirmation\" outcome=\"confirmed\"",
    ] {
        wait_log_count(&cluster, evidence, 3).await;
    }
}

fn zentinel_digest(config: &str) -> &str {
    config
        .split_once("listener \"ployz-admin-")
        .and_then(|(_, rest)| rest.split_once('\"'))
        .map(|(digest, _)| digest)
        .expect("rendered Zentinel config carries its projection digest")
}

async fn wait_private_digest(cluster: &Cluster, config: &str) {
    let digest = zentinel_digest(config);
    let confirmed = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if cluster
                .machine_shell(0, "curl -fsS http://127.0.0.1:2019/config")
                .is_ok_and(|active| active.contains(&format!("ployz-admin-{digest}")))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await;
    assert!(
        confirmed.is_ok(),
        "Zentinel did not confirm the active projection digest:\n{}\n{}",
        cluster.logs(0).unwrap_or_default(),
        cluster
            .machine_shell(
                0,
                "id=$(docker ps -aq --filter label=ployz.service.name=ingress | head -n1); docker inspect --format '{{json .State}}' \"$id\"; docker logs \"$id\" 2>&1"
            )
            .unwrap_or_default()
    );
}
