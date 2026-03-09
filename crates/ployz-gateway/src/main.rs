mod state;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use clap::Parser;
use pingora::prelude::*;
use ployz::config::{Affordances, default_data_dir};
use ployz_routing::{BackendView, GatewaySnapshot, match_http_route};
use thiserror::Error;
use tracing::info;

use crate::state::{GatewayConfig, load_initial_snapshot, spawn_sync_thread};

const HTTP_LISTEN_ADDR: &str = "0.0.0.0:80";

#[derive(Parser, Debug, Clone)]
#[command(name = "ployz-gateway", about = "Ployz HTTP gateway")]
struct Cli {
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[arg(long)]
    network: Option<String>,
}

#[derive(Clone)]
struct SharedSnapshot {
    inner: Arc<RwLock<Arc<GatewaySnapshot>>>,
}

impl SharedSnapshot {
    fn new(snapshot: GatewaySnapshot) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Arc::new(snapshot))),
        }
    }

    fn load(&self) -> Arc<GatewaySnapshot> {
        self.inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn replace(&self, snapshot: GatewaySnapshot) {
        *self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::new(snapshot);
    }
}

struct GatewayApp {
    snapshot: SharedSnapshot,
    rr_state: Arc<Mutex<HashMap<String, usize>>>,
}

#[derive(Default)]
struct RequestCtx {
    backend: Option<BackendView>,
}

impl GatewayApp {
    fn new(snapshot: SharedSnapshot) -> Self {
        Self {
            snapshot,
            rr_state: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn select_backend(&self, route_id: &str, backends: &[BackendView]) -> Option<BackendView> {
        let Some(len) = (!backends.is_empty()).then_some(backends.len()) else {
            return None;
        };
        let mut rr_state = self
            .rr_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let next = rr_state.entry(route_id.to_string()).or_insert(0);
        let backend = backends[*next % len].clone();
        *next = next.wrapping_add(1);
        Some(backend)
    }
}

#[async_trait]
impl ProxyHttp for GatewayApp {
    type CTX = RequestCtx;

    fn new_ctx(&self) -> Self::CTX {
        RequestCtx::default()
    }

    async fn request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<bool> {
        let request = session.req_header();
        let host = request
            .headers
            .get("host")
            .and_then(|value| value.to_str().ok());
        let path = request.uri.path();
        let snapshot = self.snapshot.load();
        let Some(route) = match_http_route(&snapshot, host, path) else {
            session.respond_error(404).await?;
            return Ok(true);
        };
        let Some(backend) = self.select_backend(&route.route_id, &route.backends) else {
            session.respond_error(503).await?;
            return Ok(true);
        };
        ctx.backend = Some(backend);
        Ok(false)
    }

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        let Some(backend) = ctx.backend.as_ref() else {
            return Err(Error::explain(
                ErrorType::HTTPStatus(503),
                "backend was not selected",
            ));
        };
        Ok(Box::new(HttpPeer::new(backend.address, false, String::new())))
    }
}

#[derive(Debug, Error)]
enum CliError {
    #[error("failed to build gateway config: {0}")]
    Config(String),
    #[error("gateway bootstrap failed: {0}")]
    Bootstrap(String),
}

fn main() -> std::result::Result<(), CliError> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    let gateway_config = build_gateway_config(&cli)?;
    let initial_snapshot = load_initial_snapshot(&gateway_config)
        .map_err(|err| CliError::Bootstrap(err.to_string()))?;
    let shared_snapshot = SharedSnapshot::new(initial_snapshot);
    spawn_sync_thread(gateway_config.clone(), shared_snapshot.clone())
        .map_err(|err| CliError::Bootstrap(err.to_string()))?;

    let mut server =
        Server::new(None).map_err(|err| CliError::Bootstrap(format!("server init: {err}")))?;
    server.bootstrap();

    let mut service = http_proxy_service(&server.configuration, GatewayApp::new(shared_snapshot));
    service.add_tcp(HTTP_LISTEN_ADDR);
    server.add_service(service);

    info!(listen = HTTP_LISTEN_ADDR, "gateway listening");
    server.run_forever()
}

fn build_gateway_config(cli: &Cli) -> std::result::Result<GatewayConfig, CliError> {
    let data_dir = match &cli.data_dir {
        Some(data_dir) => data_dir.clone(),
        None => default_data_dir(&Affordances::detect()),
    };
    let network = match &cli.network {
        Some(network) => network.clone(),
        None => resolve_active_network(&data_dir)
            .ok_or_else(|| CliError::Config("no active network marker was found".into()))?,
    };
    Ok(GatewayConfig { data_dir, network })
}

fn resolve_active_network(data_dir: &PathBuf) -> Option<String> {
    std::fs::read_to_string(data_dir.join("active_network"))
        .ok()
        .map(|body| body.trim().to_string())
        .filter(|body| !body.is_empty())
}
