use serde::Serialize;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::{fs, io};

const DEFAULT_GOSSIP_PORT: u16 = 51001;
const DEFAULT_API_PORT: u16 = 51002;

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
pub fn default_gossip_addr() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], DEFAULT_GOSSIP_PORT))
}

/// Default API address (`0.0.0.0:51002`).
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
pub fn write_config(
    paths: &Paths,
    schema: &str,
    gossip_addr: SocketAddr,
    api_addr: SocketAddr,
    bootstrap: &[String],
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
        },
        api: ApiConfig {
            addr: api_addr.to_string(),
        },
        admin: AdminConfig {
            path: paths.admin.to_string_lossy().into_owned(),
        },
    };

    let toml = toml::to_string_pretty(&cfg).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    fs::write(&paths.config, toml)?;
    fs::write(&paths.schema, schema)?;

    Ok(())
}
