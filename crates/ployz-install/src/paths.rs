use std::path::{Path, PathBuf};

use ployz_config::{
    Os, RuntimeTarget, ServiceMode, default_config_path, default_data_dir, default_socket_path,
};

use crate::platform::HostPlatform;
use crate::{INSTALL_DIR_NAME, MANIFEST_FILE_NAME};

use super::sys::{home_dir_for_current_user, sudo_user_home_dir};

#[derive(Debug, Clone)]
pub(super) struct ClientPaths {
    pub(super) config_path: PathBuf,
    pub(super) data_dir: PathBuf,
    pub(super) socket_path: String,
}

#[derive(Debug, Clone)]
pub(super) struct ConfigTarget {
    pub(super) home_dir: PathBuf,
}

#[must_use]
pub(super) fn default_manifest_path(platform: HostPlatform) -> PathBuf {
    default_data_dir(&platform.paths_context())
        .join(INSTALL_DIR_NAME)
        .join(MANIFEST_FILE_NAME)
}

pub fn find_installer_script() -> Result<PathBuf, String> {
    let current_exe =
        std::env::current_exe().map_err(|error| format!("current_exe failed: {error}"))?;
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    let candidates = [
        current_exe.with_file_name("ployz.sh"),
        current_exe
            .parent()
            .map(|parent| parent.join("ployz.sh"))
            .unwrap_or_else(|| PathBuf::from("ployz.sh")),
        std::env::current_dir()
            .map(|dir| dir.join("ployz.sh"))
            .unwrap_or_else(|_| PathBuf::from("ployz.sh")),
        workspace_root
            .clone()
            .map(|root| root.join("ployz.sh"))
            .unwrap_or_else(|| PathBuf::from("ployz.sh")),
        PathBuf::from("/usr/local/bin/ployz.sh"),
        PathBuf::from("/usr/bin/ployz.sh"),
    ];
    for candidate in candidates {
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err("ployz.sh installer script not found".into())
}

pub(super) fn resolve_manifest_path(
    runtime_target: RuntimeTarget,
    service_mode: ServiceMode,
    explicit: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }

    let platform = HostPlatform::detect();
    if platform.is_root
        && runtime_target == RuntimeTarget::Host
        && service_mode == ServiceMode::System
        && let Some(home) = sudo_user_home_dir()?
    {
        return Ok(linux_user_manifest_path(&home));
    }
    Ok(default_manifest_path(platform))
}

pub(super) fn resolve_config_target(platform: HostPlatform) -> Result<ConfigTarget, String> {
    if platform.is_root
        && platform.os == Os::Linux
        && let Some(home_dir) = sudo_user_home_dir()?
    {
        return Ok(ConfigTarget { home_dir });
    }
    Ok(ConfigTarget {
        home_dir: home_dir_for_current_user()?,
    })
}

pub(super) fn client_paths(
    runtime_target: RuntimeTarget,
    service_mode: ServiceMode,
    home_dir: &Path,
) -> ClientPaths {
    let platform = HostPlatform::detect();
    if runtime_target == RuntimeTarget::Host && service_mode == ServiceMode::System {
        return ClientPaths {
            config_path: linux_user_config_path(home_dir),
            data_dir: PathBuf::from("/var/lib/ployz"),
            socket_path: "/run/ployz/ployzd.sock".into(),
        };
    }

    if platform.is_root {
        return ClientPaths {
            config_path: default_config_path(),
            data_dir: default_data_dir(&platform.paths_context()),
            socket_path: default_socket_path(&platform.paths_context()),
        };
    }

    match platform.os {
        Os::Linux => ClientPaths {
            config_path: linux_user_config_path(home_dir),
            data_dir: linux_user_data_dir(home_dir),
            socket_path: default_socket_path(&platform.paths_context()),
        },
        Os::Darwin => ClientPaths {
            config_path: home_dir.join("Library/Application Support/ployz/config.toml"),
            data_dir: home_dir.join("Library/Application Support/ployz"),
            socket_path: default_socket_path(&platform.paths_context()),
        },
        Os::Other => ClientPaths {
            config_path: default_config_path(),
            data_dir: default_data_dir(&platform.paths_context()),
            socket_path: default_socket_path(&platform.paths_context()),
        },
    }
}

fn linux_user_manifest_path(home_dir: &Path) -> PathBuf {
    linux_user_data_dir(home_dir)
        .join(INSTALL_DIR_NAME)
        .join(MANIFEST_FILE_NAME)
}

fn linux_user_data_dir(home_dir: &Path) -> PathBuf {
    home_dir.join(".local/share/ployz")
}

fn linux_user_config_path(home_dir: &Path) -> PathBuf {
    home_dir.join(".config/ployz/config.toml")
}
