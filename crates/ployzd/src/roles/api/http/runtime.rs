//! API listener binding, connection ownership, and process wiring.

use std::convert::Infallible;
use std::future::{Future, pending};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use ployz_core::LensCollection;
use ployz_core::corrosion::MachineTransport;
use ployz_core::join::{JoinDoorMaterial, JoinMachineSubstrate};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_rustls::TlsAcceptor;

use super::config::{ApiRoleConfig, ApiRoleConfigError, ApiRoleMode};
use super::door::{JoinDoorBindError, JoinDoorListener, serve_join_connection};
use super::endpoint_network::{self, EndpointNetworkFoldError};
use super::roster::{ApiListenerValidationError, validate_listener_identity};
use super::server::{ApiService, ApiServiceRuntime};
use crate::corrosion::CorrosionClient;

const SERVER_SHUTDOWN_GRACE: Duration = Duration::from_secs(8);
const HTTP_HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_JOIN_SUBSTRATE_BYTES: usize = 1024 * 1024;
const MAX_JOIN_DOOR_CONNECTIONS: usize = 256;
const LISTENER_ACCEPT_MAX_CONSECUTIVE_FAILURES: u32 = 8;
const LISTENER_ACCEPT_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
const LISTENER_ACCEPT_MAX_BACKOFF: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub(super) struct JoinDoorAdmission {
    pub(super) material: Arc<JoinDoorMaterial>,
    pub(super) substrate: Arc<JoinMachineSubstrate>,
}

/// The listener and admission inputs that must come into existence together.
pub(super) enum JoinDoorRuntime {
    Ready {
        listener: JoinDoorListener,
        admission: JoinDoorAdmission,
    },
    DoorlessIntegrationFixture,
}

impl JoinDoorRuntime {
    async fn bind(config: &ApiRoleConfig) -> Result<Self, ApiServerError> {
        let listener = JoinDoorListener::bind(
            config.door_listen_addr().get(),
            config.door_private_key_path(),
            config.door_certificate_path(),
            config.door_fingerprint_path(),
        )
        .await
        .map_err(ApiServerError::JoinDoor)?;
        let substrate = Arc::new(load_join_substrate(config.join_substrate_path()).await?);
        let admission = JoinDoorAdmission {
            material: listener.material(),
            substrate,
        };
        Ok(Self::Ready {
            listener,
            admission,
        })
    }

    pub(super) fn admission(&self) -> Option<JoinDoorAdmission> {
        match self {
            Self::Ready { admission, .. } => Some(admission.clone()),
            Self::DoorlessIntegrationFixture => None,
        }
    }

    async fn accept(&self) -> Result<JoinDoorConnection, std::io::Error> {
        match self {
            Self::Ready { listener, .. } => {
                let (stream, peer) = listener.accept().await?;
                Ok(JoinDoorConnection {
                    stream,
                    peer,
                    acceptor: listener.acceptor(),
                })
            }
            Self::DoorlessIntegrationFixture => pending().await,
        }
    }
}

struct JoinDoorConnection {
    stream: TcpStream,
    peer: SocketAddr,
    acceptor: TlsAcceptor,
}

/// A bound public API listener and its owned lens tasks.
pub struct ApiServer {
    listener: TcpListener,
    join_door: Arc<JoinDoorRuntime>,
    service: Arc<ApiService>,
    lifecycle_failures: mpsc::UnboundedReceiver<LensCollection>,
}

impl ApiServer {
    /// Binds the authenticated mesh API and the complete public join door.
    pub async fn bind(config: ApiRoleConfig) -> Result<Self, ApiServerError> {
        let (listener, runtime) = bind_api_listener(&config, true).await?;
        let join_door = Arc::new(JoinDoorRuntime::bind(&config).await?);
        Ok(Self::from_validated_listener(listener, join_door, runtime))
    }

    /// Binds only the mesh API for the real-Corrosion integration fixture.
    #[doc(hidden)]
    pub async fn bind_without_join_door_for_integration_test(
        config: ApiRoleConfig,
    ) -> Result<Self, ApiServerError> {
        let (listener, runtime) = bind_api_listener(&config, false).await?;
        let join_door = Arc::new(JoinDoorRuntime::DoorlessIntegrationFixture);
        Ok(Self::from_validated_listener(listener, join_door, runtime))
    }

    fn from_validated_listener(
        listener: TcpListener,
        join_door: Arc<JoinDoorRuntime>,
        runtime: ApiServiceRuntime,
    ) -> Self {
        let (service, lifecycle_failures) = ApiService::new(runtime, Arc::clone(&join_door));
        Self {
            listener,
            join_door,
            service,
            lifecycle_failures,
        }
    }

    /// Serves accepted TCP peers until the caller requests controlled shutdown.
    pub async fn serve<Shutdown>(self, shutdown: Shutdown) -> Result<(), ApiServerServeError>
    where
        Shutdown: Future<Output = ()> + Send,
    {
        let Self {
            listener,
            join_door,
            service,
            mut lifecycle_failures,
        } = self;
        let (shutdown_tx, _) = watch::channel(false);
        let (endpoint_failure_tx, mut endpoint_failures) = mpsc::unbounded_channel();
        let endpoint_task = service.lenses().and_then(|lenses| {
            let runner = service.container_runner.clone()?;
            let updates = lenses.watch(LensCollection::Machines).subscribe();
            let local_machine_id = service.local_machine_id.clone();
            let shutdown = shutdown_tx.subscribe();
            Some(tokio::spawn(async move {
                if let Err(error) =
                    endpoint_network::run(updates, local_machine_id, runner, shutdown).await
                {
                    let _ = endpoint_failure_tx.send(error);
                }
            }))
        });
        let mut connections = JoinSet::new();
        let join_connection_slots = Arc::new(Semaphore::new(MAX_JOIN_DOOR_CONNECTIONS));
        let mut api_accept_failures = 0;
        let mut join_accept_failures = 0;
        let mut api_accept_retry_at = None;
        let mut join_accept_retry_at = None;
        let stop = await_server_stop_with_endpoint(
            shutdown,
            &mut lifecycle_failures,
            &mut endpoint_failures,
        );
        tokio::pin!(stop);

        let serve_result = loop {
            tokio::select! {
                biased;
                result = &mut stop => {
                    if let Err(error) = &result {
                        match error {
                            ApiServerServeError::LensRecoveryExhausted { collection } => {
                                tracing::error!(collection = collection.as_str(), "API lens recovery budget exhausted; stopping API role for supervisor restart");
                            }
                            ApiServerServeError::EndpointNetworkConvergence { detail } => {
                                tracing::error!(%detail, "API endpoint-network convergence failed; stopping API role for supervisor restart");
                            }
                            ApiServerServeError::ListenerAcceptExhausted { listener, detail } => {
                                tracing::error!(?listener, %detail, "API listener accept recovery exhausted; stopping API role for supervisor restart");
                            }
                        }
                    }
                    let _ = shutdown_tx.send(true);
                    break result;
                }
                () = wait_for_accept_retry(api_accept_retry_at), if api_accept_retry_at.is_some() => {
                    api_accept_retry_at = None;
                }
                () = wait_for_accept_retry(join_accept_retry_at), if join_accept_retry_at.is_some() => {
                    join_accept_retry_at = None;
                }
                accepted = listener.accept(), if api_accept_retry_at.is_none() => match accepted {
                    Ok((stream, peer)) => {
                        api_accept_failures = 0;
                        let service = Arc::clone(&service);
                        let shutdown = shutdown_tx.subscribe();
                        connections.spawn(async move {
                            serve_connection(stream, peer, service, shutdown).await;
                        });
                    }
                    Err(error) => {
                        api_accept_failures += 1;
                        if api_accept_failures >= LISTENER_ACCEPT_MAX_CONSECUTIVE_FAILURES {
                            break Err(ApiServerServeError::ListenerAcceptExhausted {
                                listener: ApiListenerKind::MeshApi,
                                detail: error.to_string(),
                            });
                        }
                        let delay = listener_accept_backoff(api_accept_failures);
                        tracing::warn!(error = %error, attempt = api_accept_failures, ?delay, "API listener accept failed");
                        api_accept_retry_at = Some(Instant::now() + delay);
                    }
                },
                accepted = join_door.accept(), if join_accept_retry_at.is_none() => match accepted {
                    Ok(JoinDoorConnection { stream, peer, acceptor }) => {
                        join_accept_failures = 0;
                        match Arc::clone(&join_connection_slots).try_acquire_owned() {
                            Ok(permit) => {
                                let service = Arc::clone(&service);
                                let shutdown = shutdown_tx.subscribe();
                                connections.spawn(async move {
                                    serve_join_connection(
                                        stream,
                                        peer,
                                        acceptor,
                                        service,
                                        shutdown,
                                        permit,
                                    )
                                    .await;
                                });
                            }
                            Err(_) => {
                                tracing::warn!(
                                    maximum = MAX_JOIN_DOOR_CONNECTIONS,
                                    "join door connection bound reached"
                                );
                            }
                        }
                    }
                    Err(error) => {
                        join_accept_failures += 1;
                        if join_accept_failures >= LISTENER_ACCEPT_MAX_CONSECUTIVE_FAILURES {
                            break Err(ApiServerServeError::ListenerAcceptExhausted {
                                listener: ApiListenerKind::JoinDoor,
                                detail: error.to_string(),
                            });
                        }
                        let delay = listener_accept_backoff(join_accept_failures);
                        tracing::warn!(error = %error, attempt = join_accept_failures, ?delay, "join door listener accept failed");
                        join_accept_retry_at = Some(Instant::now() + delay);
                    }
                },
                Some(result) = connections.join_next(), if !connections.is_empty() => {
                    if let Err(error) = result {
                        tracing::warn!(error = %error, "API connection task failed");
                    }
                }
            }
        };

        if tokio::time::timeout(SERVER_SHUTDOWN_GRACE, drain_connections(&mut connections))
            .await
            .is_err()
        {
            connections.abort_all();
            drain_connections(&mut connections).await;
        }
        if let Some(endpoint_task) = endpoint_task {
            endpoint_task.abort();
            let _ = endpoint_task.await;
        }
        match Arc::try_unwrap(service) {
            Ok(service) => service.shutdown().await,
            Err(_) => tracing::warn!("API service retained a connection reference during shutdown"),
        }
        serve_result
    }
}

fn listener_accept_backoff(consecutive_failures: u32) -> Duration {
    let shift = consecutive_failures.saturating_sub(1).min(31);
    LISTENER_ACCEPT_INITIAL_BACKOFF
        .saturating_mul(1_u32 << shift)
        .min(LISTENER_ACCEPT_MAX_BACKOFF)
}

async fn wait_for_accept_retry(retry_at: Option<Instant>) {
    let Some(retry_at) = retry_at else {
        pending().await
    };
    tokio::time::sleep_until(retry_at).await;
}

async fn bind_api_listener(
    config: &ApiRoleConfig,
    enable_host_effects: bool,
) -> Result<(TcpListener, ApiServiceRuntime), ApiServerError> {
    let listen_addr = config.listen_addr();
    let corrosion = CorrosionClient::new(config.corrosion().clone())
        .map_err(ApiServerError::CorrosionClientConfiguration)?;
    let local_machine = if matches!(config.mode(), ApiRoleMode::Ordinary) {
        Some(
            validate_listener_identity(
                &corrosion,
                config.cluster_id(),
                config.local_machine_id(),
                listen_addr,
            )
            .await
            .map_err(ApiServerError::ListenerIdentity)?,
        )
    } else {
        None
    };
    let listener = TcpListener::bind(listen_addr)
        .await
        .map_err(|source| ApiServerError::Bind {
            listen_addr,
            source,
        })?;
    let container_runner = local_machine
        .as_ref()
        .filter(|_| enable_host_effects)
        .map(|machine| {
            let subnet = match &machine.transport {
                MachineTransport::Wireguard { subnet_v4, .. }
                | MachineTransport::Tailscale { subnet_v4, .. } => subnet_v4,
            };
            Arc::new(endpoint_network::runner_for_subnet(subnet))
        });
    let deploy_effects = container_runner.as_ref().map(|runner| {
        Arc::new(super::deploy_effects::DeployHostEffects::new(Arc::clone(
            runner,
        )))
    });
    let controller = Arc::new(super::controller::ControllerStore::new(
        corrosion.clone(),
        config.cluster_id().clone(),
        config.local_machine_id().clone(),
    ));
    let controller_forwarder = Arc::new(
        super::controller_forwarding::ControllerForwarder::new(
            corrosion.clone(),
            config.cluster_id().clone(),
            config.local_machine_id().clone(),
            listen_addr.port(),
            Arc::clone(&controller),
        )
        .map_err(ApiServerError::MeshHttpClient)?,
    );
    let controller_lock = Arc::new(tokio::sync::Mutex::new(()));
    let node_workflows = match &deploy_effects {
        Some(effects) => Some(Arc::new(
            super::node_workflows::NodeWorkflows::open(
                config.workflow_directory(),
                Arc::clone(effects),
            )
            .await
            .map_err(|error| ApiServerError::NodeWorkflows(error.to_string()))?,
        )),
        None => None,
    };
    let (simple_deploy_store, simple_deploy) = match (&deploy_effects, &node_workflows) {
        (Some(effects), Some(workflows)) => {
            let store = Arc::new(super::simple_deploy_store::CorrosionSimpleDeployStore::new(
                corrosion.clone(),
                config.cluster_id().clone(),
            ));
            let hosts = Arc::new(
                super::deploy_hosts::MeshDeployHosts::new(
                    config.local_machine_id().clone(),
                    config.cluster_id().clone(),
                    listen_addr.port(),
                    corrosion.clone(),
                    Arc::clone(&controller),
                    Arc::clone(effects),
                    Arc::clone(workflows),
                )
                .map_err(ApiServerError::MeshHttpClient)?,
            );
            let deploy = Arc::new(super::simple_deploy::SimpleDeploy::new(
                config.local_machine_id().clone(),
                store.clone(),
                hosts,
            ));
            (Some(store), Some(deploy))
        }
        (None, None) => (None, None),
        _ => unreachable!("host effects and node workflows are created together"),
    };
    let runtime = ApiServiceRuntime {
        corrosion,
        cluster_id: config.cluster_id().clone(),
        local_machine_id: config.local_machine_id().clone(),
        listen_addr,
        corrosion_gossip_port: config.corrosion_gossip_port(),
        build: config.build().to_owned(),
        mode: config.mode().clone(),
        upgrade_store: config.upgrade_store().clone(),
        keeper_upgrade_socket_path: config.keeper_upgrade_socket_path().to_path_buf(),
        upgrade_supervisor: config.upgrade_supervisor(),
        controller,
        controller_forwarder,
        controller_lock,
        simple_deploy,
        simple_deploy_store,
        deploy_effects,
        node_workflows,
        container_runner,
    };
    Ok((listener, runtime))
}

pub(super) async fn await_lens_lifecycle_failure(
    lifecycle_failures: &mut mpsc::UnboundedReceiver<LensCollection>,
) -> Option<ApiServerServeError> {
    lifecycle_failures
        .recv()
        .await
        .map(|collection| ApiServerServeError::LensRecoveryExhausted { collection })
}

#[cfg(test)]
pub(super) async fn await_server_stop<Shutdown>(
    shutdown: Shutdown,
    lifecycle_failures: &mut mpsc::UnboundedReceiver<LensCollection>,
) -> Result<(), ApiServerServeError>
where
    Shutdown: Future<Output = ()>,
{
    tokio::pin!(shutdown);
    tokio::select! {
        biased;
        () = &mut shutdown => Ok(()),
        Some(error) = await_lens_lifecycle_failure(lifecycle_failures) => Err(error),
    }
}

async fn await_server_stop_with_endpoint<Shutdown>(
    shutdown: Shutdown,
    lifecycle_failures: &mut mpsc::UnboundedReceiver<LensCollection>,
    endpoint_failures: &mut mpsc::UnboundedReceiver<EndpointNetworkFoldError>,
) -> Result<(), ApiServerServeError>
where
    Shutdown: Future<Output = ()>,
{
    tokio::pin!(shutdown);
    tokio::select! {
        biased;
        () = &mut shutdown => Ok(()),
        Some(error) = await_lens_lifecycle_failure(lifecycle_failures) => Err(error),
        Some(error) = endpoint_failures.recv() => Err(ApiServerServeError::EndpointNetworkConvergence {
            detail: error.to_string(),
        }),
    }
}

async fn drain_connections(connections: &mut JoinSet<()>) {
    while let Some(result) = connections.join_next().await {
        if let Err(error) = result {
            tracing::warn!(error = %error, "API connection task failed during shutdown");
        }
    }
}

async fn serve_connection(
    stream: TcpStream,
    peer: SocketAddr,
    service: Arc<ApiService>,
    mut shutdown: watch::Receiver<bool>,
) {
    let service_for_requests = Arc::clone(&service);
    let shutdown_for_requests = shutdown.clone();
    let mut builder = http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(HTTP_HEADER_READ_TIMEOUT);
    let connection = builder.serve_connection(
        TokioIo::new(stream),
        service_fn(move |request| {
            let service = Arc::clone(&service_for_requests);
            let shutdown = shutdown_for_requests.clone();
            async move { Ok::<_, Infallible>(service.handle(peer, request, shutdown).await) }
        }),
    );
    tokio::pin!(connection);
    tokio::select! {
        result = &mut connection => log_connection_result(result),
        changed = shutdown.changed() => {
            if changed.is_ok() && *shutdown.borrow() {
                connection.as_mut().graceful_shutdown();
                log_connection_result(connection.await);
            }
        }
    }
}

fn log_connection_result(result: Result<(), hyper::Error>) {
    if let Err(error) = result {
        tracing::debug!(error = %error, "API HTTP/1 connection ended");
    }
}

pub(super) async fn load_join_substrate(
    path: &std::path::Path,
) -> Result<JoinMachineSubstrate, ApiServerError> {
    let metadata =
        tokio::fs::metadata(path)
            .await
            .map_err(|source| ApiServerError::JoinSubstrateRead {
                path: path.to_path_buf(),
                source,
            })?;
    if metadata.len() > MAX_JOIN_SUBSTRATE_BYTES as u64 {
        return Err(ApiServerError::JoinSubstrateTooLarge {
            path: path.to_path_buf(),
            limit: MAX_JOIN_SUBSTRATE_BYTES,
        });
    }
    let bytes =
        tokio::fs::read(path)
            .await
            .map_err(|source| ApiServerError::JoinSubstrateRead {
                path: path.to_path_buf(),
                source,
            })?;
    if bytes.len() > MAX_JOIN_SUBSTRATE_BYTES {
        return Err(ApiServerError::JoinSubstrateTooLarge {
            path: path.to_path_buf(),
            limit: MAX_JOIN_SUBSTRATE_BYTES,
        });
    }
    serde_json::from_slice::<JoinMachineSubstrate>(&bytes).map_err(|error| {
        ApiServerError::JoinSubstrateInvalid {
            path: path.to_path_buf(),
            detail: error.to_string(),
        }
    })
}

/// Runs the API role using its supervisor-loaded environment file.
pub async fn run_from_environment() -> Result<(), ApiRoleRuntimeError> {
    let config = ApiRoleConfig::from_environment().map_err(ApiRoleRuntimeError::Configuration)?;
    let server = ApiServer::bind(config)
        .await
        .map_err(ApiRoleRuntimeError::Server)?;
    server
        .serve(wait_for_process_shutdown())
        .await
        .map_err(ApiRoleRuntimeError::Serve)
}

async fn wait_for_process_shutdown() {
    #[cfg(unix)]
    {
        let terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
        let interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt());
        match (terminate, interrupt) {
            (Ok(mut terminate), Ok(mut interrupt)) => {
                tokio::select! {
                    _ = terminate.recv() => {}
                    _ = interrupt.recv() => {}
                }
            }
            (Err(error), _) | (_, Err(error)) => {
                tracing::warn!(error = %error, "could not install API shutdown signal handler");
                if let Err(error) = tokio::signal::ctrl_c().await {
                    tracing::warn!(error = %error, "could not wait for API shutdown signal");
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(error = %error, "could not wait for API shutdown signal");
        }
    }
}

/// A bounded API role startup failure.
#[derive(Debug, thiserror::Error)]
pub enum ApiServerError {
    #[error("could not build the local Corrosion client: {0}")]
    CorrosionClientConfiguration(crate::corrosion::CorrosionClientConfigError),
    #[error("could not build the bounded mesh HTTP client: {0}")]
    MeshHttpClient(reqwest::Error),
    #[error("could not open node-local durable workflows: {0}")]
    NodeWorkflows(String),
    #[error(transparent)]
    ListenerIdentity(ApiListenerValidationError),
    #[error(transparent)]
    JoinDoor(JoinDoorBindError),
    #[error("could not read join substrate {path:?}: {source}")]
    JoinSubstrateRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("join substrate {path:?} exceeds the {limit}-byte limit")]
    JoinSubstrateTooLarge { path: PathBuf, limit: usize },
    #[error("join substrate {path:?} is invalid: {detail}")]
    JoinSubstrateInvalid { path: PathBuf, detail: String },
    #[error("could not bind API listener {listen_addr}: {source}")]
    Bind {
        listen_addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
}

/// A bounded API role serving failure that requires supervisor restart.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiServerServeError {
    #[error("API lens {collection:?} exhausted its Corrosion recovery budget")]
    LensRecoveryExhausted { collection: LensCollection },
    #[error("API endpoint-network convergence failed: {detail}")]
    EndpointNetworkConvergence { detail: String },
    #[error("{listener:?} listener exhausted accept recovery: {detail}")]
    ListenerAcceptExhausted {
        listener: ApiListenerKind,
        detail: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiListenerKind {
    MeshApi,
    JoinDoor,
}

/// A bounded API role process failure.
#[derive(Debug, thiserror::Error)]
pub enum ApiRoleRuntimeError {
    #[error(transparent)]
    Configuration(ApiRoleConfigError),
    #[error(transparent)]
    Server(ApiServerError),
    #[error(transparent)]
    Serve(ApiServerServeError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doorless_integration_fixture_cannot_expose_partial_admission() {
        let runtime = JoinDoorRuntime::DoorlessIntegrationFixture;

        assert!(runtime.admission().is_none());
    }

    #[test]
    fn listener_accept_backoff_is_bounded_and_increasing() {
        assert_eq!(listener_accept_backoff(1), Duration::from_millis(100));
        assert_eq!(listener_accept_backoff(2), Duration::from_millis(200));
        assert_eq!(listener_accept_backoff(8), Duration::from_secs(5));
        assert_eq!(listener_accept_backoff(u32::MAX), Duration::from_secs(5));
    }
}
