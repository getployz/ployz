use std::sync::{Arc, Mutex};

use tokio::task::JoinHandle;

/// Owned mint tasks, aborted on control shutdown.
#[derive(Clone, Default)]
pub struct MintTaskRegistry {
    handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl MintTaskRegistry {
    pub fn spawn(&self, future: impl std::future::Future<Output = ()> + Send + 'static) {
        let mut handles = self
            .handles
            .lock()
            .expect("mint task registry lock is not poisoned");
        handles.retain(|handle| !handle.is_finished());
        handles.push(tokio::spawn(future));
    }

    pub fn abort_all(&self) {
        let mut handles = self
            .handles
            .lock()
            .expect("mint task registry lock is not poisoned");
        for handle in handles.drain(..) {
            handle.abort();
        }
    }
}
