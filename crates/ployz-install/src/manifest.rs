use std::fs;
use std::path::{Path, PathBuf};

use ployz_config::{RuntimeTarget, ServiceMode};

use crate::{InstallManifest, ServiceBackend};

impl InstallManifest {
    pub fn load_from_path(path: &Path) -> Result<Self, String> {
        let raw = fs::read_to_string(path)
            .map_err(|error| format!("read install manifest '{}': {error}", path.display()))?;
        let mut source_kind = None;
        let mut source_version = None;
        let mut source_git_url = None;
        let mut source_git_ref = None;
        let mut bin_dir = None;
        let mut assets_dir = None;
        let mut config_path = None;
        let mut data_dir = None;
        let mut socket_path = None;
        let mut installer_path = None;
        let mut ployzctl_path = None;
        let mut ployzd_path = None;
        let mut gateway_path = None;
        let mut dns_path = None;
        let mut corrosion_path = None;
        let mut runtime_target = None;
        let mut service_mode = None;
        let mut service_backend = None;

        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let Some((key, raw_value)) = trimmed.split_once('=') else {
                return Err(format!(
                    "invalid install manifest line in '{}': {trimmed}",
                    path.display()
                ));
            };
            let value = parse_shell_value(raw_value)?;
            match key {
                "SOURCE_KIND" => source_kind = Some(value),
                "SOURCE_VERSION" => source_version = non_empty(value),
                "SOURCE_GIT_URL" => source_git_url = non_empty(value),
                "SOURCE_GIT_REF" => source_git_ref = non_empty(value),
                "BIN_DIR" => bin_dir = Some(PathBuf::from(value)),
                "ASSETS_DIR" => assets_dir = Some(PathBuf::from(value)),
                "CONFIG_PATH" => config_path = Some(PathBuf::from(value)),
                "DATA_DIR" => data_dir = Some(PathBuf::from(value)),
                "SOCKET_PATH" => socket_path = Some(value),
                "INSTALLER_PATH" => installer_path = Some(PathBuf::from(value)),
                "PLOYZCTL_PATH" => ployzctl_path = Some(PathBuf::from(value)),
                "PLOYZD_PATH" => ployzd_path = Some(PathBuf::from(value)),
                "PLOYZ_GATEWAY_PATH" => gateway_path = Some(PathBuf::from(value)),
                "PLOYZ_DNS_PATH" => dns_path = Some(PathBuf::from(value)),
                "CORROSION_PATH" => corrosion_path = Some(PathBuf::from(value)),
                "RUNTIME_TARGET" => runtime_target = Some(parse_runtime_target(&value)?),
                "SERVICE_MODE" => service_mode = Some(parse_service_mode(&value)?),
                "SERVICE_BACKEND" => {
                    service_backend = non_empty(value)
                        .map(|backend| ServiceBackend::parse(&backend))
                        .transpose()?
                }
                _ => {}
            }
        }

        Ok(Self {
            source_kind: required_value(source_kind, "SOURCE_KIND", path)?,
            source_version,
            source_git_url,
            source_git_ref,
            bin_dir: required_value(bin_dir, "BIN_DIR", path)?,
            assets_dir: required_value(assets_dir, "ASSETS_DIR", path)?,
            config_path: required_value(config_path, "CONFIG_PATH", path)?,
            data_dir: required_value(data_dir, "DATA_DIR", path)?,
            socket_path: required_value(socket_path, "SOCKET_PATH", path)?,
            installer_path: required_value(installer_path, "INSTALLER_PATH", path)?,
            ployzctl_path: required_value(ployzctl_path, "PLOYZCTL_PATH", path)?,
            ployzd_path: required_value(ployzd_path, "PLOYZD_PATH", path)?,
            gateway_path: required_value(gateway_path, "PLOYZ_GATEWAY_PATH", path)?,
            dns_path: required_value(dns_path, "PLOYZ_DNS_PATH", path)?,
            corrosion_path: required_value(corrosion_path, "CORROSION_PATH", path)?,
            runtime_target: required_value(runtime_target, "RUNTIME_TARGET", path)?,
            service_mode: required_value(service_mode, "SERVICE_MODE", path)?,
            service_backend,
        })
    }

    pub fn store_to_path(&self, path: &Path) -> Result<(), String> {
        let Some(parent) = path.parent() else {
            return Err(format!(
                "invalid install manifest path '{}'",
                path.display()
            ));
        };
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "create install manifest dir '{}': {error}",
                parent.display()
            )
        })?;
        let content = [
            env_line("SOURCE_KIND", &self.source_kind),
            env_line_opt("SOURCE_VERSION", self.source_version.as_deref()),
            env_line_opt("SOURCE_GIT_URL", self.source_git_url.as_deref()),
            env_line_opt("SOURCE_GIT_REF", self.source_git_ref.as_deref()),
            env_line("BIN_DIR", &self.bin_dir.display().to_string()),
            env_line("ASSETS_DIR", &self.assets_dir.display().to_string()),
            env_line("CONFIG_PATH", &self.config_path.display().to_string()),
            env_line("DATA_DIR", &self.data_dir.display().to_string()),
            env_line("SOCKET_PATH", &self.socket_path),
            env_line("INSTALLER_PATH", &self.installer_path.display().to_string()),
            env_line("PLOYZCTL_PATH", &self.ployzctl_path.display().to_string()),
            env_line("PLOYZD_PATH", &self.ployzd_path.display().to_string()),
            env_line(
                "PLOYZ_GATEWAY_PATH",
                &self.gateway_path.display().to_string(),
            ),
            env_line("PLOYZ_DNS_PATH", &self.dns_path.display().to_string()),
            env_line("CORROSION_PATH", &self.corrosion_path.display().to_string()),
            env_line("RUNTIME_TARGET", runtime_target_name(self.runtime_target)),
            env_line("SERVICE_MODE", service_mode_name(self.service_mode)),
            env_line_opt(
                "SERVICE_BACKEND",
                self.service_backend.map(ServiceBackend::as_str),
            ),
        ]
        .join("\n");
        fs::write(path, format!("{content}\n"))
            .map_err(|error| format!("write install manifest '{}': {error}", path.display()))
    }
}

pub(super) fn validate_install_manifest(manifest: &InstallManifest) -> Result<(), String> {
    let required = [
        &manifest.installer_path,
        &manifest.ployzctl_path,
        &manifest.ployzd_path,
        &manifest.gateway_path,
        &manifest.dns_path,
        &manifest.corrosion_path,
    ];
    for path in required {
        if !path.exists() {
            return Err(format!(
                "install manifest references missing file '{}'",
                path.display()
            ));
        }
    }
    Ok(())
}

pub(super) fn runtime_target_name(runtime_target: RuntimeTarget) -> &'static str {
    match runtime_target {
        RuntimeTarget::Docker => "docker",
        RuntimeTarget::Host => "host",
    }
}

fn parse_runtime_target(value: &str) -> Result<RuntimeTarget, String> {
    match value {
        "docker" => Ok(RuntimeTarget::Docker),
        "host" => Ok(RuntimeTarget::Host),
        other => Err(format!("unsupported runtime target '{other}'")),
    }
}

pub(super) fn service_mode_name(service_mode: ServiceMode) -> &'static str {
    match service_mode {
        ServiceMode::User => "user",
        ServiceMode::System => "system",
    }
}

fn parse_service_mode(value: &str) -> Result<ServiceMode, String> {
    match value {
        "user" => Ok(ServiceMode::User),
        "system" => Ok(ServiceMode::System),
        other => Err(format!("unsupported service mode '{other}'")),
    }
}

fn env_line(key: &str, value: &str) -> String {
    format!("{key}={}", single_quote(value))
}

fn env_line_opt(key: &str, value: Option<&str>) -> String {
    env_line(key, value.unwrap_or(""))
}

fn single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn parse_shell_value(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('\'') && trimmed.ends_with('\'') {
        let inner = &trimmed[1..trimmed.len() - 1];
        return Ok(inner.replace("'\"'\"'", "'"));
    }
    if trimmed.contains(' ') {
        return Err(format!("unquoted install manifest value '{trimmed}'"));
    }
    Ok(trimmed.to_string())
}

fn required_value<T>(value: Option<T>, key: &str, path: &Path) -> Result<T, String> {
    value.ok_or_else(|| format!("missing {key} in install manifest '{}'", path.display()))
}

fn non_empty(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}
