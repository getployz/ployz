//! Secured NATS test fixture: a real `nats-server` with TLS and NKey-user
//! authorization, rendered through the concrete production NATS interfaces —
//! no parallel test-only config language.
//!
//! [`SecuredTestNats`] mints a throwaway cluster CA, a server certificate,
//! and one NKey credential per principal, then exposes per-principal
//! [`NatsConnectConfig`]s for tests to connect with.

use std::error::Error;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use ployz_core::ids::MachineId;
use ployz_core::nats_config::{
    BuildExecutorCredentialExpiresAt, CredentialGrant, CredentialName, CredentialRole,
    MintedNatsUser, NatsAuthorizationGrant, NatsInternalAuthority, NatsUserPublicKey, NatsUserSeed,
};
use ployz_core::security::NatsPrincipal;
use ployz_nats::connect::{
    NatsClientAuth, NatsClientUrl, NatsConnectConfig, NatsTlsTrust, connect_authenticated,
};
use ployz_nats::operation_api_client::OperationApiClient;
use ployz_nats::permissions::render_authorized_users;
use ployz_nats::server_config::{NatsListener, NatsServerConfig, NatsServerTlsFiles};

/// The connect timeout every suite uses against the fixture server.
pub const TEST_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

const READINESS_ATTEMPTS: u32 = 50;
const READINESS_DELAY: Duration = Duration::from_millis(100);
const READINESS_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const PORTS_FILE_ATTEMPTS: u32 = 100;
const PORTS_FILE_DELAY: Duration = Duration::from_millis(100);
const SERVER_MACHINE_ID: &str = "secured-test-core";

type FixtureError = Box<dyn Error + Send + Sync>;

/// A running TLS + NKey-authorized `nats-server` for tests.
pub struct SecuredTestNats {
    server: FixtureNatsServer,
    _dir: tempfile::TempDir,
    url: NatsClientUrl,
    port: u16,
    ca_path: PathBuf,
    authorized_users_path: PathBuf,
    controller_seed: NatsUserSeed,
    user_seed: NatsUserSeed,
    join_seed: NatsUserSeed,
    machine_seeds: Vec<(MachineId, NatsUserSeed)>,
}

impl SecuredTestNats {
    /// Starts a secured server with Controller, Operator, Join, and System users.
    pub async fn start() -> Result<Self, FixtureError> {
        Self::start_with_machines(&[]).await
    }

    /// Starts a secured server with the base principals plus one Machine user
    /// per supplied machine id.
    pub async fn start_with_machines(machine_ids: &[MachineId]) -> Result<Self, FixtureError> {
        Self::start_with_machines_and_extra_users(machine_ids, &[]).await
    }

    /// Starts a secured server with the base principals, supplied Machine
    /// users, and external Operator principal public keys such as Cloud clients.
    pub async fn start_with_machines_and_extra_users(
        machine_ids: &[MachineId],
        extra_user_public_keys: &[NatsUserPublicKey],
    ) -> Result<Self, FixtureError> {
        let extra_credentials = extra_user_public_keys
            .iter()
            .cloned()
            .map(|public_key| CredentialGrant {
                public_key,
                name: CredentialName::try_new("Test external operator")
                    .expect("test credential name"),
                role: CredentialRole::Operator,
            })
            .collect::<Vec<_>>();
        Self::start_with_machines_and_credentials(machine_ids, &extra_credentials).await
    }

    /// Starts a secured server with the base principals, supplied Machine
    /// users, and arbitrary external credential grants.
    pub async fn start_with_machines_and_credentials(
        machine_ids: &[MachineId],
        extra_credentials: &[CredentialGrant],
    ) -> Result<Self, FixtureError> {
        let dir = tempfile::TempDir::new()?;

        let identity = generate_test_cluster_identity()?;
        let tls = write_tls_material(dir.path(), &identity)?;

        let controller = identity.controller;
        let user = identity.operator;
        let join = identity.join;
        let system = MintedNatsUser::generate()?;
        let mut machine_users = Vec::with_capacity(machine_ids.len());
        for machine_id in machine_ids {
            machine_users.push((machine_id.clone(), MintedNatsUser::generate()?));
        }

        let mut authorized = vec![
            authorized_user(NatsPrincipal::Controller, &controller),
            authorized_user(NatsPrincipal::Operator, &user),
            authorized_user(NatsPrincipal::Join, &join),
            authorized_user(NatsPrincipal::System, &system),
        ];
        for (machine_id, minted) in &machine_users {
            authorized.push(authorized_user(
                NatsPrincipal::Machine {
                    machine_id: machine_id.clone(),
                },
                minted,
            ));
        }
        for credential in extra_credentials {
            authorized.push(NatsAuthorizationGrant::Credential(credential.clone()));
        }
        let authorized_users_path = dir.path().join("authorized-users.conf");
        fs::write(&authorized_users_path, render_authorized_users(&authorized))?;

        let server_config = NatsServerConfig::single_machine(
            MachineId::try_new(SERVER_MACHINE_ID)
                .expect("fixture server machine id is a valid subject token"),
            NatsListener::Loopback,
            NatsServerTlsFiles {
                cert_file: tls.cert_path,
                key_file: tls.key_path,
            },
            PathBuf::from("authorized-users.conf"),
        )?;
        let config_path = dir.path().join("nats-server.conf");
        fs::write(&config_path, server_config.render())?;

        // `-p -1` overrides the rendered 4222 with a dynamic port so
        // parallel fixtures do not collide; the actual port comes from the
        // server's ports file.
        let mut server = FixtureNatsServer::spawn(&config_path, dir.path())?;
        let port = server.wait_for_client_port(dir.path()).await?;
        let url = NatsClientUrl::try_new(format!("tls://127.0.0.1:{port}"))
            .expect("fixture-rendered NATS URL is valid");

        let fixture = Self {
            server,
            _dir: dir,
            url,
            port,
            ca_path: tls.ca_path,
            authorized_users_path,
            controller_seed: controller.seed,
            user_seed: user.seed,
            join_seed: join.seed,
            machine_seeds: machine_users
                .into_iter()
                .map(|(machine_id, minted)| (machine_id, minted.seed))
                .collect(),
        };
        fixture.wait_until_ready().await?;
        Ok(fixture)
    }

    #[must_use]
    pub fn client_url(&self) -> &NatsClientUrl {
        &self.url
    }

    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    #[must_use]
    pub fn ca_path(&self) -> &Path {
        &self.ca_path
    }

    /// The server's `authorized-users.conf`. Control-runtime tests point
    /// the ployzd authorization writer at this exact file so renders feed
    /// the same server the fixture spawned.
    #[must_use]
    pub fn authorized_users_path(&self) -> &Path {
        &self.authorized_users_path
    }

    /// The spawned `nats-server` pid, for signal-based config reloads.
    #[must_use]
    pub fn server_pid(&self) -> u32 {
        self.server.child.id()
    }

    #[must_use]
    pub fn join_seed(&self) -> &NatsUserSeed {
        &self.join_seed
    }

    #[must_use]
    pub fn user_seed(&self) -> &NatsUserSeed {
        &self.user_seed
    }

    #[must_use]
    pub fn controller_config(&self) -> NatsConnectConfig {
        self.config_with_seed(NatsPrincipal::Controller, self.controller_seed.clone())
    }

    #[must_use]
    pub fn user_config(&self) -> NatsConnectConfig {
        self.config_with_seed(NatsPrincipal::Operator, self.user_seed.clone())
    }

    #[must_use]
    pub fn join_config(&self) -> NatsConnectConfig {
        self.config_with_seed(NatsPrincipal::Join, self.join_seed.clone())
    }

    /// The connect config for a machine minted via
    /// [`SecuredTestNats::start_with_machines`].
    #[must_use]
    pub fn machine_config(&self, machine_id: &MachineId) -> Option<NatsConnectConfig> {
        self.machine_seeds
            .iter()
            .find(|(minted_machine_id, _)| minted_machine_id == machine_id)
            .map(|(minted_machine_id, seed)| {
                self.config_with_seed(
                    NatsPrincipal::Machine {
                        machine_id: minted_machine_id.clone(),
                    },
                    seed.clone(),
                )
            })
    }

    /// Builds a connect config against this server with an arbitrary seed —
    /// for tests that exercise rejection of unauthorized credentials.
    #[must_use]
    pub fn config_with_seed(
        &self,
        principal: NatsPrincipal,
        seed: NatsUserSeed,
    ) -> NatsConnectConfig {
        NatsConnectConfig {
            url: self.url.clone(),
            auth: NatsClientAuth::NkeySeed(seed),
            trust: NatsTlsTrust::ClusterCa(self.ca_path.clone()),
            principal,
        }
    }

    /// A freshly generated user seed that is not in the authorized set.
    pub fn fresh_unauthorized_seed() -> Result<NatsUserSeed, FixtureError> {
        Ok(MintedNatsUser::generate()?.seed)
    }

    async fn wait_until_ready(&self) -> Result<(), FixtureError> {
        let config = self.controller_config();
        let mut last_error = "no connection attempt made".to_owned();
        for _ in 0..READINESS_ATTEMPTS {
            match connect_authenticated(&config, READINESS_CONNECT_TIMEOUT).await {
                Ok(client) => {
                    drop(client);
                    return Ok(());
                }
                Err(error) => {
                    last_error = error.to_string();
                    tokio::time::sleep(READINESS_DELAY).await;
                }
            }
        }
        Err(Box::new(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "secured NATS test server did not become ready at {}: {last_error}",
                self.url.as_str()
            ),
        )))
    }
}

/// The one connected fixture the suites share: a [`SecuredTestNats`] server
/// plus authenticated Controller/Operator/Join clients.
pub struct TestNats {
    pub server: SecuredTestNats,
    pub controller: async_nats::Client,
    pub user: async_nats::Client,
    pub join: async_nats::Client,
}

impl TestNats {
    pub async fn start() -> Self {
        Self::start_with_machines(&[]).await
    }

    /// Starts a secured server with one minted Machine user per supplied id
    /// and connects the Controller and Operator clients.
    pub async fn start_with_machines(machine_ids: &[MachineId]) -> Self {
        let server = SecuredTestNats::start_with_machines(machine_ids)
            .await
            .expect("secured test nats starts");
        let controller =
            connect_authenticated(&server.controller_config(), TEST_NATS_CONNECT_TIMEOUT)
                .await
                .expect("controller connects");
        let user = connect_authenticated(&server.user_config(), TEST_NATS_CONNECT_TIMEOUT)
            .await
            .expect("operator connects");
        let join = connect_authenticated(&server.join_config(), TEST_NATS_CONNECT_TIMEOUT)
            .await
            .expect("join connects");

        Self {
            server,
            controller,
            user,
            join,
        }
    }

    /// The operator-facing operation API client (Operator principal). The request
    /// timeout is generous: CI runs many nats-server-backed test binaries in
    /// parallel, and a saturated runner can stall a reply well past the production
    /// default without anything being wrong.
    #[must_use]
    pub fn api(&self) -> OperationApiClient {
        OperationApiClient::new(self.user.clone()).with_request_timeout(Duration::from_secs(30))
    }

    /// A client authenticated as the machine's Machine user. Machine runtimes,
    /// gateway processes, and observation writers connect with this.
    pub async fn machine_client(&self, machine_id: &MachineId) -> async_nats::Client {
        let config = self
            .server
            .machine_config(machine_id)
            .expect("fixture knows the machine user");
        connect_authenticated(&config, TEST_NATS_CONNECT_TIMEOUT)
            .await
            .expect("machine connects")
    }
}

fn authorized_user(principal: NatsPrincipal, minted: &MintedNatsUser) -> NatsAuthorizationGrant {
    let public_key = minted.public.clone();
    match principal {
        NatsPrincipal::Operator => NatsAuthorizationGrant::Credential(CredentialGrant {
            public_key,
            name: CredentialName::try_new("Founder operator (secured-test-core)")
                .expect("test credential name"),
            role: CredentialRole::Operator,
        }),
        NatsPrincipal::Machine { machine_id } => NatsAuthorizationGrant::Internal {
            authority: NatsInternalAuthority::Machine { machine_id },
            public_key,
        },
        NatsPrincipal::BuildExecutor {
            pool_id,
            executor_id,
        } => NatsAuthorizationGrant::Credential(CredentialGrant {
            public_key,
            name: CredentialName::try_new("Build Executor (secured-test-core)")
                .expect("test credential name"),
            role: CredentialRole::BuildExecutor {
                pool_id,
                executor_id,
                expires_at: BuildExecutorCredentialExpiresAt::try_new(u64::MAX)
                    .expect("maximum timestamp is positive"),
            },
        }),
        NatsPrincipal::Controller => NatsAuthorizationGrant::Internal {
            authority: NatsInternalAuthority::Controller,
            public_key,
        },
        NatsPrincipal::Join => NatsAuthorizationGrant::Internal {
            authority: NatsInternalAuthority::Join,
            public_key,
        },
        NatsPrincipal::System => NatsAuthorizationGrant::Internal {
            authority: NatsInternalAuthority::System,
            public_key,
        },
    }
}

/// A spawned `nats-server` child killed on drop.
///
/// Spawned with `-p -1` so the kernel assigns a free port (overriding the
/// rendered config port) and `--ports_file_dir` so the assigned port is
/// readable from the server's ports file.
struct FixtureNatsServer {
    child: Child,
    stderr: Option<std::process::ChildStderr>,
}

impl FixtureNatsServer {
    fn spawn(config_path: &Path, ports_file_dir: &Path) -> Result<Self, FixtureError> {
        let mut child = Command::new("nats-server")
            .arg("--config")
            .arg(config_path)
            .arg("--port")
            .arg("-1")
            .arg("--ports_file_dir")
            .arg(ports_file_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        let stderr = child.stderr.take();
        Ok(Self { child, stderr })
    }

    async fn wait_for_client_port(&mut self, ports_file_dir: &Path) -> Result<u16, FixtureError> {
        let ports_path = ports_file_dir.join(format!("nats-server_{}.ports", self.child.id()));
        for _ in 0..PORTS_FILE_ATTEMPTS {
            if let Ok(contents) = fs::read_to_string(&ports_path)
                && let Some(port) = parse_client_port(&contents)
            {
                return Ok(port);
            }
            if let Some(status) = self.child.try_wait()? {
                return Err(Box::new(io::Error::other(format!(
                    "nats-server exited before writing ports file at {} with status {status}: {}",
                    ports_path.display(),
                    self.read_stderr()
                ))));
            }
            tokio::time::sleep(PORTS_FILE_DELAY).await;
        }
        Err(Box::new(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "nats-server ports file never appeared at {}: {}",
                ports_path.display(),
                self.read_stderr()
            ),
        )))
    }

    fn read_stderr(&mut self) -> String {
        let Some(mut stderr) = self.stderr.take() else {
            return "stderr unavailable".to_owned();
        };
        let mut output = String::new();
        match stderr.read_to_string(&mut output) {
            Ok(_) if output.trim().is_empty() => "stderr empty".to_owned(),
            Ok(_) => output.trim().to_owned(),
            Err(error) => format!("stderr read failed: {error}"),
        }
    }
}

impl Drop for FixtureNatsServer {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

/// Extracts the client port from the nats-server ports file
/// (`{"nats":["nats://0.0.0.0:61964"], ...}`).
fn parse_client_port(ports_file_contents: &str) -> Option<u16> {
    let value: serde_json::Value = serde_json::from_str(ports_file_contents).ok()?;
    let urls = value.get("nats")?.as_array()?;
    let [first, ..] = urls.as_slice() else {
        return None;
    };
    let url = first.as_str()?;
    let (_, port) = url.rsplit_once(':')?;
    port.parse().ok()
}

struct WrittenTlsMaterial {
    ca_path: PathBuf,
    cert_path: PathBuf,
    key_path: PathBuf,
}

struct TestClusterIdentity {
    ca_pem: String,
    server_cert_pem: String,
    server_key_pem: String,
    controller: MintedNatsUser,
    operator: MintedNatsUser,
    join: MintedNatsUser,
}

fn generate_test_cluster_identity() -> Result<TestClusterIdentity, FixtureError> {
    let ca_key = rcgen::KeyPair::generate()?;
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new())?;
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "ployz-cluster-ca");
    let ca_certificate = ca_params.clone().self_signed(&ca_key)?;
    let issuer = rcgen::Issuer::new(ca_params, ca_key);

    let server_key = rcgen::KeyPair::generate()?;
    let server_params =
        rcgen::CertificateParams::new(vec!["127.0.0.1".to_owned(), "localhost".to_owned()])?;
    let server_certificate = server_params.signed_by(&server_key, &issuer)?;

    Ok(TestClusterIdentity {
        ca_pem: ca_certificate.pem(),
        server_cert_pem: server_certificate.pem(),
        server_key_pem: server_key.serialize_pem(),
        controller: MintedNatsUser::generate()?,
        operator: MintedNatsUser::generate()?,
        join: MintedNatsUser::generate()?,
    })
}

fn write_tls_material(
    dir: &Path,
    identity: &TestClusterIdentity,
) -> Result<WrittenTlsMaterial, FixtureError> {
    let ca_path = dir.join("ca.pem");
    let cert_path = dir.join("server.crt");
    let key_path = dir.join("server.key");
    fs::write(&ca_path, &identity.ca_pem)?;
    fs::write(&cert_path, &identity.server_cert_pem)?;
    fs::write(&key_path, &identity.server_key_pem)?;
    Ok(WrittenTlsMaterial {
        ca_path,
        cert_path,
        key_path,
    })
}
