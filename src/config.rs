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

/// Returns the platform-appropriate default data directory.
///
/// - Linux (root):  `/var/lib/ployz`
/// - Linux (user):  `$XDG_DATA_HOME/ployz` or `~/.local/share/ployz`
/// - macOS:         `~/Library/Application Support/ployz`
/// - Other:         `~/.ployz`
pub fn default_data_dir(aff: &Affordances) -> std::path::PathBuf {
    match aff.os {
        Os::Linux if aff.is_root => "/var/lib/ployz".into(),
        Os::Linux => match std::env::var("XDG_DATA_HOME") {
            Ok(dir) => std::path::PathBuf::from(dir).join("ployz"),
            Err(_) => home_dir().join(".local/share/ployz"),
        },
        Os::Darwin => home_dir().join("Library/Application Support/ployz"),
        Os::Other => home_dir().join(".ployz"),
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
    match aff.os {
        Os::Linux if aff.is_root => "/run/ployz/ployzd.sock".into(),
        Os::Linux => match std::env::var("XDG_RUNTIME_DIR") {
            Ok(dir) => format!("{dir}/ployz/ployzd.sock"),
            Err(_) => "/tmp/ployz/ployzd.sock".into(),
        },
        Os::Darwin => {
            let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
            format!("{tmpdir}ployz/ployzd.sock")
        }
        Os::Other => "/tmp/ployz/ployzd.sock".into(),
    }
}

fn home_dir() -> std::path::PathBuf {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
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
