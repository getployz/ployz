//! One shared view of the replicated Machines table.
//!
//! One task subscribes to Machine changes and re-reads the table when Corrosion
//! reports a change, when the subscription has to be reopened, and on a slow
//! safety interval. Readers borrow the latest snapshot from a watch instead of
//! each querying Corrosion on their own tick.
//!
//! The view is one Entry Machine's observation and can lag a write by the
//! subscription round trip. Decisions that must see the store as it is at the
//! moment they run, such as name checks under the publication guard, read the
//! store directly rather than this view.

use std::{sync::Arc, time::Duration};

use ployz_core::{Machine, MachineId};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::{Error, ReplicatedObservations, ReplicatedStore, Subscription};

/// Re-read even when no change was reported, so a stalled subscription cannot
/// freeze the view.
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);
/// Wait before reopening a failed subscription.
const RETRY_INTERVAL: Duration = Duration::from_secs(1);
/// Quiet window after a change; a join fans several Machine rows out at once.
const DEBOUNCE: Duration = Duration::from_millis(200);
/// Longest a stream of changes can defer a re-read; the quiet window resets, this does not.
const DEBOUNCE_LIMIT: Duration = Duration::from_secs(2);

/// The latest Machines snapshot, or `None` until the first read succeeds.
pub type MachinesSnapshot = Option<Arc<ReplicatedObservations<Machine, MachineId>>>;

/// Handle to the shared Machines view. Cloning shares the same task.
#[derive(Clone)]
pub struct MachineView {
    latest: watch::Receiver<MachinesSnapshot>,
}

impl MachineView {
    /// Start the view task. It stops when `shutdown` is cancelled.
    #[must_use]
    pub fn start(replicated: ReplicatedStore, shutdown: CancellationToken) -> Self {
        let (publish, latest) = watch::channel(None);
        tokio::spawn(run(replicated, publish, shutdown));
        Self { latest }
    }

    /// The latest snapshot. `None` until Corrosion has answered once.
    #[must_use]
    pub fn current(&self) -> MachinesSnapshot {
        self.latest.borrow().clone()
    }

    /// Every snapshot the view publishes. Closes when the view task stops.
    #[must_use]
    pub fn watch(&self) -> watch::Receiver<MachinesSnapshot> {
        self.latest.clone()
    }

    /// A view frozen at one snapshot; its watch reports closed immediately.
    #[cfg(test)]
    pub(crate) fn fixed(snapshot: MachinesSnapshot) -> Self {
        let (_, latest) = watch::channel(snapshot);
        Self { latest }
    }
}

enum Wake {
    Changed,
    Refresh,
    Resubscribe(Error),
    Shutdown,
}

async fn run(
    replicated: ReplicatedStore,
    publish: watch::Sender<MachinesSnapshot>,
    shutdown: CancellationToken,
) {
    loop {
        // Read before subscribing: queries can work while subscriptions cannot, and
        // a fresh subscription may follow missed changes.
        refresh(&replicated, &publish).await;
        let subscription = tokio::select! {
            subscription = tokio::time::timeout(
                REFRESH_INTERVAL,
                replicated.subscribe_machine_changes(),
            ) => subscription,
            () = shutdown.cancelled() => return,
        };
        let mut subscription = match subscription {
            Ok(Ok(subscription)) => subscription,
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "Machine view subscription failed, retrying");
                tokio::select! {
                    () = tokio::time::sleep(RETRY_INTERVAL) => continue,
                    () = shutdown.cancelled() => return,
                }
            }
            Err(_) => {
                tracing::warn!("Machine view subscription snapshot stalled, retrying");
                continue;
            }
        };
        // A row written between the read above and Corrosion registering the
        // subscription arrives in the drained initial snapshot, not as a change.
        refresh(&replicated, &publish).await;
        loop {
            match wait(&mut subscription, &shutdown).await {
                Wake::Changed | Wake::Refresh => refresh(&replicated, &publish).await,
                Wake::Resubscribe(error) => {
                    tracing::warn!(error = %error, "Machine view subscription ended, reopening");
                    tokio::select! {
                        () = tokio::time::sleep(RETRY_INTERVAL) => break,
                        () = shutdown.cancelled() => return,
                    }
                }
                Wake::Shutdown => return,
            }
        }
    }
}

async fn wait(subscription: &mut Subscription, shutdown: &CancellationToken) -> Wake {
    let first = tokio::select! {
        biased;
        () = shutdown.cancelled() => return Wake::Shutdown,
        changed = subscription.changed() => changed,
        () = tokio::time::sleep(REFRESH_INTERVAL) => {
            // A snapshot that never finished within the interval is a stuck stream.
            return if subscription.snapshot_in_progress() {
                Wake::Resubscribe(Error::Protocol("subscription snapshot stalled".into()))
            } else {
                Wake::Refresh
            };
        }
    };
    if let Err(error) = first {
        return Wake::Resubscribe(error);
    }
    let quiet = tokio::time::sleep(DEBOUNCE);
    tokio::pin!(quiet);
    let limit = tokio::time::sleep(DEBOUNCE_LIMIT);
    tokio::pin!(limit);
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => return Wake::Shutdown,
            () = &mut limit => return Wake::Changed,
            changed = subscription.changed() => {
                if let Err(error) = changed {
                    return Wake::Resubscribe(error);
                }
                quiet.as_mut().reset(tokio::time::Instant::now() + DEBOUNCE);
            }
            () = &mut quiet => return Wake::Changed,
        }
    }
}

async fn refresh(replicated: &ReplicatedStore, publish: &watch::Sender<MachinesSnapshot>) {
    match replicated.machines().await {
        Ok(snapshot) => {
            publish.send_if_modified(|current| {
                if current.as_deref() == Some(&snapshot) {
                    return false;
                }
                *current = Some(Arc::new(snapshot));
                true
            });
        }
        Err(error) => eprintln!("failed to read the Machines table for the Machine view: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ployz_core::{
        AdvertisedEndpoint, Machine, MachineId, MachineName, MachineRuntime, WireGuardPublicKey,
    };
    use tokio_util::sync::CancellationToken;

    use super::MachineView;
    use crate::corrosion::fake_cluster;

    fn machine(name: &str, octet: u8) -> Machine {
        Machine {
            labels: Default::default(),
            accepts_builds: true,
            accepts_services: true,
            accepts_ingress: true,
            id: MachineId::random(),
            name: MachineName::parse(name).unwrap(),
            subnet: format!("10.210.{octet}.0/24").parse().unwrap(),
            public_key: WireGuardPublicKey([octet; 32]),
            public_ip: None,
            advertised_endpoints: vec![AdvertisedEndpoint(
                format!("192.0.2.{octet}:51820").parse().unwrap(),
            )],
            runtime: MachineRuntime::default(),
        }
    }

    #[tokio::test]
    async fn publishes_the_table_on_start_and_after_each_change() {
        let (replicated, server) = fake_cluster::store_with_subscriptions().await;
        let first = machine("first", 1);
        replicated.publish_local_machine(&first).await.unwrap();
        let shutdown = CancellationToken::new();
        let view = MachineView::start(replicated.clone(), shutdown.clone());
        let mut latest = view.watch();

        let initial = tokio::time::timeout(Duration::from_secs(2), async {
            latest.wait_for(Option::is_some).await.unwrap().clone()
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(initial.observations, vec![first.clone()]);

        let second = machine("second", 2);
        replicated.publish_local_machine(&second).await.unwrap();
        let updated = tokio::time::timeout(Duration::from_secs(2), async {
            latest
                .wait_for(|snapshot| {
                    snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.observations.len() == 2)
                })
                .await
                .unwrap()
                .clone()
        })
        .await
        .unwrap()
        .unwrap();
        assert!(updated.observations.contains(&second));
        assert_eq!(view.current().unwrap().observations.len(), 2);

        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(2), async {
            while latest.changed().await.is_ok() {}
        })
        .await
        .expect("the view closes its watch on shutdown");
        server.abort();
    }

    #[tokio::test]
    async fn serves_queries_while_subscriptions_are_unavailable() {
        let (replicated, server) = fake_cluster::store().await;
        let only = machine("only", 1);
        replicated.publish_local_machine(&only).await.unwrap();
        let shutdown = CancellationToken::new();
        let view = MachineView::start(replicated, shutdown.clone());
        let mut latest = view.watch();

        let snapshot = tokio::time::timeout(Duration::from_secs(2), async {
            latest.wait_for(Option::is_some).await.unwrap().clone()
        })
        .await
        .expect("a view without subscriptions still reads the table")
        .unwrap();
        assert_eq!(snapshot.observations, vec![only]);

        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(2), async {
            while latest.changed().await.is_ok() {}
        })
        .await
        .expect("the view stops on shutdown");
        server.abort();
    }

    #[tokio::test]
    async fn a_steady_stream_of_changes_cannot_defer_the_re_read_past_the_limit() {
        let (replicated, server) = fake_cluster::store_with_subscriptions().await;
        let shutdown = CancellationToken::new();
        let view = MachineView::start(replicated.clone(), shutdown.clone());
        let mut latest = view.watch();
        tokio::time::timeout(Duration::from_secs(2), async {
            latest.wait_for(Option::is_some).await.unwrap();
        })
        .await
        .unwrap();

        // Change rows faster than the quiet window for longer than the limit.
        let writer = tokio::spawn({
            let replicated = replicated.clone();
            async move {
                for octet in 1..=60 {
                    replicated
                        .publish_local_machine(&machine(&format!("m{octet}"), octet))
                        .await
                        .unwrap();
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        });
        let observed = tokio::time::timeout(Duration::from_millis(2900), async {
            latest
                .wait_for(|snapshot| {
                    snapshot
                        .as_ref()
                        .is_some_and(|snapshot| !snapshot.observations.is_empty())
                })
                .await
                .unwrap()
                .clone()
        })
        .await
        .expect("the limit publishes a snapshot while writes continue")
        .unwrap();
        assert!(!observed.observations.is_empty());
        assert!(
            !writer.is_finished(),
            "the snapshot arrived while writes were still flowing"
        );

        writer.abort();
        shutdown.cancel();
        server.abort();
    }
}
