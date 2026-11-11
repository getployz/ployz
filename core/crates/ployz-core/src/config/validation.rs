//! Strict service-setting admission and normalization for SDK callers in Node and the browser.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use super::*;
use crate::{IngressHost, ServiceName, value::is_dns_label};

/// The most vCPUs `cpuLimit` allows.
pub const CPU_LIMIT_MAX: f64 = 64.0;
/// The most GB `memLimit` allows.
pub const MEM_LIMIT_MAX: f64 = 1024.0;
/// The most replicas a Service runs.
pub const REPLICAS_MAX: u8 = 50;
/// The most restarts `maxRetries` allows.
pub const MAX_RETRIES_MAX: u8 = 100;
/// The longest command: start, pre-deploy, build and Setup Commands.
pub const COMMAND_MAX: usize = 2000;
/// The longest container image reference.
pub const IMAGE_MAX: usize = 500;
/// The longest Dockerfile path.
pub const DOCKERFILE_PATH_MAX: usize = 500;
/// The longest GitHub repository name, as `owner/name`.
pub const REPOSITORY_MAX: usize = 300;
/// The longest Git branch name.
pub const BRANCH_MAX: usize = 255;
/// The longest healthcheck path.
pub const HEALTHCHECK_PATH_MAX: usize = 500;
/// The longest a healthcheck may take, in seconds.
pub const HEALTHCHECK_TIMEOUT_MAX: u16 = 300;
/// How long a healthcheck takes when its timeout isn't given, in seconds.
pub const HEALTHCHECK_TIMEOUT_DEFAULT: u16 = 300;
/// Every restart policy.
pub const RESTART_POLICIES: [&str; 4] = ["always", "no", "on-failure", "unless-stopped"];
/// Every update order.
pub const UPDATE_ORDERS: [&str; 3] = ["start-first", "stop-first", "rolling"];

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
    UpdateOrder(Option<ServiceUpdateOrder>),
    Replicas(u8),
    CpuLimit(Option<f64>),
    MemLimit(Option<f64>),
    PrivateDns(ServiceName),
    Routes(Vec<ServiceRoute>),
    ManagedHostnames(Vec<ServiceManagedHostname>),
    ManagedHostnameValue(ServiceManagedHostname),
    ManagedHostnamePrefix(String),
    /// An image a Service may run: the one rule the Store and the dashboard both check.
    ImageReference(String),
    Build(ServiceBuildConfig),
    Template(Option<ServiceTemplate>),
}

/// Decode and normalize one supported authored setting.
///
/// # Errors
/// Returns ConfigError for an unknown field, invalid shape, unsupported value, or out-of-range setting.
pub fn parse_service_setting(input: Value) -> Result<Value, ConfigError> {
    let field = input
        .get("field")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let mut setting: ServiceSettingInput =
        serde_json::from_value(input).map_err(|_| ConfigError::at("setting", expected(&field)))?;
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
        if matches!(field.as_str(), "version" | "env" | "mounts" | "configs") {
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
            Self::Command(value) => trimmed(value, "command", COMMAND_MAX),
            Self::PreDeployCommand(value) | Self::StartCommand(value) => {
                optional_trimmed(value, "command", COMMAND_MAX)
            }
            Self::Healthcheck(ServiceHealthcheck::Http {
                path,
                timeout_seconds,
            }) => {
                healthcheck_path(path)?;
                timeout(*timeout_seconds)
            }
            Self::Healthcheck(ServiceHealthcheck::Command {
                command,
                timeout_seconds,
            }) => {
                trimmed(command, "healthcheck.command", COMMAND_MAX)?;
                timeout(*timeout_seconds)
            }
            Self::Healthcheck(ServiceHealthcheck::None)
            | Self::RestartPolicy(_)
            | Self::UpdateOrder(_)
            | Self::PrivateDns(_) => Ok(()),
            Self::HealthcheckPath(value) => healthcheck_path(value),
            Self::HealthcheckTimeoutSeconds(value) => timeout(*value),
            Self::MaxRetries(value) => range(
                *value <= MAX_RETRIES_MAX,
                "maxRetries",
                expected("maxRetries"),
            ),
            Self::Replicas(value) => range(
                (1..=REPLICAS_MAX).contains(value),
                "replicas",
                expected("replicas"),
            ),
            Self::CpuLimit(value) => limit(*value, CPU_LIMIT_MAX, "cpuLimit"),
            Self::MemLimit(value) => limit(*value, MEM_LIMIT_MAX, "memLimit"),
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
                        route.target_port != Some(0),
                        "routes",
                        "Expected a port from 1–65535",
                    )?;
                }
                Ok(())
            }
            Self::ManagedHostnames(values) => {
                let mut seen = std::collections::BTreeSet::new();
                for value in values.iter_mut() {
                    managed_hostname(value)?;
                    if !seen.insert(value.prefix.clone()) {
                        return Err(ConfigError::at(
                            "managedHostnames",
                            "Each managed domain needs a distinct subdomain",
                        ));
                    }
                }
                Ok(())
            }
            Self::ManagedHostnameValue(value) => managed_hostname(value),
            Self::ManagedHostnamePrefix(value) => hostname_prefix(value),
            Self::ImageReference(value) => {
                trimmed(value, "image", IMAGE_MAX)?;
                range(
                    crate::is_image_reference(value),
                    "image",
                    "expected an image like nginx:1.27 or ghcr.io/acme/web:1.4",
                )
            }
            Self::Template(_) => Ok(()),
            Self::Build(value) => {
                optional_trimmed(&mut value.command, "build.command", COMMAND_MAX)?;
                optional_trimmed(
                    &mut value.dockerfile_path,
                    "build.dockerfilePath",
                    DOCKERFILE_PATH_MAX,
                )?;
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
            access,
            root_dir: path,
            branch,
            ..
        } => {
            range(*version == 2, "source.version", "Expected version 2")?;
            trimmed(repository, "source.repository", REPOSITORY_MAX)?;
            range(
                (1..=9_007_199_254_740_991).contains(repository_id),
                "source.repository",
                "Expected positive safe GitHub IDs",
            )?;
            if let super::service::ServiceGitAccess::GithubInstallation { installation_id } = access
            {
                range(
                    (1..=9_007_199_254_740_991).contains(installation_id),
                    "source.access",
                    "Expected a positive safe GitHub installation ID",
                )?;
            }
            root_dir(path)?;
            match branch {
                ServiceGitBranch::Connected { name } => trimmed(name, "source.branch", BRANCH_MAX),
                ServiceGitBranch::Disconnected { previous_name } => {
                    optional_trimmed(previous_name, "source.branch", BRANCH_MAX)
                }
            }
        }
        ServiceSource::Image {
            version,
            image,
            credentials,
        } => {
            range(*version == 1, "source.version", "Expected version 1")?;
            trimmed(image, "source.image", IMAGE_MAX)?;
            if let ServiceImageCredentials::Configured { credential_id } = credentials {
                range(
                    uuid::Uuid::parse_str(credential_id).is_ok(),
                    "source.credentials",
                    "Expected a credential ID",
                )?;
            }
            Ok(())
        }
    }
}

pub(super) fn trimmed(value: &mut String, path: &str, max: usize) -> Result<(), ConfigError> {
    range(
        !value.contains('\0'),
        path,
        "Null characters are not allowed",
    )?;
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
    trimmed(value, "healthcheck.path", HEALTHCHECK_PATH_MAX)?;
    range(
        value.starts_with('/') && !value.chars().any(char::is_control),
        "healthcheck.path",
        "Expected an absolute HTTP path without control characters",
    )
}

fn timeout(value: u16) -> Result<(), ConfigError> {
    range(
        (1..=HEALTHCHECK_TIMEOUT_MAX).contains(&value),
        "healthcheck.timeoutSeconds",
        "Expected 1–300 seconds",
    )
}

fn managed_hostname(value: &mut ServiceManagedHostname) -> Result<(), ConfigError> {
    hostname_prefix(&mut value.prefix)?;
    range(
        value.target_port != Some(0),
        "managedHostnames",
        "Expected a port from 1–65535",
    )
}

fn hostname_prefix(value: &mut String) -> Result<(), ConfigError> {
    *value = value.trim().to_lowercase();
    range(
        is_dns_label(value),
        "managedHostname.prefix",
        "Expected a lowercase DNS label",
    )
}

fn limit(value: Option<f64>, max: f64, path: &str) -> Result<(), ConfigError> {
    range(
        value.is_none_or(|n| n.is_finite() && n > 0.0 && n <= max),
        path,
        expected(path),
    )
}

/// What a numeric setting takes: the same words whether the value is out of range or not a number at all.
fn expected(field: &str) -> &'static str {
    match field {
        "replicas" => "Expected a whole number of replicas from 1–50",
        "maxRetries" => "Expected a whole number of retries from 0–100",
        "cpuLimit" => "Expected vCPUs above 0 and up to 64, for example 0.5",
        "memLimit" => "Expected GB above 0 and up to 1024, for example 0.5 or 2",
        "healthcheckTimeoutSeconds" => "Expected 1–300 seconds",
        "updateOrder" => "Expected start-first, stop-first or rolling",
        _ => "Invalid setting value",
    }
}

fn range(valid: bool, path: &str, message: &str) -> Result<(), ConfigError> {
    if valid {
        Ok(())
    } else {
        Err(ConfigError::at(path, message))
    }
}

#[cfg(test)]
#[expect(clippy::indexing_slicing, reason = "JSON fixtures")]
mod tests {
    use super::*;

    #[test]
    fn a_value_that_is_not_a_number_says_what_the_setting_takes() {
        let message = |field: &str, value: Value| {
            parse_service_setting(serde_json::json!({ "field": field, "value": value }))
                .unwrap_err()
                .message
        };
        for value in [
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!(51),
        ] {
            assert_eq!(message("replicas", value), expected("replicas"));
        }
        assert_eq!(
            message("memLimit", serde_json::json!("1x")),
            expected("memLimit")
        );
        assert_eq!(
            message("memLimit", serde_json::json!(0)),
            expected("memLimit")
        );
    }

    const ROUTE_ID: &str = "00000000-0000-4000-8000-000000000001";

    fn complete_config() -> Value {
        serde_json::json!({
            "version": 2,
            "privateDns": "api",
            "source": {"version": 2, "type": "git", "repository": " acme/api ",
                "repositoryId": 42,
                "access": {"type": "github-installation", "installationId": 7},
                "rootDir": " /apps/api/ ",
                "branch": {"type": "connected", "name": " main "}},
            "preDeployCommand": "  npm run migrate ",
            "startCommand": " npm start\n",
            "healthcheck": {"type": "http", "path": " /health ", "timeoutSeconds": 5},
            "restartPolicy": "on-failure",
            "maxRetries": 3,
            "updateOrder": "rolling",
            "replicas": 2,
            "cpuLimit": 0.5,
            "memLimit": 2,
            "routes": [{"id": ROUTE_ID, "hostname": " WWW.Example.COM ", "targetPort": 8080}],
            "managedHostnames": [{"prefix": " Web ", "targetPort": null}],
            "build": {"buildMethod": "auto", "dockerfilePath": " Dockerfile ", "command": " make "},
            "template": {"id": "postgres", "version": 3},
            "env": {"KEY": {"kind": "literal", "value": "  untouched  "}},
            "mounts": [{"volumeResourceId": "v1", "volumeName": " data ", "mountPath": "/data"}],
        })
    }

    fn parsed(input: Value) -> Result<Value, (String, String)> {
        parse_service_config(input)
            .map(|config| serde_json::to_value(config).expect("config is JSON"))
            .map_err(|error| (error.path, error.message))
    }

    fn rejected(input: Value) -> (String, String) {
        parsed(input).unwrap_err()
    }

    #[test]
    fn a_complete_config_normalizes_every_authored_field() {
        assert_eq!(
            parsed(complete_config()).unwrap(),
            serde_json::json!({
                "version": 2,
                "source": {"version": 2, "type": "git", "repository": "acme/api",
                    "repositoryId": 42,
                    "access": {"type": "github-installation", "installationId": 7},
                    "rootDir": "/apps/api",
                    "branch": {"type": "connected", "name": "main"}},
                "preDeployCommand": "npm run migrate",
                "startCommand": "npm start",
                "healthcheck": {"type": "http", "path": "/health", "timeoutSeconds": 5},
                "restartPolicy": "on-failure",
                "maxRetries": 3,
                "updateOrder": "rolling",
                "replicas": 2,
                "cpuLimit": 0.5,
                "memLimit": 2.0,
                "privateDns": "api",
                "routes": [{"id": ROUTE_ID, "hostname": "www.example.com", "targetPort": 8080}],
                "managedHostnames": [{"prefix": "web", "targetPort": null}],
                "build": {"buildMethod": "railpack", "dockerfilePath": "Dockerfile", "command": "make"},
                "template": {"id": "postgres", "version": 3},
                "env": {"KEY": {"kind": "literal", "value": "  untouched  "}},
                "mounts": [{"volumeResourceId": "v1", "volumeName": " data ", "mountPath": "/data"}],
            })
        );
    }

    #[test]
    fn a_minimal_config_keeps_its_defaults_and_omits_unset_fields() {
        let minimal = serde_json::json!({
            "version": 2, "privateDns": "api",
            "source": {"version": 1, "type": "empty", "rootDir": "/"},
            "preDeployCommand": null, "startCommand": null,
            "healthcheck": {"type": "none"}, "restartPolicy": "unless-stopped",
        });
        assert_eq!(
            parsed(minimal).unwrap(),
            serde_json::json!({
                "version": 2,
                "source": {"version": 1, "type": "empty", "rootDir": "/"},
                "preDeployCommand": null, "startCommand": null,
                "healthcheck": {"type": "none"}, "restartPolicy": "unless-stopped",
                "maxRetries": 10, "replicas": 1, "cpuLimit": null, "memLimit": null,
                "privateDns": "api", "routes": [], "managedHostnames": [],
                "build": {"buildMethod": "railpack", "dockerfilePath": null, "command": null},
                "env": {}, "mounts": [],
            })
        );
    }

    #[test]
    fn each_source_and_healthcheck_variant_normalizes() {
        let with = |field: &str, value: Value| {
            let mut input = complete_config();
            input[field] = value;
            parsed(input).map(|mut config| config[field].take())
        };
        for (source, normalized) in [
            (
                serde_json::json!({"version": 1, "type": "empty", "rootDir": " /apps// "}),
                Err((
                    "source.rootDir",
                    "Expected an absolute repository directory",
                )),
            ),
            (
                serde_json::json!({"version": 1, "type": "empty", "rootDir": " /apps/ "}),
                Ok(serde_json::json!({"version": 1, "type": "empty", "rootDir": "/apps"})),
            ),
            (
                serde_json::json!({"version": 1, "type": "image", "image": " nginx:1.27 ",
                    "credentials": {"type": "none"}}),
                Ok(
                    serde_json::json!({"version": 1, "type": "image", "image": "nginx:1.27",
                    "credentials": {"type": "none"}}),
                ),
            ),
            (
                serde_json::json!({"version": 1, "type": "image", "image": "nginx",
                    "credentials": {"type": "configured", "credentialId": "nope"}}),
                Err(("source.credentials", "Expected a credential ID")),
            ),
            (
                serde_json::json!({"version": 2, "type": "git", "repository": "acme/api",
                    "repositoryId": 0, "access": {"type": "public"}, "rootDir": "/",
                    "branch": {"type": "disconnected", "previousName": null}}),
                Err(("source.repository", "Expected positive safe GitHub IDs")),
            ),
            (
                serde_json::json!({"version": 2, "type": "git", "repository": "acme/api",
                    "repositoryId": 1, "access": {"type": "public"}, "rootDir": "/",
                    "branch": {"type": "disconnected", "previousName": " dev "}}),
                Ok(
                    serde_json::json!({"version": 2, "type": "git", "repository": "acme/api",
                    "repositoryId": 1, "access": {"type": "public"}, "rootDir": "/",
                    "branch": {"type": "disconnected", "previousName": "dev"}}),
                ),
            ),
            (
                serde_json::json!({"version": 3, "type": "empty", "rootDir": "/"}),
                Err(("source.version", "Expected version 1")),
            ),
        ] {
            assert_eq!(
                with("source", source),
                normalized.map_err(|(path, message)| (path.to_owned(), message.to_owned()))
            );
        }
        for (healthcheck, normalized) in [
            (
                serde_json::json!({"type": "command", "command": " pg_isready \n", "timeoutSeconds": 300}),
                Ok(
                    serde_json::json!({"type": "command", "command": "pg_isready", "timeoutSeconds": 300}),
                ),
            ),
            (
                serde_json::json!({"type": "command", "command": "x", "timeoutSeconds": 301}),
                Err(("healthcheck.timeoutSeconds", "Expected 1–300 seconds")),
            ),
            (
                serde_json::json!({"type": "http", "path": "health", "timeoutSeconds": 5}),
                Err((
                    "healthcheck.path",
                    "Expected an absolute HTTP path without control characters",
                )),
            ),
            (
                serde_json::json!({"type": "none"}),
                Ok(serde_json::json!({"type": "none"})),
            ),
        ] {
            assert_eq!(
                with("healthcheck", healthcheck),
                normalized.map_err(|(path, message)| (path.to_owned(), message.to_owned()))
            );
        }
    }

    /// One invalid value per field that normalization can reject, in the order the first error is reported.
    fn invalid_fields() -> [(&'static str, Value, (&'static str, &'static str)); 11] {
        [
            (
                "build",
                serde_json::json!({"buildMethod": "dockerfile", "dockerfilePath": "Dockerfile", "command": "a\0b"}),
                ("build.command", "Null characters are not allowed"),
            ),
            (
                "cpuLimit",
                serde_json::json!(0),
                ("cpuLimit", expected("cpuLimit")),
            ),
            (
                "healthcheck",
                serde_json::json!({"type": "http", "path": "nope", "timeoutSeconds": 5}),
                (
                    "healthcheck.path",
                    "Expected an absolute HTTP path without control characters",
                ),
            ),
            (
                "managedHostnames",
                serde_json::json!([{"prefix": "Bad_Label", "targetPort": null}]),
                ("managedHostname.prefix", "Expected a lowercase DNS label"),
            ),
            (
                "maxRetries",
                serde_json::json!(101),
                ("maxRetries", expected("maxRetries")),
            ),
            (
                "memLimit",
                serde_json::json!(2000),
                ("memLimit", expected("memLimit")),
            ),
            (
                "preDeployCommand",
                serde_json::json!(" "),
                (
                    "command",
                    "Expected a non-empty value within the length limit",
                ),
            ),
            (
                "replicas",
                serde_json::json!(0),
                ("replicas", expected("replicas")),
            ),
            (
                "routes",
                serde_json::json!([{"id": "x", "hostname": "a.example.com", "targetPort": null}]),
                ("routes", "Route IDs must be valid and unique"),
            ),
            (
                "source",
                serde_json::json!({"version": 1, "type": "empty", "rootDir": "relative"}),
                (
                    "source.rootDir",
                    "Expected an absolute repository directory",
                ),
            ),
            (
                "startCommand",
                serde_json::json!("a\0b"),
                ("command", "Null characters are not allowed"),
            ),
        ]
    }

    #[test]
    fn each_invalid_field_reports_its_path_and_message() {
        let owned = |(path, message): (&str, &str)| (path.to_owned(), message.to_owned());
        for (field, value, error) in invalid_fields() {
            let mut input = complete_config();
            input[field] = value;
            assert_eq!(rejected(input), owned(error), "{field}");
        }
    }

    #[test]
    fn the_first_error_is_the_alphabetically_first_invalid_field() {
        let owned = |(path, message): (&str, &str)| (path.to_owned(), message.to_owned());
        let invalid = invalid_fields();
        let mut input = complete_config();
        for (field, value, _) in &invalid {
            input[field] = value.clone();
        }
        let valid = complete_config();
        for (field, _, error) in &invalid {
            assert_eq!(rejected(input.clone()), owned(*error), "before {field}");
            input[field] = valid[field].clone();
        }
        parsed(input).unwrap();
    }

    #[test]
    fn a_wrong_version_is_reported_before_any_field_is_normalized() {
        let mut input = complete_config();
        input["version"] = serde_json::json!(1);
        input["replicas"] = serde_json::json!(0);
        assert_eq!(
            rejected(input),
            ("version".to_owned(), "Expected version 2".to_owned())
        );
    }

    #[test]
    fn a_document_that_does_not_decode_is_an_invalid_service_configuration() {
        let invalid = (
            "service".to_owned(),
            "Invalid service configuration".to_owned(),
        );
        for (field, value) in [
            ("unexpected", serde_json::json!(true)),
            ("privateDns", serde_json::json!("Invalid_DNS")),
            ("restartPolicy", serde_json::json!("sometimes")),
            ("updateOrder", serde_json::json!("sideways")),
            ("replicas", serde_json::json!(1.5)),
            ("maxRetries", serde_json::json!(-1)),
            (
                "template",
                serde_json::json!({"id": "postgres", "version": 0}),
            ),
        ] {
            let mut input = complete_config();
            input[field] = value;
            assert_eq!(rejected(input), invalid, "{field}");
        }
        assert_eq!(rejected(Value::Null), invalid);
    }

    #[test]
    fn route_and_hostname_rules_hold_across_entries() {
        let route = |id: &str, hostname: &str, port: Value| serde_json::json!({"id": id, "hostname": hostname, "targetPort": port});
        let other = "00000000-0000-4000-8000-000000000002";
        for (routes, error) in [
            (
                serde_json::json!([
                    route(ROUTE_ID, "a.example.com", Value::Null),
                    route(ROUTE_ID, "b.example.com", Value::Null)
                ]),
                ("routes", "Route IDs must be valid and unique"),
            ),
            (
                serde_json::json!([route(ROUTE_ID, "not a host", Value::Null)]),
                ("routes", "Invalid public hostname"),
            ),
            (
                serde_json::json!([route(ROUTE_ID, "a.example.com", serde_json::json!(0))]),
                ("routes", "Expected a port from 1–65535"),
            ),
        ] {
            let mut input = complete_config();
            input["routes"] = routes;
            assert_eq!(rejected(input), (error.0.to_owned(), error.1.to_owned()));
        }
        let mut input = complete_config();
        input["routes"] = serde_json::json!([
            route(ROUTE_ID, " A.Example.com ", Value::Null),
            route(other, "B.example.com", serde_json::json!(80)),
        ]);
        assert_eq!(
            parsed(input).unwrap()["routes"],
            serde_json::json!([
                route(ROUTE_ID, "a.example.com", Value::Null),
                route(other, "b.example.com", serde_json::json!(80)),
            ])
        );

        for (hostnames, error) in [
            (
                serde_json::json!([{"prefix": "web", "targetPort": null}, {"prefix": " WEB ", "targetPort": 80}]),
                (
                    "managedHostnames",
                    "Each managed domain needs a distinct subdomain",
                ),
            ),
            (
                serde_json::json!([{"prefix": "web", "targetPort": 0}]),
                ("managedHostnames", "Expected a port from 1–65535"),
            ),
        ] {
            let mut input = complete_config();
            input["managedHostnames"] = hostnames;
            assert_eq!(rejected(input), (error.0.to_owned(), error.1.to_owned()));
        }
    }
}
