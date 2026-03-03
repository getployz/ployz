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
    Dev,
    Agent,
    Prod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireGuardBackend {
    Kernel,
    Userspace,
    Docker,
    Memory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceBackend {
    System,
    Docker,
    Memory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeBackend {
    None,
    UserspaceProxy,
    HostRoutingHelper,
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
}

#[derive(Debug, Clone, Serialize)]
struct RuntimeDefaults {
    data_dir: PathBuf,
    socket: String,
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

#[derive(Debug, Clone)]
pub struct Profile {
    pub mode: Mode,
    pub wireguard: WireGuardBackend,
    pub services: ServiceBackend,
    pub bridge: BridgeBackend,
}

pub fn resolve_profile(aff: &Affordances, mode: Mode) -> Profile {
    match (aff.os, mode) {
        (Os::Linux, Mode::Prod) => {
            let wireguard = if aff.has_kernel_wireguard && aff.is_root {
                WireGuardBackend::Kernel
            } else {
                WireGuardBackend::Userspace
            };
            Profile {
                mode,
                wireguard,
                services: ServiceBackend::System,
                bridge: BridgeBackend::None,
            }
        }
        (Os::Linux, Mode::Dev | Mode::Agent) => Profile {
            mode,
            wireguard: WireGuardBackend::Userspace,
            services: ServiceBackend::System,
            bridge: BridgeBackend::None,
        },
        (Os::Darwin, Mode::Dev) => Profile {
            mode,
            wireguard: if aff.has_docker {
                WireGuardBackend::Docker
            } else {
                WireGuardBackend::Userspace
            },
            services: if aff.has_docker {
                ServiceBackend::Docker
            } else {
                ServiceBackend::Memory
            },
            bridge: BridgeBackend::UserspaceProxy,
        },
        (Os::Darwin, Mode::Agent | Mode::Prod) => Profile {
            mode,
            wireguard: WireGuardBackend::Userspace,
            services: ServiceBackend::System,
            bridge: if aff.has_wg_helper {
                BridgeBackend::HostRoutingHelper
            } else {
                BridgeBackend::UserspaceProxy
            },
        },
        (Os::Other, _) => Profile {
            mode,
            wireguard: WireGuardBackend::Memory,
            services: ServiceBackend::Memory,
            bridge: BridgeBackend::None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aff(os: Os, kernel_wg: bool, root: bool, docker: bool, wg_helper: bool) -> Affordances {
        Affordances {
            os,
            has_kernel_wireguard: kernel_wg,
            has_docker: docker,
            is_root: root,
            has_wg_helper: wg_helper,
        }
    }

    #[test]
    fn linux_prod_prefers_kernel_when_available() {
        let p = resolve_profile(&aff(Os::Linux, true, true, false, false), Mode::Prod);
        assert_eq!(p.wireguard, WireGuardBackend::Kernel);
        assert_eq!(p.services, ServiceBackend::System);
    }

    #[test]
    fn macos_dev_prefers_docker_backends() {
        let p = resolve_profile(&aff(Os::Darwin, false, false, true, false), Mode::Dev);
        assert_eq!(p.wireguard, WireGuardBackend::Docker);
        assert_eq!(p.services, ServiceBackend::Docker);
    }

    #[test]
    fn unknown_os_falls_back_to_memory() {
        let p = resolve_profile(&aff(Os::Other, false, false, false, false), Mode::Agent);
        assert_eq!(p.wireguard, WireGuardBackend::Memory);
        assert_eq!(p.services, ServiceBackend::Memory);
    }
}
