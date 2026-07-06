//! Owned background task registry, aborted on daemon shutdown.

use std::sync::{Arc, Mutex};

use tokio::task::JoinSet;

#[derive(Debug, Clone, Default)]
pub struct TaskRegistry {
    handles: Arc<Mutex<JoinSet<()>>>,
}

impl TaskRegistry {
    pub fn spawn(&self, future: impl std::future::Future<Output = ()> + Send + 'static) {
        let mut handles = self
            .handles
            .lock()
            .expect("task registry lock is not poisoned");
        handles.spawn(future);
    }

    pub fn abort_all(&self) {
        let mut handles = self
            .handles
            .lock()
            .expect("task registry lock is not poisoned");
        handles.abort_all();
    }
}
