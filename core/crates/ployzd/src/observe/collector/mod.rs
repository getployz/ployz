//! The Collector: an OpenTelemetry Collector the daemon runs on a Machine connected to
//! Cloud, reading every Ployz container's `json-file` log, masking Sealed values and
//! shipping the lines to Cloud's OTLP route.
//!
//! The daemon owns it like any helper container. Its config, the mask of Sealed values
//! and its token live under `/run`, so a Machine that reboots, or one Cloud no longer
//! has a push target for, keeps none of them.

mod config;
mod mask;
mod token;

use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
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
    sealed_env::{Unseal as _, record_from_label},
};

use config::Paths;

const TICK: Duration = Duration::from_secs(10);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(10);
const HEALTH_RETRY: Duration = Duration::from_millis(200);
const DEFAULT_TOKEN_SECONDS: i64 = 3_600;
const TOKEN_SECONDS_ENV: &str = "PLOYZ_OTLP_TOKEN_SECONDS";
const PRIVATE_FILE: u32 = 0o600;

/// The Collector of one daemon, or nothing where the daemon runs no containers.
#[derive(Clone, Default)]
pub(crate) struct Collector(Option<Arc<Inner>>);

struct Inner {
    docker: LocalDocker,
    data_dir: PathBuf,
    machine_root: PathBuf,
    token_seconds: i64,
    state: Mutex<State>,
}

struct State {
    values: SealedValues,
    refresh_token_at: Option<i64>,
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
            data_dir,
            machine_root,
            token_seconds,
            state: Mutex::new(State {
                values: SealedValues::none(),
                refresh_token_at: None,
                torn_down: false,
            }),
        })))
    }

    /// Returns once the Collector masks the Sealed values of a container about to start,
    /// so its first line is already masked. Where the Collector cannot be brought up to
    /// date it is removed first, so the container's output waits on its own log file.
    pub(crate) async fn cover(&self, container_labels: &HashMap<String, String>) {
        let Some(inner) = &self.0 else { return };
        if !container_labels.contains_key(labels::SEALED_ENV) {
            return;
        }
        let values = match inner.values_of(container_labels) {
            Ok(values) => values,
            Err(error) => {
                tracing::warn!(error = %ployz_core::error_chain::inline(&error), "could not read a container's Sealed values for the Collector");
                return;
            }
        };
        let mut state = inner.state.lock().await;
        if let Err(error) = inner.converge(&mut state, values).await {
            tracing::warn!(error = %ployz_core::error_chain::inline(&error), "the Collector could not take a new Sealed value, so it is stopped");
            if let Err(error) = inner.service().remove().await {
                tracing::warn!(error = %error, "could not stop the Collector");
            }
        }
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

    async fn tick(&self) -> Duration {
        let mut state = self.state.lock().await;
        let found = self.live_values().await;
        if let Err(error) = self.converge(&mut state, found).await {
            tracing::warn!(error = %ployz_core::error_chain::inline(&error), "the Collector is not converged");
        }
        state.refresh_token_at.map_or(TICK, |at| {
            let until = u64::try_from(at - unix_seconds()).unwrap_or(0);
            TICK.min(Duration::from_secs(until.max(1)))
        })
    }

    async fn live_values(&self) -> SealedValues {
        let mut found = SealedValues::none();
        let containers = match self.docker.managed_containers().await {
            Ok(containers) => containers,
            Err(error) => {
                tracing::warn!(error = %ployz_core::error_chain::inline(&error), "could not list containers for the Collector");
                return found;
            }
        };
        for (_, container_labels) in containers {
            match self.values_of(&container_labels) {
                Ok(values) => found.merge(values),
                Err(error) => {
                    tracing::warn!(error = %ployz_core::error_chain::inline(&error), "could not read a container's Sealed values for the Collector");
                }
            }
        }
        found
    }

    async fn converge(&self, state: &mut State, found: SealedValues) -> Result<(), Error> {
        let service = self.service();
        let target = store::identity(&self.data_dir).and_then(|identity| {
            let target = identity.push_target.clone()?;
            Some((identity, target))
        });
        let Some((identity, target)) = target else {
            if !state.torn_down {
                service.remove().await?;
                remove_dir(&self.machine_root.join("run/ployz/collector"))?;
                state.values = SealedValues::none();
                state.refresh_token_at = None;
                state.torn_down = true;
            }
            return Ok(());
        };
        state.torn_down = false;
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
        write_if_changed(&paths.base_file(), config::BASE.as_bytes())?;
        write_if_changed(&paths.mask_file(), mask.as_bytes())?;
        let desired = DesiredContainer::new(
            crate::images::COLLECTOR,
            config::body(&paths, &config::endpoint(&target)),
            &[config::BASE.as_bytes(), mask.as_bytes()],
        );
        match service.ensure(desired).await? {
            Ensure::Keep => Ok(()),
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
