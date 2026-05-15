use super::*;
use crate::deploy::{
    BranchManifestResourceMode, BranchManifestResourceModeOverride, BranchNamespaceManifestRequest,
    BranchRenderError, render_branch_namespace_manifest, stable_fingerprint,
};

fn branch_request(
    source_namespace: impl Into<String>,
    target_namespace: impl Into<String>,
) -> BranchNamespaceManifestRequest {
    BranchNamespaceManifestRequest {
        source_namespace: source_namespace.into(),
        target_namespace: target_namespace.into(),
        default_service_mode: BranchManifestResourceMode::Branch,
        default_volume_mode: BranchManifestResourceMode::Fresh,
        services: Vec::new(),
        volumes: Vec::new(),
    }
}

fn mode_override(
    name: impl Into<String>,
    mode: BranchManifestResourceMode,
) -> BranchManifestResourceModeOverride {
    BranchManifestResourceModeOverride {
        name: name.into(),
        mode,
    }
}

fn test_service() -> ServiceSpec {
    let mut service = test_service_spec("db", Placement::replicated(1), "postgres:17");
    service.template.mounts = vec![Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    }];
    service
}

fn test_volume_record(namespace: &Namespace, volume: &str, machine: &str) -> VolumeRecord {
    let deploy_id = DeployId::new("deploy-1");
    VolumeRecord {
        namespace: namespace.clone(),
        volume_name: volume.into(),
        scope: VolumeScope::Single,
        machine_id: MachineId::new(machine),
        quota: "10G".into(),
        mode: "0750".into(),
        owner: "999:999".into(),
        attached_services: vec!["db".into()],
        created_at: 1,
        created_by_deploy_id: deploy_id.clone(),
        last_modified_at: 1,
        last_modified_by_deploy_id: deploy_id,
    }
}

async fn seed_committed_service(
    store: &StoreDriver,
    namespace: &Namespace,
    service: ServiceSpec,
    volumes: Vec<VolumeRecord>,
) {
    let revision_hash = format!("rev-{}", service.name);
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
                service: service.name,
                release: ServiceRelease::direct(revision_hash, Vec::new(), deploy_id, 1),
            }],
            volumes,
            deploy: test_deploy_record(namespace, "deploy-1"),
        })
        .await
        .expect("seed committed service");
}

#[tokio::test]
async fn render_branch_namespace_manifest_branches_services_and_fresh_volumes_by_default() {
    let store = StoreDriver::memory();
    let source = Namespace::new("prod");
    seed_committed_service(
        &store,
        &source,
        test_service(),
        vec![test_volume_record(&source, "data", "machine-a")],
    )
    .await;

    let manifest = render_branch_namespace_manifest(&store, &branch_request("prod", "pr-39"))
        .await
        .expect("render branch manifest");

    assert_eq!(manifest.namespace, Namespace::new("pr-39"));
    let intent = manifest.intent.expect("branch service intent");
    let [service] = intent.services.as_slice() else {
        panic!(
            "expected one service branch hint, got {:?}",
            intent.services
        );
    };
    assert_eq!(service.service, "db");
    assert_eq!(
        service.intent,
        ServiceIntent::Branch {
            source_namespace: Namespace::new("prod"),
            source_service: "db".into(),
            expected_source_revision_hash: Some("rev-db".into()),
        }
    );
    assert!(intent.volumes.is_empty());
}

#[tokio::test]
async fn render_branch_namespace_manifest_clones_opted_in_volumes() {
    let store = StoreDriver::memory();
    let source = Namespace::new("prod");
    let source_volume = test_volume_record(&source, "data", "machine-a");
    let expected_source_record_fingerprint = stable_fingerprint(&source_volume);
    seed_committed_service(&store, &source, test_service(), vec![source_volume]).await;

    let mut request = branch_request("prod", "pr-39");
    request.volumes = vec![mode_override("data", BranchManifestResourceMode::Branch)];
    let manifest = render_branch_namespace_manifest(&store, &request)
        .await
        .expect("render branch manifest");

    let intent = manifest.intent.expect("branch intent");
    let [volume] = intent.volumes.as_slice() else {
        panic!("expected one volume clone hint, got {:?}", intent.volumes);
    };
    assert_eq!(volume.volume, "data");
    assert_eq!(
        volume.intent,
        VolumeIntent::Clone {
            source_namespace: Namespace::new("prod"),
            source_volume: "data".into(),
            data_policy: VolumeCloneDataPolicy::Raw,
            consistency: VolumeCloneConsistency::CrashConsistent,
            expected_source_record_fingerprint: Some(expected_source_record_fingerprint),
        }
    );
}

#[tokio::test]
async fn render_branch_namespace_manifest_service_override_fresh_removes_hint() {
    let store = StoreDriver::memory();
    let source = Namespace::new("prod");
    seed_committed_service(
        &store,
        &source,
        test_service(),
        vec![test_volume_record(&source, "data", "machine-a")],
    )
    .await;

    let mut request = branch_request("prod", "pr-39");
    request.services = vec![mode_override("db", BranchManifestResourceMode::Fresh)];
    let manifest = render_branch_namespace_manifest(&store, &request)
        .await
        .expect("render branch manifest");

    assert!(manifest.intent.is_none());
}

#[tokio::test]
async fn render_branch_namespace_manifest_rejects_empty_source() {
    let store = StoreDriver::memory();

    let error = render_branch_namespace_manifest(&store, &branch_request("missing", "pr-39"))
        .await
        .expect_err("empty source should fail");

    assert!(matches!(error, BranchRenderError::EmptySource { .. }));
}

#[tokio::test]
async fn branch_prepare_record_preserves_compiled_source_evidence() {
    let store = StoreDriver::memory();
    let mut machine = MachineMembership::seed(
        MachineId::new("founder"),
        PublicKey([42; 32]),
        OverlayIp("fd00::42".parse().expect("valid overlay")),
        None,
        Vec::new(),
    );
    machine.lifecycle = MachineLifecycle::Active;
    store
        .upsert_self_machine(&machine)
        .await
        .expect("seed active machine");
    let source = Namespace::new("prod");
    let source_volume = test_volume_record(&source, "data", "founder");
    let expected_source_record_fingerprint = stable_fingerprint(&source_volume);
    seed_committed_service(&store, &source, test_service(), vec![source_volume]).await;

    let mut request = branch_request("prod", "pr-39");
    request.volumes = vec![mode_override("data", BranchManifestResourceMode::Branch)];
    let manifest = render_branch_namespace_manifest(&store, &request)
        .await
        .expect("render branch manifest");

    let prepared = prepare(
        &store,
        &MachineId::new("founder"),
        &manifest,
        &NoopParticipantProbe,
        DeployId::new("prepare-branch"),
        600,
    )
    .await
    .expect("prepare compiled branch manifest");

    assert_eq!(prepared.namespace, Namespace::new("pr-39"));
    let stored_manifest: DeployManifest =
        serde_json::from_str(&prepared.manifest_json).expect("stored manifest json");
    assert_eq!(stored_manifest.namespace, Namespace::new("pr-39"));
    let intent = stored_manifest.intent.expect("stored branch intent");
    let [service] = intent.services.as_slice() else {
        panic!(
            "expected one stored service branch hint, got {:?}",
            intent.services
        );
    };
    assert_eq!(
        service.intent,
        ServiceIntent::Branch {
            source_namespace: Namespace::new("prod"),
            source_service: "db".into(),
            expected_source_revision_hash: Some("rev-db".into()),
        }
    );
    let [volume] = intent.volumes.as_slice() else {
        panic!(
            "expected one stored volume clone hint, got {:?}",
            intent.volumes
        );
    };
    assert_eq!(
        volume.intent,
        VolumeIntent::Clone {
            source_namespace: Namespace::new("prod"),
            source_volume: "data".into(),
            data_policy: VolumeCloneDataPolicy::Raw,
            consistency: VolumeCloneConsistency::CrashConsistent,
            expected_source_record_fingerprint: Some(expected_source_record_fingerprint),
        }
    );
}

#[tokio::test]
async fn render_branch_namespace_manifest_rejects_unknown_and_duplicate_overrides() {
    let store = StoreDriver::memory();
    let source = Namespace::new("prod");
    seed_committed_service(
        &store,
        &source,
        test_service(),
        vec![test_volume_record(&source, "data", "machine-a")],
    )
    .await;

    let mut request = branch_request("prod", "pr-39");
    request.services = vec![mode_override("missing", BranchManifestResourceMode::Fresh)];
    let unknown = render_branch_namespace_manifest(&store, &request)
        .await
        .expect_err("unknown override should fail");
    assert!(matches!(unknown, BranchRenderError::UnknownService { .. }));

    let mut request = branch_request("prod", "pr-39");
    request.volumes = vec![mode_override("missing", BranchManifestResourceMode::Branch)];
    let unknown_volume = render_branch_namespace_manifest(&store, &request)
        .await
        .expect_err("unknown volume override should fail");
    assert!(matches!(
        unknown_volume,
        BranchRenderError::UnknownVolume { .. }
    ));

    let mut request = branch_request("prod", "pr-39");
    request.services = vec![
        mode_override("db", BranchManifestResourceMode::Branch),
        mode_override("db", BranchManifestResourceMode::Fresh),
    ];
    let duplicate_service = render_branch_namespace_manifest(&store, &request)
        .await
        .expect_err("duplicate service override should fail");
    assert!(matches!(
        duplicate_service,
        BranchRenderError::DuplicateService { .. }
    ));

    let mut request = branch_request("prod", "pr-39");
    request.volumes = vec![
        mode_override("data", BranchManifestResourceMode::Branch),
        mode_override("data", BranchManifestResourceMode::Fresh),
    ];
    let duplicate = render_branch_namespace_manifest(&store, &request)
        .await
        .expect_err("duplicate override should fail");
    assert!(matches!(
        duplicate,
        BranchRenderError::DuplicateVolume { .. }
    ));
}
