use directories::{BaseDirs, ProjectDirs};
use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
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

#[derive(Debug, Clone)]
pub struct Affordances {
    pub os: Os,
    pub has_kernel_wireguard: bool,
    pub has_docker: bool,
    pub is_root: bool,
    pub has_wg_helper: bool,
}

impl Affordances {
    #[must_use]
    pub fn detect() -> Self {
        let os = if cfg!(target_os = "linux") {
            Os::Linux
        } else if cfg!(target_os = "macos") {
            Os::Darwin
        } else {
            Os::Other
        };
        let is_root = cfg!(unix) && unsafe { libc::geteuid() } == 0;
        let has_docker = std::process::Command::new("docker")
            .arg("info")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        let has_wg_helper = std::process::Command::new("wg")
            .arg("--help")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        Self {
            os,
            has_kernel_wireguard: false,
            has_docker,
            is_root,
            has_wg_helper,
        }
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
    "10.210.0.0/16".to_string()
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

/// Returns the platform-appropriate default data directory.
///
/// - Linux (root):  `/var/lib/ployz`
/// - Linux (user):  `$XDG_DATA_HOME/ployz` or `~/.local/share/ployz`
/// - macOS:         `~/Library/Application Support/ployz`
/// - Other:         project data dir (`~/.ployz` fallback)
#[must_use]
pub fn default_data_dir(aff: &Affordances) -> PathBuf {
    match aff.os {
        Os::Linux if aff.is_root => "/var/lib/ployz".into(),
        Os::Linux | Os::Darwin | Os::Other => project_dirs()
            .map(|dirs| dirs.data_local_dir().to_path_buf())
            .unwrap_or_else(|| home_dir().join(".ployz")),
    }
}

/// Returns the platform-appropriate default socket path.
///
/// Sockets are runtime artifacts, not persistent data — they belong
/// in the OS runtime directory, not the data directory.
///
/// - Linux (root):  `/run/ployz/ployzd.sock`
/// - Linux (user):  `$XDG_RUNTIME_DIR/ployz/ployzd.sock`
/// - macOS:         `$TMPDIR/ployz/ployzd.sock` (per-user, per-boot)
/// - Other:         `/tmp/ployz/ployzd.sock`
#[must_use]
pub fn default_socket_path(aff: &Affordances) -> String {
    let path = match aff.os {
        Os::Linux if aff.is_root => PathBuf::from("/run/ployz/ployzd.sock"),
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

/// Returns the default config file path.
///
/// Can be overridden via `PLOYZ_CONFIG`.
#[must_use]
pub fn default_config_path() -> PathBuf {
    project_dirs()
        .map(|dirs| dirs.config_dir().join("config.toml"))
        .unwrap_or_else(|| home_dir().join(".config/ployz/config.toml"))
}

/// Returns the effective config file path, honoring CLI override and `PLOYZ_CONFIG`.
pub fn resolve_config_path(cli_config_path: Option<PathBuf>) -> PathBuf {
    cli_config_path
        .or_else(|| std::env::var_os("PLOYZ_CONFIG").map(PathBuf::from))
        .unwrap_or_else(default_config_path)
}

#[allow(clippy::result_large_err)]
pub fn load_client_config(
    cli_config_path: Option<PathBuf>,
    cli_socket: Option<String>,
    aff: &Affordances,
) -> std::result::Result<ClientConfig, ConfigLoadError> {
    let overrides = ClientOverrides { socket: cli_socket };

    build_figment(cli_config_path, aff)
        .merge(Serialized::defaults(overrides))
        .extract()
        .map_err(ConfigLoadError::from)
}

#[allow(clippy::result_large_err)]
pub fn load_daemon_config(
    cli_config_path: Option<PathBuf>,
    cli_data_dir: Option<PathBuf>,
    cli_socket: Option<String>,
    cli_remote_control_port: Option<u16>,
    aff: &Affordances,
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

    build_figment(cli_config_path, aff)
        .merge(Serialized::defaults(overrides))
        .extract()
        .map_err(ConfigLoadError::from)
}

fn build_figment(cli_config_path: Option<PathBuf>, aff: &Affordances) -> Figment {
    let defaults = RuntimeDefaults {
        data_dir: default_data_dir(aff),
        socket: default_socket_path(aff),
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

pub fn validate_runtime(
    runtime_target: RuntimeTarget,
    service_mode: ServiceMode,
    aff: &Affordances,
) -> std::result::Result<(), String> {
    match runtime_target {
        RuntimeTarget::Docker => {
            if !aff.has_docker {
                return Err("docker runtime requires a running Docker daemon".into());
            }
            if service_mode != ServiceMode::User {
                return Err("docker runtime only supports user service mode".into());
            }
            Ok(())
        }
        RuntimeTarget::Host => {
            if aff.os == Os::Other {
                return Err("host runtime is not supported on this platform".into());
            }
            if service_mode == ServiceMode::System {
                if aff.os != Os::Linux {
                    return Err("system service mode is only supported on Linux".into());
                }
                if !aff.is_root {
                    return Err("system service mode requires sudo/root".into());
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aff(os: Os, docker: bool) -> Affordances {
        Affordances {
            os,
            has_kernel_wireguard: false,
            has_docker: docker,
            is_root: false,
            has_wg_helper: false,
        }
    }

    #[test]
    fn docker_runtime_requires_docker() {
        assert!(
            validate_runtime(
                RuntimeTarget::Docker,
                ServiceMode::User,
                &aff(Os::Linux, false)
            )
            .is_err()
        );
        assert!(
            validate_runtime(
                RuntimeTarget::Docker,
                ServiceMode::User,
                &aff(Os::Linux, true)
            )
            .is_ok()
        );
    }

    #[test]
    fn docker_runtime_rejects_system_service_mode() {
        assert!(
            validate_runtime(
                RuntimeTarget::Docker,
                ServiceMode::System,
                &aff(Os::Linux, true)
            )
            .is_err()
        );
    }

    #[test]
    fn host_runtime_rejects_unknown_os() {
        assert!(
            validate_runtime(
                RuntimeTarget::Host,
                ServiceMode::User,
                &aff(Os::Other, false)
            )
            .is_err()
        );
        assert!(
            validate_runtime(
                RuntimeTarget::Host,
                ServiceMode::User,
                &aff(Os::Linux, false)
            )
            .is_ok()
        );
    }

    #[test]
    fn host_system_service_requires_linux_root() {
        assert!(
            validate_runtime(
                RuntimeTarget::Host,
                ServiceMode::System,
                &aff(Os::Darwin, false)
            )
            .is_err()
        );

        let mut affordances = aff(Os::Linux, false);
        affordances.is_root = true;
        assert!(validate_runtime(RuntimeTarget::Host, ServiceMode::System, &affordances).is_ok());
    }

    #[test]
    fn daemon_config_reads_builtin_images_manifest_from_env() {
        let manifest_path = std::env::temp_dir().join("ployz-builtins-config-test.toml");
        // SAFETY: the test sets and clears a process env var for its own duration.
        unsafe {
            std::env::set_var("PLOYZ_BUILTIN_IMAGES_MANIFEST", &manifest_path);
        }

        let loaded = load_daemon_config(None, None, None, None, &aff(Os::Darwin, true))
            .expect("daemon config should load");

        assert_eq!(
            loaded.builtin_images_manifest.as_deref(),
            Some(manifest_path.as_path())
        );

        // SAFETY: the test removes the env var it set above.
        unsafe {
            std::env::remove_var("PLOYZ_BUILTIN_IMAGES_MANIFEST");
        }
    }
}
