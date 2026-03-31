use crate::services::supervisor::SidecarHandle;
use ployz_runtime_api::{Result, RuntimeError};

pub(crate) struct ManagedServiceHandle {
    inner: ManagedServiceInner,
}

enum ManagedServiceInner {
    Noop,
    Sidecar(Box<SidecarHandle>),
}

impl ManagedServiceHandle {
    #[must_use]
    pub(crate) fn noop() -> Self {
        Self {
            inner: ManagedServiceInner::Noop,
        }
    }

    pub(crate) fn sidecar(handle: SidecarHandle) -> Self {
        Self {
            inner: ManagedServiceInner::Sidecar(Box::new(handle)),
        }
    }

    pub(crate) async fn shutdown(&mut self, service_name: &'static str) -> Result<()> {
        match &mut self.inner {
            ManagedServiceInner::Noop => Ok(()),
            ManagedServiceInner::Sidecar(handle) => handle.shutdown().await.map_err(|error| {
                RuntimeError::operation(
                    "managed service shutdown",
                    format!("{service_name}: {error}"),
                )
            }),
        }
    }

    pub(crate) async fn detach(&mut self, service_name: &'static str) -> Result<()> {
        match &mut self.inner {
            ManagedServiceInner::Noop => Ok(()),
            ManagedServiceInner::Sidecar(handle) => handle.detach().await.map_err(|error| {
                RuntimeError::operation(
                    "managed service detach",
                    format!("{service_name}: {error}"),
                )
            }),
        }
    }
}
