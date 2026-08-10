//! The bounded local worker that owns a lens subscription and state cell.

use std::sync::Arc;
use std::time::Duration;

use ployz_core::ids::ClusterName;
use ployz_core::{ApiRefusal, CorrosionRetryAfterSeconds, LensCollection, LensSnapshot};
use tokio::sync::oneshot::error::TryRecvError;
use tokio::sync::{mpsc, oneshot, watch};

use super::adapter::{
    LensInput, LensStore, LensStoreError, LensSubscription, LensSubscriptionEvent,
};
use super::snapshots;
use super::{LensEngineConfig, LensRecoveryPolicy, LensWatch};
use crate::corrosion::CorrosionClientError;

type LensCell = Option<Result<LensSnapshot, ApiRefusal>>;

#[derive(Debug, Clone, Copy)]
struct RecoveryBudget {
    policy: LensRecoveryPolicy,
    attempts: u32,
}

impl RecoveryBudget {
    const fn new(policy: LensRecoveryPolicy) -> Self {
        Self {
            policy,
            attempts: 0,
        }
    }

    fn begin_attempt(&mut self) -> Option<Duration> {
        if self.attempts >= self.policy.max_attempts().get() {
            return None;
        }

        let shifts = self.attempts;
        self.attempts = self.attempts.saturating_add(1);
        let factor = 1_u32.checked_shl(shifts).unwrap_or(u32::MAX);
        Some(
            self.policy
                .initial_backoff()
                .checked_mul(factor)
                .unwrap_or(self.policy.max_backoff())
                .min(self.policy.max_backoff()),
        )
    }

    fn reset(&mut self) {
        self.attempts = 0;
    }
}

pub(super) fn start_lens_with_store<Store>(
    store: Arc<Store>,
    collection: LensCollection,
    config: LensEngineConfig,
    lifecycle_failures: mpsc::UnboundedSender<LensCollection>,
) -> LensWatch
where
    Store: LensStore + 'static,
{
    let (sender, receiver) = watch::channel::<LensCell>(None);
    let (shutdown, shutdown_receiver) = oneshot::channel();
    let task = tokio::spawn(run_lens(
        store,
        collection,
        config,
        sender,
        shutdown_receiver,
        lifecycle_failures,
    ));
    LensWatch {
        receiver,
        shutdown: Some(shutdown),
        task,
    }
}

async fn run_lens<Store>(
    store: Arc<Store>,
    collection: LensCollection,
    config: LensEngineConfig,
    sender: watch::Sender<LensCell>,
    mut shutdown: oneshot::Receiver<()>,
    lifecycle_failures: mpsc::UnboundedSender<LensCollection>,
) where
    Store: LensStore + 'static,
{
    let mut recovery = RecoveryBudget::new(config.recovery());
    let recovery_context = LensRecoveryContext {
        store: store.as_ref(),
        collection,
        config: &config,
    };
    let mut last_snapshot = None;
    let (mut active, snapshot) = match recover_current(
        &recovery_context,
        None,
        RecoveryPlan::InitialSnapshot,
        &mut recovery,
        &mut shutdown,
        &sender,
    )
    .await
    {
        RecoveryOutcome::Current { active, snapshot } => (active, snapshot),
        RecoveryOutcome::Terminal(refusal) => {
            publish_terminal(&sender, refusal);
            return;
        }
        RecoveryOutcome::Exhausted => {
            if shutdown_requested(&mut shutdown, &sender) {
                return;
            }
            publish_exhaustion(&sender, &lifecycle_failures, collection, config.recovery());
            return;
        }
        RecoveryOutcome::Stopped => return,
    };
    recovery.reset();
    publish_snapshot(&sender, &mut last_snapshot, snapshot);

    loop {
        let plan = match wait_for_invalidation(&mut active, &mut shutdown, &sender).await {
            Ok(()) => match refresh_snapshot(store.as_ref(), collection, &config).await {
                Ok(snapshot) => {
                    recovery.reset();
                    publish_snapshot(&sender, &mut last_snapshot, snapshot);
                    continue;
                }
                Err(LensRefreshError::Terminal(refusal)) => {
                    publish_terminal(&sender, refusal);
                    return;
                }
                Err(LensRefreshError::Store(error)) => {
                    tracing::debug!(?error, "Corrosion lens refresh failed after invalidation");
                    recovery_plan(&error)
                }
            },
            Err(LensWaitError::Stopped) => return,
            Err(LensWaitError::Store(error)) => recovery_plan(&error),
        };

        match recover_current(
            &recovery_context,
            Some(active),
            plan,
            &mut recovery,
            &mut shutdown,
            &sender,
        )
        .await
        {
            RecoveryOutcome::Current {
                active: restored,
                snapshot,
            } => {
                recovery.reset();
                publish_snapshot(&sender, &mut last_snapshot, snapshot);
                active = restored;
            }
            RecoveryOutcome::Terminal(refusal) => {
                publish_terminal(&sender, refusal);
                return;
            }
            RecoveryOutcome::Exhausted => {
                if shutdown_requested(&mut shutdown, &sender) {
                    return;
                }
                publish_exhaustion(&sender, &lifecycle_failures, collection, config.recovery());
                return;
            }
            RecoveryOutcome::Stopped => return,
        }
    }
}

fn shutdown_requested(
    shutdown: &mut oneshot::Receiver<()>,
    sender: &watch::Sender<LensCell>,
) -> bool {
    sender.is_closed() || !matches!(shutdown.try_recv(), Err(TryRecvError::Empty))
}

fn publish_exhaustion(
    sender: &watch::Sender<LensCell>,
    lifecycle_failures: &mpsc::UnboundedSender<LensCollection>,
    collection: LensCollection,
    recovery: LensRecoveryPolicy,
) {
    publish_terminal(sender, unavailable_refusal(recovery));
    let _ = lifecycle_failures.send(collection);
}

fn publish_snapshot(
    sender: &watch::Sender<LensCell>,
    last_snapshot: &mut Option<LensSnapshot>,
    snapshot: LensSnapshot,
) {
    if last_snapshot.as_ref() == Some(&snapshot) {
        return;
    }
    *last_snapshot = Some(snapshot.clone());
    sender.send_replace(Some(Ok(snapshot)));
}

fn publish_terminal(sender: &watch::Sender<LensCell>, refusal: ApiRefusal) {
    sender.send_replace(Some(Err(refusal)));
}

fn unavailable_refusal(policy: LensRecoveryPolicy) -> ApiRefusal {
    let capped = policy
        .max_backoff()
        .as_secs()
        .clamp(1, u64::from(CorrosionRetryAfterSeconds::MAX));
    let seconds = match u8::try_from(capped) {
        Ok(seconds) => seconds,
        Err(_) => CorrosionRetryAfterSeconds::MAX,
    };
    let retry_after_seconds = match CorrosionRetryAfterSeconds::try_new(seconds) {
        Ok(retry_after_seconds) => retry_after_seconds,
        Err(_) => CorrosionRetryAfterSeconds::DEFAULT,
    };
    ApiRefusal::CorrosionUnavailable {
        retry_after_seconds,
    }
}

struct ActiveInput {
    input: LensInput,
    subscription: Box<dyn LensSubscription>,
}

struct ActiveLens {
    inputs: Vec<ActiveInput>,
}

impl ActiveLens {
    async fn resume<Store>(
        self,
        store: &Store,
        config: &LensEngineConfig,
    ) -> Result<Self, LensStoreError>
    where
        Store: LensStore,
    {
        let mut inputs = Vec::with_capacity(self.inputs.len());
        for mut input in self.inputs {
            inputs.push(resume_input(store, &mut input, config.cluster_id()).await?);
        }
        Ok(Self { inputs })
    }
}

async fn resume_input<Store>(
    store: &Store,
    input: &mut ActiveInput,
    cluster_id: &ClusterName,
) -> Result<ActiveInput, LensStoreError>
where
    Store: LensStore,
{
    let input_kind = input.input;
    let identity = input.subscription.identity().clone();
    let Some(cursor) = input.subscription.cursor() else {
        return Err(LensStoreError::Corrosion(
            CorrosionClientError::SubscriptionEnded,
        ));
    };
    let subscription = store
        .resume(input_kind, cluster_id, &identity, cursor)
        .await?;
    Ok(ActiveInput {
        input: input_kind,
        subscription,
    })
}

enum LensWaitError {
    Stopped,
    Store(LensStoreError),
}

async fn establish_active<Store>(
    store: &Store,
    collection: LensCollection,
    config: &LensEngineConfig,
    shutdown: &mut oneshot::Receiver<()>,
    sender: &watch::Sender<LensCell>,
) -> Result<ActiveLens, LensWaitError>
where
    Store: LensStore,
{
    let timeout = config.limits().initial_snapshot_timeout();
    match tokio::time::timeout(
        timeout,
        establish_active_unbounded(store, collection, config, shutdown, sender),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(LensWaitError::Store(
            LensStoreError::InitialSnapshotDeadline { timeout },
        )),
    }
}

async fn establish_active_unbounded<Store>(
    store: &Store,
    collection: LensCollection,
    config: &LensEngineConfig,
    shutdown: &mut oneshot::Receiver<()>,
    sender: &watch::Sender<LensCell>,
) -> Result<ActiveLens, LensWaitError>
where
    Store: LensStore,
{
    let expected_inputs = LensInput::inputs_for(collection);
    let mut inputs = Vec::with_capacity(expected_inputs.len());
    for &input in expected_inputs {
        inputs.push(
            open_input(store, input, config)
                .await
                .map_err(LensWaitError::Store)?,
        );
    }

    let mut active = ActiveLens { inputs };
    drain_initial_snapshot(&mut active, shutdown, sender).await?;
    Ok(active)
}

async fn open_input<Store>(
    store: &Store,
    input: LensInput,
    config: &LensEngineConfig,
) -> Result<ActiveInput, LensStoreError>
where
    Store: LensStore,
{
    let subscription = store.subscribe(input, config.cluster_id()).await?;
    Ok(ActiveInput {
        input,
        subscription,
    })
}

async fn drain_initial_snapshot(
    active: &mut ActiveLens,
    shutdown: &mut oneshot::Receiver<()>,
    sender: &watch::Sender<LensCell>,
) -> Result<(), LensWaitError> {
    match active.inputs.as_mut_slice() {
        [input] => loop {
            let event =
                next_subscription_event(input.subscription.as_mut(), shutdown, sender).await?;
            if complete_snapshot_event(input, event).map_err(LensWaitError::Store)? {
                return Ok(());
            }
        },
        [first, second] => {
            let mut first_complete = false;
            let mut second_complete = false;
            while !first_complete || !second_complete {
                tokio::select! {
                    _ = &mut *shutdown => return Err(LensWaitError::Stopped),
                    _ = sender.closed() => return Err(LensWaitError::Stopped),
                    result = first.subscription.next(), if !first_complete => {
                        let event = result.map_err(LensWaitError::Store)?;
                        first_complete = complete_snapshot_event(first, event)
                            .map_err(LensWaitError::Store)?;
                    }
                    result = second.subscription.next(), if !second_complete => {
                        let event = result.map_err(LensWaitError::Store)?;
                        second_complete = complete_snapshot_event(second, event)
                            .map_err(LensWaitError::Store)?;
                    }
                }
            }
            Ok(())
        }
        [] | [_, _, _, ..] => Err(unexpected_input_shape()),
    }
}

fn complete_snapshot_event(
    input: &ActiveInput,
    event: LensSubscriptionEvent,
) -> Result<bool, LensStoreError> {
    match event {
        LensSubscriptionEvent::SnapshotRow(row) => {
            let _ = row;
            Ok(false)
        }
        LensSubscriptionEvent::SnapshotEnd { watermark } => {
            if input.subscription.cursor() == Some(watermark) {
                Ok(true)
            } else {
                Err(LensStoreError::Corrosion(CorrosionClientError::Protocol {
                    detail: "Corrosion snapshot watermark did not match its contiguous cursor"
                        .to_owned(),
                }))
            }
        }
        LensSubscriptionEvent::Invalidation => {
            Err(LensStoreError::Corrosion(CorrosionClientError::Protocol {
                detail: "Corrosion emitted a live invalidation before its snapshot watermark"
                    .to_owned(),
            }))
        }
    }
}

async fn next_subscription_event(
    subscription: &mut dyn LensSubscription,
    shutdown: &mut oneshot::Receiver<()>,
    sender: &watch::Sender<LensCell>,
) -> Result<LensSubscriptionEvent, LensWaitError> {
    tokio::select! {
        _ = &mut *shutdown => Err(LensWaitError::Stopped),
        _ = sender.closed() => Err(LensWaitError::Stopped),
        result = subscription.next() => result.map_err(LensWaitError::Store),
    }
}

async fn wait_for_invalidation(
    active: &mut ActiveLens,
    shutdown: &mut oneshot::Receiver<()>,
    sender: &watch::Sender<LensCell>,
) -> Result<(), LensWaitError> {
    match active.inputs.as_mut_slice() {
        [input] => {
            let event =
                next_subscription_event(input.subscription.as_mut(), shutdown, sender).await?;
            complete_live_event(event).map_err(LensWaitError::Store)
        }
        [first, second] => {
            tokio::select! {
                _ = &mut *shutdown => Err(LensWaitError::Stopped),
                _ = sender.closed() => Err(LensWaitError::Stopped),
                result = first.subscription.next() => {
                    result
                        .map_err(LensWaitError::Store)
                        .and_then(|event| complete_live_event(event).map_err(LensWaitError::Store))
                }
                result = second.subscription.next() => {
                    result
                        .map_err(LensWaitError::Store)
                        .and_then(|event| complete_live_event(event).map_err(LensWaitError::Store))
                }
            }
        }
        [] | [_, _, _, ..] => Err(unexpected_input_shape()),
    }
}

fn unexpected_input_shape() -> LensWaitError {
    LensWaitError::Store(LensStoreError::Corrosion(CorrosionClientError::Protocol {
        detail: "a lens collection declared an unsupported number of Corrosion inputs".to_owned(),
    }))
}

fn complete_live_event(event: LensSubscriptionEvent) -> Result<(), LensStoreError> {
    match event {
        LensSubscriptionEvent::Invalidation => Ok(()),
        LensSubscriptionEvent::SnapshotRow(row) => {
            let _ = row;
            Err(unexpected_live_snapshot_frame())
        }
        LensSubscriptionEvent::SnapshotEnd { .. } => Err(unexpected_live_snapshot_frame()),
    }
}

fn unexpected_live_snapshot_frame() -> LensStoreError {
    LensStoreError::Corrosion(CorrosionClientError::Protocol {
        detail: "Corrosion emitted a snapshot frame after its live watermark".to_owned(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryPlan {
    InitialSnapshot,
    Resume,
    FreshSnapshot,
}

fn recovery_plan(error: &LensStoreError) -> RecoveryPlan {
    match error {
        LensStoreError::Corrosion(
            CorrosionClientError::NonContiguousChange { .. }
            | CorrosionClientError::ChangeIdExhausted { .. }
            | CorrosionClientError::SubscriptionIdentityChanged,
        )
        | LensStoreError::InitialSnapshotDeadline { .. }
        | LensStoreError::MissingColumns { .. }
        | LensStoreError::DuplicateColumns { .. }
        | LensStoreError::UnexpectedColumns { .. }
        | LensStoreError::RowBeforeColumns { .. }
        | LensStoreError::UnexpectedRow { .. }
        | LensStoreError::SnapshotEnded { .. } => RecoveryPlan::FreshSnapshot,
        LensStoreError::Corrosion(_) | LensStoreError::QueryDeadline { .. } => RecoveryPlan::Resume,
    }
}

enum RecoveryOutcome {
    Current {
        active: ActiveLens,
        snapshot: LensSnapshot,
    },
    Terminal(ApiRefusal),
    Exhausted,
    Stopped,
}

struct LensRecoveryContext<'a, Store> {
    store: &'a Store,
    collection: LensCollection,
    config: &'a LensEngineConfig,
}

async fn recover_current<Store>(
    context: &LensRecoveryContext<'_, Store>,
    mut previous: Option<ActiveLens>,
    mut plan: RecoveryPlan,
    recovery: &mut RecoveryBudget,
    shutdown: &mut oneshot::Receiver<()>,
    sender: &watch::Sender<LensCell>,
) -> RecoveryOutcome
where
    Store: LensStore,
{
    let mut first_candidate = true;
    loop {
        let backoff = if first_candidate && plan == RecoveryPlan::InitialSnapshot {
            None
        } else {
            let Some(backoff) = recovery.begin_attempt() else {
                return RecoveryOutcome::Exhausted;
            };
            Some(backoff)
        };

        let candidate = match (plan, previous.take()) {
            (RecoveryPlan::Resume, Some(active)) => {
                match active.resume(context.store, context.config).await {
                    Ok(active) => Ok(active),
                    Err(_) => establish_active(
                        context.store,
                        context.collection,
                        context.config,
                        shutdown,
                        sender,
                    )
                    .await
                    .map_err(recovery_store_error),
                }
            }
            (_, _) => establish_active(
                context.store,
                context.collection,
                context.config,
                shutdown,
                sender,
            )
            .await
            .map_err(recovery_store_error),
        };

        match candidate {
            Ok(active) => {
                match refresh_snapshot(context.store, context.collection, context.config).await {
                    Ok(snapshot) => return RecoveryOutcome::Current { active, snapshot },
                    Err(LensRefreshError::Terminal(refusal)) => {
                        return RecoveryOutcome::Terminal(refusal);
                    }
                    Err(LensRefreshError::Store(error)) => {
                        tracing::debug!(?error, "Corrosion lens refresh failed during recovery");
                        previous = Some(active);
                        plan = recovery_plan(&error);
                    }
                }
            }
            Err(RecoveryAttemptError::Stopped) => return RecoveryOutcome::Stopped,
            Err(RecoveryAttemptError::Store(error)) => {
                tracing::debug!(?error, "Corrosion lens restoration failed during recovery");
                previous = None;
                plan = RecoveryPlan::FreshSnapshot;
            }
        }

        let backoff = match backoff {
            Some(backoff) => backoff,
            None => {
                let Some(backoff) = recovery.begin_attempt() else {
                    return RecoveryOutcome::Exhausted;
                };
                backoff
            }
        };
        tokio::select! {
            _ = &mut *shutdown => return RecoveryOutcome::Stopped,
            _ = sender.closed() => return RecoveryOutcome::Stopped,
            _ = tokio::time::sleep(backoff) => {}
        }
        first_candidate = false;
    }
}

enum RecoveryAttemptError {
    Stopped,
    Store(LensStoreError),
}

fn recovery_store_error(error: LensWaitError) -> RecoveryAttemptError {
    match error {
        LensWaitError::Stopped => RecoveryAttemptError::Stopped,
        LensWaitError::Store(error) => RecoveryAttemptError::Store(error),
    }
}

enum LensRefreshError {
    Store(LensStoreError),
    Terminal(ApiRefusal),
}

async fn refresh_snapshot<Store>(
    store: &Store,
    collection: LensCollection,
    config: &LensEngineConfig,
) -> Result<LensSnapshot, LensRefreshError>
where
    Store: LensStore,
{
    let timeout = config.limits().query_timeout();
    match tokio::time::timeout(
        timeout,
        refresh_snapshot_unbounded(store, collection, config),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(LensRefreshError::Store(LensStoreError::QueryDeadline {
            timeout,
        })),
    }
}

async fn refresh_snapshot_unbounded<Store>(
    store: &Store,
    collection: LensCollection,
    config: &LensEngineConfig,
) -> Result<LensSnapshot, LensRefreshError>
where
    Store: LensStore,
{
    let max_rows = config.limits().max_rows();
    match collection {
        LensCollection::Machines => {
            let (cluster_rows, machine_rows) = tokio::join!(
                store.query_rows(LensInput::Cluster, config.cluster_id(), max_rows),
                store.query_rows(LensInput::Machines, config.cluster_id(), max_rows),
            );
            let cluster_rows = cluster_rows.map_err(LensRefreshError::Store)?;
            let machine_rows = machine_rows.map_err(LensRefreshError::Store)?;
            snapshots::machines_snapshot(config.cluster_id(), cluster_rows, machine_rows)
                .map_err(LensRefreshError::Terminal)
        }
        LensCollection::Services => {
            let rows = store
                .query_rows(LensInput::Namespaces, config.cluster_id(), max_rows)
                .await
                .map_err(LensRefreshError::Store)?;
            Ok(snapshots::services_snapshot(config.cluster_id(), rows))
        }
        LensCollection::Endpoints => {
            let rows = store
                .query_rows(LensInput::Endpoints, config.cluster_id(), max_rows)
                .await
                .map_err(LensRefreshError::Store)?;
            Ok(snapshots::endpoints_snapshot(config.cluster_id(), rows))
        }
        LensCollection::MachineStatus => {
            let rows = store
                .query_rows(LensInput::MachineStatus, config.cluster_id(), max_rows)
                .await
                .map_err(LensRefreshError::Store)?;
            Ok(snapshots::machine_status_snapshot(
                config.cluster_id(),
                rows,
            ))
        }
        LensCollection::Operations => {
            let rows = store
                .query_rows(LensInput::Operations, config.cluster_id(), max_rows)
                .await
                .map_err(LensRefreshError::Store)?;
            Ok(snapshots::operations_snapshot(config.cluster_id(), rows))
        }
    }
}
