use super::*;
use crate::deploy::{
    MigrateRenderError, MigrateServiceManifestRequest, render_migrate_service_manifest,
    validate_migrate_service_manifest_request,
};

fn migrate_request(
    namespace: impl Into<String>,
    service: impl Into<String>,
    target_machine: impl Into<String>,
) -> MigrateServiceManifestRequest {
    MigrateServiceManifestRequest {
        namespace: namespace.into(),
        service: service.into(),
        target_machine: target_machine.into(),
    }
}

fn test_service() -> ServiceSpec {
    test_service_with_mounts(
        "db",
        vec![Mount {
            source: MountSource::Volume("data".into()),
            target: "/var/lib/postgresql/data".into(),
            readonly: false,
        }],
    )
}

fn test_service_with_mounts(name: &str, mounts: Vec<Mount>) -> ServiceSpec {
    let mut service = test_service_spec(name, Placement::replicated(1), "postgres:17");
    service.template.mounts = mounts;
    service
}

fn test_volume_record(namespace: &Namespace, volume: &str, machine: &str) -> VolumeRecord {
    test_volume_record_with_scope(namespace, volume, machine, VolumeScope::Single)
}

fn test_volume_record_with_scope(
    namespace: &Namespace,
    volume: &str,
    machine: &str,
    scope: VolumeScope,
) -> VolumeRecord {
    VolumeRecord {
        namespace: namespace.clone(),
        volume_name: volume.into(),
        scope,
        machine_id: MachineId::new(machine),
        quota: "10G".into(),
        mode: "0750".into(),
        owner: "999:999".into(),
        attached_services: vec!["db".into()],
        created_at: 1,
        created_by_deploy_id: DeployId::new("deploy-1"),
        last_modified_at: 1,
        last_modified_by_deploy_id: DeployId::new("deploy-1"),
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
                release: ServiceRelease::direct(revision_hash, Vec::new(), deploy_id.clone(), 1),
            }],
            volumes,
            deploy: test_deploy_record(namespace, "deploy-1"),
        })
        .await
        .expect("seed committed service");
}

#[test]
fn validate_migrate_service_manifest_request_rejects_non_segment_namespace() {
    let error =
        validate_migrate_service_manifest_request(&migrate_request("prod/main", "db", "machine-b"))
            .expect_err("invalid namespace should fail");

    assert!(matches!(error, MigrateRenderError::InvalidRequest { .. }));
}

#[test]
fn validate_migrate_service_manifest_request_rejects_non_segment_service() {
    let error = validate_migrate_service_manifest_request(&migrate_request(
        "prod",
        "db/primary",
        "machine-b",
    ))
    .expect_err("invalid service should fail");

    assert!(matches!(error, MigrateRenderError::InvalidRequest { .. }));
}

#[tokio::test]
async fn render_migrate_service_manifest_adds_volume_move_hint() {
    let store = StoreDriver::memory();
    let namespace = Namespace::new("prod");
    seed_committed_service(
        &store,
        &namespace,
        test_service(),
        vec![test_volume_record(&namespace, "data", "machine-a")],
    )
    .await;

    let manifest =
        render_migrate_service_manifest(&store, &migrate_request("prod", "db", "machine-b"))
            .await
            .expect("render migrate manifest");

    let intent = manifest.intent.expect("intent");
    let [hint] = intent.volumes.as_slice() else {
        panic!("expected one volume move hint");
    };
    assert_eq!(hint.volume, "data");
    assert_eq!(
        hint.intent,
        VolumeIntent::Move {
            from_machine: "machine-a".into(),
            to_machine: "machine-b".into(),
        }
    );
}

#[tokio::test]
async fn render_migrate_service_manifest_rejects_missing_service() {
    let store = StoreDriver::memory();
    let namespace = Namespace::new("prod");
    seed_committed_service(
        &store,
        &namespace,
        test_service(),
        vec![test_volume_record(&namespace, "data", "machine-a")],
    )
    .await;

    let error =
        render_migrate_service_manifest(&store, &migrate_request("prod", "api", "machine-b"))
            .await
            .expect_err("missing service should fail");

    assert!(matches!(error, MigrateRenderError::ServiceMissing { .. }));
}

#[tokio::test]
async fn render_migrate_service_manifest_rejects_service_without_managed_volumes() {
    let store = StoreDriver::memory();
    let namespace = Namespace::new("prod");
    let service = test_service_with_mounts(
        "db",
        vec![Mount {
            source: MountSource::Tmpfs,
            target: "/var/lib/postgresql/data".into(),
            readonly: false,
        }],
    );
    seed_committed_service(&store, &namespace, service, Vec::new()).await;

    let error =
        render_migrate_service_manifest(&store, &migrate_request("prod", "db", "machine-b"))
            .await
            .expect_err("service without managed volumes should fail");

    assert!(matches!(
        error,
        MigrateRenderError::NoManagedVolumeMounts { .. }
    ));
}

#[tokio::test]
async fn render_migrate_service_manifest_rejects_bind_mounts() {
    let store = StoreDriver::memory();
    let namespace = Namespace::new("prod");
    let service = test_service_with_mounts(
        "db",
        vec![
            Mount {
                source: MountSource::Volume("data".into()),
                target: "/data".into(),
                readonly: false,
            },
            Mount {
                source: MountSource::Bind("/srv/db".into()),
                target: "/host-data".into(),
                readonly: false,
            },
        ],
    );
    seed_committed_service(
        &store,
        &namespace,
        service,
        vec![test_volume_record(&namespace, "data", "machine-a")],
    )
    .await;

    let error =
        render_migrate_service_manifest(&store, &migrate_request("prod", "db", "machine-b"))
            .await
            .expect_err("bind mounts should fail migration rendering");

    assert!(matches!(
        error,
        MigrateRenderError::UnsupportedBindMount { .. }
    ));
}

#[tokio::test]
async fn render_migrate_service_manifest_rejects_duplicate_managed_volume_mounts() {
    let store = StoreDriver::memory();
    let namespace = Namespace::new("prod");
    let service = test_service_with_mounts(
        "db",
        vec![
            Mount {
                source: MountSource::Volume("data".into()),
                target: "/data-a".into(),
                readonly: false,
            },
            Mount {
                source: MountSource::Volume("data".into()),
                target: "/data-b".into(),
                readonly: false,
            },
        ],
    );
    seed_committed_service(
        &store,
        &namespace,
        service,
        vec![test_volume_record(&namespace, "data", "machine-a")],
    )
    .await;

    let error =
        render_migrate_service_manifest(&store, &migrate_request("prod", "db", "machine-b"))
            .await
            .expect_err("duplicate managed mount should fail");

    assert!(matches!(
        error,
        MigrateRenderError::DuplicateManagedVolumeMount { .. }
    ));
}

#[tokio::test]
async fn render_migrate_service_manifest_rejects_missing_committed_volume() {
    let store = StoreDriver::memory();
    let namespace = Namespace::new("prod");
    seed_committed_service(&store, &namespace, test_service(), Vec::new()).await;

    let error =
        render_migrate_service_manifest(&store, &migrate_request("prod", "db", "machine-b"))
            .await
            .expect_err("missing committed volume should fail");

    assert!(matches!(
        error,
        MigrateRenderError::MissingCommittedVolume { .. }
    ));
}

#[tokio::test]
async fn render_migrate_service_manifest_rejects_already_on_target() {
    let store = StoreDriver::memory();
    let namespace = Namespace::new("prod");
    seed_committed_service(
        &store,
        &namespace,
        test_service(),
        vec![test_volume_record(&namespace, "data", "machine-b")],
    )
    .await;

    let error =
        render_migrate_service_manifest(&store, &migrate_request("prod", "db", "machine-b"))
            .await
            .expect_err("already-on-target volume should fail");

    assert!(matches!(error, MigrateRenderError::AlreadyOnTarget { .. }));
}

#[tokio::test]
async fn render_migrate_service_manifest_rejects_shared_volume() {
    let store = StoreDriver::memory();
    let namespace = Namespace::new("prod");
    seed_committed_service(
        &store,
        &namespace,
        test_service(),
        vec![test_volume_record_with_scope(
            &namespace,
            "data",
            "machine-a",
            VolumeScope::Shared,
        )],
    )
    .await;

    let error =
        render_migrate_service_manifest(&store, &migrate_request("prod", "db", "machine-b"))
            .await
            .expect_err("shared volume should fail");

    assert!(matches!(
        error,
        MigrateRenderError::UnsupportedVolumeScope { .. }
    ));
}

#[tokio::test]
async fn render_migrate_service_manifest_sorts_multi_volume_hints() {
    let store = StoreDriver::memory();
    let namespace = Namespace::new("prod");
    let service = test_service_with_mounts(
        "db",
        vec![
            Mount {
                source: MountSource::Volume("beta".into()),
                target: "/beta".into(),
                readonly: false,
            },
            Mount {
                source: MountSource::Volume("alpha".into()),
                target: "/alpha".into(),
                readonly: false,
            },
        ],
    );
    seed_committed_service(
        &store,
        &namespace,
        service,
        vec![
            test_volume_record(&namespace, "beta", "machine-a"),
            test_volume_record(&namespace, "alpha", "machine-a"),
        ],
    )
    .await;

    let manifest =
        render_migrate_service_manifest(&store, &migrate_request("prod", "db", "machine-b"))
            .await
            .expect("render migrate manifest");

    let intent = manifest.intent.expect("intent");
    let volumes = intent
        .volumes
        .iter()
        .map(|hint| hint.volume.as_str())
        .collect::<Vec<_>>();
    assert_eq!(volumes, vec!["alpha", "beta"]);
}
