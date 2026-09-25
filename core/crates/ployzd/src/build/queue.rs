//! Machine-local FIFO admission. Permits follow execution, not the RPC task.
//!
//! The number of build slots is the Machine's build concurrency. The Runner
//! follows the Machine record and resizes when that value changes, so a
//! changed limit applies to queued Builds without restarting the daemon.

use crate::machine::LocalMachineRecord;
use ployz_build::{BuildError, HostPolicy};
use ployz_core::{BuildConcurrency, Machine};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};
use tokio::{
    sync::{AcquireError, OwnedSemaphorePermit, Semaphore, watch},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

pub(crate) struct Runner {
    pub(super) policy: HostPolicy,
    active: Arc<Semaphore>,
    slots: Arc<Mutex<Slots>>,
    waiting: Arc<Semaphore>,
    /// Builds running now, for the Machine's published observation.
    running: watch::Sender<u32>,
    pub(super) shutdown: CancellationToken,
}

/// `available + held - debt == size`. A lowered limit cannot take permits back
/// from running Builds, so it records debt and forgets them as they finish.
struct Slots {
    size: usize,
    debt: usize,
}

/// One admitted build slot. Dropping it frees the slot unless a lowered limit
/// is still owed.
pub(super) struct Slot {
    permit: Option<OwnedSemaphorePermit>,
    slots: Arc<Mutex<Slots>>,
    /// Set while the slot counts as a running Build.
    running: Option<watch::Sender<u32>>,
}

impl Slot {
    /// Count this slot as a running Build until it drops. Capability checks
    /// hold slots too, but they are not Builds.
    pub(super) fn count_as_build(&mut self, runner: &Runner) {
        runner.running.send_modify(|running| *running += 1);
        self.running = Some(runner.running.clone());
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let Some(running) = self.running.take() {
            running.send_modify(|running| *running -= 1);
        }
        let mut slots = self.slots.lock().expect("build slot lock");
        if slots.debt > 0
            && let Some(permit) = self.permit.take()
        {
            slots.debt -= 1;
            permit.forget();
        }
    }
}

impl Runner {
    pub(crate) fn new(
        policy: HostPolicy,
        shutdown: CancellationToken,
    ) -> Result<Arc<Self>, BuildError> {
        policy.validate()?;
        Ok(Arc::new(Self {
            waiting: Arc::new(Semaphore::new(policy.queue_capacity)),
            active: Arc::new(Semaphore::new(1)),
            slots: Arc::new(Mutex::new(Slots { size: 1, debt: 0 })),
            running: watch::Sender::new(0),
            policy,
            shutdown,
        }))
    }

    /// The number of Builds running now; it changes as they start and end.
    pub(crate) fn running_builds(&self) -> watch::Receiver<u32> {
        self.running.subscribe()
    }

    /// Follow the Machine record's build concurrency until shutdown: the slots
    /// resize when it changes, never per arrival.
    pub(crate) fn follow(self: &Arc<Self>, mut records: watch::Receiver<Arc<LocalMachineRecord>>) {
        let runner = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                let limit = records
                    .borrow_and_update()
                    .machine()
                    .map_or(BuildConcurrency::ONE, Machine::effective_build_concurrency);
                runner.resize(limit);
                tokio::select! {
                    biased;
                    () = runner.shutdown.cancelled() => return,
                    changed = records.changed() => if changed.is_err() { return },
                }
            }
        });
    }

    /// Set the number of simultaneous Builds. Raising it admits queued Builds
    /// in FIFO order; lowering it lets running Builds finish.
    fn resize(&self, limit: BuildConcurrency) {
        let limit = usize::from(limit);
        let mut slots = self.slots.lock().expect("build slot lock");
        if limit == slots.size {
            return;
        }
        if limit > slots.size {
            let grow = limit - slots.size;
            let repaid = grow.min(slots.debt);
            slots.debt -= repaid;
            self.active.add_permits(grow - repaid);
        } else {
            let shrink = slots.size - limit;
            slots.debt += shrink - self.active.forget_permits(shrink);
        }
        slots.size = limit;
    }

    pub(super) fn enter(
        &self,
    ) -> Result<
        Entry<impl Future<Output = Result<OwnedSemaphorePermit, AcquireError>> + Send>,
        QueueError,
    > {
        if self.shutdown.is_cancelled() {
            return Err(QueueError::Stopping);
        }
        let mut admission = Box::pin(self.active.clone().acquire_owned());
        // Enroll synchronously so a release or a slow progress consumer cannot
        // let a later arrival overtake this waiter before its first await.
        match admission
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(result) => result
                .map(|permit| Entry::Active(self.slot(permit)))
                .map_err(|_| QueueError::Stopping),
            Poll::Pending => {
                let capacity = self
                    .waiting
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| QueueError::Full)?;
                Ok(Entry::Waiting(Waiting {
                    _capacity: capacity,
                    admission,
                    expires: Instant::now() + self.policy.queue_timeout,
                }))
            }
        }
    }

    pub(super) async fn admit<F>(&self, waiting: Waiting<F>) -> Result<Slot, QueueError>
    where
        F: Future<Output = Result<OwnedSemaphorePermit, AcquireError>>,
    {
        tokio::select! {
            biased;
            () = tokio::time::sleep_until(waiting.expires) => Err(QueueError::Expired),
            result = waiting.admission => result.map(|permit| self.slot(permit)).map_err(|_| QueueError::Stopping),
        }
    }

    fn slot(&self, permit: OwnedSemaphorePermit) -> Slot {
        Slot {
            permit: Some(permit),
            slots: Arc::clone(&self.slots),
            running: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum QueueError {
    #[error("Build queue is full; execution was not attempted")]
    Full,
    #[error("Build queue timeout expired; execution was not attempted")]
    Expired,
    #[error("Build daemon is stopping; execution was not attempted")]
    Stopping,
}

pub(super) enum Entry<F> {
    Active(Slot),
    Waiting(Waiting<F>),
}

pub(super) struct Waiting<F> {
    _capacity: OwnedSemaphorePermit,
    admission: Pin<Box<F>>,
    expires: Instant,
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::poll;
    use std::{task::Poll, time::Duration};

    fn active(runner: &Runner) -> Slot {
        let Entry::Active(slot) = runner.enter().unwrap() else {
            panic!("expected admission")
        };
        slot
    }
    fn waiting(runner: &Runner) -> impl Future<Output = Result<Slot, QueueError>> {
        let Entry::Waiting(waiting) = runner.enter().unwrap() else {
            panic!("expected waiting")
        };
        runner.admit(waiting)
    }
    fn limit(value: &str) -> BuildConcurrency {
        BuildConcurrency::parse(value).unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn build_concurrency_bounds_simultaneous_builds_and_follows_changes() {
        let runner = Runner::new(HostPolicy::default(), CancellationToken::new()).unwrap();
        runner.resize(BuildConcurrency::automatic(false, Some(12_000_000_000)));
        let first = active(&runner);
        let second = active(&runner);
        let third = active(&runner);
        let mut fourth = Box::pin(waiting(&runner));
        assert!(
            poll!(&mut fourth).is_pending(),
            "automatic 3 admitted a fourth"
        );

        // Raising the limit admits the queued Build without a new arrival.
        runner.resize(limit("4"));
        let Poll::Ready(Ok(fourth)) = poll!(&mut fourth) else {
            panic!("raised limit did not admit the queued Build")
        };

        // Lowering it lets running Builds finish and admits nothing until
        // fewer than the new limit remain.
        runner.resize(limit("2"));
        let mut fifth = Box::pin(waiting(&runner));
        drop(first);
        drop(second);
        assert!(poll!(&mut fifth).is_pending(), "lowered limit was exceeded");
        drop(third);
        let Poll::Ready(Ok(fifth)) = poll!(&mut fifth) else {
            panic!("Build was not admitted under the lowered limit")
        };
        assert!(matches!(runner.enter(), Ok(Entry::Waiting(_))));
        drop((fourth, fifth));
        let _both = (active(&runner), active(&runner));
    }

    #[tokio::test(start_paused = true)]
    async fn only_counted_slots_are_running_builds() {
        let runner = Runner::new(HostPolicy::default(), CancellationToken::new()).unwrap();
        let running = runner.running_builds();
        let check = active(&runner);
        assert_eq!(*running.borrow(), 0, "a capability check is not a Build");
        drop(check);
        let mut build = active(&runner);
        build.count_as_build(&runner);
        assert_eq!(*running.borrow(), 1);
        drop(build);
        assert_eq!(*running.borrow(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn enrolled_waiter_cannot_be_overtaken_before_it_is_polled() {
        let runner = Runner::new(HostPolicy::default(), CancellationToken::new()).unwrap();
        let owner = active(&runner);
        let _first = runner.enter().unwrap();
        drop(owner);
        assert!(
            matches!(runner.enter(), Ok(Entry::Waiting(_))),
            "new arrival overtook an enrolled waiter"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn fifo_bounds_expiry_and_cancelled_waiters_release_capacity() {
        let runner = Runner::new(HostPolicy::default(), CancellationToken::new()).unwrap();
        let owner = active(&runner);
        let mut entries = (0..8).map(|_| waiting(&runner)).collect::<Vec<_>>();
        assert!(matches!(runner.enter(), Err(QueueError::Full)));
        let mut first = Box::pin(entries.remove(0));
        let mut second = Box::pin(entries.remove(0));
        assert!(poll!(&mut first).is_pending());
        assert!(poll!(&mut second).is_pending());
        drop(owner);
        assert!(poll!(&mut second).is_pending());
        let Poll::Ready(Ok(owner)) = poll!(&mut first) else {
            panic!("FIFO head did not get admission")
        };
        // Cancelling a queued future removes both its FIFO entry and capacity.
        drop(second);
        drop(entries);
        let mut expiring = Box::pin(waiting(&runner));
        assert!(poll!(&mut expiring).is_pending());
        tokio::time::advance(Duration::from_secs(599)).await;
        assert!(poll!(&mut expiring).is_pending());
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(matches!(
            poll!(&mut expiring),
            Poll::Ready(Err(QueueError::Expired))
        ));
        assert!(matches!(runner.enter(), Ok(Entry::Waiting(_))));
        drop(owner);
        drop(active(&runner));
    }
}
