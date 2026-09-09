//! Machine-local FIFO admission. Permits follow execution, not the RPC task.

use ployz_build::{BuildError, HostPolicy};
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Waker},
};
use tokio::{
    sync::{AcquireError, OwnedSemaphorePermit, Semaphore},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

pub(crate) struct Runner {
    pub(super) policy: HostPolicy,
    active: Arc<Semaphore>,
    waiting: Arc<Semaphore>,
    pub(super) shutdown: CancellationToken,
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
            policy,
            shutdown,
        }))
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
            Poll::Ready(result) => result.map(Entry::Active).map_err(|_| QueueError::Stopping),
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

    pub(super) async fn admit<F>(
        &self,
        waiting: Waiting<F>,
    ) -> Result<OwnedSemaphorePermit, QueueError>
    where
        F: Future<Output = Result<OwnedSemaphorePermit, AcquireError>>,
    {
        tokio::select! {
            biased;
            () = tokio::time::sleep_until(waiting.expires) => Err(QueueError::Expired),
            result = waiting.admission => result.map_err(|_| QueueError::Stopping),
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
    Active(OwnedSemaphorePermit),
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

    fn active(runner: &Runner) -> OwnedSemaphorePermit {
        let Entry::Active(permit) = runner.enter().unwrap() else {
            panic!("expected admission")
        };
        permit
    }
    fn waiting(runner: &Runner) -> impl Future<Output = Result<OwnedSemaphorePermit, QueueError>> {
        let Entry::Waiting(waiting) = runner.enter().unwrap() else {
            panic!("expected waiting")
        };
        runner.admit(waiting)
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
