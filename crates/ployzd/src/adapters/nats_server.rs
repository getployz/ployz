//! Supervised `nats-server` config and process command preparation.

#[cfg(test)]
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::process::Command;

#[cfg(test)]
use ployz_nats::connect::NatsClientEndpoint;
use ployz_nats::connect::NatsClientUrl;
#[cfg(test)]
pub use ployz_nats::server_config::{NatsServerConfig, NatsServerConfigError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsServerLaunch {
    #[cfg(test)]
    Supervised(PreparedNatsServerService),
    External(NatsClientUrl),
}

impl NatsServerLaunch {
    #[must_use]
    #[cfg(test)]
    pub fn client_url(&self) -> NatsClientUrl {
        match self {
            Self::Supervised(service) => service.client_url(),
            Self::External(url) => url.clone(),
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedNatsServerService {
    binary_path: PathBuf,
    config_path: PathBuf,
    rendered_config: String,
    client_endpoint: NatsClientEndpoint,
}

#[cfg(test)]
impl PreparedNatsServerService {
    pub fn prepare(
        binary_path: PathBuf,
        config_path: PathBuf,
        config: NatsServerConfig,
    ) -> Result<Self, NatsServerConfigError> {
        validate_process_path("binary_path", &binary_path)?;
        validate_process_path("config_path", &config_path)?;

        Ok(Self {
            binary_path,
            config_path,
            rendered_config: config.render(),
            client_endpoint: NatsClientEndpoint::tcp(config.client_host(), config.port()),
        })
    }

    #[must_use]
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.binary_path);
        command.arg("--config").arg(&self.config_path);
        command
    }

    #[must_use]
    pub fn rendered_config(&self) -> &str {
        &self.rendered_config
    }

    #[must_use]
    pub fn client_url(&self) -> NatsClientUrl {
        NatsClientUrl::from_endpoint(&self.client_endpoint)
    }

    #[must_use]
    pub fn client_endpoint(&self) -> NatsClientEndpoint {
        self.client_endpoint.clone()
    }
}

#[cfg(test)]
fn validate_process_path(field: &'static str, value: &Path) -> Result<(), NatsServerConfigError> {
    let rendered = value.to_string_lossy();
    if rendered.is_empty()
        || rendered
            .chars()
            .any(|character| matches!(character, '\n' | '\r' | '\0'))
    {
        return Err(NatsServerConfigError::InvalidPath {
            field,
            value: value.to_path_buf(),
        });
    }

    Ok(())
}
