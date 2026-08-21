use std::{collections::BTreeSet, fmt::Display, process, time::Duration};

use ployz_core::{
    CloudPairing, LocalMachinePhase, Machine, MachineId, MachineName, MachineObservation,
    MachineUpdate, MembershipObservation, PairingCredential, PublicIpUpdate, UNREGISTRY_PORT,
    WireGuardPublicKey,
};
use ployz_testkit::{Cluster, ClusterPlan, join_request};

fn image_ingest_catalog(cluster: &Cluster, index: usize, gateway: impl Display) -> bool {
    cluster
        .shell(
            index,
            &format!(
                "curl --fail --silent --connect-timeout 1 http://{gateway}:{UNREGISTRY_PORT}/v2/"
            ),
        )
        .is_ok()
}

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn initializes_joins_converges_restarts_and_tears_down() {
    let plan = ClusterPlan::new(&format!("l3-001-{}", process::id()), 2).unwrap();
    let cluster = Cluster::create(plan.clone()).unwrap();
    let expected = cluster
        .initialize_two_with_join_barrier()
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
                .map(|observation| observation.machine.id)
                .collect::<BTreeSet<_>>(),
            expected
        );
        assert!(
            observations
                .iter()
                .all(|machine| machine.membership == MembershipObservation::Up)
        );
    }
    let metrics = cluster
        .machine_shell(0, "curl -fsS http://127.0.0.1:51090/metrics")
        .unwrap();
    assert!(metrics.contains(&format!(
        "ployz_ployzd_build_info{{version=\"{}\"}} 1",
        env!("CARGO_PKG_VERSION")
    )));

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
async fn machine_add_and_cloud_join_reach_participating() {
    let cloud_pairing = CloudPairing::parse(
        "ws://127.0.0.1:9",
        PairingCredential::parse("unreachable-test-relay").unwrap(),
    )
    .unwrap();
    for run in 1..=2 {
        for (path, cloud_pairing) in [
            ("machine-add", None),
            ("cloud-enroll", Some(cloud_pairing.clone())),
        ] {
            let plan = ClusterPlan::new(
                &format!("l3-join-startup-{path}-{run}-{}", process::id()),
                2,
            )
            .unwrap();
            let cluster = Cluster::create(plan).unwrap();
            cluster.wait_ready(Duration::from_secs(60)).await.unwrap();
            let first = cluster.initialize_first().await.unwrap();
            wait_for(&cluster, 0, Duration::from_secs(60), |machines| {
                machines
                    .iter()
                    .any(|machine| machine.machine.id == first.id)
            })
            .await;

            let registration = cluster.register_second().await.unwrap();
            let mut request = join_request(&first, &registration);
            request.cloud_pairing = cloud_pairing;
            cluster.join(1, request).await.unwrap();

            tokio::time::timeout(Duration::from_secs(60), async {
                loop {
                    if tokio::time::timeout(Duration::from_secs(1), cluster.inspect(1))
                        .await
                        .is_ok_and(|details| {
                            details.is_ok_and(|details| {
                                details.phase == LocalMachinePhase::Participating
                            })
                        })
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("{path} daemon did not become participating after join"));
        }
    }
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
    let ghost = registered.assigned_machine.id;
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
async fn adds_a_third_machine_without_coordination_or_rollback() {
    let plan = ClusterPlan::new(&format!("l3-add-{}", process::id()), 3).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    cluster.wait_ready(Duration::from_secs(30)).await.unwrap();
    let first = cluster.initialize_first().await.unwrap();
    wait_for(&cluster, 0, Duration::from_secs(60), |machines| {
        machines
            .iter()
            .any(|machine| machine.machine.id == first.id)
    })
    .await;
    let second = cluster.add_machine(0, 1, "machine-2").await.unwrap();
    wait_for(&cluster, 0, Duration::from_secs(60), |machines| {
        machines.len() == 2
    })
    .await;
    let third = cluster.add_machine(0, 2, "machine-3").await.unwrap();
    let expected = [first.id, second.id, third.id]
        .into_iter()
        .collect::<BTreeSet<_>>();
    for entry in 0..3 {
        wait_for(&cluster, entry, Duration::from_secs(60), |machines| {
            machines
                .iter()
                .map(|machine| machine.machine.id)
                .collect::<BTreeSet<_>>()
                == expected
        })
        .await;
    }
}

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn updates_removes_and_inspects_machine_network_state() {
    let plan = ClusterPlan::new(&format!("l3-admin-{}", process::id()), 2).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    let [first, second] = cluster.initialize_two().await.unwrap();
    let original = second.clone();

    let renamed = update_eventually(
        &cluster,
        second.name.as_str(),
        MachineUpdate {
            name: Some("renamed-by-name".parse().unwrap()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(renamed.id, second.id);
    assert!(
        cluster
            .update_machine(
                0,
                "missing-machine",
                MachineUpdate {
                    name: Some("unused".parse().unwrap()),
                    ..Default::default()
                },
            )
            .await
            .is_err()
    );
    assert!(
        cluster
            .update_machine(
                0,
                second.id.as_str(),
                MachineUpdate {
                    name: Some(first.name.clone()),
                    ..Default::default()
                },
            )
            .await
            .is_err()
    );

    let endpoint = cluster.endpoint(1).unwrap();
    let updated = cluster
        .update_machine(
            0,
            second.id.as_str(),
            MachineUpdate {
                name: Some("renamed-by-id".parse().unwrap()),
                public_ip: PublicIpUpdate::Set("203.0.113.9".parse().unwrap()),
                advertised_endpoints: Some(vec![endpoint]),
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.id, original.id);
    assert_eq!(updated.subnet, original.subnet);
    assert_eq!(updated.management_address, original.management_address);
    assert_eq!(updated.public_key, original.public_key);
    wait_for(&cluster, 0, Duration::from_secs(60), |machines| {
        machines.iter().any(|machine| {
            machine.machine.id == second.id
                && machine.machine.name.as_str() == "renamed-by-id"
                && machine.machine.public_ip == Some("203.0.113.9".parse().unwrap())
        })
    })
    .await;
    let removed_ip = cluster
        .update_machine(
            0,
            second.id.as_str(),
            MachineUpdate {
                public_ip: PublicIpUpdate::Remove,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(removed_ip.public_ip, None);

    cluster.block_gossip(0).unwrap();
    cluster.block_gossip(1).unwrap();
    let duplicate_name = MachineName::parse("partition-duplicate").unwrap();
    cluster
        .update_machine(
            0,
            first.id.as_str(),
            MachineUpdate {
                name: Some(duplicate_name.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    cluster
        .update_machine(
            1,
            second.id.as_str(),
            MachineUpdate {
                name: Some(duplicate_name),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    for (entry, local_id) in [(0, &first.id), (1, &second.id)] {
        let visible = wait_for(&cluster, entry, Duration::from_secs(10), |machines| {
            machines.iter().any(|machine| {
                &machine.machine.id == local_id
                    && machine.machine.name.as_str() == "partition-duplicate"
            })
        })
        .await;
        assert_eq!(
            visible
                .iter()
                .filter(|machine| machine.machine.name.as_str() == "partition-duplicate")
                .count(),
            1
        );
    }
    cluster.unblock_gossip(0).unwrap();
    cluster.unblock_gossip(1).unwrap();
    for entry in 0..2 {
        let converged = wait_for(&cluster, entry, Duration::from_secs(60), |machines| {
            machines
                .iter()
                .filter(|machine| machine.machine.name.as_str() == "partition-duplicate")
                .count()
                == 2
        })
        .await;
        assert!(
            converged
                .iter()
                .any(|machine| machine.machine.id == first.id)
        );
        assert!(
            converged
                .iter()
                .any(|machine| machine.machine.id == second.id)
        );
    }

    for (index, local, peer) in [(0, &first, &second), (1, &second, &first)] {
        let device = cluster.inspect_wireguard(index).await.unwrap();
        assert_eq!(device.public_key, local.public_key);
        assert_eq!(device.listen_port, 51820);
        let wireguard_peer = device
            .peers
            .iter()
            .find(|wireguard_peer| wireguard_peer.public_key == peer.public_key)
            .unwrap();
        assert!(
            wireguard_peer
                .allowed_ips
                .iter()
                .any(|address| address.to_string() == format!("{}/128", peer.management_address.0))
        );
        assert!(wireguard_peer.allowed_ips.contains(&peer.subnet.into()));
    }
    let unknown = WireGuardPublicKey([9; 32]);
    cluster.inject_wireguard_peer(0, unknown).unwrap();
    assert!(
        cluster
            .inspect_wireguard(0)
            .await
            .unwrap()
            .peers
            .iter()
            .any(|peer| peer.public_key == unknown && peer.machine.is_none())
    );

    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let first_rtts = cluster.inspect_rtts(0).await.unwrap_or_default();
        let second_rtts = cluster.inspect_rtts(1).await.unwrap_or_default();
        if first_rtts.iter().any(|observation| {
            observation
                .machine
                .as_ref()
                .is_some_and(|peer| peer.id == second.id)
        }) && second_rtts.iter().any(|observation| {
            observation
                .machine
                .as_ref()
                .is_some_and(|peer| peer.id == first.id)
        }) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "RTT samples did not appear"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    assert_partitioned_field_collisions_survive_convergence(&cluster, &first).await;

    let target_container = format!("{:0<64}", "target");
    let other_container = format!("{:0<64}", "other");
    cluster
        .seed_container_row(0, &target_container, &second.id)
        .unwrap();
    cluster
        .seed_container_row(0, &other_container, &first.id)
        .unwrap();
    cluster.remove_machine(0, second.id).await.unwrap();
    assert!(
        !cluster
            .replicated_row_exists(0, "machines", second.id.as_str())
            .unwrap()
    );
    assert!(
        !cluster
            .replicated_row_exists(0, "containers", &target_container)
            .unwrap()
    );
    assert!(
        cluster
            .replicated_row_exists(0, "containers", &other_container)
            .unwrap()
    );
    assert_eq!(
        cluster.inspect(1).await.unwrap().phase,
        ployz_core::LocalMachinePhase::Participating
    );
    wait_for(&cluster, 1, Duration::from_secs(75), |machines| {
        machines
            .iter()
            .any(|machine| machine.machine.id == second.id)
    })
    .await;
}

async fn assert_partitioned_field_collisions_survive_convergence(
    cluster: &Cluster,
    first: &Machine,
) {
    cluster.block_gossip(0).unwrap();
    cluster.block_gossip(1).unwrap();
    let mut conflicting = first.clone();
    conflicting.id = MachineId::random();
    conflicting.name = MachineName::parse("partition-field-conflict").unwrap();
    cluster
        .seed_machine_collision(0, first, &conflicting.id, &conflicting.name)
        .unwrap();
    wait_for(cluster, 0, Duration::from_secs(10), |machines| {
        machines
            .iter()
            .any(|machine| machine.machine.id == conflicting.id)
    })
    .await;
    assert!(
        cluster
            .machines(1)
            .await
            .unwrap()
            .iter()
            .all(|machine| machine.machine.id != conflicting.id)
    );
    cluster.unblock_gossip(0).unwrap();
    cluster.unblock_gossip(1).unwrap();
    for entry in 0..2 {
        let converged = wait_for(cluster, entry, Duration::from_secs(60), |machines| {
            machines
                .iter()
                .filter(|machine| machine.machine.subnet == first.subnet)
                .count()
                == 2
                && machines
                    .iter()
                    .filter(|machine| {
                        machine.machine.management_address == first.management_address
                    })
                    .count()
                    == 2
                && machines
                    .iter()
                    .filter(|machine| machine.machine.public_key == first.public_key)
                    .count()
                    == 2
        })
        .await;
        assert!(
            converged
                .iter()
                .any(|machine| machine.machine.id == first.id)
        );
        assert!(
            converged
                .iter()
                .any(|machine| machine.machine.id == conflicting.id)
        );
    }
}

async fn wait_for(
    cluster: &Cluster,
    entry: usize,
    timeout: Duration,
    condition: impl Fn(&[MachineObservation]) -> bool,
) -> Vec<MachineObservation> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(machines) = cluster.machines(entry).await
            && condition(&machines)
        {
            return machines;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "entry-relative replicated observations did not converge"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn update_eventually(
    cluster: &Cluster,
    target: &str,
    update: MachineUpdate,
) -> ployz_core::Machine {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(machine) = cluster.update_machine(0, target, update.clone()).await {
            return machine;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "routed Machine update did not become reachable"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
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
                    &format!("l3-image-container-{index}"),
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
async fn image_ingest_opens_on_push_and_stays_reachable_from_containers() {
    let mut plan = ClusterPlan::new(&format!("l3-ingest-{}", process::id()), 2).unwrap();
    for machine in &mut plan.machines {
        machine
            .daemon_args
            .extend(["--data-dir".into(), "/var/lib/ployz/data".into()]);
    }
    let cluster = Cluster::create(plan).unwrap();
    for index in 0..2 {
        cluster
            .shell(index, "ln -s data/corrosion /var/lib/ployz/corrosion")
            .unwrap();
    }
    let machines = cluster.initialize_two().await.unwrap();
    let gateway = machines.first().unwrap().subnet.gateway().0;
    assert!(
        !image_ingest_catalog(&cluster, 0, gateway),
        "ingest must not listen until image ingest is opened"
    );
    cluster.docker(0, &["pull", "alpine:3.23.3"]).unwrap();
    cluster
        .docker(0, &["pull", "--platform", "linux/amd64", "busybox:1.37.0"])
        .unwrap();
    cluster
        .shell(
            0,
            "ployz image push --machine machine-1 --platform linux/amd64 busybox:1.37.0",
        )
        .unwrap();
    assert!(
        image_ingest_catalog(&cluster, 0, gateway),
        "push opens ingest on the Machine Gateway"
    );
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
                "/opt/ployz/images/alpine.tar:/alpine.tar:ro",
                "quay.io/skopeo/stable:v1.20.0",
                "copy",
                "--dest-tls-verify=false",
                "docker-archive:/alpine.tar",
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
    cluster
        .shell(0, "ln -s /bin/true /usr/local/bin/resolvconf")
        .unwrap();
    cluster.reset(0).await.unwrap();
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if cluster
                .inspect(0)
                .await
                .is_ok_and(|details| details.phase == LocalMachinePhase::Uninitialized)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("reset did not complete:\n{}", cluster.logs(0).unwrap()));
    assert!(
        !image_ingest_catalog(&cluster, 0, gateway),
        "reset must stop ingest on the Machine Gateway"
    );
}

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn image_push_pulls_from_the_source_machine_without_opening_dest_ingest() {
    let mut plan = ClusterPlan::new(&format!("l3-peer-pull-{}", process::id()), 2).unwrap();
    for machine in &mut plan.machines {
        machine
            .daemon_args
            .extend(["--data-dir".into(), "/var/lib/ployz/data".into()]);
    }
    let cluster = Cluster::create(plan).unwrap();
    for index in 0..2 {
        cluster
            .shell(index, "ln -s data/corrosion /var/lib/ployz/corrosion")
            .unwrap();
    }
    let machines = cluster.initialize_two().await.unwrap();
    let gateways = [
        machines[0].subnet.gateway().0,
        machines[1].subnet.gateway().0,
    ];
    for (index, gateway) in gateways.into_iter().enumerate() {
        assert!(
            !image_ingest_catalog(&cluster, index, gateway),
            "ingest must not listen until a Machine is the push source"
        );
    }
    cluster
        .docker(0, &["pull", "--platform", "linux/amd64", "busybox:1.37.0"])
        .unwrap();
    cluster
        .shell(0, "ployz image push --platform linux/amd64 busybox:1.37.0")
        .unwrap();
    let ingest = [
        image_ingest_catalog(&cluster, 0, gateways[0]),
        image_ingest_catalog(&cluster, 1, gateways[1]),
    ];
    assert_ne!(
        ingest[0], ingest[1],
        "only the push source opens ingest; the destination pulls"
    );
    for index in 0..2 {
        assert!(
            cluster
                .images(index, Some("busybox:1.37.0".into()))
                .await
                .unwrap()
                .images
                .iter()
                .any(|image| image
                    .repo_tags
                    .iter()
                    .any(|tag| tag.contains("busybox:1.37.0"))),
            "machine {index} must have the image after push-once peer pull"
        );
    }
}

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn multi_platform_direct_push_retains_success_beside_target_failure() {
    let mut plan = ClusterPlan::new(&format!("l3-image-push-{}", process::id()), 2).unwrap();
    plan.machines
        .first_mut()
        .expect("cluster plan has a first Machine")
        .daemon_args = vec![
        "--containerd-socket".into(),
        "/missing/containerd.sock".into(),
    ];
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

    let pushed = cluster.shell(
        0,
        &format!("ployz --connect root@127.0.0.1 image push --platform linux/arm64 {image}"),
    );
    let failure = pushed.expect_err("one unavailable target must fail the command");
    assert!(
        failure
            .to_string()
            .contains(machines.first().unwrap().id.as_str()),
        "unavailable source must be named: {failure}"
    );
    let retained = cluster.images(1, Some(image.clone())).await.unwrap();
    let image = retained
        .images
        .iter()
        .find(|entry| entry.repo_tags.contains(&image))
        .unwrap_or_else(|| {
            panic!(
                "successful target retained the original image reference\npush error: {failure}\nimages: {retained:?}"
            )
        });
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
async fn daemon_stays_ready_when_image_ingest_cannot_open() {
    let mut disabled = ClusterPlan::new(&format!("l3-ingest-store-{}", process::id()), 2).unwrap();
    for machine in &mut disabled.machines {
        machine
            .environment
            .insert("PLOYZ_TESTKIT_CONTAINERD_STORE".into(), "0".into());
    }
    let disabled = Cluster::create(disabled).unwrap();
    let disabled_machines = disabled.initialize_two().await.unwrap();
    let disabled_gateway = disabled_machines
        .first()
        .expect("initialized two machines")
        .subnet
        .gateway()
        .0;
    assert!(!disabled.images(0, None).await.unwrap().containerd_store);
    assert!(!image_ingest_catalog(&disabled, 0, disabled_gateway));
    disabled
        .docker(0, &["pull", "--platform", "linux/amd64", "busybox:1.37.0"])
        .unwrap();
    let push = disabled.shell(0, "ployz image push --machine machine-1 busybox:1.37.0");
    let failure = push.expect_err("unsupported store must fail image ingest");
    assert!(
        failure.to_string().contains("containerd image store"),
        "{failure}"
    );
    drop(disabled);

    let mut missing = ClusterPlan::new(&format!("l3-ingest-socket-{}", process::id()), 2).unwrap();
    for machine in &mut missing.machines {
        machine.daemon_args = vec![
            "--containerd-socket".into(),
            "/missing/containerd.sock".into(),
        ];
    }
    let missing = Cluster::create(missing).unwrap();
    let missing_machines = missing.initialize_two().await.unwrap();
    let missing_gateway = missing_machines
        .first()
        .expect("initialized two machines")
        .subnet
        .gateway()
        .0;
    assert!(missing.images(0, None).await.unwrap().containerd_store);
    assert!(!image_ingest_catalog(&missing, 0, missing_gateway));
    missing
        .docker(0, &["pull", "--platform", "linux/amd64", "busybox:1.37.0"])
        .unwrap();
    let push = missing.shell(0, "ployz image push --machine machine-1 busybox:1.37.0");
    let failure = push.expect_err("missing containerd socket must fail image ingest");
    assert!(
        failure.to_string().contains("containerd socket"),
        "{failure}"
    );
}
