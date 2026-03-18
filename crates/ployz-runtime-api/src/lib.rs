mod identity;

use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use ployz_types::model::DeployId;
use ployz_types::{Error, Result as PloyzResult, spec::Namespace};

pub use identity::{Identity, IdentityError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartableWorkload {
    pub container_name: String,
    pub was_running: bool,
}

#[async_trait]
pub trait RuntimeHandle: Send + Sync {
    async fn shutdown(self: Box<Self>) -> std::result::Result<(), String>;

    async fn detach(self: Box<Self>) -> std::result::Result<(), String> {
        Ok(())
    }
}

pub struct NoopRuntimeHandle;

#[async_trait]
impl RuntimeHandle for NoopRuntimeHandle {
    async fn shutdown(self: Box<Self>) -> std::result::Result<(), String> {
        Ok(())
    }
}

#[derive(Clone, Default)]
pub struct NamespaceLockManager {
    held: Arc<Mutex<std::collections::HashMap<String, DeployId>>>,
}

impl NamespaceLockManager {
    pub fn try_acquire(
        &self,
        namespace: &Namespace,
        deploy_id: &DeployId,
    ) -> PloyzResult<NamespaceLock> {
        let mut guard = self.lock_inner();
        if let Some(current) = guard.get(&namespace.0) {
            return Err(Error::operation(
                "namespace_lock",
                format!(
                    "namespace '{}' is already locked by deploy '{}'",
                    namespace, current
                ),
            ));
        }
        guard.insert(namespace.0.clone(), deploy_id.clone());
        Ok(NamespaceLock {
            held: Arc::clone(&self.held),
            namespace: namespace.clone(),
        })
    }

    fn lock_inner(&self) -> MutexGuard<'_, std::collections::HashMap<String, DeployId>> {
        self.held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub struct NamespaceLock {
    held: Arc<Mutex<std::collections::HashMap<String, DeployId>>>,
    namespace: Namespace,
}

impl Drop for NamespaceLock {
    fn drop(&mut self) {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        held.remove(&self.namespace.0);
    }
}
