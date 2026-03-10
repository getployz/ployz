use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::error::{Error, Result};
use crate::model::DeployId;
use crate::spec::Namespace;

#[derive(Clone, Default)]
pub struct NamespaceLockManager {
    held: Arc<Mutex<HashMap<String, DeployId>>>,
}

impl NamespaceLockManager {
    pub fn try_acquire(
        &self,
        namespace: &Namespace,
        deploy_id: &DeployId,
    ) -> Result<NamespaceLock> {
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

    fn lock_inner(&self) -> MutexGuard<'_, HashMap<String, DeployId>> {
        self.held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub struct NamespaceLock {
    held: Arc<Mutex<HashMap<String, DeployId>>>,
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
