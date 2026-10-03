use std::{
    collections::{BTreeMap, BTreeSet},
    process::{self, Command},
    time::Duration,
};

use ployz::deploy::plan_deploy;
use ployz_core::{
    ContainerId, ContainerKind, GetIngressProxyConfigRequest, InspectContainerRequest,
    ListMachinesRequest, Machine, MachineId, MachineTarget, Namespace, RequestedServiceSpec,
    ResolvedServiceSpec, ServiceId, StartContainerRequest, op,
};
use ployz_testkit::{Cluster, ClusterPlan};
use tokio_util::sync::CancellationToken;

const SERVE: &str = "while true; do printf 'HTTP/1.1 200 OK\\r\\nContent-Length: 3\\r\\n\\r\\nok\\n' | nc -l -p 8080; done";

/// Draining web-2 moves a hooked replicated Service and a single-Container Service onto
/// web-1 without a serving gap, without rerunning the hook, on the same images; a rerun,
/// now against an already-cordoned Server, is a no-op.
#[tokio::test]
#[ignore = "informing: requires the privileged Ployz testkit image"]
async fn drain_moves_containers_off_without_a_gap_or_a_deploy() {
    let plan = ClusterPlan::new(&format!("l3-drain-{}", process::id()), 2).unwrap();
    let probe_machine = plan.machine_name(0);
    let cluster = Cluster::create(plan).unwrap();
    let [web1, web2] = cluster.initialize_two().await.unwrap();
    let direct = cluster.api_address(0).unwrap();
    let mut client = connect(&direct).await;
    cli(
        &direct,
        &[
            "server",
            "set",
            web1.id.as_str(),
            "--accepts-ingress=true",
            "--ingress-image",
            "caddy:2.10.2",
        ],
    );

    let web = serde_json::from_value::<RequestedServiceSpec>(serde_json::json!({
        "name": "web",
        "mode": { "mode": "replicated", "replicas": 2 },
        "container": { "image": "alpine:3.23.3", "command": ["sh", "-c", SERVE], "pull_policy": "missing" },
        "ports": [{ "mode": "ingress", "hostname": "web.test", "load_balancer_port": 80, "container_port": 8080, "http_protocol": "http" }],
        "pre_deploy": { "command": ["true"] },
        "update": { "order": "start_first" }
    }))
    .unwrap();
    let web_id = deploy(&mut client, &web).await;
    let solo: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
        "service_id": ServiceId::random(),
        "name": "solo",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "alpine:3.23.3", "command": ["sh", "-c", SERVE], "pull_policy": "missing" },
        "ports": [{ "mode": "ingress", "hostname": "solo.test", "load_balancer_port": 80, "container_port": 8080, "http_protocol": "http" }]
    }))
    .unwrap();
    let solo_id = solo.service_id;
    create_and_start(&mut client, &web2, solo).await;

    let before = running(&mut client, &[web_id, solo_id], 3).await;
    assert_eq!(
        on(&before, &web2.id),
        2,
        "one web replica and solo start on web-2"
    );
    wait_routed(&mut client, &web1, &before).await;
    let hooks = hook_containers(&mut client, &web_id).await;
    assert!(!hooks.is_empty(), "the deploy ran its pre-deploy hook");
    let images = image_ids(&mut client, &before).await;

    let (done, finished) = tokio::sync::watch::channel(false);
    let probe = tokio::task::spawn_blocking({
        move || {
            let mut probes = 0;
            while !*finished.borrow() {
                for host in ["web.test", "solo.test"] {
                    let output = Command::new("docker")
                        .args(["exec", &probe_machine, "curl", "-fsS", "-H"])
                        .arg(format!("Host: {host}"))
                        .arg("http://127.0.0.1")
                        .output()
                        .unwrap();
                    assert!(
                        output.status.success(),
                        "{host} stopped serving during the drain: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "ok");
                }
                probes += 1;
                std::thread::sleep(Duration::from_millis(25));
            }
            probes
        }
    });
    let drained = tokio::task::spawn_blocking({
        let direct = direct.clone();
        let server = web2.name.to_string();
        move || cli(&direct, &["server", "drain", &server])
    })
    .await
    .unwrap();
    done.send(true).unwrap();
    assert!(probe.await.unwrap() > 0);
    assert!(
        drained.contains(&format!(
            "app/web: moved 1 from {} to {}",
            web2.name, web1.name
        )) && drained.contains(&format!(
            "app/solo: moved 1 from {} to {}",
            web2.name, web1.name
        )) && drained.contains(&format!("Still on {}: ployz-system/ingress\n", web2.name))
            && drained.contains("Turning the services role back on does not move anything back."),
        "{drained}"
    );

    let after = running(&mut client, &[web_id, solo_id], 3).await;
    assert_eq!(on(&after, &web1.id), 3, "everything runs on web-1");
    assert_eq!(hook_containers(&mut client, &web_id).await, hooks);
    let mut moved_images = image_ids(&mut client, &after)
        .await
        .into_values()
        .collect::<Vec<_>>();
    let mut original_images = images.into_values().collect::<Vec<_>>();
    moved_images.sort();
    original_images.sort();
    assert_eq!(moved_images, original_images);

    let rerun: serde_json::Value = serde_json::from_str(&cli(
        &direct,
        &["--json", "server", "drain", web2.name.as_str()],
    ))
    .unwrap();
    assert_eq!(rerun.get("services"), Some(&serde_json::json!([])));
    // The Ingress Proxy follows the ingress role, not the services role.
    assert_eq!(
        rerun.get("remaining"),
        Some(&serde_json::json!(["ployz-system/ingress"]))
    );
    assert_eq!(
        running(&mut client, &[web_id, solo_id], 3)
            .await
            .into_keys()
            .collect::<BTreeSet<_>>(),
        after.into_keys().collect::<BTreeSet<_>>()
    );
}

/// `latest` moves on in the registry before the drain; the moved Container still runs
/// the image it ran, not the newer `latest`.
#[tokio::test]
#[ignore = "informing: requires the privileged Ployz testkit image"]
async fn drain_keeps_the_running_image_when_its_tag_moved() {
    let plan = ClusterPlan::new(&format!("l3-drain-tag-{}", process::id()), 2).unwrap();
    let registry = format!("{}-registry", plan.name());
    let network = plan.network();
    let labels = plan
        .labels()
        .into_iter()
        .flat_map(|(key, value)| ["--label".to_owned(), format!("{key}={value}")])
        .collect::<Vec<_>>();
    let cluster = Cluster::create(plan).unwrap();
    let [web1, web2] = cluster.initialize_two().await.unwrap();
    let mut run = vec!["run", "-d", "--name", &registry, "--network", &network];
    run.extend(labels.iter().map(String::as_str));
    run.push("registry:2");
    docker(&run);
    // Docker trusts plain HTTP only on loopback, so each Machine forwards loopback to it.
    for index in 0..2 {
        cluster
            .machine_shell(
                index,
                &format!(
                    "nohup socat TCP-LISTEN:5000,fork,reuseaddr TCP:{registry}:5000 >/dev/null 2>&1 &"
                ),
            )
            .unwrap();
    }
    let image = "127.0.0.1:5000/drain-app:latest";
    cluster
        .machine_shell(
            1,
            &format!(
                "for i in $(seq 50); do docker tag alpine:3.23.3 {image} && docker push {image} && exit 0; sleep 0.2; done; exit 1"
            ),
        )
        .unwrap();
    let direct = cluster.api_address(0).unwrap();
    let mut client = connect(&direct).await;
    // web-1 takes no Services yet, so the only replica pulls `latest` on web-2.
    cli(
        &direct,
        &[
            "server",
            "set",
            web1.id.as_str(),
            "--accepts-services=false",
        ],
    );
    wait_accepts_services(&mut client, &web1.id, false).await;
    let app = serde_json::from_value::<RequestedServiceSpec>(serde_json::json!({
        "name": "app",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": image, "command": ["sleep", "infinity"], "pull_policy": "always" }
    }))
    .unwrap();
    let app_id = deploy(&mut client, &app).await;
    cli(
        &direct,
        &["server", "set", web1.id.as_str(), "--accepts-services=true"],
    );
    wait_accepts_services(&mut client, &web1.id, true).await;
    let before = running(&mut client, &[app_id], 1).await;
    assert_eq!(on(&before, &web2.id), 1);
    let original = image_ids(&mut client, &before)
        .await
        .into_values()
        .next()
        .unwrap();

    // Retag `latest` to a different image, and leave web-1 holding that one.
    cluster
        .machine_shell(
            0,
            &format!(
                "docker create --name retag alpine:3.23.3 true && docker commit --change 'LABEL drain=v2' retag {image} && docker rm retag && docker push {image}"
            ),
        )
        .unwrap();
    let retagged = cluster
        .machine_shell(
            0,
            &format!("docker image inspect --format '{{{{.Id}}}}' {image}"),
        )
        .unwrap();
    assert_ne!(retagged.trim(), original);

    let drained = cli(&direct, &["server", "drain", web2.name.as_str()]);
    assert!(
        drained.contains(&format!(
            "app/app: moved 1 from {} to {}",
            web2.name, web1.name
        )),
        "{drained}"
    );
    let after = running(&mut client, &[app_id], 1).await;
    assert_eq!(on(&after, &web1.id), 1);
    assert_eq!(
        image_ids(&mut client, &after)
            .await
            .into_values()
            .next()
            .unwrap(),
        original
    );
}

async fn connect(direct: &str) -> ployz::connect::Client {
    ployz::connect::connect(
        std::path::Path::new("/missing-ployz-test-config"),
        Some(direct),
        None,
    )
    .await
    .unwrap()
}

fn cli(direct: &str, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--connect",
            direct,
            "--ployz-config",
            "/missing-ployz-test-config",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "ployz {} failed: stdout={} stderr={}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn docker(args: &[&str]) {
    let output = Command::new("docker").args(args).output().unwrap();
    assert!(
        output.status.success(),
        "docker {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn deploy(
    client: &mut ployz::connect::Client,
    requested: &RequestedServiceSpec,
) -> ServiceId {
    let machines = client
        .call::<op::ListMachines>(ListMachinesRequest {}, None)
        .await
        .unwrap()
        .machines;
    let live = client
        .live_services(ployz_core::EnvironmentValues::Included)
        .await
        .unwrap();
    let snapshot = ployz::deploy::DeploySnapshot {
        machines,
        containers: live
            .containers
            .successes
            .into_iter()
            .flat_map(|success| success.value)
            .collect(),
        ..Default::default()
    };
    let plan = plan_deploy(
        &ployz::deploy::DeployIntent::apply_all(
            Namespace::parse("app").unwrap(),
            [requested],
            ployz::deploy::PlanOptions {
                skip_health_monitor: true,
                ..Default::default()
            },
        ),
        &snapshot,
    )
    .unwrap();
    let outcome = client.confirm(&plan, &CancellationToken::new(), None).await;
    assert!(
        matches!(outcome, ployz::deploy::DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    plan.operations
        .iter()
        .find_map(|row| {
            if let ployz::deploy::DeployOperation::RunContainer { spec, .. } = &row.operation {
                Some(spec.service_id)
            } else {
                None
            }
        })
        .expect("the deploy runs a Container")
}

async fn create_and_start(
    client: &mut ployz::connect::Client,
    machine: &Machine,
    spec: ResolvedServiceSpec,
) {
    let created = client
        .create_container(
            machine.id,
            ContainerKind::ServiceContainer,
            Namespace::parse("app").unwrap(),
            spec,
            None,
        )
        .await
        .unwrap();
    client
        .call::<op::StartContainer>(
            StartContainerRequest {
                container_id: created.container_id,
            },
            Some(&MachineTarget::from(&machine.id)),
        )
        .await
        .unwrap();
}

/// Running ServiceContainers of these Services, by Container ID.
async fn running(
    client: &mut ployz::connect::Client,
    services: &[ServiceId],
    count: usize,
) -> BTreeMap<ContainerId, ployz_core::ContainerObservation> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let live = client
            .live_services(ployz_core::EnvironmentValues::Redacted)
            .await
            .unwrap();
        let running = live
            .services()
            .into_iter()
            .filter(|service| services.contains(&service.service_id))
            .flat_map(|service| service.containers)
            .map(|container| container.as_observation().clone())
            .filter(|container| {
                matches!(
                    container.runtime,
                    ployz_core::ContainerRuntimeObservation::Running { .. }
                )
            })
            .map(|container| (container.container_id, container))
            .collect::<BTreeMap<_, _>>();
        if running.len() == count {
            return running;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "expected {count} running Containers, saw {}",
            running.len()
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn on(
    containers: &BTreeMap<ContainerId, ployz_core::ContainerObservation>,
    machine: &MachineId,
) -> usize {
    containers
        .values()
        .filter(|container| container.machine_id == *machine)
        .count()
}

async fn hook_containers(
    client: &mut ployz::connect::Client,
    service: &ServiceId,
) -> BTreeSet<ContainerId> {
    client
        .live_services(ployz_core::EnvironmentValues::Redacted)
        .await
        .unwrap()
        .services()
        .into_iter()
        .filter(|observed| observed.service_id == *service)
        .flat_map(|observed| observed.hook_containers)
        .map(|container| container.as_observation().container_id)
        .collect()
}

async fn image_ids(
    client: &mut ployz::connect::Client,
    containers: &BTreeMap<ContainerId, ployz_core::ContainerObservation>,
) -> BTreeMap<ContainerId, String> {
    let mut images = BTreeMap::new();
    for (id, container) in containers {
        let details = client
            .call::<op::InspectContainer>(
                InspectContainerRequest { container_id: *id },
                Some(&MachineTarget::from(&container.machine_id)),
            )
            .await
            .unwrap();
        images.insert(*id, details.image_id.expect("the daemon reports image IDs"));
    }
    images
}

/// web-1's Ingress Proxy routes to every one of these Containers.
async fn wait_routed(
    client: &mut ployz::connect::Client,
    machine: &Machine,
    containers: &BTreeMap<ContainerId, ployz_core::ContainerObservation>,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        if let Ok(config) = client
            .call::<op::GetIngressProxyConfig>(
                GetIngressProxyConfigRequest {},
                Some(&MachineTarget::from(&machine.id)),
            )
            .await
            && containers.values().all(|container| {
                container
                    .address
                    .is_some_and(|address| config.config().contains(&format!("{}:8080", address.0)))
            })
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "web-1's Ingress Proxy never routed to every Container"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_accepts_services(client: &mut ployz::connect::Client, id: &MachineId, accepts: bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let machines = client
            .call::<op::ListMachines>(ListMachinesRequest {}, None)
            .await
            .unwrap()
            .machines;
        if machines
            .iter()
            .any(|machine| machine.machine.id == *id && machine.machine.accepts_services == accepts)
        {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
