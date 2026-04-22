mod manifest;
mod paths;
mod render;
mod service;
mod sys;

use std::path::{Path, PathBuf};

use crate::platform::{HostPlatform, validate_runtime};
use paths::{ClientPaths, ConfigTarget, client_paths, resolve_config_target, resolve_manifest_path};
use ployz_config::{RuntimeTarget, ServiceMode};
use render::write_client_config;
use service::{ensure_user_service, install_system_service, promote_system_binaries, user_backend};

use self::manifest::validate_install_manifest;

const SERVICE_LABEL: &str = "dev.ployz.ployzd";
const INSTALL_DIR_NAME: &str = "install";
const MANIFEST_FILE_NAME: &str = "manifest.env";

pub use self::paths::find_installer_script;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceBackend {
    SystemdUser,
    SystemdSystem,
    LaunchAgent,
}

impl ServiceBackend {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SystemdUser => "systemd-user",
            Self::SystemdSystem => "systemd-system",
            Self::LaunchAgent => "launch-agent",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "systemd-user" => Ok(Self::SystemdUser),
            "systemd-system" => Ok(Self::SystemdSystem),
            "launch-agent" => Ok(Self::LaunchAgent),
            other => Err(format!("unsupported service backend '{other}'")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct InstallManifest {
    pub source_kind: String,
    pub source_version: Option<String>,
    pub source_git_url: Option<String>,
    pub source_git_ref: Option<String>,
    pub bin_dir: PathBuf,
    pub assets_dir: PathBuf,
    pub config_path: PathBuf,
    pub data_dir: PathBuf,
    pub socket_path: String,
    pub installer_path: PathBuf,
    pub ployz_path: PathBuf,
    pub ployzd_path: PathBuf,
    pub gateway_path: PathBuf,
    pub dns_path: PathBuf,
    pub corrosion_path: PathBuf,
    pub runtime_target: RuntimeTarget,
    pub service_mode: ServiceMode,
    pub service_backend: Option<ServiceBackend>,
}

pub fn daemon_install(
    runtime_target: RuntimeTarget,
    service_mode: ServiceMode,
    manifest_path: Option<&Path>,
) -> Result<InstallManifest, String> {
    let platform = HostPlatform::detect();
    validate_runtime(runtime_target, service_mode, platform)?;
    let manifest_path = resolve_manifest_path(runtime_target, service_mode, manifest_path)?;
    let mut manifest = InstallManifest::load_from_path(&manifest_path)?;
    let ConfigTarget { home_dir } = resolve_config_target(platform)?;
    let ClientPaths {
        config_path,
        data_dir,
        socket_path,
    } = client_paths(runtime_target, service_mode, &home_dir);

    validate_install_manifest(&manifest)?;
    write_client_config(&config_path, &data_dir, &socket_path)?;

    match service_mode {
        ServiceMode::User => {
            ensure_user_service(
                platform,
                &manifest.ployzd_path,
                &data_dir,
                &socket_path,
                runtime_target,
                service_mode,
            )?;
            manifest.service_backend = Some(user_backend(platform)?);
        }
        ServiceMode::System => {
            promote_system_binaries(&manifest)?;
            install_system_service(&manifest.assets_dir, runtime_target, service_mode)?;
            manifest.service_backend = Some(ServiceBackend::SystemdSystem);
        }
    }

    manifest.config_path = config_path;
    manifest.data_dir = data_dir;
    manifest.socket_path = socket_path;
    manifest.runtime_target = runtime_target;
    manifest.service_mode = service_mode;
    manifest.store_to_path(&manifest_path)?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_round_trip_preserves_fields() {
        let manifest = InstallManifest {
            source_kind: "payload".into(),
            source_version: Some("v1.2.3".into()),
            source_git_url: None,
            source_git_ref: None,
            bin_dir: PathBuf::from("/tmp/bin dir"),
            assets_dir: PathBuf::from("/tmp/assets"),
            config_path: PathBuf::from("/tmp/config.toml"),
            data_dir: PathBuf::from("/tmp/data"),
            socket_path: "/tmp/socket.sock".into(),
            installer_path: PathBuf::from("/tmp/bin/ployz.sh"),
            ployz_path: PathBuf::from("/tmp/bin/ployz"),
            ployzd_path: PathBuf::from("/tmp/bin/ployzd"),
            gateway_path: PathBuf::from("/tmp/bin/ployz-gateway"),
            dns_path: PathBuf::from("/tmp/bin/ployz-dns"),
            corrosion_path: PathBuf::from("/tmp/bin/corrosion"),
            runtime_target: RuntimeTarget::Host,
            service_mode: ServiceMode::System,
            service_backend: Some(ServiceBackend::SystemdSystem),
        };

        let path =
            std::env::temp_dir().join(format!("ployz-install-manifest-{}.env", std::process::id()));
        manifest.store_to_path(&path).expect("store manifest");
        let loaded = InstallManifest::load_from_path(&path).expect("load manifest");
        assert_eq!(loaded.source_kind, "payload");
        assert_eq!(loaded.source_version.as_deref(), Some("v1.2.3"));
        assert_eq!(loaded.bin_dir, PathBuf::from("/tmp/bin dir"));
        assert_eq!(loaded.runtime_target, RuntimeTarget::Host);
        assert_eq!(loaded.service_mode, ServiceMode::System);
        assert_eq!(loaded.service_backend, Some(ServiceBackend::SystemdSystem));
        std::fs::remove_file(path).expect("remove temp manifest");
    }
}
