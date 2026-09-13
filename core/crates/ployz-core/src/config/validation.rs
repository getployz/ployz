//! Strict service-setting admission and normalization for native and browser callers.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use super::*;
use crate::{ClusterDomainLabel, IngressHost, ServiceName};

/// Validation errors identify the setting, never echo credentials or authored values.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, thiserror::Error, TS)]
#[error("{path}: {message}")]
pub struct ConfigError {
    pub path: String,
    pub message: String,
}

impl ConfigError {
    pub(super) fn at(path: &str, message: &str) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

/// Field decoding for forms and full documents uses this same typed boundary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    tag = "field",
    content = "value",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum ServiceSettingInput {
    Name(String),
    Source(ServiceSource),
    RootDir(String),
    Command(String),
    PreDeployCommand(Option<String>),
    StartCommand(Option<String>),
    Healthcheck(ServiceHealthcheck),
    HealthcheckPath(String),
    HealthcheckTimeoutSeconds(u16),
    RestartPolicy(ServiceRestartPolicy),
    MaxRetries(u8),
    Cron(Option<String>),
    Replicas(u8),
    CpuLimit(Option<f64>),
    MemLimit(Option<f64>),
    PrivateDns(ServiceName),
    Routes(Vec<ServiceRoute>),
    ManagedHostname(Option<ServiceManagedHostname>),
    ManagedHostnameValue(ServiceManagedHostname),
    ManagedHostnamePrefix(String),
    Build(ServiceBuildConfig),
}

/// Decode and normalize one supported authored setting.
///
/// # Errors
/// Returns ConfigError for an unknown field, invalid shape, unsupported value, or out-of-range setting.
pub fn parse_service_setting(input: Value) -> Result<Value, ConfigError> {
    let mut setting: ServiceSettingInput = serde_json::from_value(input)
        .map_err(|_| ConfigError::at("setting", "Invalid setting value"))?;
    setting.normalize()?;
    Ok(serde_json::to_value(setting)
        .expect("setting is JSON")
        .get_mut("value")
        .expect("serialized setting value")
        .take())
}

/// Decode complete service settings and normalize each authored field using the shared field rules.
///
/// # Errors
/// Returns ConfigError for unexpected fields, an unsupported version, or any invalid setting.
pub fn parse_service_config(value: Value) -> Result<ServiceConfig, ConfigError> {
    let config: ServiceConfig = serde_json::from_value(value)
        .map_err(|_| ConfigError::at("service", "Invalid service configuration"))?;
    if config.settings.version != 2 {
        return Err(ConfigError::at("version", "Expected version 2"));
    }
    let mut value = serde_json::to_value(config).expect("config is JSON");
    for (field, value) in value.as_object_mut().expect("config is an object") {
        if matches!(
            field.as_str(),
            "version" | "env" | "mounts" | "variableGroupAttachments"
        ) {
            continue;
        }
        *value = parse_service_setting(serde_json::json!({ "field": field, "value": value }))?;
    }
    serde_json::from_value(value)
        .map_err(|_| ConfigError::at("service", "Invalid service configuration"))
}

impl ServiceSettingInput {
    fn normalize(&mut self) -> Result<(), ConfigError> {
        match self {
            Self::Name(value) => trimmed(value, "name", 64),
            Self::Source(value) => normalize_source(value),
            Self::RootDir(value) => root_dir(value),
            Self::Command(value) => trimmed(value, "command", 2000),
            Self::PreDeployCommand(value) | Self::StartCommand(value) => {
                optional_trimmed(value, "command", 2000)
            }
            Self::Healthcheck(ServiceHealthcheck::Http {
                path,
                timeout_seconds,
            }) => {
                healthcheck_path(path)?;
                timeout(*timeout_seconds)
            }
            Self::Healthcheck(ServiceHealthcheck::None)
            | Self::RestartPolicy(_)
            | Self::PrivateDns(_) => Ok(()),
            Self::HealthcheckPath(value) => healthcheck_path(value),
            Self::HealthcheckTimeoutSeconds(value) => timeout(*value),
            Self::MaxRetries(value) => range(*value <= 100, "maxRetries", "Expected 0–100 retries"),
            Self::Replicas(value) => range(*value <= 50, "replicas", "Expected 0–50 replicas"),
            Self::CpuLimit(value) => limit(*value, 64.0, "cpuLimit"),
            Self::MemLimit(value) => limit(*value, 1024.0, "memLimit"),
            Self::Cron(value) => optional_trimmed(value, "cron", usize::MAX),
            Self::Routes(routes) => {
                let mut ids = std::collections::BTreeSet::new();
                for route in routes {
                    range(
                        uuid::Uuid::parse_str(&route.id).is_ok() && ids.insert(route.id.clone()),
                        "routes",
                        "Route IDs must be valid and unique",
                    )?;
                    route.hostname = route.hostname.trim().to_lowercase();
                    IngressHost::parse(&route.hostname)
                        .map_err(|_| ConfigError::at("routes", "Invalid public hostname"))?;
                    range(
                        route.target_port != 0,
                        "routes",
                        "Expected a port from 1–65535",
                    )?;
                }
                Ok(())
            }
            Self::ManagedHostname(Some(value)) | Self::ManagedHostnameValue(value) => {
                hostname_prefix(&mut value.prefix)?;
                range(
                    value.target_port != Some(0),
                    "managedHostname",
                    "Expected a port from 1–65535",
                )
            }
            Self::ManagedHostname(None) => Ok(()),
            Self::ManagedHostnamePrefix(value) => hostname_prefix(value),
            Self::Build(value) => {
                optional_trimmed(
                    &mut value.dockerfile_path,
                    "build.dockerfilePath",
                    usize::MAX,
                )?;
                for path in &mut value.watch_paths {
                    trimmed(path, "build.watchPaths", usize::MAX)?;
                }
                Ok(())
            }
        }
    }
}

fn normalize_source(source: &mut ServiceSource) -> Result<(), ConfigError> {
    match source {
        ServiceSource::Empty {
            version,
            root_dir: path,
        } => {
            range(*version == 1, "source.version", "Expected version 1")?;
            root_dir(path)
        }
        ServiceSource::Git {
            version,
            repository,
            repository_id,
            installation_id,
            root_dir: path,
            branch,
            ..
        } => {
            range(*version == 2, "source.version", "Expected version 2")?;
            trimmed(repository, "source.repository", 300)?;
            range(
                (1..=9_007_199_254_740_991).contains(repository_id)
                    && (1..=9_007_199_254_740_991).contains(installation_id),
                "source.repository",
                "Expected positive safe GitHub IDs",
            )?;
            root_dir(path)?;
            match branch {
                ServiceGitBranch::Connected { name } => trimmed(name, "source.branch", 255),
                ServiceGitBranch::Disconnected { previous_name } => {
                    optional_trimmed(previous_name, "source.branch", 255)
                }
            }
        }
        ServiceSource::Image {
            version,
            image,
            auto_update,
            ..
        } => {
            range(*version == 1, "source.version", "Expected version 1")?;
            trimmed(image, "source.image", 500)?;
            if let ServiceImageAutoUpdate::TrackTag { tag } = auto_update {
                trimmed(tag, "source.autoUpdate", 255)?;
            }
            Ok(())
        }
    }
}

fn trimmed(value: &mut String, path: &str, max: usize) -> Result<(), ConfigError> {
    *value = value.trim().into();
    range(
        !value.is_empty() && value.chars().count() <= max,
        path,
        "Expected a non-empty value within the length limit",
    )
}

fn optional_trimmed(value: &mut Option<String>, path: &str, max: usize) -> Result<(), ConfigError> {
    if let Some(value) = value {
        trimmed(value, path, max)?;
    }
    Ok(())
}

fn root_dir(value: &mut String) -> Result<(), ConfigError> {
    trimmed(value, "source.rootDir", usize::MAX)?;
    range(
        value.starts_with('/')
            && !value.contains("//")
            && value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"/._-".contains(&c)),
        "source.rootDir",
        "Expected an absolute repository directory",
    )?;
    if value != "/" {
        *value = value.trim_end_matches('/').into();
    }
    Ok(())
}

fn healthcheck_path(value: &mut String) -> Result<(), ConfigError> {
    trimmed(value, "healthcheck.path", 500)?;
    range(
        value.starts_with('/'),
        "healthcheck.path",
        "Healthcheck path must start with /",
    )
}

fn timeout(value: u16) -> Result<(), ConfigError> {
    range(
        (1..=300).contains(&value),
        "healthcheck.timeoutSeconds",
        "Expected 1–300 seconds",
    )
}

fn hostname_prefix(value: &mut String) -> Result<(), ConfigError> {
    *value = value.trim().to_lowercase();
    ClusterDomainLabel::parse(&*value)
        .map(|_| ())
        .map_err(|_| ConfigError::at("managedHostname.prefix", "Expected a lowercase DNS label"))
}

fn limit(value: Option<f64>, max: f64, path: &str) -> Result<(), ConfigError> {
    range(
        value.is_none_or(|n| n.is_finite() && n > 0.0 && n <= max),
        path,
        "Expected a positive limit within the supported range",
    )
}

fn range(valid: bool, path: &str, message: &str) -> Result<(), ConfigError> {
    if valid {
        Ok(())
    } else {
        Err(ConfigError::at(path, message))
    }
}
