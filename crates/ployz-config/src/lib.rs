pub mod corrosion;

use directories::{BaseDirs, ProjectDirs};
use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Linux,
    Darwin,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeTarget {
    Docker,
    Host,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceMode {
    User,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostPathsContext {
    pub os: Os,
    pub is_root: bool,
}

#[derive(Debug, Error)]
pub enum ConfigLoadError {
    #[error("failed to load configuration: {0}")]
    Load(Box<figment::Error>),
}

impl From<figment::Error> for ConfigLoadError {
    fn from(error: figment::Error) -> Self {
        Self::Load(Box::new(error))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClientConfig {
    pub socket: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DaemonConfig {
    pub data_dir: PathBuf,
    pub socket: String,
    #[serde(default)]
    pub builtin_images_manifest: Option<PathBuf>,
    #[serde(default = "default_cluster_cidr")]
    pub cluster_cidr: String,
    #[serde(default = "default_subnet_prefix_len")]
    pub subnet_prefix_len: u8,
    #[serde(default = "default_remote_control_port")]
    pub remote_control_port: u16,
    #[serde(default = "default_gateway_listen_addr")]
    pub gateway_listen_addr: String,
    #[serde(default = "default_gateway_threads")]
    pub gateway_threads: usize,
}

fn default_cluster_cidr() -> String {
    "10.101.0.0/16".to_string()
}

fn default_subnet_prefix_len() -> u8 {
    24
}

fn default_remote_control_port() -> u16 {
    4317
}

fn default_gateway_listen_addr() -> String {
    "0.0.0.0:80".to_string()
}

fn default_gateway_threads() -> usize {
    2
}

#[derive(Debug, Clone, Serialize)]
struct RuntimeDefaults {
    data_dir: PathBuf,
    socket: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    builtin_images_manifest: Option<PathBuf>,
    cluster_cidr: String,
    subnet_prefix_len: u8,
    remote_control_port: u16,
    gateway_listen_addr: String,
    gateway_threads: usize,
}

#[derive(Debug, Clone, Serialize)]
struct ClientOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    socket: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct DaemonOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    data_dir: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    socket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    builtin_images_manifest: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cluster_cidr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subnet_prefix_len: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_control_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gateway_listen_addr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gateway_threads: Option<usize>,
}

#[must_use]
pub fn default_data_dir(context: &HostPathsContext) -> PathBuf {
    match context.os {
        Os::Linux if context.is_root => "/var/lib/ployz".into(),
        Os::Linux | Os::Darwin | Os::Other => project_dirs()
            .map(|dirs| dirs.data_local_dir().to_path_buf())
            .unwrap_or_else(|| home_dir().join(".ployz")),
    }
}

#[must_use]
pub fn default_socket_path(context: &HostPathsContext) -> String {
    let path = match context.os {
        Os::Linux if context.is_root => PathBuf::from("/run/ployz/ployzd.sock"),
        Os::Linux => base_dirs()
            .and_then(|dirs| {
                dirs.runtime_dir()
                    .map(|runtime| runtime.join("ployz/ployzd.sock"))
            })
            .unwrap_or_else(|| PathBuf::from("/tmp/ployz/ployzd.sock")),
        Os::Darwin => std::env::temp_dir().join("ployz/ployzd.sock"),
        Os::Other => PathBuf::from("/tmp/ployz/ployzd.sock"),
    };
    path.to_string_lossy().into_owned()
}

#[must_use]
pub fn default_config_path() -> PathBuf {
    project_dirs()
        .map(|dirs| dirs.config_dir().join("config.toml"))
        .unwrap_or_else(|| home_dir().join(".config/ployz/config.toml"))
}

pub fn resolve_config_path(cli_config_path: Option<PathBuf>) -> PathBuf {
    cli_config_path
        .or_else(|| std::env::var_os("PLOYZ_CONFIG").map(PathBuf::from))
        .unwrap_or_else(default_config_path)
}

/// Path to a network's directory: `<data_dir>/networks/<name>/`
#[must_use]
pub fn network_dir(data_dir: &Path, name: &str) -> PathBuf {
    data_dir.join("networks").join(name)
}

/// Path to a network's config file: `<data_dir>/networks/<name>/network.json`
#[must_use]
pub fn network_config_path(data_dir: &Path, name: &str) -> PathBuf {
    network_dir(data_dir, name).join("network.json")
}

/// Read the active network name from `<data_dir>/active_network`.
#[must_use]
pub fn read_active_network(data_dir: &Path) -> Option<String> {
    std::fs::read_to_string(data_dir.join("active_network"))
        .ok()
        .map(|content| content.trim().to_string())
        .filter(|content| !content.is_empty())
}

pub fn load_client_config(
    cli_config_path: Option<PathBuf>,
    cli_socket: Option<String>,
    context: &HostPathsContext,
) -> std::result::Result<ClientConfig, ConfigLoadError> {
    let overrides = ClientOverrides { socket: cli_socket };

    build_figment(cli_config_path, context)
        .merge(Serialized::defaults(overrides))
        .extract()
        .map_err(ConfigLoadError::from)
}

pub fn load_daemon_config(
    cli_config_path: Option<PathBuf>,
    cli_data_dir: Option<PathBuf>,
    cli_socket: Option<String>,
    cli_remote_control_port: Option<u16>,
    context: &HostPathsContext,
) -> std::result::Result<DaemonConfig, ConfigLoadError> {
    let overrides = DaemonOverrides {
        data_dir: cli_data_dir,
        socket: cli_socket,
        builtin_images_manifest: None,
        cluster_cidr: None,
        subnet_prefix_len: None,
        remote_control_port: cli_remote_control_port,
        gateway_listen_addr: None,
        gateway_threads: None,
    };

    build_figment(cli_config_path, context)
        .merge(Serialized::defaults(overrides))
        .extract()
        .map_err(ConfigLoadError::from)
}

fn build_figment(cli_config_path: Option<PathBuf>, context: &HostPathsContext) -> Figment {
    let defaults = RuntimeDefaults {
        data_dir: default_data_dir(context),
        socket: default_socket_path(context),
        builtin_images_manifest: None,
        cluster_cidr: default_cluster_cidr(),
        subnet_prefix_len: default_subnet_prefix_len(),
        remote_control_port: default_remote_control_port(),
        gateway_listen_addr: default_gateway_listen_addr(),
        gateway_threads: default_gateway_threads(),
    };

    let mut figment = Figment::new().merge(Serialized::defaults(defaults));
    let config_path = resolve_config_path(cli_config_path);
    if config_path.exists() {
        figment = figment.merge(Toml::file(config_path));
    }

    figment.merge(Env::prefixed("PLOYZ_"))
}

fn base_dirs() -> Option<BaseDirs> {
    BaseDirs::new()
}

fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("", "", "ployz")
}

fn home_dir() -> PathBuf {
    base_dirs()
        .map(|dirs| dirs.home_dir().to_path_buf())
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_config_reads_builtin_images_manifest_from_env() {
        let manifest_path = std::env::temp_dir().join("ployz-builtins-config-test.toml");
        unsafe {
            std::env::set_var("PLOYZ_BUILTIN_IMAGES_MANIFEST", &manifest_path);
        }

        let loaded = load_daemon_config(
            None,
            None,
            None,
            None,
            &HostPathsContext {
                os: Os::Darwin,
                is_root: false,
            },
        )
        .expect("daemon config should load");

        assert_eq!(
            loaded.builtin_images_manifest.as_deref(),
            Some(manifest_path.as_path())
        );

        unsafe {
            std::env::remove_var("PLOYZ_BUILTIN_IMAGES_MANIFEST");
        }
    }
}
