use ployz_store_api::{
    ClusterStore, DeployCommitStore, DeployReadStore, DeployWriteStore, InviteStore, MachineStore,
    SyncProbe,
};
use ployz_test_support::MemoryStore;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct StoreDriver {
    machine: Arc<dyn MachineStore>,
    invite: Arc<dyn InviteStore>,
    deploy_read: Arc<dyn DeployReadStore>,
    deploy_write: Arc<dyn DeployWriteStore>,
    deploy_commit: Arc<dyn DeployCommitStore>,
    sync: Arc<dyn SyncProbe>,
}

impl StoreDriver {
    #[must_use]
    pub(crate) fn memory_with(store: Arc<MemoryStore>) -> Self {
        Self {
            machine: Arc::clone(&store) as Arc<dyn MachineStore>,
            invite: Arc::clone(&store) as Arc<dyn InviteStore>,
            deploy_read: Arc::clone(&store) as Arc<dyn DeployReadStore>,
            deploy_write: Arc::clone(&store) as Arc<dyn DeployWriteStore>,
            deploy_commit: Arc::clone(&store) as Arc<dyn DeployCommitStore>,
            sync: Arc::clone(&store) as Arc<dyn SyncProbe>,
        }
    }

    #[must_use]
    pub(crate) fn from_store<T>(store: Arc<T>) -> Self
    where
        T: ClusterStore + 'static,
    {
        Self {
            machine: Arc::clone(&store) as Arc<dyn MachineStore>,
            invite: Arc::clone(&store) as Arc<dyn InviteStore>,
            deploy_read: Arc::clone(&store) as Arc<dyn DeployReadStore>,
            deploy_write: Arc::clone(&store) as Arc<dyn DeployWriteStore>,
            deploy_commit: Arc::clone(&store) as Arc<dyn DeployCommitStore>,
            sync: store as Arc<dyn SyncProbe>,
        }
    }

    #[must_use]
    pub(crate) fn machine_store(&self) -> Arc<dyn MachineStore> {
        Arc::clone(&self.machine)
    }

    #[must_use]
    pub(crate) fn machine(&self) -> Arc<dyn MachineStore> {
        Arc::clone(&self.machine)
    }

    #[must_use]
    pub(crate) fn invite(&self) -> Arc<dyn InviteStore> {
        Arc::clone(&self.invite)
    }

    pub(crate) fn deploy_read(&self) -> Arc<dyn DeployReadStore> {
        Arc::clone(&self.deploy_read)
    }

    #[must_use]
    pub(crate) fn deploy_write(&self) -> Arc<dyn DeployWriteStore> {
        Arc::clone(&self.deploy_write)
    }

    #[must_use]
    pub(crate) fn deploy_commit(&self) -> Arc<dyn DeployCommitStore> {
        Arc::clone(&self.deploy_commit)
    }

    #[must_use]
    pub(crate) fn sync_probe(&self) -> Arc<dyn SyncProbe> {
        Arc::clone(&self.sync)
    }
}
