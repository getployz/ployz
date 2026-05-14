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

#[must_use]
pub fn detect_host_paths_context() -> HostPathsContext {
    HostPathsContext {
        os: detect_os(),
        is_root: current_user_is_root(),
    }
}

#[must_use]
pub fn detect_os() -> Os {
    if cfg!(target_os = "linux") {
        Os::Linux
    } else if cfg!(target_os = "macos") {
        Os::Darwin
    } else {
        Os::Other
    }
}

#[must_use]
pub fn current_user_is_root() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: `geteuid` has no Rust-side preconditions.
        unsafe { libc::geteuid() == 0 }
    }

    #[cfg(not(unix))]
    {
        false
    }
}

#[derive(Debug, Error)]
pub enum ConfigLoadError {
    #[error("failed to load configuration: {0}")]
    Load(#[from] figment::Error),
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
    pub storage: StorageConfig,
    #[serde(default)]
    pub build: BuildConfig,
    #[serde(default)]
    pub certificates: CertificateConfig,
    #[serde(default)]
    pub builtin_images_manifest: Option<PathBuf>,
    #[serde(default)]
    pub daemon_metrics_listen_addr: Option<String>,
    #[serde(default)]
    pub dns_metrics_listen_addr: Option<String>,
    #[serde(default)]
    pub gateway_metrics_listen_addr: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub az: Option<String>,
    #[serde(default = "default_cluster_cidr")]
    pub cluster_cidr: String,
    #[serde(default = "default_subnet_prefix_len")]
    pub subnet_prefix_len: u8,
    #[serde(default = "default_zfs_transfer_port")]
    pub zfs_transfer_port: u16,
    #[serde(default = "default_gateway_listen_addr")]
    pub gateway_listen_addr: String,
    #[serde(default)]
    pub gateway_https_listen_addr: Option<String>,
    #[serde(default = "default_gateway_threads")]
    pub gateway_threads: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StorageConfig {
    #[serde(default)]
    pub backend: VolumeBackend,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zfs_root: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub btrfs_root: Option<PathBuf>,
    #[serde(default = "default_overcommit_ratio")]
    pub overcommit_ratio: f64,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            backend: VolumeBackend::default(),
            zfs_root: None,
            btrfs_root: None,
            overcommit_ratio: default_overcommit_ratio(),
        }
    }
}

impl StorageConfig {
    #[must_use]
    pub fn is_zfs_backend(&self) -> bool {
        self.backend == VolumeBackend::Zfs
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VolumeBackend {
    #[default]
    Zfs,
    DockerVolume,
    Btrfs,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BuildConfig {
    #[serde(default)]
    pub default_backend: BuildBackend,
}

impl Default for BuildConfig {
    fn default() -> Self {
        Self {
            default_backend: BuildBackend::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BuildBackend {
    #[default]
    Dockerfile,
    Railpack,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CertificateConfig {
    #[serde(default)]
    pub issuer: CertificateIssuerBackend,
}

impl Default for CertificateConfig {
    fn default() -> Self {
        Self {
            issuer: CertificateIssuerBackend::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CertificateIssuerBackend {
    #[default]
    Acme,
    Static,
    Imported,
}

fn default_overcommit_ratio() -> f64 {
    1.0
}

fn default_cluster_cidr() -> String {
    "10.101.0.0/16".to_string()
}

fn default_subnet_prefix_len() -> u8 {
    24
}

fn default_zfs_transfer_port() -> u16 {
    4319
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
    storage: StorageConfig,
    build: BuildConfig,
    certificates: CertificateConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    builtin_images_manifest: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    daemon_metrics_listen_addr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dns_metrics_listen_addr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gateway_metrics_listen_addr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    az: Option<String>,
    cluster_cidr: String,
    subnet_prefix_len: u8,
    zfs_transfer_port: u16,
    gateway_listen_addr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    gateway_https_listen_addr: Option<String>,
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
    storage: Option<StorageConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    build: Option<BuildConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    certificates: Option<CertificateConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    builtin_images_manifest: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    daemon_metrics_listen_addr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dns_metrics_listen_addr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gateway_metrics_listen_addr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    az: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cluster_cidr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subnet_prefix_len: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    zfs_transfer_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gateway_listen_addr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gateway_https_listen_addr: Option<String>,
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

#[allow(clippy::result_large_err)]
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

#[allow(clippy::result_large_err)]
pub fn load_daemon_config(
    cli_config_path: Option<PathBuf>,
    cli_data_dir: Option<PathBuf>,
    cli_socket: Option<String>,
    cli_zfs_transfer_port: Option<u16>,
    context: &HostPathsContext,
) -> std::result::Result<DaemonConfig, ConfigLoadError> {
    let overrides = DaemonOverrides {
        data_dir: cli_data_dir,
        socket: cli_socket,
        storage: None,
        build: None,
        certificates: None,
        builtin_images_manifest: None,
        daemon_metrics_listen_addr: None,
        dns_metrics_listen_addr: None,
        gateway_metrics_listen_addr: None,
        region: None,
        az: None,
        cluster_cidr: None,
        subnet_prefix_len: None,
        zfs_transfer_port: cli_zfs_transfer_port,
        gateway_listen_addr: None,
        gateway_https_listen_addr: None,
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
        storage: StorageConfig::default(),
        build: BuildConfig::default(),
        certificates: CertificateConfig::default(),
        builtin_images_manifest: None,
        daemon_metrics_listen_addr: None,
        dns_metrics_listen_addr: None,
        gateway_metrics_listen_addr: None,
        region: None,
        az: None,
        cluster_cidr: default_cluster_cidr(),
        subnet_prefix_len: default_subnet_prefix_len(),
        zfs_transfer_port: default_zfs_transfer_port(),
        gateway_listen_addr: default_gateway_listen_addr(),
        gateway_https_listen_addr: None,
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

    fn context(os: Os, is_root: bool) -> HostPathsContext {
        HostPathsContext { os, is_root }
    }

    #[test]
    fn daemon_config_reads_builtin_images_manifest_from_env() {
        let manifest_path = std::env::temp_dir().join("ployz-builtins-config-test.toml");
        unsafe {
            std::env::set_var("PLOYZ_BUILTIN_IMAGES_MANIFEST", &manifest_path);
        }

        let loaded = load_daemon_config(None, None, None, None, &context(Os::Darwin, false))
            .expect("daemon config should load");

        assert_eq!(
            loaded.builtin_images_manifest.as_deref(),
            Some(manifest_path.as_path())
        );

        unsafe {
            std::env::remove_var("PLOYZ_BUILTIN_IMAGES_MANIFEST");
        }
    }

    #[test]
    fn daemon_config_reads_metrics_listen_addrs_from_env() {
        unsafe {
            std::env::set_var("PLOYZ_DAEMON_METRICS_LISTEN_ADDR", "127.0.0.1:9100");
            std::env::set_var("PLOYZ_DNS_METRICS_LISTEN_ADDR", "127.0.0.1:9101");
            std::env::set_var("PLOYZ_GATEWAY_METRICS_LISTEN_ADDR", "127.0.0.1:9102");
        }

        let loaded = load_daemon_config(None, None, None, None, &context(Os::Darwin, false))
            .expect("daemon config should load");

        assert_eq!(
            loaded.daemon_metrics_listen_addr.as_deref(),
            Some("127.0.0.1:9100")
        );
        assert_eq!(
            loaded.dns_metrics_listen_addr.as_deref(),
            Some("127.0.0.1:9101")
        );
        assert_eq!(
            loaded.gateway_metrics_listen_addr.as_deref(),
            Some("127.0.0.1:9102")
        );

        unsafe {
            std::env::remove_var("PLOYZ_DAEMON_METRICS_LISTEN_ADDR");
            std::env::remove_var("PLOYZ_DNS_METRICS_LISTEN_ADDR");
            std::env::remove_var("PLOYZ_GATEWAY_METRICS_LISTEN_ADDR");
        }
    }

    #[test]
    fn daemon_config_reads_topology_from_env() {
        unsafe {
            std::env::set_var("PLOYZ_REGION", "eu-primary");
            std::env::set_var("PLOYZ_AZ", "hel1-a");
        }

        let loaded = load_daemon_config(None, None, None, None, &context(Os::Darwin, false))
            .expect("daemon config should load");

        assert_eq!(loaded.region.as_deref(), Some("eu-primary"));
        assert_eq!(loaded.az.as_deref(), Some("hel1-a"));

        unsafe {
            std::env::remove_var("PLOYZ_REGION");
            std::env::remove_var("PLOYZ_AZ");
        }
    }

    #[test]
    fn daemon_config_reads_zfs_transfer_port_from_env_and_cli() {
        unsafe {
            std::env::set_var("PLOYZ_ZFS_TRANSFER_PORT", "4444");
        }

        let from_env = load_daemon_config(None, None, None, None, &context(Os::Darwin, false))
            .expect("daemon config should load");
        let from_cli =
            load_daemon_config(None, None, None, Some(5555), &context(Os::Darwin, false))
                .expect("daemon config should load");

        assert_eq!(from_env.zfs_transfer_port, 4444);
        assert_eq!(from_cli.zfs_transfer_port, 5555);

        unsafe {
            std::env::remove_var("PLOYZ_ZFS_TRANSFER_PORT");
        }
    }

    #[test]
    fn daemon_config_reads_storage_zfs_root_from_toml() {
        let path =
            std::env::temp_dir().join(format!("ployz-storage-config-{}.toml", std::process::id()));
        std::fs::write(&path, "[storage]\nzfs_root = \"tank/ployz\"\n").expect("write config");

        let loaded = load_daemon_config(
            Some(path.clone()),
            None,
            None,
            None,
            &context(Os::Darwin, false),
        )
        .expect("daemon config should load");

        assert_eq!(
            loaded.storage.zfs_root.as_deref(),
            Some(Path::new("tank/ployz"))
        );
        assert!((loaded.storage.overcommit_ratio - 1.0).abs() < f64::EPSILON);

        std::fs::remove_file(path).expect("remove config");
    }

    #[test]
    fn daemon_config_reads_backend_selection_from_toml() {
        let path = std::env::temp_dir().join(format!(
            "ployz-backend-selection-{}.toml",
            std::process::id()
        ));
        std::fs::write(
            &path,
            "[storage]\nbackend = \"docker-volume\"\nbtrfs_root = \"/var/lib/ployz/btrfs\"\n\n[build]\ndefault_backend = \"railpack\"\n\n[certificates]\nissuer = \"static\"\n",
        )
        .expect("write config");

        let loaded = load_daemon_config(
            Some(path.clone()),
            None,
            None,
            None,
            &context(Os::Darwin, false),
        )
        .expect("daemon config should load");

        assert_eq!(loaded.storage.backend, VolumeBackend::DockerVolume);
        assert!(!loaded.storage.is_zfs_backend());
        assert_eq!(
            loaded.storage.btrfs_root.as_deref(),
            Some(Path::new("/var/lib/ployz/btrfs"))
        );
        assert_eq!(loaded.build.default_backend, BuildBackend::Railpack);
        assert_eq!(loaded.certificates.issuer, CertificateIssuerBackend::Static);

        std::fs::remove_file(path).expect("remove config");
    }

    #[test]
    fn daemon_config_reads_storage_overcommit_ratio_from_toml() {
        let path = std::env::temp_dir().join(format!(
            "ployz-storage-overcommit-{}.toml",
            std::process::id()
        ));
        std::fs::write(
            &path,
            "[storage]\nzfs_root = \"tank/ployz\"\novercommit_ratio = 1.5\n",
        )
        .expect("write config");

        let loaded = load_daemon_config(
            Some(path.clone()),
            None,
            None,
            None,
            &context(Os::Darwin, false),
        )
        .expect("daemon config should load");

        assert!((loaded.storage.overcommit_ratio - 1.5).abs() < f64::EPSILON);

        std::fs::remove_file(path).expect("remove config");
    }
}
