//! Initial policy is part of the first registration and durable Join assignment.

use super::*;

#[tokio::test]
async fn registration_publishes_initial_policy_and_join_persists_it() {
    let (entry, replicated, _founder, data_dir, server) = participating().await;
    let joiner_dir =
        std::env::temp_dir().join(format!("ployzd-policy-join-{}", MachineId::random()));
    let joiner_store = LocalMachineStore::open(&joiner_dir).unwrap();
    let public_key = joiner_store.record().private_key().public_key();
    let mut identity = request("builder", public_key);
    identity.machine_id = joiner_store.record().id();
    let mut wire = serde_json::to_value(identity).unwrap();
    wire.as_object_mut().unwrap().insert(
        "initial_policy".into(),
        serde_json::json!({
            "labels": {"pool": "build"},
            "accepts_builds": true,
            "accepts_services": false,
            "accepts_ingress": false
        }),
    );
    let registration = entry
        .register(serde_json::from_value(wire).unwrap())
        .await
        .unwrap();
    let assigned = &registration.assigned_machine;
    assert!(
        !assigned.accepts_services,
        "first assignment must refuse Services"
    );
    assert!(
        !assigned.accepts_ingress,
        "first assignment must refuse Ingress"
    );
    assert!(assigned.accepts_builds);
    assert_eq!(assigned.labels.get("pool").unwrap().as_str(), "build");
    assert_eq!(
        replicated
            .machine(assigned.id.as_str())
            .await
            .unwrap()
            .as_ref(),
        Some(assigned)
    );
    let expected = assigned.clone();
    let joiner = LocalMachine::new(RecordOwner::spawn(joiner_store).unwrap());
    joiner
        .enroll(ployz_core::EnrollRequest {
            role: ployz_core::EnrollAs::Member { registration },
            wireguard_mtu: None,
            pinned: Default::default(),
        })
        .await
        .unwrap();
    drop(joiner);
    let reopened = LocalMachineStore::open(&joiner_dir).unwrap();
    assert_eq!(reopened.record().phase(), LocalMachinePhase::Joining);
    let persisted = reopened.record().machine().unwrap();
    assert_eq!(persisted.id, expected.id);
    assert_eq!(persisted.labels, expected.labels);
    assert_eq!(persisted.accepts_builds, expected.accepts_builds);
    assert_eq!(persisted.accepts_services, expected.accepts_services);
    assert_eq!(persisted.accepts_ingress, expected.accepts_ingress);
    server.abort();
    drop(entry);
    let _ = std::fs::remove_dir_all(data_dir);
    let _ = std::fs::remove_dir_all(joiner_dir);
}

/// A Register replaces the row an earlier Register wrote with its whole assignment,
/// even when that earlier request named a Management Identity: a Register writes no
/// identity, and never the mark that keeps a row its Machine published.
#[tokio::test]
async fn registration_with_a_changed_policy_replaces_the_registered_row() {
    let (local, replicated, data_dir, server) = sqlite_entry("ployzd-register-policy").await;
    let mut wire = serde_json::to_value(RegisterRequest {
        join_approval: None,
        initial_policy: ployz_core::InitialMachinePolicy {
            accepts_builds: false,
            ..Default::default()
        },
        ..request("peer", WireGuardPublicKey([7; 32]))
    })
    .unwrap();
    *wire.pointer_mut("/runtime/management_identity").unwrap() = serde_json::json!("11".repeat(32));
    let identity: RegisterRequest = serde_json::from_value(wire).unwrap();
    assert!(identity.runtime.management_identity.is_some());
    let assigned = local
        .register(identity.clone())
        .await
        .unwrap()
        .assigned_machine;
    assert_eq!(assigned.runtime.management_identity, None);
    assert_eq!(
        local
            .register(identity.clone())
            .await
            .unwrap()
            .assigned_machine,
        assigned
    );
    let changed = local
        .register(RegisterRequest {
            join_approval: None,
            name: ployz_core::MachineName::parse("renamed").unwrap(),
            initial_policy: ployz_core::InitialMachinePolicy {
                accepts_services: false,
                ..Default::default()
            },
            ..identity
        })
        .await
        .unwrap()
        .assigned_machine;
    assert!(!changed.accepts_services);
    assert!(changed.accepts_builds);
    assert_eq!(changed.name.as_str(), "renamed");
    assert_eq!(
        replicated.machine(assigned.id.as_str()).await.unwrap(),
        Some(changed)
    );
    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}
