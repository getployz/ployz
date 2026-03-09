use serde::Serialize;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::{fs, io};

pub const DEFAULT_GOSSIP_PORT: u16 = 51001;
pub const DEFAULT_API_PORT: u16 = 51002;

/// Filesystem paths for a Corrosion data directory.
#[derive(Debug, Clone)]
pub struct Paths {
    pub dir: PathBuf,
    pub config: PathBuf,
    pub schema: PathBuf,
    pub db: PathBuf,
    pub admin: PathBuf,
}

impl Paths {
    /// Derive all paths from a root data directory.
    #[must_use] 
    pub fn new(data_dir: &Path) -> Self {
        let dir = data_dir.join("corrosion");
        Self {
            config: dir.join("config.toml"),
            schema: dir.join("schema.sql"),
            db: dir.join("store.db"),
            admin: dir.join("admin.sock"),
            dir,
        }
    }
}

/// Default gossip address (`0.0.0.0:51001`).
#[must_use] 
pub fn default_gossip_addr() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], DEFAULT_GOSSIP_PORT))
}

/// Default API address (`0.0.0.0:51002`).
#[must_use] 
pub fn default_api_addr() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], DEFAULT_API_PORT))
}

/// Corrosion TOML configuration.
#[derive(Debug, Serialize)]
struct Config {
    db: DbConfig,
    gossip: GossipConfig,
    api: ApiConfig,
    admin: AdminConfig,
}

#[derive(Debug, Serialize)]
struct DbConfig {
    path: String,
    schema_paths: Vec<String>,
}

#[derive(Debug, Serialize)]
struct GossipConfig {
    addr: String,
    bootstrap: Vec<String>,
    plaintext: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    member_id: Option<u16>,
}

#[derive(Debug, Serialize)]
struct ApiConfig {
    addr: String,
}

#[derive(Debug, Serialize)]
struct AdminConfig {
    path: String,
}

/// Write `config.toml` and `schema.sql` to disk.
/// Derive a deterministic u16 member_id from a network ID string.
/// Uses FNV-style hashing to map to u16 space.
fn network_id_to_member_id(network_id: &str) -> u16 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in network_id.as_bytes() {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    (hash & 0xFFFF) as u16
}

pub fn write_config(
    paths: &Paths,
    schema: &str,
    gossip_addr: SocketAddr,
    api_addr: SocketAddr,
    bootstrap: &[String],
    network_id: Option<&str>,
) -> io::Result<()> {
    fs::create_dir_all(&paths.dir)?;

    let cfg = Config {
        db: DbConfig {
            path: paths.db.to_string_lossy().into_owned(),
            schema_paths: vec![paths.schema.to_string_lossy().into_owned()],
        },
        gossip: GossipConfig {
            addr: gossip_addr.to_string(),
            bootstrap: bootstrap.to_vec(),
            plaintext: true,
            member_id: network_id.map(network_id_to_member_id),
        },
        api: ApiConfig {
            addr: api_addr.to_string(),
        },
        admin: AdminConfig {
            path: paths.admin.to_string_lossy().into_owned(),
        },
    };

    let toml = toml::to_string_pretty(&cfg).map_err(io::Error::other)?;
    fs::write(&paths.config, toml)?;
    fs::write(&paths.schema, schema)?;

    Ok(())
}
