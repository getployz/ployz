use super::*;
use crate::deploy::export_manifest;
use ployz_error::DeployError;
use ployz_error::Error as PloyzError;

#[tokio::test]
async fn export_manifest_includes_stored_volume_declarations() {
    let store = StoreDriver::memory();
    let namespace = Namespace::new("prod");
    let service = test_service_spec("db", Placement::replicated(1), "postgres:17");
    let revision_hash = "rev-db".to_string();
    let deploy_id = DeployId::new("deploy-1");

    store
        .commit_deploy(&DeployCommit {
            namespace: namespace.clone(),
            revisions: vec![ServiceRevisionRecord {
                namespace: namespace.clone(),
                service: service.name.clone(),
                revision_hash: revision_hash.clone(),
                spec_json: serde_json::to_string(&service).expect("serialize service"),
                created_by: MachineId::new("local"),
                created_at: 1,
            }],
            removed_services: Vec::new(),
            removed_volumes: Vec::new(),
            branch_lineage: Vec::new(),
            volume_movements: Vec::new(),
            volume_branches: Vec::new(),
            phase_commits: Vec::new(),
            releases: vec![ServiceReleaseRecord {
                namespace: namespace.clone(),
                service: service.name.clone(),
                release: ServiceRelease::direct(revision_hash, Vec::new(), deploy_id.clone(), 1),
            }],
            volumes: vec![VolumeRecord {
                namespace: namespace.clone(),
                volume_name: "data".into(),
                scope: VolumeScope::Single,
                machine_id: MachineId::new("machine-a"),
                quota: "10G".into(),
                mode: "0750".into(),
                owner: "999:999".into(),
                attached_services: vec!["db".into()],
                created_at: 1,
                created_by_deploy_id: deploy_id.clone(),
                last_modified_at: 1,
                last_modified_by_deploy_id: deploy_id.clone(),
            }],
            deploy: test_deploy_record(&namespace, "deploy-1"),
        })
        .await
        .expect("seed release and volume");

    let manifest = export_manifest(&store, &namespace)
        .await
        .expect("export manifest");

    let [volume] = manifest.volumes.as_slice() else {
        panic!("expected one volume declaration");
    };
    assert_eq!(volume.name, "data");
    assert_eq!(volume.scope, VolumeScope::Single);
    assert_eq!(volume.quota, "10G");
    assert_eq!(volume.mode, "0750");
    assert_eq!(volume.owner, "999:999");
    manifest.validate().expect("export should validate");
}

#[tokio::test]
async fn export_manifest_surfaces_release_referencing_missing_revision() {
    let store = StoreDriver::memory();
    let namespace = Namespace::new("prod");
    let deploy_id = DeployId::new("deploy-1");

    store
        .commit_deploy(&DeployCommit {
            namespace: namespace.clone(),
            revisions: Vec::new(),
            removed_services: Vec::new(),
            removed_volumes: Vec::new(),
            branch_lineage: Vec::new(),
            volume_movements: Vec::new(),
            volume_branches: Vec::new(),
            phase_commits: Vec::new(),
            releases: vec![ServiceReleaseRecord {
                namespace: namespace.clone(),
                service: "api".into(),
                release: ServiceRelease::direct("missing-rev", Vec::new(), deploy_id.clone(), 1),
            }],
            volumes: Vec::new(),
            deploy: test_deploy_record(&namespace, "deploy-1"),
        })
        .await
        .expect("seed corrupt release");

    let error = export_manifest(&store, &namespace)
        .await
        .expect_err("missing revision should fail export");

    assert_eq!(
        error,
        PloyzError::Deploy(DeployError::StoredReleaseMissingRevision {
            service: "api".into(),
            revision_hash: "missing-rev".into()
        })
    );
}

#[tokio::test]
async fn export_manifest_surfaces_stored_spec_service_mismatch() {
    let store = StoreDriver::memory();
    let namespace = Namespace::new("prod");
    let mut service = test_service_spec("db", Placement::replicated(1), "postgres:17");
    service.name = "wrong-service".into();
    let revision_hash = "rev-api".to_string();
    let deploy_id = DeployId::new("deploy-1");

    store
        .commit_deploy(&DeployCommit {
            namespace: namespace.clone(),
            revisions: vec![ServiceRevisionRecord {
                namespace: namespace.clone(),
                service: "api".into(),
                revision_hash: revision_hash.clone(),
                spec_json: serde_json::to_string(&service).expect("serialize service"),
                created_by: MachineId::new("local"),
                created_at: 1,
            }],
            removed_services: Vec::new(),
            removed_volumes: Vec::new(),
            branch_lineage: Vec::new(),
            volume_movements: Vec::new(),
            volume_branches: Vec::new(),
            phase_commits: Vec::new(),
            releases: vec![ServiceReleaseRecord {
                namespace: namespace.clone(),
                service: "api".into(),
                release: ServiceRelease::direct(revision_hash, Vec::new(), deploy_id.clone(), 1),
            }],
            volumes: Vec::new(),
            deploy: test_deploy_record(&namespace, "deploy-1"),
        })
        .await
        .expect("seed release");

    let error = export_manifest(&store, &namespace)
        .await
        .expect_err("mismatched stored spec should fail export");

    assert_eq!(
        error,
        PloyzError::Deploy(DeployError::StoredSpecServiceMismatch {
            stored_service: "wrong-service".into(),
            release_service: "api".into()
        })
    );
}
