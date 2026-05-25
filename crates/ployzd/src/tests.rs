use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::{
    CorrosionCleanup, CorrosionFailure, CorrosionPhase, CorrosionStartMode, CorrosionStop,
    DaemonConfig, DaemonErrorKind, DaemonRuntime, PeerFailure, PeerPhase, SchemaPhase,
    StartupReport,
};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

#[test]
fn daemon_config_derives_state_subpaths() {
    let root = temp_state_dir("config");
    let config = test_config(&root);

    assert_eq!(config.state_dir(), root.as_path());
    assert_eq!(config.peer_identity_path(), root.join("peer.key").as_path());
    assert_eq!(
        config.corrosion_root_dir(),
        root.join("corrosion").as_path()
    );
}

#[tokio::test]
async fn missing_corrosion_binary_is_setup_error() {
    let root = temp_state_dir("missing-bin");
    let config = test_config(&root).with_corrosion_binary_path("/definitely/missing/corrosion");

    let result = DaemonRuntime::start(config).await;

    let Err(error) = result else {
        panic!("expected missing binary to fail");
    };
    assert!(matches!(error.kind(), DaemonErrorKind::Corrosion { .. }));
    let report = error.startup_report();
    assert_eq!(
        report.corrosion().phase(),
        CorrosionPhase::Failed(CorrosionFailure::Agent)
    );
    assert_eq!(report.schema().phase(), SchemaPhase::Configured);
    assert_eq!(report.peer().phase(), PeerPhase::Configured);
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn daemon_boot_applies_membership_schema_and_reports_ready() {
    let root = temp_state_dir("boot");
    let config = test_config(&root).with_corrosion_start_mode(CorrosionStartMode::StartManaged);
    let runtime = DaemonRuntime::start(config).await.expect("daemon");

    assert!(runtime.startup_report().is_ready());
    assert_eq!(
        runtime.startup_report().corrosion().phase(),
        CorrosionPhase::Ready
    );
    assert_eq!(
        runtime.startup_report().schema().phase(),
        SchemaPhase::Ready
    );
    assert_eq!(runtime.startup_report().peer().phase(), PeerPhase::Ready);
    assert!(runtime.startup_report().endpoint_id().is_some());
    assert_machines_table_exists(runtime.store()).await;

    let stopped = runtime.shutdown().await.expect("shutdown");
    assert!(!stopped.is_ready());
    assert_eq!(stopped.corrosion().phase(), CorrosionPhase::Ready);
    assert_corrosion_stopped(stopped.corrosion().cleanup());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn corrosion_cleanup_does_not_overwrite_startup_failure() {
    let root = temp_state_dir("report");
    let report = StartupReport::configured(&test_config(&root))
        .with_corrosion_failed(CorrosionFailure::Store)
        .with_corrosion_shutdown(&polis::CorrosionShutdown::Forced);

    assert_eq!(
        report.corrosion().phase(),
        CorrosionPhase::Failed(CorrosionFailure::Store)
    );
    assert_eq!(
        report.corrosion().cleanup(),
        CorrosionCleanup::Stopped(CorrosionStop::Forced)
    );
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn daemon_restart_preserves_peer_endpoint_id_and_schema_usability() {
    let root = temp_state_dir("restart");
    let config = test_config(&root).with_corrosion_start_mode(CorrosionStartMode::StartManaged);
    let first = DaemonRuntime::start(config.clone()).await.expect("first");
    let first_endpoint = first.endpoint_id();
    let machine_id = restart_machine_id();
    assert_machines_table_exists(first.store()).await;
    upsert_machine_row(first.store(), &machine_id).await;
    first.shutdown().await.expect("first shutdown");

    let second = DaemonRuntime::start(config).await.expect("second");
    assert_eq!(second.endpoint_id(), first_endpoint);
    assert_machines_table_exists(second.store()).await;
    assert_machine_row_exists(second.store(), &machine_id).await;
    second.shutdown().await.expect("second shutdown");
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn daemon_adopts_existing_corrosion_agent_without_claiming_process_shutdown() {
    let root = temp_state_dir("adopt-corrosion");
    let config = test_config(&root).with_corrosion_start_mode(CorrosionStartMode::AdoptExisting);
    let existing = polis::test_support::CorrosionAgentFixtureConfig::persistent_from_config(
        config.corrosion_config().clone(),
    )
    .start()
    .await
    .expect("existing corrosion");
    let existing_store = existing.store().expect("existing store");

    let runtime = DaemonRuntime::start(config).await.expect("daemon");
    assert_machines_table_exists(runtime.store()).await;

    let stopped = runtime.shutdown().await.expect("shutdown daemon");
    assert_eq!(stopped.corrosion().phase(), CorrosionPhase::Ready);
    assert_eq!(stopped.corrosion().cleanup(), CorrosionCleanup::Unmanaged);
    assert_machines_table_exists(&existing_store).await;

    existing
        .shutdown(Duration::from_millis(250))
        .await
        .expect("existing corrosion shutdown");
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn daemon_refuses_to_adopt_corrosion_from_another_state_directory() {
    let existing_root = temp_state_dir("adopt-existing-root");
    let existing_config =
        test_config(&existing_root).with_corrosion_start_mode(CorrosionStartMode::StartManaged);
    let existing = polis::test_support::CorrosionAgentFixtureConfig::persistent_from_config(
        existing_config.corrosion_config().clone(),
    )
    .start()
    .await
    .expect("existing corrosion");

    let adopt_root = temp_state_dir("adopt-wrong-root");
    let adopt_config =
        DaemonConfig::for_test_state_dir(&adopt_root, existing_config.corrosion_addresses())
            .with_corrosion_start_mode(CorrosionStartMode::AdoptExisting);

    let result = DaemonRuntime::start(adopt_config).await;

    let Err(error) = result else {
        panic!("expected daemon adoption to fail");
    };
    assert!(matches!(
        error.kind(),
        DaemonErrorKind::Corrosion {
            source: polis::CorrosionAgentError::Ownership { .. }
        }
    ));
    assert_eq!(
        error.startup_report().corrosion().phase(),
        CorrosionPhase::Failed(CorrosionFailure::Agent)
    );

    existing
        .shutdown(Duration::from_millis(250))
        .await
        .expect("existing corrosion shutdown");
    let _ = fs::remove_dir_all(existing_root);
    let _ = fs::remove_dir_all(adopt_root);
}

#[tokio::test]
async fn start_or_adopt_refuses_foreign_corrosion_instead_of_starting_over_it() {
    let existing_root = temp_state_dir("start-or-adopt-existing-root");
    let existing_config =
        test_config(&existing_root).with_corrosion_start_mode(CorrosionStartMode::StartManaged);
    let existing = polis::test_support::CorrosionAgentFixtureConfig::persistent_from_config(
        existing_config.corrosion_config().clone(),
    )
    .start()
    .await
    .expect("existing corrosion");

    let adopt_root = temp_state_dir("start-or-adopt-wrong-root");
    let adopt_config =
        DaemonConfig::for_test_state_dir(&adopt_root, existing_config.corrosion_addresses());

    let result = DaemonRuntime::start(adopt_config).await;

    let Err(error) = result else {
        panic!("expected daemon adoption to fail");
    };
    assert!(matches!(
        error.kind(),
        DaemonErrorKind::Corrosion {
            source: polis::CorrosionAgentError::Ownership { .. }
        }
    ));

    existing
        .shutdown(Duration::from_millis(250))
        .await
        .expect("existing corrosion shutdown");
    let _ = fs::remove_dir_all(existing_root);
    let _ = fs::remove_dir_all(adopt_root);
}

#[cfg(unix)]
#[tokio::test]
async fn shutdown_reports_managed_corrosion_that_already_exited() {
    let root = temp_state_dir("unexpected-corrosion-exit");
    let config = test_config(&root).with_corrosion_start_mode(CorrosionStartMode::StartManaged);
    let runtime = DaemonRuntime::start(config).await.expect("daemon");
    let pid = runtime
        .corrosion_process_id()
        .expect("managed corrosion process id");

    let status = std::process::Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .status()
        .expect("kill corrosion");
    assert!(status.success());
    tokio::time::sleep(Duration::from_millis(100)).await;

    let result = runtime.shutdown().await;

    let Err(error) = result else {
        panic!("expected shutdown to report unexpected corrosion exit");
    };
    match error.kind() {
        DaemonErrorKind::Shutdown {
            corrosion: Some(polis::CorrosionAgentError::UnexpectedExit { exit }),
            ..
        } => assert!(!exit.status().is_empty()),
        other => panic!("expected unexpected corrosion exit, got {other:?}"),
    }
    assert_eq!(
        error.startup_report().corrosion().cleanup(),
        CorrosionCleanup::Stopped(CorrosionStop::UnexpectedExit)
    );
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn invalid_state_directory_returns_setup_error() {
    let root = temp_state_dir("invalid-state-file");
    fs::write(&root, b"not a directory").expect("state file");
    let config = test_config(&root);

    let result = DaemonRuntime::start(config).await;

    let Err(error) = result else {
        panic!("expected invalid state to fail");
    };
    assert!(matches!(error.kind(), DaemonErrorKind::Setup { .. }));
    let report = error.startup_report();
    assert_eq!(report.corrosion().phase(), CorrosionPhase::Configured);
    assert_eq!(report.schema().phase(), SchemaPhase::Configured);
    assert_eq!(report.peer().phase(), PeerPhase::Configured);
    let _ = fs::remove_file(root);
}

#[tokio::test]
async fn peer_start_failure_rolls_back_corrosion_and_reports_stopped_report() {
    let root = temp_state_dir("peer-failure");
    fs::create_dir_all(root.join("peer.key")).expect("peer identity dir");
    let config = test_config(&root);

    let result = DaemonRuntime::start(config).await;

    let Err(error) = result else {
        panic!("expected peer startup to fail");
    };
    assert!(matches!(error.kind(), DaemonErrorKind::Peer { .. }));
    let report = error.startup_report();
    assert_eq!(report.corrosion().phase(), CorrosionPhase::Ready);
    assert_corrosion_stopped(report.corrosion().cleanup());
    assert_eq!(report.schema().phase(), SchemaPhase::Ready);
    assert_eq!(
        report.peer().phase(),
        PeerPhase::Failed(PeerFailure::Startup)
    );
    let _ = fs::remove_dir_all(root);
}

async fn assert_machines_table_exists(store: &polis::CorrosionStore) {
    let query = polis::StoreStatement::new(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name = 'machines'",
    )
    .expect("query");
    let rows = store
        .query(&query, polis::StoreTimeout::seconds(2).expect("timeout"))
        .await
        .expect("rows");

    assert_eq!(
        rows.rows().first().and_then(|row| row.text("name").ok()),
        Some("machines")
    );
}

async fn upsert_machine_row(store: &polis::CorrosionStore, machine_id: &polis::StoreMachineId) {
    let row = polis::MachineRow::new(
        machine_id.clone(),
        polis::IslandId::parse("restart-island").expect("island id"),
        polis::IrohEndpointId::parse("restart-endpoint").expect("endpoint"),
        polis::WireGuardPublicKey::parse("restart-wireguard-key").expect("wireguard key"),
        polis::OverlayIp::parse("fd00::10").expect("overlay ip"),
        polis::MembershipLifecycle::Active,
        polis::RowEpoch::new(1).expect("epoch"),
        1,
    );
    let statement = polis::upsert_machine_statement(&row).expect("upsert statement");
    store
        .execute_transaction(&[statement], test_store_timeout())
        .await
        .expect("upsert row");
}

async fn assert_machine_row_exists(
    store: &polis::CorrosionStore,
    machine_id: &polis::StoreMachineId,
) {
    let query = polis::MachineRowQuery::by_machine_id(machine_id).expect("machine query");
    let rows = store
        .query(query.statement(), test_store_timeout())
        .await
        .expect("query row");
    let row = query
        .decode_optional(&rows)
        .expect("decode row")
        .expect("persisted row");

    assert_eq!(row.machine_id(), machine_id);
}

fn restart_machine_id() -> polis::StoreMachineId {
    polis::StoreMachineId::parse("restart-machine").expect("machine id")
}

fn test_store_timeout() -> polis::StoreTimeout {
    polis::StoreTimeout::seconds(2).expect("timeout")
}

fn temp_state_dir(label: &str) -> PathBuf {
    let next = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let root = short_temp_root().join(format!("pzd-{label}-{}-{next}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_file(&root);
    root
}

fn short_temp_root() -> PathBuf {
    if cfg!(unix) {
        PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    }
}

fn test_corrosion_addresses() -> polis::CorrosionAgentAddresses {
    polis::test_support::corrosion_agent_addresses().expect("corrosion addresses")
}

fn test_config(root: &Path) -> DaemonConfig {
    DaemonConfig::for_test_state_dir(root, test_corrosion_addresses())
        .with_corrosion_shutdown_timeout(Duration::from_millis(250))
}

fn assert_corrosion_stopped(cleanup: CorrosionCleanup) {
    match cleanup {
        CorrosionCleanup::Stopped(CorrosionStop::Graceful | CorrosionStop::Forced) => {}
        CorrosionCleanup::Stopped(CorrosionStop::UnexpectedExit)
        | CorrosionCleanup::NotRun
        | CorrosionCleanup::Unmanaged
        | CorrosionCleanup::Failed => panic!("expected corrosion stopped, got {cleanup:?}"),
    }
}
