use std::{collections::BTreeSet, process, time::Duration};

use ployz_core::{MembershipObservation, WireGuardPublicKey};
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
