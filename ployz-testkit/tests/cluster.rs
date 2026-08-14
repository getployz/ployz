use std::{collections::BTreeSet, process, time::Duration};

use ployz_core::{
    MachineName, MachineObservation, MachineUpdate, MembershipObservation, PublicIpUpdate,
    WireGuardPublicKey,
};
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
                .map(|machine| machine.machine.id.clone())
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
    cluster.unblock_gossip(0).unwrap();
    cluster.unblock_gossip(1).unwrap();
    for entry in 0..2 {
        wait_for(&cluster, entry, Duration::from_secs(60), |machines| {
            machines
                .iter()
                .filter(|machine| machine.machine.name.as_str() == "partition-duplicate")
                .count()
                == 2
        })
        .await;
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
        assert!(wireguard_peer.allowed_ips.contains(&peer.subnet.0.into()));
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

    let target_container = format!("{:0<64}", "target");
    let other_container = format!("{:0<64}", "other");
    cluster
        .seed_container_row(0, &target_container, &second.id)
        .unwrap();
    cluster
        .seed_container_row(0, &other_container, &first.id)
        .unwrap();
    cluster.remove_machine(0, second.id.clone()).await.unwrap();
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

async fn wait_for(
    cluster: &Cluster,
    entry: usize,
    timeout: Duration,
    condition: impl Fn(&[MachineObservation]) -> bool,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if cluster
            .machines(entry)
            .await
            .is_ok_and(|machines| condition(&machines))
        {
            return;
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
