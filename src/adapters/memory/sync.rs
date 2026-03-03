use crate::dataplane::traits::{PortResult, SyncProbe};
use std::sync::atomic::{AtomicBool, Ordering};

pub struct MemorySyncProbe {
    synced: AtomicBool,
}

impl Default for MemorySyncProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl MemorySyncProbe {
    pub fn new() -> Self {
        Self {
            synced: AtomicBool::new(true),
        }
    }

    pub fn set_synced(&self, synced: bool) {
        self.synced.store(synced, Ordering::SeqCst);
    }
}

impl SyncProbe for MemorySyncProbe {
    async fn sync_complete(&self) -> PortResult<bool> {
        Ok(self.synced.load(Ordering::SeqCst))
    }
}
