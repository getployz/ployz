//! Runs the Corrosion agent in a Docker container the daemon creates and keeps.

use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    future::Future,
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

use bollard::{
    Docker,
    models::{
        ContainerCreateBody, HostConfig, HostConfigLogConfig, Mount, MountType, RestartPolicy,
        RestartPolicyNameEnum,
    },
};
use serde::Serialize;

use crate::{
    docker::{DesiredContainer, Ensure, ManagedService},
    filesystem::atomic_write,
};
use ployz_core::{CORROSION_API_PORT, CORROSION_GOSSIP_PORT};

use super::{AdminClient, ApiClient, Error, ReplicatedStore, Statement};

pub const IMAGE: &str = "ghcr.io/unlabs-dev/corrosion:2026.6.15";
pub const DEFAULT_CONTAINER_NAME: &str = "ployz-corrosion";
const TOKEN_FILE: &str = ".api-token";
const SCHEMA: &str = include_str!("schema.sql");
const START_TIMEOUT: Duration = Duration::from_secs(4 * 60 + 30);
// A daemon restart no longer restarts a running Corrosion, so a kept container that stays
// silent this long is treated as wedged and replaced once, as every restart used to do.
const KEPT_READY_TIMEOUT: Duration = Duration::from_secs(60);

/// Where Corrosion keeps its files under the Ployz data and run directories.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorrosionPaths {
    pub data_dir: PathBuf,
    pub run_dir: PathBuf,
}

impl CorrosionPaths {
    #[must_use]
    pub fn under(ployz_data_dir: &Path, ployz_run_dir: &Path) -> Self {
        Self {
            data_dir: ployz_data_dir.join("corrosion"),
            run_dir: ployz_run_dir.join("corrosion"),
        }
    }

    #[must_use]
    pub fn token_file(&self) -> PathBuf {
        self.data_dir.join(TOKEN_FILE)
    }

    #[must_use]
    pub fn admin_socket(&self) -> PathBuf {
        self.run_dir.join("admin.sock")
    }
}

pub struct CorrosionConfig {
    paths: CorrosionPaths,
    api_address: SocketAddr,
    gossip_address: SocketAddr,
    container_name: String,
    // TODO: every peer remains a bootstrap target; add partial selection only after a product decision.
    bootstrap: Vec<SocketAddr>,
}

impl CorrosionConfig {
    #[must_use]
    pub fn new(
        data_dir: impl Into<PathBuf>,
        run_dir: impl Into<PathBuf>,
        api_address: SocketAddr,
        gossip_address: SocketAddr,
        container_name: impl Into<String>,
    ) -> Self {
        Self {
            paths: CorrosionPaths {
                data_dir: data_dir.into(),
                run_dir: run_dir.into(),
            },
            api_address,
            gossip_address,
            container_name: container_name.into(),
            bootstrap: Vec::new(),
        }
    }

    #[must_use]
    pub fn local(data_dir: impl Into<PathBuf>, run_dir: impl Into<PathBuf>) -> Self {
        Self::new(
            data_dir,
            run_dir,
            SocketAddr::from((Ipv4Addr::LOCALHOST, CORROSION_API_PORT)),
            SocketAddr::from((Ipv4Addr::LOCALHOST, CORROSION_GOSSIP_PORT)),
            DEFAULT_CONTAINER_NAME,
        )
    }

    #[must_use]
    pub fn with_bootstrap(mut self, peers: impl IntoIterator<Item = SocketAddr>) -> Self {
        self.bootstrap = peers.into_iter().collect();
        self
    }

    pub async fn start(&self) -> Result<RunningCorrosion, Error> {
        bounded_start(async {
            let files = self.install()?;
            let api = ApiClient::new(self.api_address, &files.token)?;
            let admin = AdminClient::new(self.paths.admin_socket());
            let docker = Docker::connect_with_socket_defaults()?;
            let service = DockerService {
                service: ManagedService::host(docker, self.container_name.clone(), IMAGE),
                data_dir: self.paths.data_dir.clone(),
                run_dir: self.paths.run_dir.clone(),
            };
            let ensured = service.start(&files).await?;
            wait_ready_or_replace(
                ensured == Ensure::Keep,
                || async { api.query(Statement::new("SELECT 1", [])).await.is_ok() },
                async {
                    service.service.remove().await?;
                    service.start(&files).await?;
                    Ok(())
                },
            )
            .await?;
            Ok(RunningCorrosion {
                store: ReplicatedStore::new(api),
                admin,
                service,
            })
        })
        .await
    }

    fn install(&self) -> Result<InstalledFiles, Error> {
        create_private_dir(&self.paths.data_dir)?;
        create_private_dir(&self.paths.run_dir)?;
        let token = load_or_create_token(&self.paths.token_file())?;
        let schema_path = self.paths.data_dir.join("schema.sql");
        // TODO: Ployz deliberately has no replicated Store Schema/value evolution contract;
        // mixed-version Machines may omit or fail to read newer data. Do not add migrations or version gates.
        atomic_write(&schema_path, SCHEMA.as_bytes(), 0o644)?;

        let config = FileConfig {
            db: DbConfig {
                path: self.paths.data_dir.join("store.db"),
                schema_paths: vec![schema_path],
            },
            gossip: GossipConfig {
                addr: self.gossip_address,
                bootstrap: self.sorted_bootstrap(),
                plaintext: true,
            },
            api: ApiConfig {
                addr: self.api_address,
                authz: AuthzConfig {
                    bearer_token: token.clone(),
                },
            },
            admin: AdminConfig {
                path: self.paths.admin_socket(),
            },
        };
        let encoded = toml::to_string(&config)?;
        // TODO: retain the loose Corrosion-owned config boundary until ownership is decided.
        atomic_write(
            &self.paths.data_dir.join("config.toml"),
            encoded.as_bytes(),
            0o600,
        )?;
        Ok(InstalledFiles {
            token,
            config: encoded.into_bytes(),
        })
    }

    // Peer order varies between record reads; an unsorted list would replace Corrosion on restart.
    fn sorted_bootstrap(&self) -> Vec<String> {
        let mut peers = self.bootstrap.clone();
        peers.sort_unstable();
        peers.dedup();
        peers.iter().map(ToString::to_string).collect()
    }
}

struct InstalledFiles {
    token: String,
    config: Vec<u8>,
}

/// Removes the Corrosion container and its run directory without a running
/// handle, for a reset the daemon did not finish before it stopped.
pub async fn remove_retained(run_dir: &Path) -> Result<(), Error> {
    let docker = Docker::connect_with_socket_defaults()?;
    remove(
        &ManagedService::host(docker, DEFAULT_CONTAINER_NAME, IMAGE),
        run_dir,
    )
    .await
}

async fn remove(service: &ManagedService, run_dir: &Path) -> Result<(), Error> {
    service.remove().await?;
    match fs::remove_dir_all(run_dir) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

pub struct RunningCorrosion {
    store: ReplicatedStore,
    admin: AdminClient,
    service: DockerService,
}

impl RunningCorrosion {
    #[must_use]
    pub fn store(&self) -> &ReplicatedStore {
        &self.store
    }

    #[must_use]
    pub fn admin_client(&self) -> AdminClient {
        self.admin.clone()
    }

    pub async fn membership_states(&self) -> Result<Vec<super::MembershipState>, Error> {
        self.admin.membership_states().await
    }

    pub async fn cleanup(&mut self) -> Result<(), Error> {
        remove(&self.service.service, &self.service.run_dir).await
    }
}

struct DockerService {
    service: ManagedService,
    data_dir: PathBuf,
    run_dir: PathBuf,
}

impl DockerService {
    async fn start(&self, files: &InstalledFiles) -> Result<Ensure, Error> {
        let mounts = [&self.data_dir, &self.run_dir]
            .into_iter()
            .map(|path| Mount {
                typ: Some(MountType::BIND),
                source: Some(path.to_string_lossy().into_owned()),
                target: Some(path.to_string_lossy().into_owned()),
                ..Default::default()
            })
            .collect();
        let owner = fs::metadata(&self.data_dir)?;
        let config = ContainerCreateBody {
            image: Some(IMAGE.into()),
            cmd: Some(vec![
                "corrosion".into(),
                "agent".into(),
                "-c".into(),
                self.data_dir
                    .join("config.toml")
                    .to_string_lossy()
                    .into_owned(),
            ]),
            user: Some(format!("{}:{}", owner.uid(), owner.gid())),
            labels: Some(HashMap::from([("ployzd.managed".into(), String::new())])),
            host_config: Some(HostConfig {
                network_mode: Some("host".into()),
                restart_policy: Some(RestartPolicy {
                    name: Some(RestartPolicyNameEnum::UNLESS_STOPPED),
                    ..Default::default()
                }),
                log_config: Some(HostConfigLogConfig {
                    typ: Some("local".into()),
                    ..Default::default()
                }),
                mounts: Some(mounts),
                ..Default::default()
            }),
            ..Default::default()
        };
        self.service
            .ensure_host(DesiredContainer::new(
                config,
                &[&files.config, SCHEMA.as_bytes()],
            ))
            .await
            .map_err(Into::into)
    }
}

async fn bounded_start<F, T>(start: F) -> Result<T, Error>
where
    F: Future<Output = Result<T, Error>>,
{
    tokio::time::timeout(START_TIMEOUT, start)
        .await
        .map_err(|_| {
            Error::Api(format!(
                "Corrosion did not start within {} seconds; inspect `journalctl -u ployz -n 100 --no-pager`",
                START_TIMEOUT.as_secs(),
            ))
        })?
}

async fn wait_ready_or_replace<F, Fut, R>(kept: bool, mut ready: F, replace: R) -> Result<(), Error>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
    R: Future<Output = Result<(), Error>>,
{
    if kept
        && tokio::time::timeout(KEPT_READY_TIMEOUT, wait_ready(&mut ready))
            .await
            .is_err()
    {
        tracing::warn!("kept Corrosion container did not answer; replacing it");
        replace.await?;
    }
    wait_ready(ready).await;
    Ok(())
}

async fn wait_ready<F, Fut>(mut ready: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    loop {
        if ready().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn create_private_dir(path: &Path) -> Result<(), Error> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn load_or_create_token(path: &Path) -> Result<String, Error> {
    match OpenOptions::new().read(true).open(path) {
        Ok(mut file) => {
            let mut token = String::new();
            file.read_to_string(&mut token)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
            validate_token(token)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let token = format!(
                "{}{}",
                ployz_core::MachineId::random(),
                ployz_core::MachineId::random()
            );
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(path)?;
            file.write_all(token.as_bytes())?;
            file.sync_all()?;
            Ok(token)
        }
        Err(error) => Err(error.into()),
    }
}

fn validate_token(token: String) -> Result<String, Error> {
    if token.len() == 64
        && token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(token)
    } else {
        Err(Error::Protocol("invalid persisted API token".into()))
    }
}

#[derive(Serialize)]
struct FileConfig {
    db: DbConfig,
    gossip: GossipConfig,
    api: ApiConfig,
    admin: AdminConfig,
}

#[derive(Serialize)]
struct DbConfig {
    path: PathBuf,
    schema_paths: Vec<PathBuf>,
}

#[derive(Serialize)]
struct GossipConfig {
    addr: SocketAddr,
    bootstrap: Vec<String>,
    plaintext: bool,
}

#[derive(Serialize)]
struct ApiConfig {
    addr: SocketAddr,
    authz: AuthzConfig,
}

#[derive(Serialize)]
struct AuthzConfig {
    #[serde(rename = "bearer-token")]
    bearer_token: String,
}

#[derive(Serialize)]
struct AdminConfig {
    path: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::Instant;

    #[tokio::test(start_paused = true)]
    async fn corrosion_readiness_wait_keeps_polling_after_fifteen_seconds() {
        let started = Instant::now();
        wait_ready(|| async { started.elapsed() >= Duration::from_secs(16) }).await;
        assert!(
            started.elapsed() >= Duration::from_secs(16),
            "probe must succeed only after 15 seconds, got {:?}",
            started.elapsed()
        );
    }

    async fn start_with(kept: bool, ready_after: Option<Duration>) -> (bool, Duration) {
        let started = Instant::now();
        let replaced = std::cell::Cell::new(false);
        bounded_start(wait_ready_or_replace(
            kept,
            || {
                let ready =
                    replaced.get() || ready_after.is_some_and(|after| started.elapsed() >= after);
                async move { ready }
            },
            async {
                replaced.set(true);
                Ok(())
            },
        ))
        .await
        .unwrap();
        (replaced.get(), started.elapsed())
    }

    #[tokio::test(start_paused = true)]
    async fn kept_corrosion_that_never_answers_is_replaced() {
        let (replaced, elapsed) = start_with(true, None).await;
        assert!(replaced);
        assert!(elapsed >= KEPT_READY_TIMEOUT, "{elapsed:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn answering_kept_corrosion_is_not_replaced() {
        assert!(!start_with(true, Some(Duration::from_secs(5))).await.0);
    }

    #[tokio::test(start_paused = true)]
    async fn new_corrosion_gets_the_full_wait_without_replacement() {
        let (replaced, elapsed) = start_with(false, Some(Duration::from_secs(120))).await;
        assert!(!replaced);
        assert!(elapsed >= Duration::from_secs(120), "{elapsed:?}");
    }

    #[test]
    fn install_is_byte_stable() {
        let root = tempfile::tempdir().unwrap();
        let peer = |last| SocketAddr::from(([10, 0, 0, last], 51820));
        let config = |peers: Vec<SocketAddr>| {
            CorrosionConfig::local(root.path().join("data"), root.path().join("run"))
                .with_bootstrap(peers)
        };
        let first = config(vec![peer(2), peer(1), peer(3)]).install().unwrap();
        let second = config(vec![peer(3), peer(1), peer(2), peer(1)])
            .install()
            .unwrap();
        assert_eq!(first.token, second.token);
        assert_eq!(first.config, second.config);
        assert_eq!(
            fs::read(root.path().join("data/config.toml")).unwrap(),
            second.config
        );
        let encoded = String::from_utf8(second.config).unwrap();
        assert!(
            encoded
                .contains(r#"bootstrap = ["10.0.0.1:51820", "10.0.0.2:51820", "10.0.0.3:51820"]"#),
            "{encoded}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn corrosion_start_is_bounded() {
        let error = tokio::time::timeout(
            Duration::from_secs(5 * 60),
            bounded_start(std::future::pending::<Result<(), Error>>()),
        )
        .await
        .expect("Corrosion startup must return before the systemd startup ceiling")
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("journalctl -u ployz -n 100 --no-pager")
        );
    }
}
