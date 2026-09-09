//! Initial policy is part of the first registration and durable Join assignment.

use super::*;

#[tokio::test]
async fn registration_publishes_initial_policy_and_join_persists_it() {
    let (allocator, replicated, _founder, data_dir, server) = participating().await;
    let joiner_dir =
        std::env::temp_dir().join(format!("ployzd-policy-join-{}", MachineId::random()));
    let joiner_store = LocalMachineStore::open(&joiner_dir).unwrap();
    let public_key = joiner_store.record().private_key().public_key();
    let mut wire = serde_json::to_value(request("builder", public_key)).unwrap();
    wire.as_object_mut().unwrap().insert(
        "initial_policy".into(),
        serde_json::json!({
            "labels": {"pool": "build"},
            "accepts_builds": true,
            "accepts_services": false,
            "accepts_ingress": false
        }),
    );
    let registration = allocator
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
    let joiner = LocalMachine::new(Arc::new(Mutex::new(joiner_store)), watch::channel(false).0);
    joiner
        .join(JoinRequest {
            registration,
            wireguard_mtu: None,
            cloud_pairing: None,
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
    drop(allocator);
    let _ = std::fs::remove_dir_all(data_dir);
    let _ = std::fs::remove_dir_all(joiner_dir);
}

#[tokio::test]
async fn registration_replay_refuses_current_policy_mismatch_without_publication() {
    let (local, replicated, _founder, data_dir, server) = participating().await;
    let identity = request("peer", WireGuardPublicKey([7; 32]));
    let assigned = local
        .register(identity.clone())
        .await
        .unwrap()
        .assigned_machine;
    let replay = local.register(identity.clone()).await.unwrap();
    assert_eq!(replay.assigned_machine, assigned);
    for changed in [
        ployz_core::InitialMachinePolicy {
            accepts_services: false,
            ..Default::default()
        },
        ployz_core::InitialMachinePolicy {
            labels: [("pool".parse().unwrap(), "build".parse().unwrap())].into(),
            ..Default::default()
        },
    ] {
        let outcome = local
            .register(RegisterRequest {
                initial_policy: changed,
                ..identity.clone()
            })
            .await;
        assert!(
            outcome.is_err(),
            "enrollment must refuse a different current policy"
        );
        assert_eq!(
            replicated
                .machine(assigned.id.as_str())
                .await
                .unwrap()
                .as_ref(),
            Some(&assigned)
        );
    }
    let mut edited = assigned;
    edited.accepts_builds = false;
    replicated.publish_local_machine(&edited).await.unwrap();
    assert!(
        local.register(identity).await.is_err(),
        "an old enrollment retry must not overwrite an operator edit"
    );
    assert_eq!(
        replicated
            .machine(edited.id.as_str())
            .await
            .unwrap()
            .as_ref(),
        Some(&edited)
    );
    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}
