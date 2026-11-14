//! This Machine's Machine API, with Machine Target routing applied by construction.

mod ends_on_shutdown;
mod local;
mod provenance;
mod routing;

use std::{convert::Infallible, path::PathBuf, sync::Arc, task::Context};

use ployz_core::{
    MachineId, MachineRpcServer, ManagementScope, RUNTIME_WATCH_MESSAGE_SIZE_LIMIT, Rpc, op,
};
use tonic::{
    body::Body,
    codec::CompressionEncoding,
    codegen::{Service, http},
    service::Routes,
};

use provenance::Provenance;

use crate::{
    corrosion::{AdminClient, ReplicatedStore},
    docker::{ContainerRuntime, ImageIngest},
    machine::RecordOwner,
};

pub use ends_on_shutdown::EndsOnShutdown;
pub use routing::{MachineProxy, ProxyRoute, RoutingRequest, TargetResolutionError, resolve_route};

#[cfg(test)]
pub(crate) use local::MachineService;
#[cfg(test)]
pub(crate) mod test_socket;

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
    /// A builder over the record `owner` holds, with no Cluster attached until `with_cluster`.
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
    pub(crate) fn with_build_grants(mut self, grants: Arc<crate::management::BuildGrants>) -> Self {
        self.service.grants = grants;
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
    pub(crate) fn with_volume_plugin(mut self, plugin: crate::storage::Plugin) -> Self {
        self.service = self.service.with_volume_plugin(plugin);
        self
    }

    #[must_use]
    pub(crate) fn with_observe_socket(mut self, socket: PathBuf) -> Self {
        self.service = self.service.with_observe_socket(socket);
        self
    }

    /// Where an operator's Selected Endpoint goes: the running WireGuard mesh.
    #[must_use]
    pub(crate) fn with_endpoint_chooser(
        mut self,
        chooser: crate::network::EndpointChooser,
    ) -> Self {
        self.service.endpoint_chooser = Some(chooser);
        self
    }

    /// Where list and inspect answers read this Machine's path to each peer.
    #[must_use]
    pub(crate) fn with_paths(mut self, paths: crate::network::PathBook) -> Self {
        self.service.paths = paths;
        self
    }

    #[must_use]
    pub(crate) fn with_published_routes(
        mut self,
        published: tokio::sync::watch::Receiver<crate::ingress::PublishedRoutes>,
    ) -> Self {
        self.service.published_routes = published;
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
    let proxy = MachineProxy::new(routes(service.clone()), machine_id, port, replicated)
        .with_paths(service.paths.clone())
        .for_machine(local);
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
            .max_decoding_message_size(RUNTIME_WATCH_MESSAGE_SIZE_LIMIT)
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

    fn call(&mut self, mut request: http::Request<Body>) -> Self::Future {
        if self.service.local().activating() && !is_read(request.uri().path()) {
            return Box::pin(async { Ok(activating().into_http()) });
        }
        let Some(connection) = request
            .extensions()
            .get::<iroh::endpoint::WeakConnectionHandle>()
        else {
            return match refuse_local(&mut request) {
                None => self.proxy.call(request),
                Some(refused) => Box::pin(async { Ok(refused) }),
            };
        };
        let remote = connection
            .upgrade()
            .and(request.extensions().get::<ployz_core::ClientKey>().copied());
        let service = self.service.clone();
        let proxy = self.proxy.clone();
        Box::pin(async move {
            let Some(remote) = remote else {
                return Ok(
                    tonic::Status::unauthenticated("management connection closed").into_http(),
                );
            };
            if let Some(refused) = refuse_remote(&service, remote, &mut request).await {
                return Ok(refused);
            }
            // Carry the authenticated key into detached/queued local work as well.
            let mut proxy = proxy.with_local(routes(service.with_management_client(remote)));
            proxy.call(request).await
        })
    }
}

/// The refusal of a call that arrived over this Server's local socket or a mesh connection,
/// or else the call marked with the scope it is served at.
///
/// A daemon that advertises `ployz.machine.enforced.v1` never gives a mesh caller Full scope:
/// only this Server's local socket and a Device's own connection make Full calls, which is
/// what the trusted snapshot rests on.
fn refuse_local(request: &mut http::Request<Body>) -> Option<http::Response<Body>> {
    let local = request
        .extensions()
        .get::<tonic::transport::server::UdsConnectInfo>()
        .is_some();
    let provenance = if local && !Provenance::of_headers(request.headers()).operator() {
        Provenance::new(ManagementScope::Full, false)
    } else {
        Provenance::new(ManagementScope::Operator, false)
    };
    if !local && request.uri().path() == op::SetManagementClient::PATH {
        return Some(
            ployz_core::rpc::hinted_status(
                tonic::Code::PermissionDenied,
                &NeedsManagementConnection,
                serde_json::json!({ "capability": ployz_core::SET_MANAGEMENT_CLIENT_CAPABILITY }),
            )
            .into_http(),
        );
    }
    provenance.stamp(request.headers_mut());
    if !local && ployz_core::scope_of(request.uri().path()) == Some(ManagementScope::Full) {
        let capability =
            ployz_core::capability_of(request.uri().path()).expect("catalogued Full route");
        return Some(ployz_core::rpc::hinted_status(
            tonic::Code::PermissionDenied,
            &NeedsDirectDevice,
            serde_json::json!({ "capability": capability, "next": "Connect directly to the target Server as an approved Device, or over SSH." }),
        )
        .into_http());
    }
    refuse_scope(request.uri().path(), provenance.scope(), false)
}

/// The refusal of a call over a Management connection that `remote` authenticated, or else
/// the call marked as Cloud routed at the scope its Device slot gives it.
async fn refuse_remote(
    service: &local::MachineService,
    remote: ployz_core::ClientKey,
    request: &mut http::Request<Body>,
) -> Option<http::Response<Body>> {
    let local = service.local();
    let verification = request.uri().path() == op::DescribeContract::PATH;
    // Negotiation is read-only: Cloud must save a verified candidate before
    // any operational RPC can activate it and retire the previous credential.
    if !verification && let Err(error) = local.activate_management_client(remote).await {
        return Some(ployz_core::rpc::caused_status(tonic::Code::Unavailable, &error).into_http());
    }
    let record = local.record();
    let allowed = if verification {
        record.admits_management_client(&remote)
    } else {
        record.accepts_management_client(&remote)
    };
    if !allowed {
        return Some(tonic::Status::unauthenticated("management credential revoked").into_http());
    }
    let scope = record
        .management_scope(&remote)
        .unwrap_or(ManagementScope::Operator);
    let awaiting = record.trusts_after_activation(&remote);
    if let Some(refused) = refuse_scope(request.uri().path(), scope, awaiting) {
        return Some(refused);
    }
    if request.uri().path() == op::SetManagementClient::PATH {
        if let Err(error) = crate::machine::LocalMachine::require_slot_authority(
            crate::machine::Caller::Management(remote),
            &record,
        ) {
            return Some(
                ployz_core::rpc::caused_status(tonic::Code::PermissionDenied, &error).into_http(),
            );
        }
        // Forwarding drops the client key, and only a Machine's own `cloud` slot
        // governs its slots, so a slot change is never routed to another Machine.
        let routing = ployz_core::routing_from_metadata(
            &tonic::metadata::MetadataMap::from_headers(request.headers().clone()),
        );
        if !matches!(routing, Ok(RoutingRequest::Local)) {
            return Some(
                tonic::Status::invalid_argument("slots change only on the Server that holds them")
                    .into_http(),
            );
        }
    }
    Provenance::new(scope, true).stamp(request.headers_mut());
    None
}

/// Whether `path` is a catalog route the coverage table classes as a read.
fn is_read(path: &str) -> bool {
    ployz_core::capability_of(path).is_some_and(|capability| {
        ployz_core::authority::coverage::authorization(capability)
            == Some(ployz_core::authority::coverage::Authorization::Read)
    })
}

/// The answer to management work while an activation waits for its commit.
fn activating() -> tonic::Status {
    ployz_core::rpc::hinted_status(
        tonic::Code::Unavailable,
        &crate::machine::LocalMachineError::Activating,
        serde_json::json!({
            "capability": ployz_core::ENFORCED_CAPABILITY,
            "reason": "activating",
        }),
    )
}

/// The way to Full access, said in the refusal itself: a v0.3.4 CLI prints only the
/// message, and has no `device` command.
const FULL_ACCESS_NEXT: &str =
    "run `ployz device request` (update ployz first if it has no `device` command)";

#[derive(Debug, thiserror::Error)]
#[error("{capability} needs Full access: {FULL_ACCESS_NEXT}")]
struct NeedsFullAccess {
    capability: &'static str,
}

#[derive(Debug, thiserror::Error)]
#[error("Management slots require a local or authenticated management connection")]
struct NeedsManagementConnection;

/// Said in the message itself, since a CLI before 0.4 prints only the message, and sends a
/// Full call for another Server through the entry, where this refuses it.
#[derive(Debug, thiserror::Error)]
#[error(
    "This Server takes this call only from an approved Device connected to it directly, or over SSH. A ployz CLI older than 0.4 sends it through another Server: upgrade ployz on this computer to 0.4, then run the command again."
)]
struct NeedsDirectDevice;

#[derive(Debug, thiserror::Error)]
#[error(
    "{capability}: this Server awaits its Cluster's activation and doesn't trust Devices added before Ployz 0.4 until then. Run this over SSH, or activate with `ployz device enforce` and keep this Device"
)]
struct TrustedAfterActivation {
    capability: &'static str,
}

/// A call `scope` may not make, refused naming its capability. `awaiting` says the caller is
/// a Device this Server trusts only once its Cluster activates, so the way on is activation.
fn refuse_scope(
    path: &str,
    scope: ManagementScope,
    awaiting: bool,
) -> Option<http::Response<Body>> {
    if ployz_core::scope_of(path).is_none_or(|needed| scope >= needed) {
        return None;
    }
    let capability = ployz_core::capability_of(path)?;
    if awaiting {
        return Some(
            ployz_core::rpc::hinted_status(
                tonic::Code::PermissionDenied,
                &TrustedAfterActivation { capability },
                serde_json::json!({
                    "capability": capability,
                    "reason": "awaiting_activation",
                    "next": "ployz device enforce",
                }),
            )
            .into_http(),
        );
    }
    Some(
        ployz_core::rpc::hinted_status(
            tonic::Code::PermissionDenied,
            &NeedsFullAccess { capability },
            serde_json::json!({ "capability": capability, "next": "ployz device request" }),
        )
        .into_http(),
    )
}
