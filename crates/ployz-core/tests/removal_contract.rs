use std::collections::{BTreeMap, BTreeSet};

use ployz_core::corrosion::{
    CorrosionDocumentVersion, CorrosionNamespaceName, CorrosionServiceName, CorrosionTimestamp,
    NamespaceDocument, OperationInitiator, OperatorWriteProvenance, PublishedService,
    ServicePlacement, ServiceReplicaCount, StoredRow,
};
use ployz_core::deploy::ImageReference;
use ployz_core::ids::{ClusterName, DeployName, PeerName};
use ployz_core::{
    ServiceRemoveRefusal, ServiceRemoveRequest, ServiceRemoveSelection, select_service_removal,
};

fn cluster() -> ClusterName {
    ClusterName::try_new("main").expect("cluster")
}

fn request() -> ServiceRemoveRequest {
    ServiceRemoveRequest {
        namespace_name: CorrosionNamespaceName::try_new("production").expect("namespace"),
        service_name: CorrosionServiceName::try_new("web").expect("service"),
    }
}

fn provenance(peer: &str, written_at: &str) -> OperatorWriteProvenance {
    OperatorWriteProvenance {
        written_by: OperationInitiator::Peer {
            peer_id: PeerName::try_new(peer).expect("peer"),
        },
        written_at: CorrosionTimestamp::try_new(written_at).expect("timestamp"),
    }
}

fn service(image: &str) -> PublishedService {
    PublishedService {
        image: ImageReference::try_new(image).expect("image"),
        env_fingerprints: BTreeMap::new(),
        placement: ServicePlacement::Replicated {
            replicas: ServiceReplicaCount::try_new(1).expect("replicas"),
        },
        pinned_machines: BTreeSet::new(),
        active_deploy: DeployName::try_new("release-1").expect("deploy"),
        previous_image: None,
        deployed_at: CorrosionTimestamp::try_new("2026-08-09T00:00:00Z").expect("timestamp"),
    }
}

fn namespace() -> NamespaceDocument {
    NamespaceDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: cluster(),
        provenance: provenance("operator", "2026-08-09T00:00:00Z"),
        name: CorrosionNamespaceName::try_new("production").expect("namespace"),
        services: BTreeMap::from([
            (
                CorrosionServiceName::try_new("web").expect("service"),
                service(
                    "registry.example/web@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                ),
            ),
            (
                CorrosionServiceName::try_new("worker").expect("service"),
                service(
                    "registry.example/worker@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                ),
            ),
        ]),
    }
}

#[test]
fn service_removal_replaces_the_exact_namespace_without_the_named_service() {
    let request = request();
    let original = namespace();
    let document = serde_json::to_string(&original).expect("json");
    let replacement_provenance = provenance("new-operator", "2026-08-10T00:00:00Z");
    let selection = select_service_removal(
        &cluster(),
        Some(StoredRow::new("production", document.clone())),
        &request,
        replacement_provenance.clone(),
    )
    .expect("selection");

    let ServiceRemoveSelection::Replace {
        namespace_name,
        stored_document,
        replacement_document,
    } = selection
    else {
        panic!("service should be selected for removal")
    };
    assert_eq!(namespace_name, request.namespace_name);
    assert_eq!(stored_document, document);
    assert_eq!(replacement_document.provenance, replacement_provenance);
    assert!(
        !replacement_document
            .services
            .contains_key(&request.service_name)
    );
    assert_eq!(
        replacement_document
            .services
            .get(&CorrosionServiceName::try_new("worker").expect("service")),
        original
            .services
            .get(&CorrosionServiceName::try_new("worker").expect("service"))
    );
}

#[test]
fn service_removal_distinguishes_absent_namespace_from_unselectable() {
    let request = request();
    assert!(matches!(
        select_service_removal(
            &cluster(),
            None,
            &request,
            provenance("operator", "2026-08-10T00:00:00Z"),
        ),
        Err(ServiceRemoveRefusal::NotFound { .. })
    ));

    assert_eq!(
        select_service_removal(
            &cluster(),
            Some(StoredRow::new(request.namespace_name.as_str(), "not json",)),
            &request,
            provenance("operator", "2026-08-10T00:00:00Z"),
        ),
        Err(ServiceRemoveRefusal::NamespaceStoredRowUnselectable {
            namespace_name: request.namespace_name.clone(),
        })
    );
}

#[test]
fn service_removal_is_already_absent_when_the_namespace_has_no_named_service() {
    let request = request();
    let mut namespace = namespace();
    namespace.services.remove(&request.service_name);
    let document = serde_json::to_string(&namespace).expect("json");

    assert_eq!(
        select_service_removal(
            &cluster(),
            Some(StoredRow::new("production", document)),
            &request,
            provenance("operator", "2026-08-10T00:00:00Z"),
        ),
        Ok(ServiceRemoveSelection::AlreadyAbsent {
            namespace_name: request.namespace_name,
            service_name: request.service_name,
        })
    );
}
