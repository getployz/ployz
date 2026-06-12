//! Owned background task registry, aborted on runtime shutdown.

use std::sync::{Arc, Mutex};

use tokio::task::JoinHandle;

#[derive(Debug, Clone, Default)]
pub struct TaskRegistry {
    handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl TaskRegistry {
    pub fn spawn(&self, future: impl std::future::Future<Output = ()> + Send + 'static) {
        let mut handles = self
            .handles
            .lock()
            .expect("task registry lock is not poisoned");
        handles.retain(|handle| !handle.is_finished());
        handles.push(tokio::spawn(future));
    }

    pub fn abort_all(&self) {
        let mut handles = self
            .handles
            .lock()
            .expect("task registry lock is not poisoned");
        for handle in handles.drain(..) {
            handle.abort();
        }
    }
}
