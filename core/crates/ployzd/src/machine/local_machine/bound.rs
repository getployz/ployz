//! Work at Full scope that outlives the call that started it: an Exec, a port-forward
//! tunnel, a Build, a Build Grant. Each is bound to the management key that asked for it.
//! Every key change, once its record is written and before it returns, ends the work whose
//! key no longer holds Full scope and waits until that work stops, through
//! [`LocalMachine::finish_key_change`]. A read of a secret for the work checks the key again
//! under the lock the key change ends work under, so none starts once it has returned.
//!
//! Work from this Server's local socket, and the daemon's own, is bound to no key and ends
//! only on its own.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use ployz_core::{ClientKey, ManagementScope};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::{Caller, Error, LocalMachine};
use crate::machine::LocalMachineRecord;

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
    end: CancellationToken,
    /// Closed once every [`Bound`] of the work has dropped.
    running: watch::Sender<()>,
}

/// Work that runs bound to the key that started it. A key change that ends the work waits
/// until this drops, so the work keeps it until it has stopped acting for that key.
#[must_use]
pub struct Bound {
    tie: Tie,
    _running: Option<watch::Receiver<()>>,
}

/// What ties work to the key that started it, holding no key change up: whether a key change
/// ended it, and reads that run only while none has.
#[derive(Clone)]
pub struct Tie(Arc<Binding>);

struct Binding {
    end: CancellationToken,
    /// `None` for work no management key started.
    key: Option<(BoundWork, u64, ClientKey)>,
}

impl Drop for Binding {
    fn drop(&mut self) {
        if let Some((work, id, _)) = &self.key {
            work.open.lock().expect("bound work lock").work.remove(id);
        }
    }
}

impl BoundWork {
    pub(super) fn new(records: watch::Receiver<Arc<LocalMachineRecord>>) -> Self {
        Self {
            open: Arc::default(),
            records,
        }
    }

    /// Bind new work to `key`, checked under the lock [`Self::end_unheld`] takes once a key
    /// change wrote its record, so the change either sees this work or refuses it here.
    fn bind(&self, key: ClientKey) -> Result<Bound, Error> {
        let mut open = self.open.lock().expect("bound work lock");
        holds_full(&self.records.borrow(), &key)?;
        let id = open.next;
        open.next += 1;
        let end = CancellationToken::new();
        let (running, held) = watch::channel(());
        open.work.insert(
            id,
            Entry {
                key,
                end: end.clone(),
                running,
            },
        );
        Ok(Bound {
            tie: Tie(Arc::new(Binding {
                end,
                key: Some((self.clone(), id, key)),
            })),
            _running: Some(held),
        })
    }

    /// End the work whose key no longer holds Full scope as the keys stand, and wait until
    /// each piece has stopped. A key change calls this once its record is written.
    pub(super) async fn end_unheld(&self) {
        let stopping: Vec<watch::Sender<()>> = {
            let mut open = self.open.lock().expect("bound work lock");
            let record = self.records.borrow().clone();
            open.work
                .extract_if(|_, entry| holds_full(&record, &entry.key).is_err())
                .map(|(_, entry)| {
                    entry.end.cancel();
                    entry.running
                })
                .collect()
        };
        for running in stopping {
            running.closed().await;
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
            _running: None,
        }
    }

    #[must_use]
    pub fn tie(&self) -> &Tie {
        &self.tie
    }

    /// The tie alone, for work a key change ends without waiting for it.
    #[must_use]
    pub fn into_tie(self) -> Tie {
        self.tie
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

    /// Run `read` while the work's key holds Full scope as the keys stand, under the lock a
    /// key change ends work under: once that change has returned, no read starts.
    ///
    /// # Errors
    /// Returns [`Error::ManagementRevoked`] or [`Error::NeedsFullScope`] when the key no
    /// longer holds Full scope, and the latter when a key change ended the work.
    pub fn read<T>(&self, read: impl FnOnce() -> T) -> Result<T, Error> {
        let Some((work, _, key)) = &self.0.key else {
            return Ok(read());
        };
        let _open = work.open.lock().expect("bound work lock");
        holds_full(&work.records.borrow(), key)?;
        if self.is_ended() {
            return Err(Error::NeedsFullScope);
        }
        Ok(read())
    }

    /// The tie of work the daemon itself started, for a test that has no Machine to ask.
    #[cfg(test)]
    #[must_use]
    pub fn for_test() -> Self {
        Bound::unbound().into_tie()
    }
}

impl LocalMachine {
    /// Bind work that outlives this call to the key that asked for it, at Full scope. A
    /// key change that leaves the key short of Full ends the work before it returns.
    ///
    /// # Errors
    /// Returns [`Error::NeedsFullScope`] for an Operator's call, and
    /// [`Error::ManagementRevoked`] when this Machine no longer accepts the caller's key.
    pub fn bind_full(&self) -> Result<Bound, Error> {
        let full = self.scope_now()?.full()?;
        match full.caller() {
            Caller::Management(key) => self.bound.bind(key),
            Caller::LocalSocket | Caller::Routed => Ok(Bound::unbound()),
        }
    }
}
