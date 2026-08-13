use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::SocketAddr,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

use bollard::{
    Docker,
    errors::Error as DockerError,
    models::{
        ContainerCreateBody, HostConfig, HostConfigLogConfig, Mount, MountType, RestartPolicy,
        RestartPolicyNameEnum,
    },
    query_parameters::{
        CreateContainerOptionsBuilder, CreateImageOptionsBuilder, RemoveContainerOptionsBuilder,
    },
};
use futures_util::TryStreamExt;
use serde::Serialize;

use super::{AdminClient, ApiClient, Error, ReplicatedStore, Statement};

pub const IMAGE: &str = "ghcr.io/unlabs-dev/corrosion:2026.6.15";
pub const DEFAULT_CONTAINER_NAME: &str = "ployz-corrosion";
pub const DEFAULT_API_ADDRESS: &str = "127.0.0.1:51002";
pub const DEFAULT_GOSSIP_ADDRESS: &str = "127.0.0.1:51001";
const TOKEN_FILE: &str = ".api-token";
const SCHEMA: &str = include_str!("schema.sql");

pub struct CorrosionConfig {
    data_dir: PathBuf,
    run_dir: PathBuf,
    api_address: SocketAddr,
    gossip_address: SocketAddr,
    container_name: String,
    // TODO(UT-100): every peer remains a bootstrap target; add partial selection only after a product decision.
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
            data_dir: data_dir.into(),
            run_dir: run_dir.into(),
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
            DEFAULT_API_ADDRESS
                .parse()
                .expect("static address is valid"),
            DEFAULT_GOSSIP_ADDRESS
                .parse()
                .expect("static address is valid"),
            DEFAULT_CONTAINER_NAME,
        )
    }

    #[must_use]
    pub fn with_bootstrap(mut self, peers: impl IntoIterator<Item = SocketAddr>) -> Self {
        self.bootstrap = peers.into_iter().collect();
        self
    }

    pub async fn start(&self) -> Result<RunningCorrosion, Error> {
        let token = self.install()?;
        let api = ApiClient::new(self.api_address, &token)?;
        let admin = AdminClient::new(self.run_dir.join("admin.sock"));
        let docker = Docker::connect_with_socket_defaults()?;
        let service = DockerService {
            docker,
            name: self.container_name.clone(),
            data_dir: self.data_dir.clone(),
            run_dir: self.run_dir.clone(),
        };
        service.start().await?;
        wait_ready(&api).await?;
        Ok(RunningCorrosion {
            store: ReplicatedStore::new(api),
            admin,
            service,
        })
    }

    fn install(&self) -> Result<String, Error> {
        create_private_dir(&self.data_dir)?;
        create_private_dir(&self.run_dir)?;
        let token = load_or_create_token(&self.data_dir.join(TOKEN_FILE))?;
        let schema_path = self.data_dir.join("schema.sql");
        // TODO(EO-019): Ployz deliberately has no replicated Store Schema/value evolution contract;
        // mixed-version Machines may omit or fail to read newer data. Do not add migrations or version gates.
        atomic_write(&schema_path, SCHEMA.as_bytes(), 0o644)?;

        let config = FileConfig {
            db: DbConfig {
                path: self.data_dir.join("store.db"),
                schema_paths: vec![schema_path],
            },
            gossip: GossipConfig {
                addr: self.gossip_address,
                bootstrap: self.bootstrap.iter().map(ToString::to_string).collect(),
                plaintext: true,
            },
            api: ApiConfig {
                addr: self.api_address,
                authz: AuthzConfig {
                    bearer_token: token.clone(),
                },
            },
            admin: AdminConfig {
                path: self.run_dir.join("admin.sock"),
            },
        };
        let encoded = toml::to_string(&config)?;
        // TODO(UT-101): retain the loose Corrosion-owned config boundary until ownership is decided.
        atomic_write(
            &self.data_dir.join("config.toml"),
            encoded.as_bytes(),
            0o600,
        )?;
        Ok(token)
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

    pub async fn membership_states(&self) -> Result<Vec<serde_json::Value>, Error> {
        self.admin
            .command(&serde_json::json!({"Cluster": "MembershipStates"}))
            .await
    }

    pub async fn stop(&mut self) -> Result<(), Error> {
        self.service.stop().await
    }

    pub async fn cleanup(&mut self) -> Result<(), Error> {
        self.service.cleanup().await
    }
}

struct DockerService {
    docker: Docker,
    name: String,
    data_dir: PathBuf,
    run_dir: PathBuf,
}

impl DockerService {
    async fn start(&self) -> Result<(), Error> {
        match self.docker.inspect_container(&self.name, None).await {
            Ok(container) => {
                let configured_image = container.config.and_then(|config| config.image);
                if configured_image.as_deref() != Some(IMAGE) {
                    self.cleanup().await?;
                    self.create().await?;
                } else if !container
                    .state
                    .and_then(|state| state.running)
                    .unwrap_or(false)
                {
                    self.docker.start_container(&self.name, None).await?;
                }
            }
            Err(error) if is_not_found(&error) => self.create().await?,
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    async fn create(&self) -> Result<(), Error> {
        if let Err(error) = self.docker.inspect_image(IMAGE).await {
            if !is_not_found(&error) {
                return Err(error.into());
            }
            self.docker
                .create_image(
                    Some(
                        CreateImageOptionsBuilder::default()
                            .from_image(IMAGE)
                            .build(),
                    ),
                    None,
                    None,
                )
                .try_collect::<Vec<_>>()
                .await?;
        }

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
        self.docker
            .create_container(
                Some(
                    CreateContainerOptionsBuilder::default()
                        .name(&self.name)
                        .build(),
                ),
                config,
            )
            .await?;
        self.docker.start_container(&self.name, None).await?;
        Ok(())
    }

    async fn stop(&self) -> Result<(), Error> {
        match self.docker.stop_container(&self.name, None).await {
            Ok(()) => Ok(()),
            Err(error) if is_not_found(&error) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    async fn cleanup(&self) -> Result<(), Error> {
        self.stop().await?;
        match self
            .docker
            .remove_container(
                &self.name,
                Some(RemoveContainerOptionsBuilder::default().v(true).build()),
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(error) if is_not_found(&error) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

fn is_not_found(error: &DockerError) -> bool {
    matches!(
        error,
        DockerError::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}

async fn wait_ready(api: &ApiClient) -> Result<(), Error> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if api
                .query(Statement::new("SELECT 1 FROM cluster LIMIT 1", []))
                .await
                .is_ok()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| Error::Api("service did not become ready within 15 seconds".into()))?;
    Ok(())
}

fn atomic_write(path: &Path, contents: &[u8], mode: u32) -> Result<(), Error> {
    let temporary = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(mode)
        .open(&temporary)?;
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    file.write_all(contents)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    File::open(path.parent().expect("installed file has a parent"))?.sync_all()?;
    Ok(())
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
