use ployz_core::{IngressDoors, RpcErrorCode, SET_CLUSTER_DOORS_CAPABILITY, WireGuardPublicKey};

use super::{harness::*, *};

fn claiming(name: &str) -> RegisterRequest {
    RegisterRequest {
        capabilities: vec![SET_CLUSTER_DOORS_CAPABILITY.to_owned()],
        ..request(name, WireGuardPublicKey([2; 32]))
    }
}

#[tokio::test]
async fn a_closed_cluster_refuses_a_joiner_that_claims_nothing_and_publishes_no_row() {
    let (data_dir, store, founder) = open_store("ployzd-register-doors");
    let (replicated, server) = fake_cluster::store().await;
    replicated.publish_local_machine(&founder).await.unwrap();
    let service = machine_service(store, replicated.clone(), None);
    replicated
        .publish_cluster_doors(&IngressDoors::closed())
        .await
        .unwrap();

    let refused = rpc_register(&service, request("old", WireGuardPublicKey([1; 32])))
        .await
        .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Unsupported, "{refused:?}");
    assert_eq!(refused.details["capability"], SET_CLUSTER_DOORS_CAPABILITY);
    assert_eq!(refused.details["target"], "old");
    assert_eq!(replicated.machines().await.unwrap().observations.len(), 1);

    let admitted = rpc_register(&service, claiming("new")).await.unwrap();
    assert_eq!(admitted.assigned_machine.name.as_str(), "new");

    server.abort();
    drop(service);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn an_undeclared_or_public_cluster_admits_a_joiner_that_claims_nothing() {
    let (data_dir, store, founder) = open_store("ployzd-register-doors-open");
    let (replicated, server) = fake_cluster::store().await;
    replicated.publish_local_machine(&founder).await.unwrap();
    let service = machine_service(store, replicated.clone(), None);

    rpc_register(&service, request("first", WireGuardPublicKey([1; 32])))
        .await
        .unwrap();

    replicated
        .publish_cluster_doors(&IngressDoors::public())
        .await
        .unwrap();
    rpc_register(&service, request("second", WireGuardPublicKey([3; 32])))
        .await
        .unwrap();

    for doors in [IngressDoors::tunnel(), IngressDoors::both()] {
        replicated.publish_cluster_doors(&doors).await.unwrap();
        let refused = rpc_register(&service, request("third", WireGuardPublicKey([4; 32])))
            .await
            .unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::Unsupported, "{doors:?}");
    }

    server.abort();
    drop(service);
    let _ = std::fs::remove_dir_all(data_dir);
}
