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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Memory,
    Docker,
    HostExec,
    HostService,
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
    pub fn detect() -> Self {
        let os = if cfg!(target_os = "linux") {
            Os::Linux
        } else if cfg!(target_os = "macos") {
            Os::Darwin
        } else {
            Os::Other
        };
        Self {
            os,
            has_kernel_wireguard: false,
            has_docker: false,
            is_root: false,
            has_wg_helper: false,
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
    #[serde(default = "default_cluster_cidr")]
    pub cluster_cidr: String,
    #[serde(default = "default_subnet_prefix_len")]
    pub subnet_prefix_len: u8,
}

fn default_cluster_cidr() -> String {
    "10.210.0.0/16".to_string()
}

fn default_subnet_prefix_len() -> u8 {
    24
}

#[derive(Debug, Clone, Serialize)]
struct RuntimeDefaults {
    data_dir: PathBuf,
    socket: String,
    cluster_cidr: String,
    subnet_prefix_len: u8,
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
    cluster_cidr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subnet_prefix_len: Option<u8>,
}

/// Returns the platform-appropriate default data directory.
///
/// - Linux (root):  `/var/lib/ployz`
/// - Linux (user):  `$XDG_DATA_HOME/ployz` or `~/.local/share/ployz`
/// - macOS:         `~/Library/Application Support/ployz`
/// - Other:         project data dir (`~/.ployz` fallback)
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
pub fn default_config_path() -> PathBuf {
    project_dirs()
        .map(|dirs| dirs.config_dir().join("config.toml"))
        .unwrap_or_else(|| home_dir().join(".config/ployz/config.toml"))
}

/// Returns the effective config file path, honoring `PLOYZ_CONFIG`.
pub fn resolve_config_path() -> PathBuf {
    std::env::var_os("PLOYZ_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(default_config_path)
}

pub fn load_client_config(
    cli_socket: Option<String>,
    aff: &Affordances,
) -> std::result::Result<ClientConfig, ConfigLoadError> {
    let overrides = ClientOverrides { socket: cli_socket };

    build_figment(aff)
        .merge(Serialized::defaults(overrides))
        .extract()
        .map_err(ConfigLoadError::from)
}

pub fn load_daemon_config(
    cli_data_dir: Option<PathBuf>,
    cli_socket: Option<String>,
    aff: &Affordances,
) -> std::result::Result<DaemonConfig, ConfigLoadError> {
    let overrides = DaemonOverrides {
        data_dir: cli_data_dir,
        socket: cli_socket,
        cluster_cidr: None,
        subnet_prefix_len: None,
    };

    build_figment(aff)
        .merge(Serialized::defaults(overrides))
        .extract()
        .map_err(ConfigLoadError::from)
}

fn build_figment(aff: &Affordances) -> Figment {
    let defaults = RuntimeDefaults {
        data_dir: default_data_dir(aff),
        socket: default_socket_path(aff),
        cluster_cidr: default_cluster_cidr(),
        subnet_prefix_len: default_subnet_prefix_len(),
    };

    let mut figment = Figment::new().merge(Serialized::defaults(defaults));
    let config_path = resolve_config_path();
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

pub fn validate_mode(mode: Mode, aff: &Affordances) -> std::result::Result<(), String> {
    match mode {
        Mode::Memory => Ok(()),
        Mode::Docker => {
            if !aff.has_docker {
                return Err("Docker mode requires a running Docker daemon".into());
            }
            Ok(())
        }
        Mode::HostExec | Mode::HostService => {
            if aff.os == Os::Other {
                return Err(format!("{mode:?} is not supported on this platform"));
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
    fn memory_mode_always_valid() {
        assert!(validate_mode(Mode::Memory, &aff(Os::Other, false)).is_ok());
    }

    #[test]
    fn docker_mode_requires_docker() {
        assert!(validate_mode(Mode::Docker, &aff(Os::Linux, false)).is_err());
        assert!(validate_mode(Mode::Docker, &aff(Os::Linux, true)).is_ok());
    }

    #[test]
    fn host_modes_reject_unknown_os() {
        assert!(validate_mode(Mode::HostExec, &aff(Os::Other, false)).is_err());
        assert!(validate_mode(Mode::HostExec, &aff(Os::Linux, false)).is_ok());
    }

}
