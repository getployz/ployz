use super::cluster_network;
use crate::{
    corrosion::{AdminClient, ReplicatedStore, fake_cluster},
    machine::{LocalMachine, LocalMachineStore, RecordOwner, test_support::ApprovingCluster},
};

/// A Machine that joined a Cluster and caught up, so its origin is `Join`.
fn joined(dir: &std::path::Path) -> LocalMachineStore {
    let mut store = LocalMachineStore::open(dir).unwrap();
    store
        .enroll(ApprovingCluster::new(1).join(store.record()))
        .unwrap();
    store.complete_catch_up().unwrap();
    store
}

fn over(store: LocalMachineStore, replicated: ReplicatedStore) -> LocalMachine {
    LocalMachine::new(RecordOwner::spawn(store).unwrap())
        .with_cluster(Some((replicated, AdminClient::new("/no/such/admin.sock"))))
}

#[tokio::test]
async fn a_joined_machine_tunnels_into_the_cluster_network_it_replicated() {
    let dir = tempfile::tempdir().unwrap();
    let (replicated, _cluster) = fake_cluster::store().await;
    let local = over(joined(dir.path()), replicated);
    assert_eq!(
        cluster_network(&local.record(), &local).await,
        Some("10.210.0.0/16".parse().unwrap())
    );
}

#[tokio::test]
async fn a_joined_machine_before_the_network_replicates_opens_no_tunnel() {
    let dir = tempfile::tempdir().unwrap();
    let (replicated, _cluster) = fake_cluster::store_without_network().await;
    let local = over(joined(dir.path()), replicated);
    assert_eq!(cluster_network(&local.record(), &local).await, None);
}
