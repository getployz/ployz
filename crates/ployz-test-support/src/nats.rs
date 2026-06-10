//! Secured NATS test fixture: a real `nats-server` with TLS and NKey-user
//! authorization, rendered through the product config types in
//! `ployz_core::nats_config` — no parallel test-only config language.
//!
//! [`SecuredTestNats`] mints a throwaway cluster CA, a server certificate,
//! and one NKey user per principal, then exposes per-principal
//! [`NatsConnectConfig`]s for tests to connect with.

use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use ployz_core::ids::NodeId;
use ployz_core::nats_config::{
    NatsAuthorizedUser, NatsListener, NatsServerConfig, NatsServerTlsFiles, NatsUserPublicKey,
    NatsUserSeed, render_authorized_users,
};
use ployz_core::security::NatsPrincipal;
use ployz_nats::connect::{
    NatsClientAuth, NatsClientUrl, NatsConnectConfig, NatsTlsTrust, connect_authenticated,
};

const READINESS_ATTEMPTS: u32 = 50;
const READINESS_DELAY: Duration = Duration::from_millis(100);
const READINESS_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const PORTS_FILE_ATTEMPTS: u32 = 100;
const PORTS_FILE_DELAY: Duration = Duration::from_millis(100);
const SERVER_NODE_ID: &str = "secured-test-core";

type FixtureError = Box<dyn Error + Send + Sync>;

/// A running TLS + NKey-authorized `nats-server` for tests.
pub struct SecuredTestNats {
    _server: FixtureNatsServer,
    _dir: tempfile::TempDir,
    url: NatsClientUrl,
    port: u16,
    ca_path: PathBuf,
    controller_seed: NatsUserSeed,
    user_seed: NatsUserSeed,
    join_seed: NatsUserSeed,
    system_seed: NatsUserSeed,
    node_seeds: Vec<(NodeId, NatsUserSeed)>,
}

impl SecuredTestNats {
    /// Starts a secured server with Controller, User, Join, and System users.
    pub async fn start() -> Result<Self, FixtureError> {
        Self::start_with_nodes(&[]).await
    }

    /// Starts a secured server with the base principals plus one Node user
    /// per supplied node id.
    pub async fn start_with_nodes(node_ids: &[NodeId]) -> Result<Self, FixtureError> {
        let dir = tempfile::TempDir::new()?;
        let tls = write_tls_material(dir.path())?;

        let controller = mint_nkey_user()?;
        let user = mint_nkey_user()?;
        let join = mint_nkey_user()?;
        let system = mint_nkey_user()?;
        let mut node_users = Vec::with_capacity(node_ids.len());
        for node_id in node_ids {
            node_users.push((node_id.clone(), mint_nkey_user()?));
        }

        let mut authorized = vec![
            authorized_user(NatsPrincipal::Controller, &controller),
            authorized_user(NatsPrincipal::User, &user),
            authorized_user(NatsPrincipal::Join, &join),
            authorized_user(NatsPrincipal::System, &system),
        ];
        for (node_id, minted) in &node_users {
            authorized.push(authorized_user(
                NatsPrincipal::Node {
                    node_id: node_id.clone(),
                },
                minted,
            ));
        }
        fs::write(
            dir.path().join("authorized-users.conf"),
            render_authorized_users(&authorized),
        )?;

        let server_config = NatsServerConfig::single_node(
            NodeId::try_new(SERVER_NODE_ID)
                .expect("fixture server node id is a valid subject token"),
            dir.path().join("jetstream"),
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
        let server = FixtureNatsServer::spawn(&config_path, dir.path())?;
        let port = server.wait_for_client_port(dir.path()).await?;
        let url = NatsClientUrl::try_new(format!("tls://127.0.0.1:{port}"))
            .expect("fixture-rendered NATS URL is valid");

        let fixture = Self {
            _server: server,
            _dir: dir,
            url,
            port,
            ca_path: tls.ca_path,
            controller_seed: controller.seed,
            user_seed: user.seed,
            join_seed: join.seed,
            system_seed: system.seed,
            node_seeds: node_users
                .into_iter()
                .map(|(node_id, minted)| (node_id, minted.seed))
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

    #[must_use]
    pub fn controller_config(&self) -> NatsConnectConfig {
        self.config_with_seed(NatsPrincipal::Controller, self.controller_seed.clone())
    }

    #[must_use]
    pub fn user_config(&self) -> NatsConnectConfig {
        self.config_with_seed(NatsPrincipal::User, self.user_seed.clone())
    }

    #[must_use]
    pub fn join_config(&self) -> NatsConnectConfig {
        self.config_with_seed(NatsPrincipal::Join, self.join_seed.clone())
    }

    #[must_use]
    pub fn system_config(&self) -> NatsConnectConfig {
        self.config_with_seed(NatsPrincipal::System, self.system_seed.clone())
    }

    /// The connect config for a node minted via
    /// [`SecuredTestNats::start_with_nodes`].
    #[must_use]
    pub fn node_config(&self, node_id: &NodeId) -> Option<NatsConnectConfig> {
        self.node_seeds
            .iter()
            .find(|(minted_node_id, _)| minted_node_id == node_id)
            .map(|(minted_node_id, seed)| {
                self.config_with_seed(
                    NatsPrincipal::Node {
                        node_id: minted_node_id.clone(),
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
        Ok(mint_nkey_user()?.seed)
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

struct MintedNkeyUser {
    public: NatsUserPublicKey,
    seed: NatsUserSeed,
}

fn mint_nkey_user() -> Result<MintedNkeyUser, FixtureError> {
    let pair = nkeys::KeyPair::new_user();
    let seed = NatsUserSeed::try_new(pair.seed()?)?;
    let public = NatsUserPublicKey::try_new(pair.public_key())?;
    Ok(MintedNkeyUser { public, seed })
}

fn authorized_user(principal: NatsPrincipal, minted: &MintedNkeyUser) -> NatsAuthorizedUser {
    NatsAuthorizedUser {
        principal,
        nkey_public: minted.public.clone(),
    }
}

/// A spawned `nats-server` child killed on drop.
///
/// Spawned with `-p -1` so the kernel assigns a free port (overriding the
/// rendered config port) and `--ports_file_dir` so the assigned port is
/// readable from the server's ports file.
struct FixtureNatsServer {
    child: Child,
}

impl FixtureNatsServer {
    fn spawn(config_path: &Path, ports_file_dir: &Path) -> Result<Self, FixtureError> {
        let child = Command::new("nats-server")
            .arg("--config")
            .arg(config_path)
            .arg("--port")
            .arg("-1")
            .arg("--ports_file_dir")
            .arg(ports_file_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(Self { child })
    }

    async fn wait_for_client_port(&self, ports_file_dir: &Path) -> Result<u16, FixtureError> {
        let ports_path = ports_file_dir.join(format!("nats-server_{}.ports", self.child.id()));
        for _ in 0..PORTS_FILE_ATTEMPTS {
            if let Ok(contents) = fs::read_to_string(&ports_path)
                && let Some(port) = parse_client_port(&contents)
            {
                return Ok(port);
            }
            tokio::time::sleep(PORTS_FILE_DELAY).await;
        }
        Err(Box::new(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "nats-server ports file never appeared at {}",
                ports_path.display()
            ),
        )))
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

/// Mints a throwaway cluster CA plus a server certificate for loopback and
/// writes them (and the server key) into the fixture directory.
fn write_tls_material(dir: &Path) -> Result<WrittenTlsMaterial, FixtureError> {
    let ca_key = rcgen::KeyPair::generate()?;
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new())?;
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "ployz-secured-test-ca");
    let ca_certificate = ca_params.self_signed(&ca_key)?;
    let ca_pem = ca_certificate.pem();

    let server_key = rcgen::KeyPair::generate()?;
    let server_params =
        rcgen::CertificateParams::new(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])?;
    let issuer = rcgen::Issuer::new(ca_params, ca_key);
    let server_certificate = server_params.signed_by(&server_key, &issuer)?;

    let ca_path = dir.join("ca.pem");
    let cert_path = dir.join("server.crt");
    let key_path = dir.join("server.key");
    fs::write(&ca_path, ca_pem)?;
    fs::write(&cert_path, server_certificate.pem())?;
    fs::write(&key_path, server_key.serialize_pem())?;
    Ok(WrittenTlsMaterial {
        ca_path,
        cert_path,
        key_path,
    })
}
