use std::sync::Arc;

use ployz_core::{
    AdvertisedEndpoint, Machine, MachineId, MachineName, MachineRuntime, WireGuardPublicKey,
};

use super::alone;
use crate::corrosion::{MachineView, ReplicatedObservations};

fn machine(name: &str, octet: u8) -> Machine {
    Machine {
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        id: MachineId::random(),
        name: MachineName::parse(name).unwrap(),
        subnet: format!("10.210.{octet}.0/24").parse().unwrap(),
        public_key: WireGuardPublicKey([octet; 32]),
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint(
            format!("192.0.2.{octet}:51820").parse().unwrap(),
        )],
        runtime: MachineRuntime::default(),
        build_concurrency: None,
    }
}

fn view(machines: &[&Machine], incomplete_ids: Vec<MachineId>) -> MachineView {
    MachineView::fixed(Some(Arc::new(ReplicatedObservations {
        observations: machines.iter().map(|machine| (*machine).clone()).collect(),
        incomplete_ids,
    })))
}

#[tokio::test]
async fn only_member_is_alone() {
    let local = machine("local", 1);

    assert!(alone(&view(&[&local], Vec::new()), local.id).await);
}

#[tokio::test]
async fn a_known_peer_is_not_alone() {
    let (local, peer) = (machine("local", 1), machine("peer", 2));

    assert!(!alone(&view(&[&local, &peer], Vec::new()), local.id).await);
}

#[tokio::test]
async fn a_partly_replicated_machine_is_not_alone() {
    let (local, peer) = (machine("local", 1), machine("peer", 2));

    assert!(!alone(&view(&[&local], vec![peer.id]), local.id).await);
}

#[tokio::test]
async fn an_unread_machine_list_is_not_alone() {
    let local = machine("local", 1);

    assert!(!alone(&MachineView::fixed(None), local.id).await);
}
