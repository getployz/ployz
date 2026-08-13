use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    net::{Ipv6Addr, SocketAddr},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    str::FromStr,
    sync::atomic::{AtomicU64, Ordering},
};

use ployz_core::MachineId;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

pub(crate) fn expand_home(path: &Path) -> PathBuf {
    path.strip_prefix("~").map_or_else(
        |_| path.to_owned(),
        |suffix| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("~"))
                .join(suffix)
        },
    )
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub current_context: String,
    #[serde(default)]
    pub contexts: BTreeMap<String, Context>,
    #[serde(skip)]
    path: PathBuf,
}

impl Config {
    #[must_use]
    pub fn new(
        path: impl Into<PathBuf>,
        current_context: impl Into<String>,
        contexts: BTreeMap<String, Context>,
    ) -> Self {
        Self {
            current_context: current_context.into(),
            contexts,
            path: path.into(),
        }
    }

    pub fn load(path: impl Into<PathBuf>) -> Result<Self, ConfigError> {
        let path = path.into();
        let yaml = fs::read_to_string(&path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        let mut config =
            serde_norway::from_str::<Self>(&yaml).map_err(|source| ConfigError::Parse {
                path: path.clone(),
                source,
            })?;
        config.path = path;
        Ok(config)
    }

    pub fn load_or_empty(path: impl Into<PathBuf>) -> Result<Self, ConfigError> {
        let path = path.into();
        match Self::load(&path) {
            Ok(config) => Ok(config),
            Err(ConfigError::Read { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                Ok(Self::new(path, "", BTreeMap::new()))
            }
            Err(error) => Err(error),
        }
    }

    pub fn save(&self) -> Result<(), ConfigError> {
        let yaml = serde_norway::to_string(self).map_err(ConfigError::Encode)?;
        let parent = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        create_private_directories(parent)?;
        let temporary = temporary_path(&self.path);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|source| ConfigError::Write {
                path: self.path.clone(),
                source,
            })?;
        let result = file
            .write_all(yaml.as_bytes())
            .and_then(|()| file.sync_all())
            .and_then(|()| fs::rename(&temporary, &self.path))
            .and_then(|()| File::open(parent)?.sync_all());
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.map_err(|source| ConfigError::Write {
            path: self.path.clone(),
            source,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

static TEMPORARY_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temporary_path(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        TEMPORARY_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

fn create_private_directories(path: &Path) -> Result<(), ConfigError> {
    let mut missing = Vec::new();
    let mut next = path;
    while !next.exists() {
        missing.push(next.to_path_buf());
        next = next.parent().unwrap_or_else(|| Path::new("."));
    }
    fs::create_dir_all(path).map_err(|source| ConfigError::CreateDirectory {
        path: path.to_owned(),
        source,
    })?;
    for directory in missing {
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(|source| {
            ConfigError::CreateDirectory {
                path: directory,
                source,
            }
        })?;
    }
    Ok(())
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    #[serde(default)]
    pub connections: Vec<Connection>,
}

impl Context {
    pub fn select_connection(&mut self, index: usize) -> bool {
        let Some(prefix) = self.connections.get_mut(..=index) else {
            return false;
        };
        prefix.rotate_right(1);
        true
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedConnections {
    pub source: ConnectionSource,
    pub connections: Vec<Connection>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectionSource {
    Direct,
    Context(String),
    LocalSocket,
}

pub fn select_connections(
    direct: Option<Connection>,
    config: Option<&Config>,
    context_override: Option<&str>,
    local_socket_available: bool,
    local_socket: impl AsRef<Path>,
) -> Result<SelectedConnections, ContextError> {
    if let Some(connection) = direct {
        return Ok(SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![connection],
        });
    }
    if let Some(config) = config {
        if config.contexts.is_empty() {
            return Err(ContextError::NoContexts(config.path.clone()));
        }
        let name = context_override.unwrap_or(&config.current_context);
        if name.is_empty() {
            return Err(ContextError::NoCurrentContext(config.path.clone()));
        }
        let context = config
            .contexts
            .get(name)
            .ok_or_else(|| ContextError::ContextNotFound {
                name: name.to_owned(),
                path: config.path.clone(),
            })?;
        if context.connections.is_empty() {
            return Err(ContextError::NoConnections {
                name: name.to_owned(),
                path: config.path.clone(),
            });
        }
        return Ok(SelectedConnections {
            source: ConnectionSource::Context(name.to_owned()),
            connections: context.connections.clone(),
        });
    }
    if local_socket_available {
        return Ok(SelectedConnections {
            source: ConnectionSource::LocalSocket,
            connections: vec![
                Connection::unix(local_socket.as_ref()).map_err(ContextError::Connection)?,
            ],
        });
    }
    Err(ContextError::NoConfig)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Connection {
    transport: Transport,
    machine_id: Option<MachineId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Transport {
    Ssh {
        destination: SshDestination,
        key_file: Option<PathBuf>,
    },
    Tcp(SocketAddr),
    Unix(PathBuf),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectionFile {
    #[serde(flatten)]
    transport: TransportFile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ssh_key_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    machine_id: Option<MachineId>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum TransportFile {
    Ssh(String),
    Tcp(SocketAddr),
    Unix(PathBuf),
}

impl Connection {
    #[must_use]
    pub fn ssh(destination: SshDestination) -> Self {
        Self {
            transport: Transport::Ssh {
                destination,
                key_file: None,
            },
            machine_id: None,
        }
    }

    #[must_use]
    pub fn tcp(address: SocketAddr) -> Self {
        Self {
            transport: Transport::Tcp(address),
            machine_id: None,
        }
    }

    pub fn unix(path: impl Into<PathBuf>) -> Result<Self, ConnectionError> {
        let path = path.into();
        if !path.is_absolute() {
            return Err(ConnectionError::UnixPath(path));
        }
        Ok(Self {
            transport: Transport::Unix(path),
            machine_id: None,
        })
    }

    #[must_use]
    pub fn with_machine_id(mut self, machine_id: MachineId) -> Self {
        self.machine_id = Some(machine_id);
        self
    }

    pub fn with_ssh_key_file(mut self, path: impl Into<PathBuf>) -> Result<Self, ConnectionError> {
        let Transport::Ssh { key_file, .. } = &mut self.transport else {
            return Err(ConnectionError::SshKeyTransport);
        };
        *key_file = Some(path.into());
        Ok(self)
    }

    #[must_use]
    pub fn transport(&self) -> &Transport {
        &self.transport
    }

    #[must_use]
    pub fn ssh_key_file(&self) -> Option<&Path> {
        match &self.transport {
            Transport::Ssh { key_file, .. } => key_file.as_deref(),
            Transport::Tcp(_) | Transport::Unix(_) => None,
        }
    }

    #[must_use]
    pub fn machine_id(&self) -> Option<&MachineId> {
        self.machine_id.as_ref()
    }
}

impl fmt::Display for Connection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.transport {
            Transport::Ssh { destination, .. } => write!(formatter, "ssh://{destination}"),
            Transport::Tcp(address) => write!(formatter, "tcp://{address}"),
            Transport::Unix(path) => write!(formatter, "unix://{}", path.display()),
        }
    }
}

impl FromStr for Connection {
    type Err = ConnectionError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Some(address) = value.strip_prefix("tcp://") {
            return address
                .parse()
                .map(Self::tcp)
                .map_err(|_| ConnectionError::TcpAddress(value.to_owned()));
        }
        if let Some(path) = value.strip_prefix("unix://") {
            return Self::unix(path);
        }
        if value.starts_with("ssh+go://") || value.starts_with("ssh+cli://") {
            return Err(ConnectionError::RemovedScheme(value.to_owned()));
        }
        if value.contains("://") && !value.starts_with("ssh://") {
            return Err(ConnectionError::UnsupportedScheme(value.to_owned()));
        }
        SshDestination::parse(value.strip_prefix("ssh://").unwrap_or(value)).map(Self::ssh)
    }
}

impl Serialize for Connection {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let transport = match &self.transport {
            Transport::Ssh { destination, .. } => TransportFile::Ssh(destination.to_string()),
            Transport::Tcp(address) => TransportFile::Tcp(*address),
            Transport::Unix(path) => TransportFile::Unix(path.clone()),
        };
        ConnectionFile {
            transport,
            ssh_key_file: self.ssh_key_file().map(Path::to_owned),
            machine_id: self.machine_id.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Connection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let file = ConnectionFile::deserialize(deserializer)?;
        let transport = match (file.transport, file.ssh_key_file) {
            (TransportFile::Ssh(destination), key_file) => Transport::Ssh {
                destination: SshDestination::parse(destination).map_err(de::Error::custom)?,
                key_file,
            },
            (TransportFile::Tcp(address), None) => Transport::Tcp(address),
            (TransportFile::Unix(path), None) if path.is_absolute() => Transport::Unix(path),
            (TransportFile::Unix(path), None) => {
                return Err(de::Error::custom(ConnectionError::UnixPath(path)));
            }
            (TransportFile::Tcp(_) | TransportFile::Unix(_), Some(_)) => {
                return Err(de::Error::custom(ConnectionError::SshKeyTransport));
            }
        };
        Ok(Self {
            transport,
            machine_id: file.machine_id,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SshDestination {
    value: String,
    target: String,
    port: Option<u16>,
}

impl SshDestination {
    pub fn parse(value: impl Into<String>) -> Result<Self, ConnectionError> {
        let value = value.into();
        let Some((user, destination)) = value.split_once('@') else {
            return Err(ConnectionError::SshDestination(value));
        };
        if !valid_ssh_user(user)
            || destination.is_empty()
            || destination.contains('@')
            || value.chars().any(char::is_whitespace)
        {
            return Err(ConnectionError::SshDestination(value));
        }
        let (host, port) = split_ssh_port(destination)
            .ok_or_else(|| ConnectionError::SshDestination(value.clone()))?;
        if !valid_ssh_host(host) {
            return Err(ConnectionError::SshDestination(value));
        }
        let target = format!("{user}@{host}");
        Ok(Self {
            value,
            target,
            port,
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }

    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }

    #[must_use]
    pub fn port(&self) -> Option<u16> {
        self.port
    }
}

fn valid_ssh_user(user: &str) -> bool {
    !user.is_empty()
        && !user.starts_with('-')
        && user
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
}

fn valid_ssh_host(host: &str) -> bool {
    if let Some(host) = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
    {
        return host.parse::<Ipv6Addr>().is_ok();
    }
    if host.contains(':') {
        return host.parse::<Ipv6Addr>().is_ok();
    }
    !host.is_empty()
        && !host.starts_with('-')
        && host
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || ".-_".contains(character))
}

fn split_ssh_port(destination: &str) -> Option<(&str, Option<u16>)> {
    if let Some(bracketed) = destination.strip_prefix('[') {
        let (host, suffix) = bracketed.split_once(']')?;
        if host.parse::<Ipv6Addr>().is_err() {
            return None;
        }
        return match suffix {
            "" => Some((destination, None)),
            value if value.starts_with(':') => value[1..]
                .parse::<u16>()
                .ok()
                .filter(|port| *port != 0)
                .map(|port| (&destination[..host.len() + 2], Some(port))),
            _ => None,
        };
    }
    if destination.matches(':').count() == 1 {
        let (host, port) = destination.rsplit_once(':')?;
        return (!host.is_empty())
            .then(|| port.parse::<u16>().ok().filter(|port| *port != 0))
            .flatten()
            .map(|port| (host, Some(port)));
    }
    if destination.contains(':') && destination.parse::<Ipv6Addr>().is_err() {
        return None;
    }
    Some((destination, None))
}

impl fmt::Display for SshDestination {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.value)
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not read Ployz config {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("could not parse Ployz config {path}: {source}")]
    Parse {
        path: PathBuf,
        source: serde_norway::Error,
    },
    #[error("could not encode Ployz config: {0}")]
    Encode(serde_norway::Error),
    #[error("could not create Ployz config directory {path}: {source}")]
    CreateDirectory { path: PathBuf, source: io::Error },
    #[error("could not write Ployz config {path}: {source}")]
    Write { path: PathBuf, source: io::Error },
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ConnectionError {
    #[error("invalid SSH destination {0:?}")]
    SshDestination(String),
    #[error("invalid TCP address {0:?}")]
    TcpAddress(String),
    #[error("Unix connection path must be absolute: {0}")]
    UnixPath(PathBuf),
    #[error("connection scheme was removed: {0:?}")]
    RemovedScheme(String),
    #[error("ssh_key_file requires the SSH transport")]
    SshKeyTransport,
    #[error("unsupported connection scheme: {0:?}")]
    UnsupportedScheme(String),
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ContextError {
    #[error("no Ployz config or local daemon socket is available")]
    NoConfig,
    #[error("no contexts found in Ployz config {0}")]
    NoContexts(PathBuf),
    #[error("current context is not set in Ployz config {0}")]
    NoCurrentContext(PathBuf),
    #[error("context {name:?} not found in Ployz config {path}")]
    ContextNotFound { name: String, path: PathBuf },
    #[error("no connections found in context {name:?} in Ployz config {path}")]
    NoConnections { name: String, path: PathBuf },
    #[error(transparent)]
    Connection(ConnectionError),
}
