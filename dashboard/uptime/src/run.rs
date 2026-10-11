//! The three process modes, wired from a parsed [`Mode`], and the shutdown signal every
//! long-running task watches.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use rustls::crypto::CryptoProvider;
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::agent::probe::Prober;
use crate::agent::run::AgentRun;
use crate::config::{AgentConfig, ControlConfig, Mode};
use crate::control::plane::ControlPlane;
use crate::link::LocalLink;
use crate::link::http::{Gate, HttpLink, serve};

/// A boot or wiring failure the process reports and exits on. Its message names variables and
/// components, never their values.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Fatal(pub String);

/// Fires once. Clones share the signal, and a dropped [`Trigger`] counts as fired so nothing can
/// outlive the process that owned it.
#[derive(Clone, Debug)]
pub struct Shutdown(watch::Receiver<bool>);

#[derive(Debug)]
pub struct Trigger(watch::Sender<bool>);

impl Shutdown {
    #[must_use]
    pub fn new() -> (Trigger, Shutdown) {
        let (tx, rx) = watch::channel(false);
        (Trigger(tx), Shutdown(rx))
    }

    pub async fn wait(&self) {
        let mut rx = self.0.clone();
        let _ = rx.wait_for(|fired| *fired).await;
    }

    #[must_use]
    pub fn fired(&self) -> bool {
        *self.0.borrow() || self.0.has_changed().is_err()
    }
}

impl Trigger {
    pub fn fire(&self) {
        self.0.send_replace(true);
    }
}

/// How long `all` waits for the engine to settle after the agent has stopped.
const ENGINE_STOP_BOUND: Duration = Duration::from_secs(30);

/// # Errors
/// Postgres refuses the first connection, the listen address is taken, the crypto provider
/// supports no TLS version the prober needs, or a task panicked.
pub async fn main(
    mode: Mode,
    crypto: Arc<CryptoProvider>,
    shutdown: Shutdown,
) -> Result<(), Fatal> {
    match mode {
        Mode::Control(config) => control(config, crypto, shutdown).await,
        Mode::Agent(config) => agent(config, crypto, shutdown).await,
        Mode::All(control, agent) => {
            let listen = control.listen;
            let (plane_stop, plane_shutdown) = Shutdown::new();
            let plane = ControlPlane::start(control, crypto.clone(), plane_shutdown).await?;
            let prober = Arc::new(prober(&agent, crypto)?);
            all(plane, plane_stop, prober, agent.run, listen, shutdown).await
        }
    }
}

/// # Errors
/// As [`main`].
pub async fn control(
    config: ControlConfig,
    crypto: Arc<CryptoProvider>,
    shutdown: Shutdown,
) -> Result<(), Fatal> {
    let listen = config.listen;
    let Some(token) = config.agent_token.clone() else {
        return Err(Fatal("Control mode needs UPTIME_AGENT_TOKEN.".to_owned()));
    };
    let gate = Gate {
        token,
        regions: config.region_codes(),
    };
    let plane = Arc::new(ControlPlane::start(config, crypto, shutdown.clone()).await?);
    let listener = bind(listen).await?;
    serve(listener, plane.clone(), Some(gate), shutdown).await;
    plane.stopped().await;
    Ok(())
}

/// # Errors
/// As [`main`].
pub async fn agent(
    config: AgentConfig,
    crypto: Arc<CryptoProvider>,
    shutdown: Shutdown,
) -> Result<(), Fatal> {
    let link = Arc::new(HttpLink::new(
        &config.control_url,
        config.token.clone(),
        config.region.clone(),
    )?);
    let prober = Arc::new(prober(&config, crypto)?);
    crate::agent::run::run(link, prober, config.run, shutdown).await
}

/// One process that is both control plane and its only agent, linked by a function call. The
/// HTTP listener still answers `/ready` for the platform. The plane watches `plane_stop`, not
/// `shutdown`, so it keeps taking reports until the agent has finished its in-flight probes.
///
/// # Errors
/// As [`main`].
pub async fn all(
    plane: ControlPlane,
    plane_stop: Trigger,
    prober: Arc<Prober>,
    agent_run: AgentRun,
    listen: SocketAddr,
    shutdown: Shutdown,
) -> Result<(), Fatal> {
    let plane = Arc::new(plane);
    let Some(region) = plane.regions().first().cloned() else {
        return Err(Fatal("UPTIME_REGIONS names no region.".to_owned()));
    };
    let listener = bind(listen).await?;
    let server = tokio::spawn(serve(listener, plane.clone(), None, shutdown.clone()));
    let link = Arc::new(LocalLink::new(plane.clone(), region));
    let agent = crate::agent::run::run(link, prober, agent_run, shutdown).await;
    plane_stop.fire();
    server
        .await
        .map_err(|e| Fatal(format!("The HTTP server task panicked: {e}.")))?;
    if tokio::time::timeout(ENGINE_STOP_BOUND, plane.stopped())
        .await
        .is_err()
    {
        tracing::warn!("the engine did not settle within the shutdown bound");
    }
    agent
}

fn prober(config: &AgentConfig, crypto: Arc<CryptoProvider>) -> Result<Prober, Fatal> {
    Prober::new(config.probe, crypto, config.agent.clone())
        .map_err(|e| Fatal(format!("The TLS client cannot be built: {e}.")))
}

async fn bind(listen: SocketAddr) -> Result<TcpListener, Fatal> {
    TcpListener::bind(listen)
        .await
        .map_err(|e| Fatal(format!("Cannot listen on {listen}: {e}.")))
}
