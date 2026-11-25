//! Work at Full scope that outlives the call that started it: an exec, a port-forward
//! tunnel, a Build, a Build Grant. Each is bound to the management key that asked for it.
//! Every key change, once its record is written and before it returns, ends the work whose
//! key no longer holds Full scope, through [`LocalMachine::finish_key_change`]. It waits at
//! most [`STOP_WITHIN`] for each piece to stop, and returns to its caller every piece that
//! did not confirm it stopped.
//!
//! A secret is read for bound work only through [`Bound::read`]. A [`Bound`] is work the key
//! change waits for, so no read is under way once the change has returned. A [`Tie`] only
//! says whether the work has ended, and reads nothing.
//!
//! Work from this Server's local socket, and the daemon's own, is bound to no key and ends
//! only on its own.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use ployz_core::{ClientKey, ManagementScope};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::{Caller, Error, LocalMachine};
use crate::machine::LocalMachineRecord;

/// How long a key change waits for the work it ended to stop. Longer than the slowest stop
/// any work bounds itself by, a Build's 70 s termination wait, so the deadline catches only
/// work that bound failed.
pub(crate) const STOP_WITHIN: Duration = Duration::from_secs(90);

/// The work bound to management keys on this Machine, which every clone of it shares.
#[derive(Clone)]
pub(super) struct BoundWork {
    open: Arc<Mutex<Open>>,
    records: watch::Receiver<Arc<LocalMachineRecord>>,
}

#[derive(Default)]
struct Open {
    next: u64,
    work: HashMap<u64, Entry>,
}

/// One piece of work still bound to `key`.
struct Entry {
    key: ClientKey,
    work: Work,
    end: CancellationToken,
    /// Closed once every [`Bound`] of the work has dropped.
    running: watch::Sender<()>,
    unconfirmed: Unconfirmed,
}

/// Why the work could not confirm it stopped, as its [`Bound`]s report it.
type Unconfirmed = Arc<Mutex<Vec<StopCause>>>;

/// Which work a key started, as a key change names it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Work {
    Exec,
    Tunnel,
    Build,
    BuildGrant,
}

impl std::fmt::Display for Work {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Exec => "an exec",
            Self::Tunnel => "a port-forward tunnel",
            Self::Build => "a Build",
            Self::BuildGrant => "a Build Grant",
        })
    }
}

/// Work running for the key that started it, which a key change that ends it waits for: the
/// work keeps a `Bound` until it has stopped acting for that key. Reads a secret only
/// through [`Self::read`].
#[must_use]
pub struct Bound {
    tie: Tie,
    /// `None` for work no management key started.
    running: Option<watch::Receiver<()>>,
}

/// What ties work to the key that started it, holding no key change up: whether a key change
/// ended it. [`Self::hold`] makes another [`Bound`] of it while none has.
#[derive(Clone)]
pub struct Tie(Arc<Binding>);

struct Binding {
    end: CancellationToken,
    /// `None` for work no management key started.
    key: Option<Keyed>,
}

struct Keyed {
    work: BoundWork,
    id: u64,
    key: ClientKey,
    unconfirmed: Unconfirmed,
}

impl Drop for Binding {
    fn drop(&mut self) {
        if let Some(keyed) = &self.key {
            keyed
                .work
                .open
                .lock()
                .expect("bound work lock")
                .work
                .remove(&keyed.id);
        }
    }
}

/// Why a piece of work did not confirm it stopped.
#[derive(Debug, thiserror::Error)]
pub enum StopCause {
    #[error("it still ran {} s after the key change ended it", STOP_WITHIN.as_secs())]
    Deadline,
    #[error("its task failed")]
    Panicked,
    /// The work's own account of why it can't confirm it stopped.
    #[error(transparent)]
    Reported(Box<dyn std::error::Error + Send + Sync>),
}

/// A key change was written, but work a key it left short of Full scope started did not
/// confirm it stopped: `work` first, for `cause`, and `others` more.
#[derive(Debug, thiserror::Error)]
#[error(
    "This Server made the key change, but {work} the key started did not confirm it stopped{}",
    more(*.others)
)]
pub struct WorkNotStopped {
    work: Work,
    #[source]
    cause: StopCause,
    others: usize,
}

fn more(others: usize) -> String {
    match others {
        0 => String::new(),
        others => format!(", nor did {others} more"),
    }
}

impl WorkNotStopped {
    /// The first work that did not confirm it stopped, and why.
    #[cfg(test)]
    pub(crate) fn first(&self) -> (Work, &StopCause) {
        (self.work, &self.cause)
    }
}

impl BoundWork {
    pub(super) fn new(records: watch::Receiver<Arc<LocalMachineRecord>>) -> Self {
        Self {
            open: Arc::default(),
            records,
        }
    }

    /// Bind new `work` to `key`, checked under the lock [`Self::end_unheld`] takes once a key
    /// change wrote its record, so the change either sees this work or refuses it here.
    fn bind(&self, key: ClientKey, work: Work) -> Result<Bound, Error> {
        let mut open = self.open.lock().expect("bound work lock");
        holds_full(&self.records.borrow(), &key)?;
        let id = open.next;
        open.next += 1;
        let end = CancellationToken::new();
        let (running, held) = watch::channel(());
        let unconfirmed = Unconfirmed::default();
        open.work.insert(
            id,
            Entry {
                key,
                work,
                end: end.clone(),
                running,
                unconfirmed: Arc::clone(&unconfirmed),
            },
        );
        Ok(Bound {
            tie: Tie(Arc::new(Binding {
                end,
                key: Some(Keyed {
                    work: self.clone(),
                    id,
                    key,
                    unconfirmed,
                }),
            })),
            running: Some(held),
        })
    }

    /// End the work whose key no longer holds Full scope as the keys stand, and wait, at most
    /// [`STOP_WITHIN`] in all, until each piece has stopped. A key change calls this once its
    /// record is written.
    ///
    /// # Errors
    /// Every piece that did not stop in time, or stopped without confirming it.
    pub(super) async fn end_unheld(&self) -> Result<(), WorkNotStopped> {
        let ending: Vec<Entry> = {
            let mut open = self.open.lock().expect("bound work lock");
            let record = self.records.borrow().clone();
            open.work
                .extract_if(|_, entry| holds_full(&record, &entry.key).is_err())
                .map(|(_, entry)| {
                    entry.end.cancel();
                    entry
                })
                .collect()
        };
        let deadline = tokio::time::Instant::now() + STOP_WITHIN;
        let mut failures = Vec::new();
        for entry in ending {
            let stopped = tokio::time::timeout_at(deadline, entry.running.closed())
                .await
                .is_ok();
            let mut causes = std::mem::take(&mut *entry.unconfirmed.lock().expect("stop lock"));
            if !stopped {
                causes.push(StopCause::Deadline);
            }
            failures.extend(causes.into_iter().map(|cause| (entry.work, cause)));
        }
        let mut failures = failures.into_iter();
        match failures.next() {
            None => Ok(()),
            Some((work, cause)) => Err(WorkNotStopped {
                work,
                cause,
                others: failures.len(),
            }),
        }
    }
}

/// Whether `key` is a slot's accepted key at Full scope in `record`.
fn holds_full(record: &LocalMachineRecord, key: &ClientKey) -> Result<(), Error> {
    if !record.accepts_management_client(key) {
        return Err(Error::ManagementRevoked);
    }
    match record.management_scope(key) {
        Some(ManagementScope::Full) => Ok(()),
        Some(ManagementScope::Operator) | None => Err(Error::NeedsFullScope),
    }
}

impl Bound {
    fn unbound() -> Self {
        Self {
            tie: Tie(Arc::new(Binding {
                end: CancellationToken::new(),
                key: None,
            })),
            running: None,
        }
    }

    #[must_use]
    pub fn tie(&self) -> &Tie {
        &self.tie
    }

    /// The tie alone, for work a key change ends without waiting for it.
    #[must_use]
    pub fn into_tie(self) -> Tie {
        self.tie.clone()
    }

    /// Whether a key change can end this work: a management key started it.
    #[must_use]
    pub fn may_end(&self) -> bool {
        self.tie.0.key.is_some()
    }

    /// Completes once a key change ended the work. Pending forever for work no key started.
    pub async fn ended(&self) {
        self.tie.ended().await;
    }

    /// Run `read` while the work's key holds Full scope as the keys stand and no key change
    /// has ended the work, under the lock a key change ends work under. The key change waits
    /// for this `Bound`, so it returns only once the read is done.
    ///
    /// # Errors
    /// Returns [`Error::ManagementRevoked`] or [`Error::NeedsFullScope`] when the key no
    /// longer holds Full scope, and the latter when a key change ended the work.
    pub fn read<T>(&self, read: impl FnOnce() -> T) -> Result<T, Error> {
        let Some(keyed) = &self.tie.0.key else {
            return Ok(read());
        };
        let _open = keyed.work.open.lock().expect("bound work lock");
        holds_full(&keyed.work.records.borrow(), &keyed.key)?;
        if self.tie.is_ended() {
            return Err(Error::NeedsFullScope);
        }
        Ok(read())
    }

    /// Stop holding the work up, telling the key change that ended it why it can't confirm
    /// the work stopped.
    pub fn unconfirmed(self, cause: impl std::error::Error + Send + Sync + 'static) {
        self.report(StopCause::Reported(Box::new(cause)));
    }

    fn report(&self, cause: StopCause) {
        if let Some(keyed) = &self.tie.0.key {
            keyed.unconfirmed.lock().expect("stop lock").push(cause);
        }
    }
}

#[cfg(test)]
impl Bound {
    /// Work the daemon itself started, for a test that has no Machine to ask.
    #[must_use]
    pub fn for_test() -> Self {
        Self::unbound()
    }
}

impl Drop for Bound {
    fn drop(&mut self) {
        // A task that panicked never reached the point where its work confirmed it stopped.
        if std::thread::panicking() && self.running.is_some() {
            self.report(StopCause::Panicked);
        }
    }
}

impl Tie {
    /// Completes once a key change ended the work. Pending forever for work no key started.
    pub async fn ended(&self) {
        self.0.end.cancelled().await;
    }

    #[must_use]
    pub fn is_ended(&self) -> bool {
        self.0.end.is_cancelled()
    }

    /// Another [`Bound`] of this work, which a key change that ends it also waits for.
    ///
    /// # Errors
    /// Returns [`Error::NeedsFullScope`] once a key change ended the work, and as
    /// [`Bound::read`] when the key no longer holds Full scope.
    pub fn hold(&self) -> Result<Bound, Error> {
        let Some(keyed) = &self.0.key else {
            return Ok(Bound {
                tie: self.clone(),
                running: None,
            });
        };
        let open = keyed.work.open.lock().expect("bound work lock");
        holds_full(&keyed.work.records.borrow(), &keyed.key)?;
        let entry = open
            .work
            .get(&keyed.id)
            .filter(|_| !self.is_ended())
            .ok_or(Error::NeedsFullScope)?;
        Ok(Bound {
            tie: self.clone(),
            running: Some(entry.running.subscribe()),
        })
    }
}

impl LocalMachine {
    /// Bind `work` that outlives this call to the key that asked for it, at Full scope. A
    /// key change that leaves the key short of Full ends the work before it returns.
    ///
    /// # Errors
    /// Returns [`Error::NeedsFullScope`] for an Operator's call, and
    /// [`Error::ManagementRevoked`] when this Machine no longer accepts the caller's key.
    pub fn bind_full(&self, work: Work) -> Result<Bound, Error> {
        let full = self.scope_now()?.full()?;
        match full.caller() {
            Caller::Management(key) => self.bound.bind(key, work),
            Caller::LocalSocket | Caller::Routed => Ok(Bound::unbound()),
        }
    }
}
