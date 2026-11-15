//! The two marks the daemon keeps on a Container while an operation is under way: that
//! it is `stopping`, so the Ingress Proxies stop routing to it, and that it is `leaving`,
//! so a peer's inspect reports it. Lifecycle sets them and observation reads them.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use ployz_core::ContainerId;
use tokio::sync::watch;

#[derive(Clone)]
pub(super) struct Standing {
    stopping: watch::Sender<BTreeMap<ContainerId, usize>>,
    leaving: Arc<std::sync::Mutex<BTreeSet<ContainerId>>>,
}

impl Standing {
    pub(super) fn new() -> Self {
        Self {
            stopping: watch::Sender::new(BTreeMap::new()),
            leaving: Arc::default(),
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
        self.leaving
            .lock()
            .expect("leaving marks lock")
            .insert(container_id);
        Leaving {
            leaving: Arc::clone(&self.leaving),
            container_id,
        }
    }

    pub(super) fn is_leaving(&self, container_id: &ContainerId) -> bool {
        self.leaving
            .lock()
            .expect("leaving marks lock")
            .contains(container_id)
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

/// An Out's hold on a Container's leaving mark.
pub struct Leaving {
    leaving: Arc<std::sync::Mutex<BTreeSet<ContainerId>>>,
    container_id: ContainerId,
}

impl Drop for Leaving {
    fn drop(&mut self) {
        if let Ok(mut leaving) = self.leaving.lock() {
            leaving.remove(&self.container_id);
        }
    }
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
