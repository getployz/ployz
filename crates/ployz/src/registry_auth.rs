use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use base64::Engine;
use ployz_core::deploy::{DeployServiceSpec, ImageReference, ImageSource, RegistryCredential};
use ployz_core::ids::ServiceId;
use serde::Deserialize;
use wait_timeout::ChildExt;

const CREDENTIAL_HELPER_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn deploy_registry_credentials(
    services: &[DeployServiceSpec],
) -> Result<BTreeMap<ServiceId, RegistryCredential>, RegistryAuthError> {
    let registry_services = services
        .iter()
        .filter(|service| matches!(service.image_source, ImageSource::Registry))
        .map(|service| (service.service_id.clone(), service.image.clone()))
        .collect::<Vec<_>>();
    if registry_services.is_empty() {
        return Ok(BTreeMap::new());
    }
    tokio::task::spawn_blocking(move || deploy_registry_credentials_blocking(&registry_services))
        .await
        .map_err(|error| RegistryAuthError::LookupTask {
            message: error.to_string(),
        })?
}

fn deploy_registry_credentials_blocking(
    services: &[(ServiceId, ImageReference)],
) -> Result<BTreeMap<ServiceId, RegistryCredential>, RegistryAuthError> {
    let Some(config) = read_docker_config()? else {
        return Ok(BTreeMap::new());
    };
    collect_registry_credentials(services, |image| credential_for_image(&config, image))
}

fn collect_registry_credentials(
    services: &[(ServiceId, ImageReference)],
    mut lookup: impl FnMut(&ImageReference) -> Result<Option<RegistryCredential>, RegistryAuthError>,
) -> Result<BTreeMap<ServiceId, RegistryCredential>, RegistryAuthError> {
    let mut by_registry = BTreeMap::new();
    let mut credentials = BTreeMap::new();
    for (service_id, image) in services {
        let registry = registry_for_image(image);
        if !by_registry.contains_key(&registry) {
            by_registry.insert(registry.clone(), lookup(image)?);
        }
        if let Some(credential) = by_registry.get(&registry).cloned().flatten() {
            credentials.insert(service_id.clone(), credential);
        }
    }
    Ok(credentials)
}

#[derive(Debug, Default, Deserialize)]
struct DockerConfig {
    #[serde(default)]
    auths: BTreeMap<String, DockerAuth>,
    #[serde(default, rename = "credsStore")]
    credentials_store: Option<String>,
    #[serde(default, rename = "credHelpers")]
    credential_helpers: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
struct DockerAuth {
    auth: Option<String>,
    username: Option<String>,
    password: Option<String>,
    #[serde(rename = "identitytoken")]
    identity_token: Option<String>,
}

fn read_docker_config() -> Result<Option<DockerConfig>, RegistryAuthError> {
    let Some(path) = docker_config_path() else {
        return Ok(None);
    };
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(RegistryAuthError::ReadConfig {
                path,
                message: error.to_string(),
            });
        }
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| RegistryAuthError::InvalidConfig {
            path,
            message: error.to_string(),
        })
}

fn docker_config_path() -> Option<PathBuf> {
    if let Some(config) = std::env::var_os("DOCKER_CONFIG") {
        return Some(PathBuf::from(config).join("config.json"));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".docker/config.json"))
}

fn credential_for_image(
    config: &DockerConfig,
    image: &ImageReference,
) -> Result<Option<RegistryCredential>, RegistryAuthError> {
    let registry = registry_for_image(image);
    let configured_server = config
        .auths
        .keys()
        .chain(config.credential_helpers.keys())
        .find(|server| same_registry(server, &registry))
        .cloned()
        .unwrap_or_else(|| registry.clone());
    let helper = config
        .credential_helpers
        .iter()
        .find(|(server, _)| same_registry(server, &registry))
        .map(|(_, helper)| helper)
        .or(config.credentials_store.as_ref());
    if let Some(helper) = helper {
        return credential_from_helper(helper, &configured_server);
    }
    let Some(auth) = config
        .auths
        .iter()
        .find(|(server, _)| same_registry(server, &registry))
        .map(|(_, auth)| auth)
    else {
        return Ok(None);
    };
    inline_credential(auth)
}

fn inline_credential(auth: &DockerAuth) -> Result<Option<RegistryCredential>, RegistryAuthError> {
    if let Some(token) = &auth.identity_token {
        return RegistryCredential::try_identity_token(token.clone())
            .map(Some)
            .map_err(|error| RegistryAuthError::InvalidCredential(error.to_string()));
    }
    if let (Some(username), Some(password)) = (&auth.username, &auth.password) {
        return RegistryCredential::try_basic(username.clone(), password.clone())
            .map(Some)
            .map_err(|error| RegistryAuthError::InvalidCredential(error.to_string()));
    }
    let Some(encoded) = &auth.auth else {
        return Ok(None);
    };
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| {
            RegistryAuthError::InvalidCredential("Docker auth is not base64".to_owned())
        })?;
    let decoded = String::from_utf8(decoded)
        .map_err(|_| RegistryAuthError::InvalidCredential("Docker auth is not UTF-8".to_owned()))?;
    let Some((username, secret)) = decoded.split_once(':') else {
        return Err(RegistryAuthError::InvalidCredential(
            "Docker auth does not contain username:secret".to_owned(),
        ));
    };
    RegistryCredential::try_basic(username, secret)
        .map(Some)
        .map_err(|error| RegistryAuthError::InvalidCredential(error.to_string()))
}

fn credential_from_helper(
    helper: &str,
    server: &str,
) -> Result<Option<RegistryCredential>, RegistryAuthError> {
    let program = format!("docker-credential-{helper}");
    let mut child = Command::new(&program)
        .arg("get")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| RegistryAuthError::CredentialHelper {
            helper: helper.to_owned(),
            message: error.to_string(),
        })?;
    let Some(mut stdin) = child.stdin.take() else {
        return Err(RegistryAuthError::CredentialHelper {
            helper: helper.to_owned(),
            message: "stdin was unavailable".to_owned(),
        });
    };
    stdin
        .write_all(server.as_bytes())
        .and_then(|()| stdin.write_all(b"\n"))
        .map_err(|error| RegistryAuthError::CredentialHelper {
            helper: helper.to_owned(),
            message: error.to_string(),
        })?;
    drop(stdin);
    match child.wait_timeout(CREDENTIAL_HELPER_TIMEOUT) {
        Ok(Some(_)) => {}
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RegistryAuthError::CredentialHelper {
                helper: helper.to_owned(),
                message: "timed out".to_owned(),
            });
        }
        Err(error) => {
            return Err(RegistryAuthError::CredentialHelper {
                helper: helper.to_owned(),
                message: error.to_string(),
            });
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|error| RegistryAuthError::CredentialHelper {
            helper: helper.to_owned(),
            message: error.to_string(),
        })?;
    if !output.status.success() {
        return Ok(None);
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct HelperCredential {
        username: String,
        secret: String,
    }
    let response = serde_json::from_slice::<HelperCredential>(&output.stdout).map_err(|error| {
        RegistryAuthError::CredentialHelper {
            helper: helper.to_owned(),
            message: error.to_string(),
        }
    })?;
    let credential = if response.username == "<token>" {
        RegistryCredential::try_identity_token(response.secret)
    } else {
        RegistryCredential::try_basic(response.username, response.secret)
    };
    credential
        .map(Some)
        .map_err(|error| RegistryAuthError::InvalidCredential(error.to_string()))
}

fn registry_for_image(image: &ImageReference) -> String {
    let name = image
        .as_str()
        .split_once('@')
        .map_or(image.as_str(), |(name, _)| name);
    let first = name.split('/').next().unwrap_or(name);
    if first.contains('.') || first.contains(':') || first == "localhost" {
        first.to_owned()
    } else {
        "https://index.docker.io/v1/".to_owned()
    }
}

fn same_registry(left: &str, right: &str) -> bool {
    fn normalized(value: &str) -> &str {
        value
            .strip_prefix("https://")
            .or_else(|| value.strip_prefix("http://"))
            .unwrap_or(value)
            .trim_end_matches('/')
    }
    let left = normalized(left);
    let right = normalized(right);
    left == right
        || matches!(left, "index.docker.io/v1" | "docker.io")
            && matches!(right, "index.docker.io/v1" | "docker.io")
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryAuthError {
    #[error("Docker config {} is unreadable: {message}", path.display())]
    ReadConfig { path: PathBuf, message: String },
    #[error("Docker config {} is invalid: {message}", path.display())]
    InvalidConfig { path: PathBuf, message: String },
    #[error("Docker registry credential is invalid: {0}")]
    InvalidCredential(String),
    #[error("Docker credential helper {helper:?} failed: {message}")]
    CredentialHelper { helper: String, message: String },
    #[error("Docker registry credential lookup task failed: {message}")]
    LookupTask { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_docker_login_auth_is_borrowed_without_exposing_the_secret() {
        let config = serde_json::from_str::<DockerConfig>(
            r#"{"auths":{"registry.example:5000":{"auth":"YWxpY2U6czNjcjN0"}}}"#,
        )
        .expect("valid Docker config");
        let credential = credential_for_image(
            &config,
            &ImageReference::try_new("registry.example:5000/acme/api:1").expect("valid image"),
        )
        .expect("credential lookup succeeds")
        .expect("credential exists");

        let RegistryCredential::Basic { username, password } = &credential else {
            panic!("inline auth should produce basic credentials");
        };
        assert_eq!(username.as_str(), "alice");
        assert_eq!(password.secret(), "s3cr3t");
        assert!(!format!("{credential:?}").contains("s3cr3t"));
    }

    #[test]
    fn inline_identity_token_is_typed_and_redacted() {
        let auth = DockerAuth {
            identity_token: Some("registry-token".to_owned()),
            ..DockerAuth::default()
        };

        let credential = inline_credential(&auth)
            .expect("credential lookup succeeds")
            .expect("credential exists");

        let RegistryCredential::IdentityToken { token } = &credential else {
            panic!("identitytoken should produce token credentials");
        };
        assert_eq!(token.secret(), "registry-token");
        assert!(!format!("{credential:?}").contains("registry-token"));
    }

    #[test]
    fn registry_lookup_runs_once_for_services_sharing_a_registry() {
        let services = [
            (
                ServiceId::try_new("api").expect("valid service id"),
                ImageReference::try_new("registry.example/acme/api:1").expect("valid image"),
            ),
            (
                ServiceId::try_new("worker").expect("valid service id"),
                ImageReference::try_new("registry.example/acme/worker:1").expect("valid image"),
            ),
        ];
        let mut lookups = 0;
        let credentials = collect_registry_credentials(&services, |_| {
            lookups += 1;
            RegistryCredential::try_basic("alice", "s3cr3t")
                .map(Some)
                .map_err(|error| RegistryAuthError::InvalidCredential(error.to_string()))
        })
        .expect("credential collection succeeds");

        assert_eq!(lookups, 1);
        assert_eq!(credentials.len(), 2);
    }
}
