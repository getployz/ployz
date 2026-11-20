//! The Collector: an OpenTelemetry Collector the daemon runs on a Machine connected to
//! Cloud, reading every Ployz container's `json-file` log, masking Sealed values and
//! shipping the lines to Cloud's OTLP route.
//!
//! The daemon owns it like any helper container. Its config, the mask of Sealed values
//! and its token live under `/run`, so a Machine that reboots, or one Cloud no longer
//! has a push target for, keeps none of them.

mod config;
mod mask;
mod requests;
mod token;

use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};

use ployz_core::{
    labels,
    private_fs::{Existing, create_private_dir},
    scrub::SealedValues,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::{
    docker::{DesiredContainer, Ensure, Error, LocalDocker, ManagedService},
    filesystem::atomic_write,
    machine::store,
    observe::{salts::Salts, ship},
    sealed_env::{Unseal as _, record_from_label},
};

use config::Paths;

const TICK: Duration = Duration::from_secs(10);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(10);
const HEALTH_RETRY: Duration = Duration::from_millis(200);
const DEFAULT_TOKEN_SECONDS: i64 = 3_600;
const TOKEN_SECONDS_ENV: &str = "PLOYZ_OTLP_TOKEN_SECONDS";
const PRIVATE_FILE: u32 = 0o600;
const SALTS_FILE: &str = "analytics-salts.json";
const SALTS_RETRY_SECONDS: i64 = 30;
const SALTS_TIMEOUT: Duration = Duration::from_secs(10);

/// The Collector of one daemon, or nothing where the daemon runs no containers.
#[derive(Clone, Default)]
pub(crate) struct Collector(Option<Arc<Inner>>);

struct Inner {
    docker: LocalDocker,
    data_dir: PathBuf,
    machine_root: PathBuf,
    token_seconds: i64,
    salts: Salts,
    salts_retry_at: AtomicI64,
    http: reqwest::Client,
    state: Mutex<State>,
}

struct Connected {
    identity: store::MachineIdentity,
    target: reqwest::Url,
}

struct State {
    values: SealedValues,
    inventoried: bool,
    refresh_token_at: Option<i64>,
    requests_loaded: Option<Vec<u8>>,
    torn_down: bool,
}

impl Collector {
    pub(crate) fn new(docker: LocalDocker, data_dir: PathBuf, machine_root: PathBuf) -> Self {
        let token_seconds = std::env::var(TOKEN_SECONDS_ENV)
            .ok()
            .and_then(|seconds| seconds.parse::<i64>().ok())
            .map_or(DEFAULT_TOKEN_SECONDS, |seconds| seconds.clamp(4, 86_400));
        Self(Some(Arc::new(Inner {
            docker,
            salts: Salts::open(data_dir.join(SALTS_FILE), unix_seconds()),
            data_dir,
            machine_root,
            token_seconds,
            salts_retry_at: AtomicI64::new(0),
            http: reqwest::Client::builder()
                .timeout(SALTS_TIMEOUT)
                .build()
                .unwrap_or_default(),
            state: Mutex::new(State {
                values: SealedValues::none(),
                inventoried: false,
                refresh_token_at: None,
                requests_loaded: None,
                torn_down: false,
            }),
        })))
    }

    /// Returns once the Collector masks the Sealed values of a container about to start,
    /// so its first line is already masked. Where the Collector cannot be brought up to
    /// date it is removed, so the container's output waits on its own log file. Where it
    /// cannot be removed either, the start fails rather than leave a Collector running
    /// without the container's values.
    pub(crate) async fn cover(
        &self,
        container_labels: &HashMap<String, String>,
    ) -> Result<(), Error> {
        let Some(inner) = &self.0 else { return Ok(()) };
        if !container_labels.contains_key(labels::SEALED_ENV) {
            return Ok(());
        }
        let mut state = inner.state.lock().await;
        let Err(error) = inner.take_values_of(&mut state, container_labels).await else {
            return Ok(());
        };
        tracing::warn!(error = %ployz_core::error_chain::inline(&error), "the Collector could not take a new Sealed value, so it is stopped");
        state.inventoried = false;
        inner.service().remove().await?;
        Ok(())
    }

    /// Keeps the Collector converged until `shutdown`, leaving it running.
    pub(crate) async fn run(&self, shutdown: CancellationToken) -> io::Result<()> {
        let Some(inner) = &self.0 else {
            shutdown.cancelled().await;
            return Ok(());
        };
        loop {
            let wait = inner.tick().await;
            tokio::select! {
                () = shutdown.cancelled() => return Ok(()),
                () = tokio::time::sleep(wait) => {}
            }
        }
    }
}

impl Inner {
    fn service(&self) -> ManagedService<LocalDocker> {
        ManagedService::new(self.docker.clone(), config::NAME)
            .with_stop_grace(config::STOP_GRACE_SECONDS)
    }

    fn values_of(&self, container_labels: &HashMap<String, String>) -> Result<SealedValues, Error> {
        let Some(record) =
            record_from_label(container_labels.get(labels::SEALED_ENV).map(String::as_str))?
        else {
            return Ok(SealedValues::none());
        };
        Ok(record.secrets(Some(&self.data_dir))?)
    }

    fn target(&self) -> Option<Connected> {
        let identity = store::identity(&self.data_dir)?;
        let target = identity.push_target.clone()?;
        Some(Connected { identity, target })
    }

    async fn take_values_of(
        &self,
        state: &mut State,
        container_labels: &HashMap<String, String>,
    ) -> Result<(), Error> {
        let Some(connected) = self.target() else {
            return self.tear_down(state).await;
        };
        let mut found = self.values_of(container_labels)?;
        if !state.inventoried {
            found.merge(self.live_values().await?);
        }
        self.converge(state, &connected, found).await
    }

    /// Runs outside the state lock, so a slow Cloud never holds up a container start.
    async fn refresh_salts(&self) {
        let now = unix_seconds();
        if !self.salts.stale(now) || now < self.salts_retry_at.load(Ordering::Relaxed) {
            return;
        }
        let Some(pusher) = ship::push_target(&self.data_dir) else {
            return;
        };
        match pusher.analytics_salts(&self.http).await {
            Ok(body) => self.salts.accept(&body, now),
            Err(error) => tracing::debug!(%error, "cannot fetch the analytics salts"),
        }
        if self.salts.stale(now) {
            self.salts_retry_at
                .store(now + SALTS_RETRY_SECONDS, Ordering::Relaxed);
        }
    }

    async fn tick(&self) -> Duration {
        self.refresh_salts().await;
        let mut state = self.state.lock().await;
        let converged = match self.target() {
            None => self.tear_down(&mut state).await,
            Some(connected) => match self.live_values().await {
                Ok(found) => self.converge(&mut state, &connected, found).await,
                Err(error) => Err(error),
            },
        };
        if let Err(error) = converged {
            tracing::warn!(error = %ployz_core::error_chain::inline(&error), "the Collector is not converged");
            return TICK;
        }
        state.refresh_token_at.map_or(TICK, |at| {
            let until = u64::try_from(at - unix_seconds()).unwrap_or(0);
            TICK.min(Duration::from_secs(until.max(1)))
        })
    }

    /// Every Sealed value of every managed container, or an error where any of them
    /// cannot be listed or opened, since a mask missing one would let it through.
    async fn live_values(&self) -> Result<SealedValues, Error> {
        let mut found = SealedValues::none();
        for (_, container_labels) in self.docker.managed_containers().await? {
            found.merge(self.values_of(&container_labels)?);
        }
        Ok(found)
    }

    /// Stopping needs no values, so a Machine with no push target tears the Collector
    /// down once, whatever a Sealed record would say.
    async fn tear_down(&self, state: &mut State) -> Result<(), Error> {
        if state.torn_down {
            return Ok(());
        }
        self.service().remove().await?;
        remove_dir(&self.machine_root.join("run/ployz/collector"))?;
        state.values = SealedValues::none();
        state.inventoried = false;
        state.refresh_token_at = None;
        state.requests_loaded = None;
        state.torn_down = true;
        Ok(())
    }

    /// Brings the Collector to what `found` says, where `found` is every Sealed value
    /// there is, so the mask it writes is never missing one.
    async fn converge(
        &self,
        state: &mut State,
        connected: &Connected,
        found: SealedValues,
    ) -> Result<(), Error> {
        let Connected { identity, target } = connected;
        let service = self.service();
        state.torn_down = false;
        state.inventoried = true;
        state.values.merge(found);
        let docker_root = self.docker.docker_root().await?;
        let paths = Paths::under(&self.machine_root, &self.data_dir, &docker_root);
        for dir in [&paths.config, &paths.token, &paths.state] {
            create_private_dir(dir, Existing::Tighten).map_err(io::Error::from)?;
        }
        let now = unix_seconds();
        if state.refresh_token_at.is_none_or(|at| now >= at) || !paths.token_file().exists() {
            let token = token::mint(
                &identity.management_secret,
                &identity.id,
                now,
                self.token_seconds,
            );
            atomic_write(&paths.token_file(), token.as_bytes(), PRIVATE_FILE)?;
            state.refresh_token_at = Some(now + self.token_seconds / 2);
        }
        let mask = mask::fragment(&state.values);
        if let Err(error) = self.salts.prune(now) {
            tracing::warn!(%error, "cannot forget closed salts");
        }
        let requests = requests::fragment(&self.salts.sites());
        fs::create_dir_all(&paths.requests)?;
        write_if_changed(&paths.base_file(), config::BASE.as_bytes())?;
        write_if_changed(&paths.mask_file(), mask.as_bytes())?;
        write_if_changed(&paths.requests_file(), requests.as_bytes())?;
        let desired = DesiredContainer::new(
            crate::images::COLLECTOR,
            config::body(&paths, &config::endpoint(target)),
            &[config::BASE.as_bytes(), mask.as_bytes()],
        );
        let loaded = state.requests_loaded.as_deref() == Some(requests.as_bytes());
        let ensured = service.ensure(desired).await?;
        state.requests_loaded = Some(requests.into_bytes());
        match ensured {
            Ensure::Keep if loaded => Ok(()),
            // The requests config is out of the spec digest, so a new salt reloads the running
            // Collector, which keeps its file offsets, instead of replacing it.
            Ensure::Keep => {
                service.signal("SIGHUP").await?;
                wait_healthy().await
            }
            Ensure::Create | Ensure::Replace | Ensure::Start => wait_healthy().await,
        }
    }
}

fn unix_seconds() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs()),
    )
    .unwrap_or(i64::MAX)
}

fn write_if_changed(path: &Path, contents: &[u8]) -> io::Result<()> {
    if fs::read(path).is_ok_and(|current| current == contents) {
        return Ok(());
    }
    atomic_write(path, contents, PRIVATE_FILE)
}

fn remove_dir(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

async fn wait_healthy() -> Result<(), Error> {
    let url = format!("http://{}/", config::HEALTH_ADDRESS);
    let client = reqwest::Client::new();
    tokio::time::timeout(HEALTH_TIMEOUT, async {
        loop {
            if client
                .get(&url)
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                return;
            }
            tokio::time::sleep(HEALTH_RETRY).await;
        }
    })
    .await
    .map_err(|_| {
        Error::Io(io::Error::new(
            io::ErrorKind::TimedOut,
            "the Collector did not report healthy",
        ))
    })
}
