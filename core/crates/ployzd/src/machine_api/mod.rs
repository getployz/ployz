//! This Machine's Machine API, with Machine Target routing applied by construction.

mod local;
mod routing;

use std::{convert::Infallible, path::PathBuf, sync::Arc, task::Context};

use ployz_core::{MachineId, MachineRpcServer, RUNTIME_WATCH_MESSAGE_SIZE_LIMIT, Rpc, op};
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
    service: local::MachineService,
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
    let proxy = MachineProxy::new(routes(service.clone()), machine_id, port, replicated);
    MachineApi {
        proxy,
        machine_id,
        service,
    }
}

fn routes(service: local::MachineService) -> Routes {
    Routes::new(
        MachineRpcServer::new(service)
            .send_compressed(CompressionEncoding::Gzip)
            .max_encoding_message_size(RUNTIME_WATCH_MESSAGE_SIZE_LIMIT),
    )
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
        let Some(connection) = request
            .extensions()
            .get::<iroh::endpoint::WeakConnectionHandle>()
        else {
            return self.proxy.call(request);
        };
        let remote = connection
            .upgrade()
            .map(|connection| *connection.remote_id().as_bytes());
        let service = self.service.clone();
        let proxy = self.proxy.clone();
        Box::pin(async move {
            let Some(remote) = remote else {
                return Ok(
                    tonic::Status::unauthenticated("management connection closed").into_http(),
                );
            };
            let local = service.local();
            let verification = request.uri().path() == op::DescribeContract::PATH;
            // Negotiation is read-only: Cloud must save a verified candidate before
            // any operational RPC can activate it and retire the previous credential.
            if !verification && let Err(error) = local.activate_management_client(remote).await {
                return Ok(tonic::Status::unavailable(error.to_string()).into_http());
            }
            let record = local.record();
            let allowed = if verification {
                record.admits_management_client(&remote)
            } else {
                record.accepts_management_client(&remote)
            };
            if !allowed {
                return Ok(
                    tonic::Status::unauthenticated("management credential revoked").into_http(),
                );
            }
            // Carry the authenticated key into detached/queued local work as well.
            let mut proxy = proxy.with_local(routes(service.with_management_client(remote)));
            proxy.call(request).await
        })
    }
}
