//! The marks the daemon keeps on a Container while an operation is under way: that it is
//! `stopping`, so the Ingress Proxies stop routing to it, that it is `leaving`, so a
//! peer's inspect reports it, and that it is `signalled`, so no write brings it In while
//! its stop waits out the grace. Lifecycle sets them and observation reads them.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use ployz_core::ContainerId;
use tokio::sync::watch;

#[derive(Clone)]
pub(super) struct Standing {
    stopping: watch::Sender<BTreeMap<ContainerId, usize>>,
    leaving: Marks,
    signalled: Marks,
}

type Marks = Arc<std::sync::Mutex<BTreeSet<ContainerId>>>;

impl Standing {
    pub(super) fn new() -> Self {
        Self {
            stopping: watch::Sender::new(BTreeMap::new()),
            leaving: Arc::default(),
            signalled: Arc::default(),
        }
    }

    pub(super) fn withdraw(&self, container_id: ContainerId) -> Withdrawal {
        self.stopping.send_if_modified(|stopping| {
            let held = stopping.entry(container_id).or_default();
            *held += 1;
            *held == 1
        });
        Withdrawal {
            stopping: self.stopping.clone(),
            container_id,
        }
    }

    pub(super) fn leave(&self, container_id: ContainerId) -> Leaving {
        Leaving {
            _mark: Mark::set(&self.leaving, container_id),
        }
    }

    pub(super) fn is_leaving(&self, container_id: &ContainerId) -> bool {
        Mark::is_set(&self.leaving, container_id)
    }

    pub(super) fn signal(&self, container_id: ContainerId) -> Signalled {
        Signalled {
            _mark: Mark::set(&self.signalled, container_id),
        }
    }

    pub(super) fn is_signalled(&self, container_id: &ContainerId) -> bool {
        Mark::is_set(&self.signalled, container_id)
    }

    pub(super) fn is_withdrawn(&self, container_id: &ContainerId) -> bool {
        self.stopping.borrow().contains_key(container_id)
    }

    pub(super) fn subscribe(&self) -> watch::Receiver<BTreeMap<ContainerId, usize>> {
        self.stopping.subscribe()
    }

    #[cfg(test)]
    pub(super) fn withdrawn_ids(&self) -> BTreeSet<ContainerId> {
        self.stopping.borrow().keys().copied().collect()
    }
}

/// A Container in one set of marks until it drops.
struct Mark {
    marks: Marks,
    container_id: ContainerId,
}

impl Mark {
    fn set(marks: &Marks, container_id: ContainerId) -> Self {
        marks.lock().expect("marks lock").insert(container_id);
        Self {
            marks: Arc::clone(marks),
            container_id,
        }
    }

    fn is_set(marks: &Marks, container_id: &ContainerId) -> bool {
        marks.lock().expect("marks lock").contains(container_id)
    }
}

impl Drop for Mark {
    fn drop(&mut self) {
        if let Ok(mut marks) = self.marks.lock() {
            marks.remove(&self.container_id);
        }
    }
}

/// An Out's hold on a Container's leaving mark.
pub struct Leaving {
    _mark: Mark,
}

/// A stop's hold on a Container's signalled mark, from the turn it was sent under
/// until it ended.
#[must_use = "the Container may go In again once its Signalled drops"]
pub struct Signalled {
    _mark: Mark,
}

/// One operation's hold on a Container's `stopping` mark.
#[must_use = "the Container returns to ingress when its Withdrawal drops"]
pub struct Withdrawal {
    stopping: watch::Sender<BTreeMap<ContainerId, usize>>,
    container_id: ContainerId,
}

impl Drop for Withdrawal {
    fn drop(&mut self) {
        self.stopping.send_if_modified(|stopping| {
            let Some(held) = stopping.get_mut(&self.container_id) else {
                return false;
            };
            *held -= 1;
            if *held > 0 {
                return false;
            }
            stopping.remove(&self.container_id);
            true
        });
    }
}
