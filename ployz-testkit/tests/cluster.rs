use std::{collections::BTreeSet, process, time::Duration};

use ployz_core::{MembershipObservation, UNREGISTRY_PORT, WireGuardPublicKey};
use ployz_testkit::{Cluster, ClusterPlan, join_request};

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn initializes_joins_converges_restarts_and_tears_down() {
    let plan = ClusterPlan::new(&format!("l3-001-{}", process::id()), 2).unwrap();
    let cluster = Cluster::create(plan.clone()).unwrap();
    let expected = cluster
        .initialize_two()
        .await
        .unwrap()
        .map(|machine| machine.id)
        .into_iter()
        .collect::<BTreeSet<_>>();

    for entry in 0..2 {
        let observations = cluster.machines(entry).await.unwrap();
        assert_eq!(
            observations
                .iter()
                .map(|observation| observation.machine.id.clone())
                .collect::<BTreeSet<_>>(),
            expected
        );
        assert!(
            observations
                .iter()
                .all(|machine| machine.membership == MembershipObservation::Up)
        );
    }

    cluster.restart(1).unwrap();
    cluster.wait_ready(Duration::from_secs(60)).await.unwrap();
    assert_eq!(
        cluster
            .machines(1)
            .await
            .unwrap()
            .into_iter()
            .map(|observation| observation.machine.id)
            .collect::<BTreeSet<_>>(),
        expected
    );
    cluster.teardown().unwrap();
    cluster.teardown().unwrap();
}

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn failed_join_leaves_the_registered_ghost_until_teardown() {
    let plan = ClusterPlan::new(&format!("l3-ghost-{}", process::id()), 2).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    cluster.wait_ready(Duration::from_secs(30)).await.unwrap();
    let first = cluster.initialize_first().await.unwrap();
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if cluster
                .machines(0)
                .await
                .is_ok_and(|machines| machines.len() == 1)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .unwrap();
    let mut registered = cluster.register_second().await.unwrap();
    let ghost = registered.assigned_machine.id.clone();
    registered.assigned_machine.public_key = WireGuardPublicKey([0; 32]);
    assert!(
        cluster
            .join(1, join_request(&first, &registered))
            .await
            .is_err()
    );

    let ids = cluster
        .machines(0)
        .await
        .unwrap()
        .into_iter()
        .map(|observation| observation.machine.id)
        .collect::<BTreeSet<_>>();
    assert!(ids.contains(&first.id));
    assert!(ids.contains(&ghost));
}

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn l3_056_image_list_preserves_machine_local_placement_and_filtering() {
    let plan = ClusterPlan::new(&format!("l3-056-{}", process::id()), 3).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    cluster.initialize_three().await.unwrap();
    let image = format!("l3.invalid/unique-{}:v1", process::id());
    let service_id = format!("{:032x}", process::id());
    for index in 0..2 {
        cluster.docker(index, &["pull", "alpine:3.23.3"]).unwrap();
        cluster
            .docker(index, &["tag", "alpine:3.23.3", &image])
            .unwrap();
        cluster
            .docker(
                index,
                &[
                    "run",
                    "--detach",
                    "--name",
                    &format!("l3-image-replica-{index}"),
                    "--label",
                    "ployz.managed",
                    "--label",
                    &format!("ployz.service.id={service_id}"),
                    "--label",
                    "ployz.service.name=l3-image",
                    &image,
                    "sleep",
                    "120",
                ],
            )
            .unwrap();
    }

    let all = cluster
        .shell(0, "ployz --connect root@127.0.0.1 image ls")
        .unwrap();
    let all_stderr = String::from_utf8_lossy(&all.stderr);
    let all = String::from_utf8_lossy(&all.stdout);
    for machine in ["machine-1", "machine-2", "machine-3"] {
        assert!(
            all.contains(machine),
            "missing {machine} from:\n{all}\n{all_stderr}"
        );
    }
    assert_eq!(all.lines().filter(|line| line.contains(&image)).count(), 2);
    let filtered = cluster
        .shell(
            0,
            &format!("ployz --connect root@127.0.0.1 image ls {image}"),
        )
        .unwrap();
    let filtered = String::from_utf8_lossy(&filtered.stdout);
    assert_eq!(
        filtered
            .lines()
            .filter(|line| line.contains(&image))
            .count(),
        2
    );
    assert!(filtered.contains("machine-1"));
    assert!(filtered.contains("machine-2"));
    assert!(!filtered.contains("machine-3"));
}

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn pinned_unregistry_starts_on_the_gateway_and_remains_container_reachable() {
    let plan = ClusterPlan::new(&format!("l3-unregistry-{}", process::id()), 2).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    let machines = cluster.initialize_two().await.unwrap();
    let gateway =
        std::net::Ipv4Addr::from(u32::from(machines.first().unwrap().subnet.0.network()) + 1);
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let ready = cluster
                .shell(
                    0,
                    &format!(
                        "docker inspect ployz-unregistry >/dev/null 2>&1 && curl --fail --silent http://{gateway}:{UNREGISTRY_PORT}/v2/ >/dev/null"
                    ),
                )
                .is_ok();
            if ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .unwrap();
    cluster.docker(0, &["pull", "alpine:3.23.3"]).unwrap();
    cluster
        .docker(
            0,
            &[
                "run",
                "--rm",
                "--network",
                "ployz",
                "alpine:3.23.3",
                "wget",
                "-qO-",
                &format!("http://{gateway}:{UNREGISTRY_PORT}/v2/"),
            ],
        )
        .unwrap();
    let pushed = format!("l3.invalid/container-push-{}:v1", process::id());
    cluster
        .docker(
            0,
            &[
                "run",
                "--rm",
                "--network",
                "ployz",
                "--volume",
                "/var/run/docker.sock:/var/run/docker.sock",
                "quay.io/skopeo/stable:v1.20.0",
                "copy",
                "--dest-tls-verify=false",
                "docker-daemon:alpine:3.23.3",
                &format!("docker://{gateway}:{UNREGISTRY_PORT}/{pushed}"),
            ],
        )
        .unwrap();
    assert!(
        cluster
            .images(0, Some(pushed.clone()))
            .await
            .unwrap()
            .images
            .iter()
            .any(|image| image.repo_tags.contains(&pushed))
    );
}

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn multi_platform_direct_push_retains_success_beside_target_failure() {
    let plan = ClusterPlan::new(&format!("l3-image-push-{}", process::id()), 2).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    let machines = cluster.initialize_two().await.unwrap();
    let image = format!("l3.invalid/multi-{}:v1", process::id());
    cluster
        .docker(0, &["pull", "--platform", "linux/amd64", "busybox:1.37.0"])
        .unwrap();
    cluster
        .docker(0, &["pull", "--platform", "linux/arm64", "busybox:1.37.0"])
        .unwrap();
    cluster
        .docker(0, &["tag", "busybox:1.37.0", &image])
        .unwrap();
    cluster.docker(0, &["stop", "ployz-unregistry"]).unwrap();

    let pushed = cluster.shell(
        0,
        &format!("ployz --connect root@127.0.0.1 image push --platform linux/arm64 {image}"),
    );
    let failure = pushed.expect_err("one unavailable target must fail the command");
    assert!(
        failure
            .to_string()
            .contains(machines.first().unwrap().id.as_str())
    );
    let retained = cluster.images(1, Some(image.clone())).await.unwrap();
    let image = retained
        .images
        .iter()
        .find(|entry| entry.repo_tags.contains(&image))
        .expect("successful target retained the original image reference");
    assert!(
        image
            .platforms
            .iter()
            .any(|platform| platform.starts_with("linux/arm64")),
        "retained platforms: {:?}",
        image.platforms
    );
    assert_ne!(machines.first().unwrap().id, machines.get(1).unwrap().id);
}

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn daemon_stays_ready_when_unregistry_prerequisites_are_missing() {
    let mut disabled = ClusterPlan::new(&format!("l3-unreg-store-{}", process::id()), 2).unwrap();
    for machine in &mut disabled.machines {
        machine
            .environment
            .insert("PLOYZ_TESTKIT_CONTAINERD_STORE".into(), "0".into());
    }
    let disabled = Cluster::create(disabled).unwrap();
    let disabled_machines = disabled.initialize_two().await.unwrap();
    let disabled_gateway =
        std::net::Ipv4Addr::from(u32::from(disabled_machines[0].subnet.0.network()) + 1);
    assert!(!disabled.images(0, None).await.unwrap().containerd_store);
    assert!(
        disabled
            .shell(0, "docker inspect ployz-unregistry")
            .is_err()
    );
    assert!(
        disabled
            .shell(
                0,
                &format!(
                    "curl --fail --silent --connect-timeout 1 http://{disabled_gateway}:{UNREGISTRY_PORT}/v2/"
                ),
            )
            .is_err()
    );
    assert!(
        disabled
            .logs(0)
            .unwrap()
            .contains("unregistry disabled: Docker is not using the containerd image store")
    );
    drop(disabled);

    let mut missing = ClusterPlan::new(&format!("l3-unreg-socket-{}", process::id()), 2).unwrap();
    for machine in &mut missing.machines {
        machine.daemon_args = vec![
            "--containerd-socket".into(),
            "/missing/containerd.sock".into(),
        ];
    }
    let missing = Cluster::create(missing).unwrap();
    let missing_machines = missing.initialize_two().await.unwrap();
    let missing_gateway =
        std::net::Ipv4Addr::from(u32::from(missing_machines[0].subnet.0.network()) + 1);
    assert!(missing.images(0, None).await.unwrap().containerd_store);
    assert!(missing.shell(0, "docker inspect ployz-unregistry").is_err());
    assert!(
        missing
            .shell(
                0,
                &format!(
                    "curl --fail --silent --connect-timeout 1 http://{missing_gateway}:{UNREGISTRY_PORT}/v2/"
                ),
            )
            .is_err()
    );
    assert!(
        missing
            .logs(0)
            .unwrap()
            .contains("unregistry disabled: no containerd socket was detected")
    );
}
