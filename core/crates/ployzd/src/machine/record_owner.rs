//! Single owner of the Local Machine record.
//!
//! One dedicated thread owns the [`LocalMachineStore`]. Mutations queue through its
//! mailbox and run one at a time; every applied mutation publishes the resulting
//! record on a watch before its reply is sent. Readers borrow the published record
//! and never take the store lock, so a read cannot block on a save, cannot be
//! poisoned, and always sees a record that a completed save produced.
//!
//! Record saves do blocking file I/O. Running them on the owner thread keeps that
//! I/O off the async runtime.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};

use thiserror::Error;
use tokio::sync::{oneshot, watch};

use super::{LocalMachineRecord, LocalMachineStore};
use crate::mutation::MutationGate;

/// A mutation the owner thread applies to the store it owns.
type Mutation =
    Box<dyn FnOnce(&mut LocalMachineStore, &watch::Sender<Arc<LocalMachineRecord>>) + Send>;

enum Message {
    Mutate(Mutation),
    /// Drop the store, releasing the data directory, then confirm and stop.
    Close(oneshot::Sender<()>),
}

/// The owner thread has stopped, so no further mutation can be applied.
///
/// The thread stops when a mutation panics, when [`close`](RecordOwner::close) is
/// called, or when the last handle is dropped. Reads keep serving the last
/// published record, and [`watch`](RecordOwner::watch) receivers close, which is
/// how the daemon notices a panicked owner and shuts down.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("local Machine record owner stopped")]
pub struct RecordOwnerStopped;

/// Handle to the thread that owns this Machine's durable record.
///
/// Cloning shares the owner. Dropping the last clone stops the thread and waits
/// for it, so the data directory is released before the drop returns.
pub struct RecordOwner {
    mailbox: mpsc::Sender<Message>,
    record: watch::Receiver<Arc<LocalMachineRecord>>,
    restart: watch::Sender<bool>,
    shared: Arc<Shared>,
}

/// Values fixed for the lifetime of the store, readable without a mutation.
struct Shared {
    data_dir: std::path::PathBuf,
    run_dir: std::path::PathBuf,
    admission_lock: Arc<tokio::sync::Mutex<()>>,
    mutation_gate: MutationGate,
    thread: Mutex<Option<thread::JoinHandle<()>>>,
    /// Live handles. Counted explicitly so exactly one drop observes being last.
    handles: AtomicUsize,
}

impl RecordOwner {
    /// Take ownership of an opened store on a dedicated thread.
    ///
    /// # Errors
    ///
    /// Returns the OS error when the owner thread cannot be spawned.
    pub fn spawn(store: LocalMachineStore) -> std::io::Result<Self> {
        let (mailbox, messages) = mpsc::channel::<Message>();
        let (published, record) = watch::channel(Arc::new(store.record().clone()));
        let (restart, _) = watch::channel(false);
        let data_dir = store.data_dir.clone();
        let run_dir = store.run_dir.clone();
        let admission_lock = Arc::clone(&store.admission_lock);
        let mutation_gate = store.mutation_gate.clone();
        let thread = thread::Builder::new()
            .name("ployzd-local-machine-record".into())
            .spawn(move || {
                let mut store = store;
                // The iterator ends once every handle has dropped its sender.
                for message in messages {
                    let mutation = match message {
                        Message::Mutate(mutation) => mutation,
                        Message::Close(closed) => {
                            drop(store);
                            let _ = closed.send(());
                            return;
                        }
                    };
                    if catch_unwind(AssertUnwindSafe(|| mutation(&mut store, &published))).is_err()
                    {
                        // The store's state is unknown, so refuse every later mutation by
                        // dropping the mailbox. Leak the store so the data directory stays
                        // claimed for the rest of this process, as a poisoned lock kept it.
                        tracing::error!(
                            "local Machine record mutation panicked; refusing further mutations"
                        );
                        std::mem::forget(store);
                        return;
                    }
                }
            })?;
        Ok(Self {
            mailbox,
            record,
            restart,
            shared: Arc::new(Shared {
                data_dir,
                run_dir,
                admission_lock,
                mutation_gate,
                thread: Mutex::new(Some(thread)),
                handles: AtomicUsize::new(1),
            }),
        })
    }

    /// Release the data directory now, whatever other handles still exist.
    ///
    /// Returns once the store is dropped. Later mutations from any handle fail with
    /// [`RecordOwnerStopped`]; reads keep the last published record.
    ///
    /// # Errors
    ///
    /// Returns [`RecordOwnerStopped`] when the owner already stopped, in which case a
    /// panicked mutation may have left the data directory claimed.
    pub async fn close(&self) -> Result<(), RecordOwnerStopped> {
        let (closed, confirmation) = oneshot::channel();
        self.mailbox
            .send(Message::Close(closed))
            .map_err(|_| RecordOwnerStopped)?;
        confirmation.await.map_err(|_| RecordOwnerStopped)
    }

    /// The last published record. Never blocks on a save.
    #[must_use]
    pub fn record(&self) -> Arc<LocalMachineRecord> {
        Arc::clone(&self.record.borrow())
    }

    /// Every record the owner publishes, for callers that wait on a phase change.
    ///
    /// The receiver closes when the owner stops.
    #[must_use]
    pub fn watch(&self) -> watch::Receiver<Arc<LocalMachineRecord>> {
        self.record.clone()
    }

    /// Apply one mutation to the store and return what it produced.
    ///
    /// Mutations run in arrival order. The record the mutation left behind is
    /// published before this returns, so a following [`record`](Self::record)
    /// observes it.
    ///
    /// # Errors
    ///
    /// Returns [`RecordOwnerStopped`] when the owner thread has stopped.
    pub async fn mutate<R>(
        &self,
        mutation: impl FnOnce(&mut LocalMachineStore) -> R + Send + 'static,
    ) -> Result<R, RecordOwnerStopped>
    where
        R: Send + 'static,
    {
        let reply = self.enqueue(mutation)?;
        reply.await.map_err(|_| RecordOwnerStopped)
    }

    /// [`mutate`](Self::mutate) for callers that are not on the async runtime.
    ///
    /// Blocks the calling thread until the mutation has been applied. Calling this
    /// from an async task panics, as any blocking wait on the runtime would.
    ///
    /// # Errors
    ///
    /// Returns [`RecordOwnerStopped`] when the owner thread has stopped.
    pub fn mutate_blocking<R>(
        &self,
        mutation: impl FnOnce(&mut LocalMachineStore) -> R + Send + 'static,
    ) -> Result<R, RecordOwnerStopped>
    where
        R: Send + 'static,
    {
        let reply = self.enqueue(mutation)?;
        reply.blocking_recv().map_err(|_| RecordOwnerStopped)
    }

    fn enqueue<R>(
        &self,
        mutation: impl FnOnce(&mut LocalMachineStore) -> R + Send + 'static,
    ) -> Result<oneshot::Receiver<R>, RecordOwnerStopped>
    where
        R: Send + 'static,
    {
        let (reply, receiver) = oneshot::channel();
        self.mailbox
            .send(Message::Mutate(Box::new(move |store, published| {
                let produced = mutation(store);
                publish(published, store.record());
                // A caller that stopped waiting does not need its reply.
                let _ = reply.send(produced);
            })))
            .map_err(|_| RecordOwnerStopped)?;
        Ok(receiver)
    }

    /// Ask the daemon to restart so the persisted phase takes effect.
    pub fn request_restart(&self) {
        self.restart.send_replace(true);
    }

    /// Observe restart requests. `true` once any operation has requested one.
    #[must_use]
    pub fn restart_requested(&self) -> watch::Receiver<bool> {
        self.restart.subscribe()
    }

    #[must_use]
    pub(crate) fn data_dir(&self) -> &Path {
        &self.shared.data_dir
    }

    #[must_use]
    pub(crate) fn run_dir(&self) -> &Path {
        &self.shared.run_dir
    }

    /// Serializes local mutations. See the lock-order note on [`LocalMachineStore`].
    #[must_use]
    pub(crate) fn admission_lock(&self) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(&self.shared.admission_lock)
    }

    #[must_use]
    pub(crate) fn mutation_gate(&self) -> &MutationGate {
        &self.shared.mutation_gate
    }
}

impl Clone for RecordOwner {
    fn clone(&self) -> Self {
        self.shared.handles.fetch_add(1, Ordering::AcqRel);
        Self {
            mailbox: self.mailbox.clone(),
            record: self.record.clone(),
            restart: self.restart.clone(),
            shared: Arc::clone(&self.shared),
        }
    }
}

impl Drop for RecordOwner {
    fn drop(&mut self) {
        // Only the last handle waits, so the data directory is free once it is gone.
        // The counter makes exactly one concurrent drop the last one.
        if self.shared.handles.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        let Some(thread) = self
            .shared
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return;
        };
        if thread.thread().id() == thread::current().id() {
            return;
        }
        let (closed, _) = oneshot::channel();
        let _ = self.mailbox.send(Message::Close(closed));
        let _ = thread.join();
    }
}

fn publish(published: &watch::Sender<Arc<LocalMachineRecord>>, record: &LocalMachineRecord) {
    published.send_if_modified(|current| {
        if **current == *record {
            return false;
        }
        *current = Arc::new(record.clone());
        true
    });
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ployz_core::{AdvertisedEndpoint, LocalMachinePhase, MachineName};

    use super::{RecordOwner, RecordOwnerStopped};
    use crate::machine::LocalMachineStore;

    fn owner() -> (tempfile::TempDir, RecordOwner) {
        let dir = tempfile::tempdir().unwrap();
        let owner = RecordOwner::spawn(LocalMachineStore::open(dir.path()).unwrap()).unwrap();
        (dir, owner)
    }

    fn initialize(store: &mut LocalMachineStore) -> Result<(), crate::machine::StoreError> {
        store
            .initialize(ployz_core::InitializeRequest {
                initial_policy: Default::default(),
                name: MachineName::parse("local").unwrap(),
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                public_ip: None,
                advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
                wireguard_mtu: None,
            })
            .map(|_| ())
    }

    #[tokio::test]
    async fn a_mutation_is_visible_to_the_next_read_and_to_watchers() {
        let (_dir, owner) = owner();
        let mut watching = owner.watch();
        assert_eq!(owner.record().phase(), LocalMachinePhase::Uninitialized);

        owner.mutate(initialize).await.unwrap().unwrap();

        assert_eq!(owner.record().phase(), LocalMachinePhase::Participating);
        let seen = tokio::time::timeout(Duration::from_secs(1), async {
            watching
                .wait_for(|record| record.phase() == LocalMachinePhase::Participating)
                .await
                .unwrap()
                .phase()
        })
        .await
        .unwrap();
        assert_eq!(seen, LocalMachinePhase::Participating);
    }

    #[tokio::test]
    async fn a_rejected_mutation_publishes_nothing() {
        let (_dir, owner) = owner();
        let mut watching = owner.watch();
        watching.mark_unchanged();

        let rejected = owner
            .mutate(|store| store.complete_catch_up())
            .await
            .unwrap();

        assert!(rejected.is_err());
        assert!(!watching.has_changed().unwrap());
    }

    #[tokio::test]
    async fn a_panicking_mutation_stops_the_owner_but_reads_keep_the_last_record() {
        let (_dir, owner) = owner();
        owner.mutate(initialize).await.unwrap().unwrap();

        let stopped = owner
            .mutate(|_| -> () { panic!("store invariant violated") })
            .await;

        assert_eq!(stopped, Err(RecordOwnerStopped));
        assert_eq!(
            owner.mutate(|store| store.record().phase()).await,
            Err(RecordOwnerStopped)
        );
        assert_eq!(owner.record().phase(), LocalMachinePhase::Participating);
    }

    #[test]
    fn dropping_the_last_handle_releases_the_data_directory_before_returning() {
        let dir = tempfile::tempdir().unwrap();
        for _ in 0..20 {
            let owner = RecordOwner::spawn(LocalMachineStore::open(dir.path()).unwrap()).unwrap();
            let clone = owner.clone();
            drop(owner);
            assert!(
                LocalMachineStore::open(dir.path()).is_err(),
                "a live handle keeps the data directory claimed"
            );
            drop(clone);
            LocalMachineStore::open(dir.path()).expect("released by the last drop");
        }
    }

    #[test]
    fn concurrent_final_drops_still_release_the_data_directory() {
        let dir = tempfile::tempdir().unwrap();
        for _ in 0..50 {
            let owner = RecordOwner::spawn(LocalMachineStore::open(dir.path()).unwrap()).unwrap();
            let clone = owner.clone();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let droppers = [owner, clone].map(|handle| {
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    drop(handle);
                })
            });
            for dropper in droppers {
                dropper.join().unwrap();
            }
            LocalMachineStore::open(dir.path()).expect("released once both drops returned");
        }
    }

    #[tokio::test]
    async fn close_releases_the_data_directory_while_other_handles_live() {
        let (dir, owner) = owner();
        let other = owner.clone();
        owner.mutate(initialize).await.unwrap().unwrap();

        owner.close().await.unwrap();

        LocalMachineStore::open(dir.path()).expect("released by close");
        assert_eq!(
            other.mutate(|store| store.record().phase()).await,
            Err(RecordOwnerStopped)
        );
        assert_eq!(other.close().await, Err(RecordOwnerStopped));
        assert_eq!(other.record().phase(), LocalMachinePhase::Participating);
    }

    #[test]
    fn blocking_mutations_work_without_a_runtime() {
        let (_dir, owner) = owner();
        owner.mutate_blocking(initialize).unwrap().unwrap();
        assert_eq!(owner.record().phase(), LocalMachinePhase::Participating);
    }

    #[test]
    fn restart_requests_reach_every_observer() {
        let (_dir, owner) = owner();
        let observed = owner.restart_requested();
        assert!(!*observed.borrow());
        owner.clone().request_restart();
        assert!(*observed.borrow());
    }
}
