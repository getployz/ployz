use std::collections::BTreeSet;

use ployz_core::corrosion::{
    CorrosionDocumentVersion, CorrosionNamespaceName, CorrosionServiceName, CorrosionTable,
    CorrosionTimestamp, OperationDocument, Principal, deploy_key, service_key,
};
use ployz_core::ids::{ClusterName, DeployName, MachineName, PeerName};

#[test]
fn namespace_and_service_names_are_lowercase_dns_labels() {
    for valid in ["a", "api", "web-2", "a1"] {
        assert!(CorrosionNamespaceName::try_new(valid).is_ok());
        assert!(CorrosionServiceName::try_new(valid).is_ok());
    }
    for invalid in ["", "API", "-api", "api-", "api_worker", "a/b"] {
        assert!(CorrosionNamespaceName::try_new(invalid).is_err());
        assert!(CorrosionServiceName::try_new(invalid).is_err());
    }
}

#[test]
fn table_catalog_contains_namespace_intent_and_machine_endpoint_testimony() {
    assert_eq!(CorrosionTable::ALL.len(), 13);
    assert!(CorrosionTable::ALL.contains(&CorrosionTable::Namespaces));
    assert!(CorrosionTable::ALL.contains(&CorrosionTable::MachineEndpoints));
    assert_eq!(
        CorrosionTable::MachineEndpoints.as_str(),
        "machine_endpoints"
    );
}

#[test]
fn service_and_deploy_keys_have_their_intended_scopes() {
    let namespace = CorrosionNamespaceName::try_new("production").expect("namespace");
    let service = CorrosionServiceName::try_new("api").expect("service");
    let deploy = DeployName::try_new("release-42").expect("deploy");

    assert_eq!(service_key(&namespace, &service), "production/api");
    assert_eq!(deploy_key(&namespace, &deploy), "production/release-42");
}

#[test]
fn operation_identity_is_namespace_wide() {
    let namespace = CorrosionNamespaceName::try_new("production").expect("namespace");
    let deploy = DeployName::try_new("release-42").expect("deploy");
    let operation = OperationDocument::deploy_created(
        CorrosionDocumentVersion::V1,
        ClusterName::try_new("main").expect("cluster"),
        MachineName::try_new("machine-a").expect("machine"),
        Principal::Peer {
            peer_id: PeerName::try_new("operator").expect("peer"),
        },
        namespace.clone(),
        deploy.clone(),
        CorrosionTimestamp::try_new("2026-08-09T00:00:00Z").expect("timestamp"),
    );
    let value = serde_json::to_value(&operation).expect("operation json");

    assert_eq!(
        value.get("namespace_id"),
        Some(&serde_json::json!("production"))
    );
    assert_eq!(
        value.get("deploy_name"),
        Some(&serde_json::json!("release-42"))
    );
    assert!(value.get("service_name").is_none());
    assert_eq!(deploy_key(&namespace, &deploy), "production/release-42");
}

#[test]
fn service_names_are_orderable_for_duplicate_detection() {
    let names = ["worker", "api", "api"]
        .into_iter()
        .map(|name| CorrosionServiceName::try_new(name).expect("service"))
        .collect::<BTreeSet<_>>();
    assert_eq!(names.len(), 2);
}
