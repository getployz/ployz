//! This Machine's Machine API, with Machine Target routing applied by construction.

mod local;
mod routing;

use std::{convert::Infallible, path::PathBuf, sync::Arc, task::Context};

use ployz_core::{MachineId, MachineRpcServer, RUNTIME_WATCH_MESSAGE_SIZE_LIMIT};
use tonic::{
    body::Body,
    codec::CompressionEncoding,
    codegen::{Service, http},
    service::Routes,
};

use crate::{
    corrosion::{AdminClient, ReplicatedStore},
    docker::{ContainerRuntime, ImageIngest},
    machine::RecordOwner,
};

pub use routing::{MachineProxy, ProxyRoute, RoutingRequest, TargetResolutionError, resolve_route};

#[cfg(test)]
pub(crate) use local::MachineService;

/// A servable Machine API. Every request it serves is routed.
#[derive(Clone)]
pub struct MachineApi {
    proxy: MachineProxy,
    machine_id: MachineId,
}

/// Configure this Machine's Machine API before it becomes servable.
pub struct MachineApiBuilder {
    service: local::MachineService,
}

impl MachineApi {
    /// Start configuring this Machine's Machine API.
    #[must_use]
    pub fn builder(owner: RecordOwner) -> MachineApiBuilder {
        MachineApiBuilder {
            service: local::MachineService::with_cluster(owner, None),
        }
    }

    /// Machine ID every dispatch treats as this Machine.
    #[must_use]
    pub fn machine_id(&self) -> MachineId {
        self.machine_id
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_local(service: MachineService) -> Self {
        wrap(service)
    }
}

impl MachineApiBuilder {
    pub(crate) fn with_builds(mut self, builds: Arc<crate::build::Runner>) -> Self {
        self.service.builds = builds;
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

    /// Apply routing and return a servable Machine API.
    #[must_use]
    pub fn build(self) -> MachineApi {
        wrap(self.service)
    }
}

fn wrap(service: local::MachineService) -> MachineApi {
    let local = service.local();
    let machine_id = local.record().id();
    let port = service.machine_api_port();
    let replicated = local.replicated().ok().cloned();
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
    MachineApi { proxy, machine_id }
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
