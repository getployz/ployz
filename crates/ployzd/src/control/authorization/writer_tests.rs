use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use ployz_core::ids::{BuildExecutorId, BuildPoolId};
use ployz_core::nats_config::{BuildExecutorCredentialExpiresAt, CredentialName, MintedNatsUser};

use super::*;
use crate::control::store::CoreStore;

#[derive(Clone)]
struct ScriptedReload {
    outcomes: Arc<Mutex<VecDeque<NatsReloadOutcome>>>,
    calls: tokio::sync::watch::Sender<usize>,
}

impl ScriptedReload {
    fn new(outcomes: impl IntoIterator<Item = NatsReloadOutcome>) -> Self {
        Self {
            outcomes: Arc::new(Mutex::new(outcomes.into_iter().collect())),
            calls: tokio::sync::watch::channel(0).0,
        }
    }

    fn calls(&self) -> usize {
        *self.calls.borrow()
    }

    fn subscribe_calls(&self) -> tokio::sync::watch::Receiver<usize> {
        self.calls.subscribe()
    }
}

impl NatsReloadRunner for ScriptedReload {
    fn reload(&self) -> NatsReloadOutcome {
        self.calls.send_modify(|calls| *calls += 1);
        self.outcomes
            .lock()
            .expect("reload outcomes")
            .pop_front()
            .expect("scripted reload outcome")
    }
}

#[derive(Clone)]
struct TestNow {
    unix_seconds: Arc<AtomicU64>,
}

impl TestNow {
    fn new(unix_seconds: u64) -> Self {
        Self {
            unix_seconds: Arc::new(AtomicU64::new(unix_seconds)),
        }
    }

    fn source(&self) -> NowSource {
        let unix_seconds = Arc::clone(&self.unix_seconds);
        Arc::new(move || Ok(unix_seconds.load(Ordering::SeqCst)))
    }

    fn advance(&self, duration: Duration) {
        self.unix_seconds
            .fetch_add(duration.as_secs(), Ordering::SeqCst);
    }
}

fn credential(name: &str) -> CredentialGrant {
    CredentialGrant {
        public_key: MintedNatsUser::generate().expect("mint credential").public,
        name: CredentialName::try_new(name).expect("credential name"),
        role: CredentialRole::Operator,
    }
}

fn build_executor_credential(name: &str, expires_at_unix_seconds: u64) -> CredentialGrant {
    build_executor_credential_for(name, "executor_a", expires_at_unix_seconds)
}

fn build_executor_credential_for(
    name: &str,
    executor_id: &str,
    expires_at_unix_seconds: u64,
) -> CredentialGrant {
    CredentialGrant {
        public_key: MintedNatsUser::generate().expect("mint credential").public,
        name: CredentialName::try_new(name).expect("credential name"),
        role: CredentialRole::BuildExecutor {
            pool_id: BuildPoolId::try_new("pool_ci").expect("pool id"),
            executor_id: BuildExecutorId::try_new(executor_id).expect("executor id"),
            expires_at: BuildExecutorCredentialExpiresAt::try_new(expires_at_unix_seconds)
                .expect("expiry"),
        },
    }
}

fn reloaded() -> NatsReloadOutcome {
    NatsReloadOutcome::Reloaded(NatsReloadEvidence {
        command: "reload".to_owned(),
        output: "ok".to_owned(),
    })
}

fn failed_reload() -> NatsReloadOutcome {
    NatsReloadOutcome::Failed(NatsReloadEvidence {
        command: "reload".to_owned(),
        output: "failed".to_owned(),
    })
}

#[tokio::test(start_paused = true)]
async fn credential_verification_rejects_a_late_authorization_disconnect() {
    let connected = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let disconnect = tokio::spawn({
        let connected = Arc::clone(&connected);
        async move {
            tokio::time::sleep(Duration::from_millis(2)).await;
            connected.store(false, Ordering::SeqCst);
        }
    });

    assert!(
        !connection_remains_authenticated(|| connected.load(Ordering::SeqCst)).await,
        "the late authorization failure prevents credential admission"
    );
    disconnect
        .await
        .expect("scripted authorization disconnect completes");
}

async fn store_with(grants: &[CredentialGrant]) -> NatsAuthorizationStore {
    let store = NatsAuthorizationStore::new(CoreStore::open_in_memory().await.expect("store"));
    for grant in grants {
        store
            .upsert(&NatsAuthorizationGrant::Credential(grant.clone()))
            .await
            .expect("seed credential");
    }
    store
}

async fn write_current(path: &Path, store: &NatsAuthorizationStore) {
    std::fs::write(
        path,
        render_authorized_users(&store.list().await.expect("list grants")),
    )
    .expect("write authorization file");
}

#[tokio::test]
async fn adding_identical_credential_is_a_no_op() {
    let credential = credential("Founder");
    let store = store_with(std::slice::from_ref(&credential)).await;
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("authorized-users.conf");
    write_current(&path, &store).await;
    let reload = ScriptedReload::new([reloaded()]);
    let writer = NatsAuthorizationWriter::start(path, store, reload.clone());
    writer.handle().render(None).await.expect("startup render");

    let result = writer
        .handle()
        .add_credential(credential)
        .await
        .expect("no-op add");

    assert_eq!(
        (result.change, result.authorization.reload, reload.calls()),
        (CredentialMutationChange::Unchanged, None, 1)
    );
    writer.shutdown();
}

#[tokio::test]
async fn readding_public_key_updates_its_name() {
    let old = credential("Founder");
    let store = store_with(std::slice::from_ref(&old)).await;
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("authorized-users.conf");
    write_current(&path, &store).await;
    let reload = ScriptedReload::new([reloaded(), reloaded()]);
    let writer = NatsAuthorizationWriter::start(path, store.clone(), reload.clone());
    writer.handle().render(None).await.expect("startup render");
    let updated = CredentialGrant {
        name: CredentialName::try_new("Nick's laptop").expect("credential name"),
        ..old
    };

    let result = writer
        .handle()
        .add_credential(updated.clone())
        .await
        .expect("update credential");

    assert_eq!(
        (
            result.change,
            result.authorization.reload.is_some(),
            store.list_credentials().await.expect("credentials"),
            reload.calls(),
        ),
        (CredentialMutationChange::Updated, true, vec![updated], 2)
    );
    writer.shutdown();
}

#[tokio::test]
async fn removing_absent_credential_is_a_no_op() {
    let founder = credential("Founder");
    let absent = credential("Absent");
    let store = store_with(std::slice::from_ref(&founder)).await;
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("authorized-users.conf");
    write_current(&path, &store).await;
    let reload = ScriptedReload::new([reloaded()]);
    let writer = NatsAuthorizationWriter::start(path, store, reload.clone());
    writer.handle().render(None).await.expect("startup render");

    let result = writer
        .handle()
        .remove_credential(absent.public_key)
        .await
        .expect("no-op remove");

    assert_eq!(
        (result.change, result.authorization.reload, reload.calls()),
        (CredentialMutationChange::Unchanged, None, 1)
    );
    writer.shutdown();
}

#[tokio::test]
async fn failed_add_reload_keeps_grant_durable_across_writer_restart() {
    let first = credential("First");
    let second = credential("Second");
    let store = store_with(std::slice::from_ref(&first)).await;
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("authorized-users.conf");
    write_current(&path, &store).await;
    let reload = ScriptedReload::new([reloaded(), failed_reload()]);
    let writer = NatsAuthorizationWriter::start(path, store.clone(), reload.clone());
    let handle = writer.handle();
    handle.render(None).await.expect("startup render");

    let failure = handle
        .add_credential(second.clone())
        .await
        .expect_err("reload fails");
    writer.shutdown();
    let restart_reload = ScriptedReload::new([reloaded()]);
    let restarted = NatsAuthorizationWriter::start(
        directory.path().join("authorized-users.conf"),
        store.clone(),
        restart_reload.clone(),
    );
    let restart_render = restarted
        .handle()
        .render(None)
        .await
        .expect("restart projection");

    assert!(matches!(
        failure,
        CredentialMutationFailure::Committed {
            failure: RenderFailure::Reload { .. }
        }
    ));
    assert_eq!(
        (
            store.list_credentials().await.expect("credentials"),
            restart_render.reload.is_some(),
            reload.calls(),
            restart_reload.calls(),
        ),
        (vec![first, second], true, 2, 1)
    );
    restarted.shutdown();
}

#[tokio::test]
async fn failed_remove_reload_keeps_revocation_durable_across_writer_restart() {
    let first = credential("First");
    let second = credential("Second");
    let store = store_with(&[first.clone(), second.clone()]).await;
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("authorized-users.conf");
    write_current(&path, &store).await;
    let reload = ScriptedReload::new([reloaded(), failed_reload()]);
    let writer = NatsAuthorizationWriter::start(path, store.clone(), reload.clone());
    let handle = writer.handle();
    handle.render(None).await.expect("startup render");

    let failure = handle
        .remove_credential(first.public_key)
        .await
        .expect_err("reload fails");
    writer.shutdown();
    let restart_reload = ScriptedReload::new([reloaded()]);
    let restarted = NatsAuthorizationWriter::start(
        directory.path().join("authorized-users.conf"),
        store.clone(),
        restart_reload.clone(),
    );
    let restart_render = restarted
        .handle()
        .render(None)
        .await
        .expect("restart projection");

    assert!(matches!(
        failure,
        CredentialMutationFailure::Committed {
            failure: RenderFailure::Reload { .. }
        }
    ));
    assert_eq!(
        (
            store.list_credentials().await.expect("credentials"),
            restart_render.reload.is_some(),
            reload.calls(),
            restart_reload.calls(),
        ),
        (vec![second], true, 2, 1)
    );
    restarted.shutdown();
}

#[tokio::test]
async fn queued_removes_cannot_delete_the_final_operator() {
    let first = credential("First");
    let second = credential("Second");
    let store = store_with(&[first.clone(), second.clone()]).await;
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("authorized-users.conf");
    write_current(&path, &store).await;
    let reload = ScriptedReload::new([reloaded(), reloaded()]);
    let writer = NatsAuthorizationWriter::start(path, store.clone(), reload);
    let handle = writer.handle();
    handle.render(None).await.expect("startup render");

    let first_remove = handle.remove_credential(first.public_key);
    let second_remove = handle.remove_credential(second.public_key);
    let outcomes = tokio::join!(first_remove, second_remove);

    assert!(matches!(
        outcomes,
        (
            Ok(CredentialMutationResult {
                change: CredentialMutationChange::Removed,
                ..
            }),
            Err(CredentialMutationFailure::Rejected {
                reason: CredentialMutationRejection::LastOperator
            })
        ) | (
            Err(CredentialMutationFailure::Rejected {
                reason: CredentialMutationRejection::LastOperator
            }),
            Ok(CredentialMutationResult {
                change: CredentialMutationChange::Removed,
                ..
            })
        )
    ));
    assert_eq!(
        store.list_credentials().await.expect("credentials").len(),
        1
    );
    writer.shutdown();
}

#[tokio::test(start_paused = true)]
async fn build_executor_expires_without_deleting_durable_history() {
    let now = current_unix_seconds().expect("current time");
    let clock = TestNow::new(now);
    let founder = credential("Founder");
    let executor = build_executor_credential("CI executor", now + 60);
    let store = store_with(&[founder.clone(), executor.clone()]).await;
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("authorized-users.conf");
    let reload = ScriptedReload::new([reloaded(), reloaded()]);
    let writer = NatsAuthorizationWriter::start_with_now(
        path.clone(),
        store.clone(),
        reload.clone(),
        clock.source(),
    );
    let health = writer.health_reader();
    writer.handle().render(None).await.expect("startup render");
    wait_for_scheduled_expiry(&health, now + 60).await;

    clock.advance(Duration::from_secs(60));
    tokio::time::advance(Duration::from_secs(60)).await;
    wait_for_reload_calls(&reload, 2).await;

    assert_eq!(reload.calls(), 2);
    assert_eq!(
        parse_rendered_credentials(&path),
        vec![founder.clone()],
        "expired executor is absent from the running authorization projection"
    );
    assert_eq!(
        store.list_credentials().await.expect("durable credentials"),
        vec![founder, executor],
        "expiry retains durable credential history"
    );
    writer.shutdown();
}

#[tokio::test(start_paused = true)]
async fn credential_mutation_replaces_the_scheduled_expiry() {
    let now = current_unix_seconds().expect("current time");
    let clock = TestNow::new(now);
    let founder = credential("Founder");
    let executor = build_executor_credential("CI executor", now + 60);
    let store = store_with(std::slice::from_ref(&founder)).await;
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("authorized-users.conf");
    let reload = ScriptedReload::new([reloaded(), reloaded(), reloaded(), reloaded()]);
    let writer =
        NatsAuthorizationWriter::start_with_now(path, store, reload.clone(), clock.source());
    let health = writer.health_reader();
    writer.handle().render(None).await.expect("startup render");
    writer
        .handle()
        .add_credential(executor.clone())
        .await
        .expect("add executor");
    wait_for_scheduled_expiry(&health, now + 60).await;
    assert_eq!(
        health.snapshot().next_expiry_at_unix_seconds,
        Some(now + 60)
    );

    let renewed = CredentialGrant {
        role: CredentialRole::BuildExecutor {
            pool_id: BuildPoolId::try_new("pool_ci").expect("pool id"),
            executor_id: BuildExecutorId::try_new("executor_a").expect("executor id"),
            expires_at: BuildExecutorCredentialExpiresAt::try_new(now + 120).expect("expiry"),
        },
        ..executor
    };
    writer
        .handle()
        .add_credential(renewed)
        .await
        .expect("renew executor");
    wait_for_scheduled_expiry(&health, now + 120).await;
    assert_eq!(
        health.snapshot().next_expiry_at_unix_seconds,
        Some(now + 120)
    );

    clock.advance(Duration::from_secs(60));
    tokio::time::advance(Duration::from_secs(60)).await;
    tokio::task::yield_now().await;
    assert_eq!(reload.calls(), 3, "the replaced deadline no longer wakes");
    clock.advance(Duration::from_secs(60));
    tokio::time::advance(Duration::from_secs(60)).await;
    wait_for_reload_calls(&reload, 4).await;
    writer.shutdown();
}

#[tokio::test(start_paused = true)]
async fn delayed_expiry_wake_filters_every_deadline_that_has_passed() {
    let now = current_unix_seconds().expect("current time");
    let clock = TestNow::new(now);
    let founder = credential("Founder");
    let first = build_executor_credential_for("First executor", "executor_a", now + 30);
    let second = build_executor_credential_for("Second executor", "executor_b", now + 60);
    let store = store_with(&[founder.clone(), first, second]).await;
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("authorized-users.conf");
    let reload = ScriptedReload::new([reloaded(), reloaded()]);
    let writer = NatsAuthorizationWriter::start_with_now(
        path.clone(),
        store,
        reload.clone(),
        clock.source(),
    );
    let health = writer.health_reader();
    writer.handle().render(None).await.expect("startup render");
    wait_for_scheduled_expiry(&health, now + 30).await;

    clock.advance(Duration::from_secs(60));
    tokio::time::advance(Duration::from_secs(60)).await;
    wait_for_reload_calls(&reload, 2).await;
    wait_for_no_scheduled_expiry(&health).await;

    assert_eq!(parse_rendered_credentials(&path), vec![founder]);
    assert_eq!(health.snapshot().next_expiry_at_unix_seconds, None);
    writer.shutdown();
}

#[tokio::test(start_paused = true)]
async fn failed_expiry_reload_is_visible_and_recovers_on_retry() {
    let now = current_unix_seconds().expect("current time");
    let clock = TestNow::new(now);
    let founder = credential("Founder");
    let executor = build_executor_credential("CI executor", now + 60);
    let store = store_with(&[founder.clone(), executor]).await;
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("authorized-users.conf");
    let reload = ScriptedReload::new([reloaded(), failed_reload(), reloaded()]);
    let writer = NatsAuthorizationWriter::start_with_now(
        path.clone(),
        store,
        reload.clone(),
        clock.source(),
    );
    let health = writer.health_reader();
    writer.handle().render(None).await.expect("startup render");
    wait_for_scheduled_expiry(&health, now + 60).await;

    clock.advance(Duration::from_secs(60));
    tokio::time::advance(Duration::from_secs(60)).await;
    wait_for_reload_calls(&reload, 2).await;
    wait_for_health_failures(&health, 1).await;
    let degraded = health.snapshot();
    assert_eq!(degraded.consecutive_failures, 1);
    assert!(
        degraded
            .last_failure
            .as_deref()
            .is_some_and(|failure| failure.contains("reload failed"))
    );

    clock.advance(RETRY_SCHEDULE.interval);
    tokio::time::advance(RETRY_SCHEDULE.interval).await;
    wait_for_reload_calls(&reload, 3).await;
    wait_for_health_failures(&health, 0).await;

    assert_eq!(health.snapshot().consecutive_failures, 0);
    assert_eq!(health.snapshot().last_failure, None);
    assert_eq!(parse_rendered_credentials(&path), vec![founder]);
    writer.shutdown();
}

async fn wait_for_reload_calls(reload: &ScriptedReload, expected: usize) {
    let mut calls = reload.subscribe_calls();
    if !wait_for_observation(&mut calls, |calls| *calls >= expected).await {
        panic!(
            "reload call count did not reach {expected}; observed {}",
            reload.calls()
        );
    }
}

async fn wait_for_scheduled_expiry(health: &NatsAuthorizationHealthReader, expected: u64) {
    if !wait_for_health_observation(health, |state| {
        state.next_expiry_at_unix_seconds == Some(expected)
    })
    .await
    {
        panic!(
            "scheduled expiry did not become {expected}; observed {:?}",
            health.snapshot().next_expiry_at_unix_seconds
        );
    }
}

async fn wait_for_no_scheduled_expiry(health: &NatsAuthorizationHealthReader) {
    if !wait_for_health_observation(health, |state| state.next_expiry_at_unix_seconds.is_none())
        .await
    {
        panic!(
            "scheduled expiry did not clear; observed {:?}",
            health.snapshot().next_expiry_at_unix_seconds
        );
    }
}

async fn wait_for_health_failures(health: &NatsAuthorizationHealthReader, expected: u64) {
    if !wait_for_health_observation(health, |state| state.consecutive_failures == expected).await {
        panic!(
            "authorization failure count did not become {expected}; observed {}",
            health.snapshot().consecutive_failures
        );
    }
}

async fn wait_for_health_observation(
    health: &NatsAuthorizationHealthReader,
    predicate: impl Fn(&ployz_sdk_types::ControlNatsAuthorizationHealth) -> bool,
) -> bool {
    let mut changes = health.subscribe_changes();
    loop {
        let observed_revision = *changes.borrow();
        if predicate(&health.snapshot()) {
            return true;
        }
        if !wait_for_observation(&mut changes, |revision| *revision > observed_revision).await {
            return false;
        }
    }
}

async fn wait_for_observation<T>(
    changes: &mut tokio::sync::watch::Receiver<T>,
    predicate: impl FnMut(&T) -> bool,
) -> bool {
    const WALL_CLOCK_TIMEOUT: Duration = Duration::from_secs(5);

    let (cancel_timeout, timeout_cancelled) = std::sync::mpsc::channel();
    let mut wall_clock_timeout = tokio::task::spawn_blocking(move || {
        timeout_cancelled.recv_timeout(WALL_CLOCK_TIMEOUT).is_err()
    });
    tokio::select! {
        result = changes.wait_for(predicate) => {
            let _ = cancel_timeout.send(());
            result.is_ok()
        }
        timed_out = &mut wall_clock_timeout => timed_out.expect("wall-clock timeout task"),
    }
}

fn parse_rendered_credentials(path: &Path) -> Vec<CredentialGrant> {
    ployz_nats::permissions::parse_authorized_users(
        &std::fs::read_to_string(path).expect("authorization file"),
    )
    .expect("parse authorization file")
    .into_iter()
    .filter_map(|grant| match grant {
        NatsAuthorizationGrant::Credential(credential) => Some(credential),
        NatsAuthorizationGrant::Internal { .. } => None,
    })
    .collect()
}
