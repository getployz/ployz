//! This Machine's Machine API, with Machine Target routing applied by construction.

mod local;
mod routing;

use std::{
    convert::Infallible,
    path::PathBuf,
    sync::{Arc, Mutex},
    task::Context,
};

use ployz_core::{CloudPairing, MachineId, MachineRpcServer, RUNTIME_WATCH_MESSAGE_SIZE_LIMIT};
use tokio::sync::watch;
use tonic::{
    body::Body,
    codec::CompressionEncoding,
    codegen::{Service, http},
    service::Routes,
};

use crate::{
    corrosion::{AdminClient, ReplicatedStore},
    docker::{ContainerRuntime, ImageIngest},
    global_reconcile::GlobalReconcileObservations,
    machine::{LocalMachine, LocalMachineError, LocalMachineStore},
};

pub use routing::{MachineProxy, ProxyRoute, RoutingRequest, TargetResolutionError, resolve_route};

#[cfg(test)]
pub(crate) use local::{MachineService, REGISTER_FORWARDED_METADATA};

/// A servable Machine API. Every request it serves is routed.
#[derive(Clone)]
pub struct MachineApi {
    proxy: MachineProxy,
    local: LocalMachine,
    machine_id: MachineId,
}

/// Configure this Machine's Machine API before it becomes servable.
pub struct MachineApiBuilder {
    service: local::MachineService,
}

impl MachineApi {
    /// Start configuring this Machine's Machine API.
    #[must_use]
    pub fn builder(
        store: Arc<Mutex<LocalMachineStore>>,
        restart: watch::Sender<bool>,
    ) -> MachineApiBuilder {
        MachineApiBuilder {
            service: local::MachineService::with_cluster(store, restart, None),
        }
    }

    /// Machine ID every dispatch treats as this Machine.
    #[must_use]
    pub fn machine_id(&self) -> MachineId {
        self.machine_id
    }

    /// Local Machine operations for daemon-owned maintenance loops.
    ///
    /// [`LocalMachine`] does not implement [`ployz_core::MachineRpc`], so this
    /// value cannot be mounted on a listener.
    #[must_use]
    pub(crate) fn local(&self) -> LocalMachine {
        self.local.clone()
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_local(service: MachineService) -> Self {
        wrap(service).expect("test Machine record is readable")
    }
}

impl MachineApiBuilder {
    #[must_use]
    pub(crate) fn with_participation(mut self, participating: watch::Sender<bool>) -> Self {
        self.service = self.service.with_participation(participating);
        self
    }

    #[must_use]
    pub(crate) fn with_cluster(mut self, cluster: Option<(ReplicatedStore, AdminClient)>) -> Self {
        self.service = self.service.with_cluster_option(cluster);
        self
    }

    #[must_use]
    pub(crate) fn with_optional_containers(mut self, containers: Option<ContainerRuntime>) -> Self {
        self.service = self.service.with_optional_containers(containers);
        self
    }

    #[must_use]
    pub(crate) fn with_ingress_data_dir(mut self, path: PathBuf) -> Self {
        self.service = self.service.with_ingress_data_dir(path);
        self
    }

    #[must_use]
    pub(crate) fn with_image_ingest(mut self, ingest: Arc<ImageIngest>) -> Self {
        self.service = self.service.with_image_ingest(ingest);
        self
    }

    #[must_use]
    pub(crate) fn with_cloud_pairing(
        mut self,
        pairing: watch::Sender<Option<CloudPairing>>,
    ) -> Self {
        self.service = self.service.with_cloud_pairing(pairing);
        self
    }

    #[must_use]
    pub(crate) fn with_global_reconcile_observations(
        mut self,
        observations: GlobalReconcileObservations,
    ) -> Self {
        self.service = self
            .service
            .with_global_reconcile_observations(observations);
        self
    }

    /// Apply routing and return a servable Machine API.
    ///
    /// # Errors
    ///
    /// Returns [`LocalMachineError::LockPoisoned`] when the local record lock is
    /// poisoned.
    pub fn build(self) -> Result<MachineApi, LocalMachineError> {
        wrap(self.service)
    }
}

fn wrap(service: local::MachineService) -> Result<MachineApi, LocalMachineError> {
    let local = service.local();
    let machine_id = local.record()?.id();
    let port = service.machine_api_port();
    let replicated = match local.replicated() {
        Ok(store) => Some(store.clone()),
        Err(LocalMachineError::ClusterStoreUnavailable) => None,
        Err(error) => return Err(error),
    };
    let proxy = MachineProxy::new(
        Routes::new(
            MachineRpcServer::new(service)
                .send_compressed(CompressionEncoding::Gzip)
                .max_encoding_message_size(RUNTIME_WATCH_MESSAGE_SIZE_LIMIT),
        ),
        machine_id,
        port,
        replicated,
    );
    Ok(MachineApi {
        proxy,
        local,
        machine_id,
    })
}

impl Service<http::Request<Body>> for MachineApi {
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = <MachineProxy as Service<http::Request<Body>>>::Future;

    fn poll_ready(
        &mut self,
        context: &mut Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.proxy.poll_ready(context)
    }

    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        self.proxy.call(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::LocalMachineStore;
    use ployz_core::MachineId;
    use std::sync::{Arc, Mutex};

    #[test]
    fn build_fails_when_the_local_record_lock_is_poisoned() {
        let data_dir =
            std::env::temp_dir().join(format!("ployzd-machine-api-poison-{}", MachineId::random()));
        let store = Arc::new(Mutex::new(LocalMachineStore::open(&data_dir).unwrap()));
        let poisoned = Arc::clone(&store);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = poisoned.lock().unwrap();
            panic!("poison the local Machine record lock");
        }));
        let (reset, _) = watch::channel(false);
        match MachineApi::builder(store, reset).build() {
            Err(error) => assert!(matches!(error, LocalMachineError::LockPoisoned)),
            Ok(_) => panic!("a poisoned lock must fail build"),
        }
        let _ = std::fs::remove_dir_all(data_dir);
    }
}
