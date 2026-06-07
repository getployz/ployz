//! Supervised `nats-server` config and process command preparation.

use std::path::{Path, PathBuf};
use std::process::Command;

pub use ployz_core::nats_config::{NatsServerConfig, NatsServerConfigError};
use ployz_nats::connect::NatsClientEndpoint;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsServerRuntime {
    Supervised(PreparedNatsServerService),
    External(NatsClientEndpoint),
}

impl NatsServerRuntime {
    #[must_use]
    pub fn client_endpoint(&self) -> NatsClientEndpoint {
        match self {
            Self::Supervised(service) => service.client_endpoint(),
            Self::External(endpoint) => endpoint.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedNatsServerService {
    binary_path: PathBuf,
    config_path: PathBuf,
    rendered_config: String,
    client_endpoint: NatsClientEndpoint,
}

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
            client_endpoint: NatsClientEndpoint::tcp(config.host(), config.port()),
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
    pub fn client_endpoint(&self) -> NatsClientEndpoint {
        self.client_endpoint.clone()
    }
}

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
