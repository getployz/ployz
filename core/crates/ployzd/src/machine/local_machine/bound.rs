//! Work at Full scope that outlives the call that started it: an exec, a port-forward
//! tunnel, a Build, a Build Grant. Each is bound to the management key that asked for it.
//! Every key change, once its record is written and before it returns, ends the work whose
//! key no longer holds Full scope, through [`LocalMachine::finish_key_change`], and waits at
//! most [`STOP_WITHIN`] for it to stop.
//!
//! The registry forgets work only on evidence that it ended: a [`Bound`] goes through
//! [`Bound::stopped`] with what its holder last [`Seen`] of the work, and only a sighting
//! [`Seen::ended`] calls evidence (an exit code, a reaped process, a joined task, a confirmed
//! Container removal) lets it go clean. Absence (not running with no exit code, a 404, a
//! busy lock, a failed read) and any other way a Bound goes leave a doubt. Work that still
//! runs or left a doubt stays, past the key change that ended it. Every key change that
//! finds its key short of Full reports it by name, until a [`Recheck`] sees evidence it
//! ended. [`ployz_decide::bound_work`] holds the rules; `ployz-verify`'s `bound_work` model
//! explores them against key changes and false reports of an end.
//!
//! A secret is read, and an effect starts, for bound work only under the lock a key change
//! ends work under: through [`Bound::read`], or by taking a fresh [`Bound`] with
//! [`Tie::hold`] right before the effect. A key change waits for every Bound, so nothing
//! reads or starts once it has returned.
//!
//! Work from this Server's local socket, and the daemon's own, is bound to no key and ends
//! only on its own.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::future::BoxFuture;
use ployz_core::{ClientKey, ManagementScope};
use ployz_decide::bound_work::Entry;
pub use ployz_decide::bound_work::Seen;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::{Caller, Error, LocalMachine};
use crate::machine::LocalMachineRecord;

/// How long a key change waits for the work it ended to stop. Longer than the slowest stop
/// any work bounds itself by, a Build's 70 s termination wait, so the deadline catches only
/// work that bound failed.
pub(crate) const STOP_WITHIN: Duration = Duration::from_secs(90);

/// A later look at work that went without evidence it ended, which a key change runs: what
/// it saw, which clears the doubt only when [`Seen::ended`], or why it could not look.
pub type Recheck = Arc<dyn Fn() -> BoxFuture<'static, Result<Seen, Cause>> + Send + Sync>;

/// Why work could not confirm it stopped, in its own words.
pub type Cause = Box<dyn std::error::Error + Send + Sync>;

/// The work bound to management keys on this Machine, which every clone of it shares.
#[derive(Clone)]
pub(super) struct BoundWork {
    open: Arc<Mutex<Open>>,
    records: watch::Receiver<Arc<LocalMachineRecord>>,
}

#[derive(Default)]
struct Open {
    next: u64,
    work: HashMap<u64, Piece>,
}

/// One piece of work bound to `key`, kept until it is settled.
struct Piece {
    key: ClientKey,
    work: Work,
    /// What names this piece to a person, once the work knows it.
    name: Option<String>,
    end: CancellationToken,
    /// How many Bounds hold the work, which a key change waits on.
    holders: Arc<watch::Sender<u32>>,
    facts: Entry<Doubt>,
}

/// A Bound that went without evidence its work ended.
struct Doubt {
    id: u64,
    cause: Arc<StopCause>,
    recheck: Option<Recheck>,
}

impl Piece {
    fn named(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.work.to_string())
    }

    fn publish(&self) {
        self.holders.send_replace(self.facts.holders());
    }
}

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

/// Work running for the key that started it, which a key change that ends it waits for. It
/// goes through [`Self::stopped`] once its holder saw the work end, or
/// [`Self::unconfirmed`]; dropped any other way, it leaves a doubt the registry keeps.
#[must_use = "a Bound goes through `stopped` or `unconfirmed`; dropped, it leaves a doubt"]
pub struct Bound {
    tie: Tie,
    released: bool,
}

/// What ties work to the key that started it, holding no key change up: whether a key change
/// ended it. [`Self::hold`] takes another [`Bound`] of it.
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
}

impl Drop for Binding {
    fn drop(&mut self) {
        if let Some(keyed) = &self.key {
            keyed.work.settle(keyed.id, Entry::untie);
        }
    }
}

/// Why a piece of work did not confirm it stopped.
#[derive(Debug, thiserror::Error)]
pub enum StopCause {
    #[error("it still ran {} s after a key change ended it", STOP_WITHIN.as_secs())]
    Deadline,
    #[error("its task failed")]
    Panicked,
    #[error("its task went without seeing it end")]
    Dropped,
    #[error("{}", unseen(*.0))]
    Unseen(Seen),
    #[error("it landed after the key change that ended it")]
    Landed,
    /// The work's own account of why it can't confirm it stopped.
    #[error(transparent)]
    Reported(Cause),
}

/// A key change was written, but work a key without Full scope started did not confirm it
/// stopped: every piece by name, and why the first did not.
#[derive(Debug, thiserror::Error)]
#[error(
    "This Server made the key change, but work a key without Full access started did not confirm it stopped: {}",
    names(.pieces)
)]
pub struct WorkNotStopped {
    pieces: Vec<(Work, String)>,
    #[source]
    cause: Arc<StopCause>,
}

/// Why a sighting that is no evidence the work ended leaves it in doubt.
fn unseen(seen: Seen) -> &'static str {
    match seen {
        Seen::Pending => {
            "it was reported not running with no exit code, or not stored yet, so it may not have started"
        }
        Seen::Forgotten => {
            "the record of it was lost, as Docker forgets every exec when it restarts"
        }
        Seen::Unchecked => "the look at it failed",
        Seen::Busy => "another holder had the lock a look at it takes",
        Seen::ExitCode
        | Seen::Reaped
        | Seen::Joined
        | Seen::ContainerGone
        | Seen::NeverSent
        | Seen::Answered
        | Seen::Landed => "it ended",
    }
}

fn names(pieces: &[(Work, String)]) -> String {
    pieces
        .iter()
        .map(|(_, name)| name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

impl WorkNotStopped {
    /// Each piece that did not confirm it stopped.
    #[cfg(test)]
    pub(crate) fn works(&self) -> Vec<Work> {
        self.pieces.iter().map(|(work, _)| *work).collect()
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
        open.work.insert(
            id,
            Piece {
                key,
                work,
                name: None,
                end: end.clone(),
                holders: Arc::new(watch::Sender::new(1)),
                facts: Entry::bound(),
            },
        );
        Ok(Bound {
            tie: Tie(Arc::new(Binding {
                end,
                key: Some(Keyed {
                    work: self.clone(),
                    id,
                    key,
                }),
            })),
            released: false,
        })
    }

    /// Change piece `id`'s facts with `change` under the lock, and forget it once settled.
    fn settle(&self, id: u64, change: impl FnOnce(&mut Entry<Doubt>)) {
        let mut open = self.open.lock().expect("bound work lock");
        let Some(piece) = open.work.get_mut(&id) else {
            return;
        };
        change(&mut piece.facts);
        piece.publish();
        if piece.facts.settled() {
            open.work.remove(&id);
        }
    }

    /// End the work whose key no longer holds Full scope as the keys stand, wait, at most
    /// [`STOP_WITHIN`] in all, until each piece it ends has stopped, and look again at every
    /// doubt such work left before. A key change calls this once its record is written.
    ///
    /// # Errors
    /// Every piece of such work, ended now or by an earlier key change, that still runs or
    /// left a doubt no look cleared.
    pub(super) async fn end_unheld(&self) -> Result<(), WorkNotStopped> {
        let deadline = tokio::time::Instant::now() + STOP_WITHIN;
        let (mut waits, looks) = {
            let mut open = self.open.lock().expect("bound work lock");
            let record = self.records.borrow().clone();
            let mut waits = Vec::new();
            let mut looks = Vec::new();
            for (id, piece) in &mut open.work {
                if holds_full(&record, &piece.key).is_ok() {
                    continue;
                }
                if piece.facts.end() {
                    piece.end.cancel();
                    waits.push(piece.holders.subscribe());
                }
                looks.extend(piece.facts.doubts().iter().filter_map(|doubt| {
                    let recheck = doubt.recheck.clone()?;
                    Some((*id, doubt.id, recheck))
                }));
            }
            (waits, looks)
        };
        for holders in &mut waits {
            let _ = tokio::time::timeout_at(deadline, holders.wait_for(|held| *held == 0)).await;
        }
        let looked = futures_util::future::join_all(looks.into_iter().map(
            |(piece, doubt, recheck)| async move {
                let seen = tokio::time::timeout_at(deadline, recheck())
                    .await
                    .unwrap_or_else(|_| Err(StopCause::Deadline.into()));
                (piece, doubt, seen)
            },
        ))
        .await;
        let mut open = self.open.lock().expect("bound work lock");
        let mut pieces = Vec::new();
        let mut landed = Vec::new();
        let mut cause = None;
        for (id, doubt, seen) in looked {
            let Some(piece) = open.work.get_mut(&id) else {
                continue;
            };
            let unseen = match seen {
                Ok(seen) if piece.facts.confirm(seen, |held| held.id == doubt) => {
                    // Its effect landed after the key change that ended it: this change
                    // reports it once, by name.
                    if seen == Seen::Landed && !landed.contains(&id) {
                        landed.push(id);
                        pieces.push((piece.work, piece.named()));
                        cause.get_or_insert_with(|| Arc::new(StopCause::Landed));
                    }
                    None
                }
                Ok(seen) => Some(StopCause::Unseen(seen)),
                Err(cause) => Some(StopCause::Reported(cause)),
            };
            // The latest look says best why the work still can't confirm it ended.
            if let Some(unseen) = unseen
                && let Some(held) = piece
                    .facts
                    .doubts_mut()
                    .iter_mut()
                    .find(|held| held.id == doubt)
            {
                held.cause = Arc::new(unseen);
            }
            piece.publish();
        }
        let record = self.records.borrow().clone();
        open.work.retain(|id, piece| {
            if piece.facts.settled() {
                return false;
            }
            if holds_full(&record, &piece.key).is_err()
                && piece.facts.unconfirmed()
                && !landed.contains(id)
            {
                pieces.push((piece.work, piece.named()));
                cause.get_or_insert_with(|| match piece.facts.doubts().first() {
                    Some(doubt) => Arc::clone(&doubt.cause),
                    None => Arc::new(StopCause::Deadline),
                });
            }
            true
        });
        match cause {
            None => Ok(()),
            Some(cause) => Err(WorkNotStopped { pieces, cause }),
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
            released: false,
        }
    }

    #[must_use]
    pub fn tie(&self) -> &Tie {
        &self.tie
    }

    /// The tie alone, once this Bound's own part of the work, such as a read, returned.
    #[must_use]
    pub fn into_tie(self) -> Tie {
        let tie = self.tie.clone();
        self.stopped(Seen::Joined);
        tie
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

    /// Name this piece of work to a person, as a key change reports it: `exec 3f2a…`.
    pub fn name(&self, name: impl Into<String>) {
        if let Some(keyed) = &self.tie.0.key
            && let Some(piece) = keyed
                .work
                .open
                .lock()
                .expect("bound work lock")
                .work
                .get_mut(&keyed.id)
        {
            piece.name = Some(name.into());
        }
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

    /// Stop holding the work up, its holder having last seen `seen` of it. Unless that is
    /// evidence the work ended ([`Seen::ended`]), it stays in doubt with no recheck, and a
    /// key change that finds its key short of Full reports it.
    pub fn stopped(mut self, seen: Seen) {
        self.release(seen, StopCause::Unseen(seen), None);
    }

    /// Stop holding the work up without having seen it end, for `cause`. A key change that
    /// finds its key short of Full runs `recheck` until it sees the work end, and reports
    /// it until then.
    pub fn unconfirmed(
        mut self,
        cause: impl std::error::Error + Send + Sync + 'static,
        recheck: Option<Recheck>,
    ) {
        self.release(
            Seen::Unchecked,
            StopCause::Reported(Box::new(cause)),
            recheck,
        );
    }

    /// Let this Bound go, its holder having last seen `seen`, leaving a doubt for `cause`
    /// unless that is evidence the work ended.
    fn release(&mut self, seen: Seen, cause: StopCause, recheck: Option<Recheck>) {
        if std::mem::replace(&mut self.released, true) {
            return;
        }
        let Some(keyed) = &self.tie.0.key else {
            return;
        };
        let mut open = keyed.work.open.lock().expect("bound work lock");
        open.next += 1;
        let doubt = Doubt {
            id: open.next,
            cause: Arc::new(cause),
            recheck,
        };
        let Some(piece) = open.work.get_mut(&keyed.id) else {
            return;
        };
        piece.facts.release(seen, doubt);
        piece.publish();
        if piece.facts.settled() {
            open.work.remove(&keyed.id);
        }
    }
}

#[cfg(test)]
impl Bound {
    /// Work the daemon itself started, for a test that has no Machine to ask.
    pub fn for_test() -> Self {
        Self::unbound()
    }
}

impl Drop for Bound {
    fn drop(&mut self) {
        if !self.released {
            let cause = if std::thread::panicking() {
                StopCause::Panicked
            } else {
                StopCause::Dropped
            };
            self.release(Seen::Unchecked, cause, None);
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

    /// Another [`Bound`] of this work, which a key change that ends it also waits for: taken
    /// under the lock a key change ends work under, so it is the check an effect makes right
    /// before it starts.
    ///
    /// # Errors
    /// Returns [`Error::NeedsFullScope`] once a key change ended the work, and as
    /// [`Bound::read`] when the key no longer holds Full scope.
    pub fn hold(&self) -> Result<Bound, Error> {
        let Some(keyed) = &self.0.key else {
            return Ok(Bound {
                tie: self.clone(),
                released: false,
            });
        };
        let mut open = keyed.work.open.lock().expect("bound work lock");
        holds_full(&keyed.work.records.borrow(), &keyed.key)?;
        let piece = open.work.get_mut(&keyed.id).ok_or(Error::NeedsFullScope)?;
        if !piece.facts.hold() {
            return Err(Error::NeedsFullScope);
        }
        piece.publish();
        Ok(Bound {
            tie: self.clone(),
            released: false,
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

#[cfg(test)]
impl BoundWork {
    /// Whether a thread holds the registry's lock.
    fn is_locked(&self) -> bool {
        matches!(
            self.open.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Notes, once dropped, whether the registry's lock was held then. A destructor that
    /// takes the lock, as a Tie's does, would wait there forever on its own thread.
    struct Probe {
        work: BoundWork,
        under_lock: Arc<Mutex<Vec<&'static str>>>,
        what: &'static str,
    }

    impl Drop for Probe {
        fn drop(&mut self) {
            if self.work.is_locked() {
                self.under_lock.lock().unwrap().push(self.what);
            }
        }
    }

    impl std::fmt::Debug for Probe {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.what)
        }
    }

    /// A cause that owns a [`Probe`].
    #[derive(Debug, thiserror::Error)]
    #[error("probed")]
    struct Probed(Probe);

    /// A doubt's recheck can own the last Tie of its own work, as a Build Grant's commit
    /// owns the grant, and that Tie's drop takes the registry's lock. A key change that
    /// clears a doubt, or replaces why one stands, drops what it took out only once it let
    /// that lock go.
    #[tokio::test]
    async fn a_key_change_drops_what_it_takes_out_of_the_registry_after_its_lock() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = crate::machine::test_support::founder(dir.path(), "probe");
        let device = crate::machine::test_support::device_in_two_slots(&mut store);
        let local = LocalMachine::new(crate::machine::RecordOwner::spawn(store).unwrap());
        let from_device = local.clone().with_management_client(device.into());
        let under_lock = Arc::new(Mutex::new(Vec::new()));
        let probe = |what| Probe {
            work: local.bound.clone(),
            under_lock: Arc::clone(&under_lock),
            what,
        };

        // Its look sees the work end, which clears the doubt.
        let owned = probe("a cleared doubt's recheck");
        from_device
            .bind_full(Work::BuildGrant)
            .unwrap()
            .unconfirmed(
                Probed(probe("a cleared doubt's cause")),
                Some(Arc::new(move || {
                    let _owned = &owned;
                    Box::pin(async { Ok(Seen::Reaped) })
                })),
            );
        // Its look fails, so a new cause replaces the old.
        from_device.bind_full(Work::Exec).unwrap().unconfirmed(
            Probed(probe("a replaced cause")),
            Some(Arc::new(|| {
                Box::pin(async { Err::<Seen, Cause>("still unread".into()) })
            })),
        );

        let removed = local
            .on_local_socket()
            .set_device(ployz_core::SetDeviceRequest::Remove {
                code: device.code(),
            })
            .await;

        assert!(
            matches!(removed, Err(Error::WorkNotStopped(_))),
            "{removed:?}"
        );
        assert_eq!(
            *under_lock.lock().unwrap(),
            Vec::<&str>::new(),
            "dropped while the registry's lock was held"
        );
    }
}
