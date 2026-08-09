use std::collections::{BTreeMap, BTreeSet};

use ployz_core::corrosion::{
    CorrosionDocumentVersion, CorrosionNamespaceName, CorrosionServiceName, CorrosionTimestamp,
    OperationInitiator, OperatorWriteProvenance, ServiceDocument, ServicePlacement,
    ServiceReplicaCount, StoredRow, service_key,
};
use ployz_core::deploy::ImageReference;
use ployz_core::ids::{ClusterName, DeployName, PeerName};
use ployz_core::{
    ServiceRemoveRowRefusal, ServiceRemoveRowRequest, ServiceRemoveRowSelection,
    select_service_removal,
};

fn cluster() -> ClusterName {
    ClusterName::try_new("main").expect("cluster")
}

fn request() -> ServiceRemoveRowRequest {
    ServiceRemoveRowRequest {
        namespace_name: CorrosionNamespaceName::try_new("production").expect("namespace"),
        service_name: CorrosionServiceName::try_new("web").expect("service"),
    }
}

fn service() -> ServiceDocument {
    ServiceDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: cluster(),
        provenance: OperatorWriteProvenance {
            written_by: OperationInitiator::Peer {
                peer_id: PeerName::try_new("operator").expect("peer"),
            },
            written_at: CorrosionTimestamp::try_new("2026-08-09T00:00:00Z")
                .expect("timestamp"),
        },
        namespace_id: CorrosionNamespaceName::try_new("production").expect("namespace"),
        name: CorrosionServiceName::try_new("web").expect("service"),
        image: ImageReference::try_new("registry.example/web@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .expect("image"),
        env_fingerprints: BTreeMap::new(),
        placement: ServicePlacement::Replicated {
            replicas: ServiceReplicaCount::try_new(1).expect("replicas"),
        },
        pinned_machines: BTreeSet::new(),
        active_deploy: DeployName::try_new("release-1").expect("deploy"),
        previous_image: None,
        deployed_at: CorrosionTimestamp::try_new("2026-08-09T00:00:00Z")
            .expect("timestamp"),
    }
}

#[test]
fn service_removal_selects_the_exact_composite_key() {
    let request = request();
    let key = service_key(&request.namespace_name, &request.service_name);
    let document = serde_json::to_string(&service()).expect("json");
    let selection = select_service_removal(
        &cluster(),
        vec![StoredRow::new(key.clone(), document.clone())],
        &request,
    )
    .expect("selection");

    assert_eq!(
        selection,
        ServiceRemoveRowSelection::Delete {
            key,
            stored_document: document,
        }
    );
}

#[test]
fn service_removal_distinguishes_absent_from_unselectable() {
    let request = request();
    assert!(matches!(
        select_service_removal(&cluster(), Vec::new(), &request),
        Err(ServiceRemoveRowRefusal::NotFound { .. })
    ));

    let key = service_key(&request.namespace_name, &request.service_name);
    assert_eq!(
        select_service_removal(
            &cluster(),
            vec![StoredRow::new(key.clone(), "not json")],
            &request,
        ),
        Err(ServiceRemoveRowRefusal::StoredRowUnselectable { key })
    );
}
