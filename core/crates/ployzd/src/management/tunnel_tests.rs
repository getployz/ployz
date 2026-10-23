use ployz_core::{
    AdvertisedEndpoint, EnrollAs, EnrollRequest, Machine, MachineId, MachineName, Registered,
};

use super::cluster_network;
use crate::{
    corrosion::{AdminClient, ReplicatedStore, fake_cluster},
    machine::{LocalMachine, LocalMachineStore, RecordOwner},
};

/// A Machine that joined a Cluster and caught up, so its origin is `Join`.
fn joined(dir: &std::path::Path) -> LocalMachineStore {
    let mut store = LocalMachineStore::open(dir).unwrap();
    let record = store.record().clone();
    let assigned_machine = Machine {
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        id: record.id(),
        name: MachineName::parse("joined").unwrap(),
        subnet: "10.210.1.0/24".parse().unwrap(),
        public_key: record.private_key().public_key(),
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.2:51820".parse().unwrap())],
        runtime: Default::default(),
        build_concurrency: None,
    };
    let mut founder = assigned_machine.clone();
    founder.id = MachineId::random();
    store
        .enroll(EnrollRequest {
            role: EnrollAs::Member {
                // From JSON, so it also builds where `Registered` has fewer fields.
                registration: serde_json::from_value::<Registered>(serde_json::json!({
                    "assigned_machine": assigned_machine,
                    "visible_peers": [founder],
                    "target_versions": {},
                }))
                .unwrap(),
            },
            wireguard_mtu: None,
            pinned: Default::default(),
        })
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
