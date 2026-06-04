//! Supervised `nats-server` config and process command preparation.

use std::path::{Path, PathBuf};
use std::process::Command;

use ployz_nats::connect::NatsClientEndpoint;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsServerConfig {
    host: String,
    port: u16,
    server_name: String,
    jetstream_store_dir: PathBuf,
}

impl NatsServerConfig {
    pub fn single_node(
        server_name: impl Into<String>,
        jetstream_store_dir: PathBuf,
    ) -> Result<Self, NatsServerConfigError> {
        let config = Self {
            host: "127.0.0.1".to_owned(),
            port: 4222,
            server_name: server_name.into(),
            jetstream_store_dir,
        };
        config.validate()?;
        Ok(config)
    }

    #[must_use]
    pub fn render(&self) -> String {
        NatsConfigRenderer::new(self).render()
    }

    #[must_use]
    pub fn client_endpoint(&self) -> NatsClientEndpoint {
        NatsClientEndpoint::tcp(&self.host, self.port)
    }

    fn validate(&self) -> Result<(), NatsServerConfigError> {
        validate_config_token("server_name", &self.server_name)?;
        validate_config_token("host", &self.host)?;
        validate_config_path("jetstream_store_dir", &self.jetstream_store_dir)?;
        Ok(())
    }
}

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
        validate_config_path("binary_path", &binary_path)?;
        validate_config_path("config_path", &config_path)?;

        Ok(Self {
            binary_path,
            config_path,
            rendered_config: config.render(),
            client_endpoint: config.client_endpoint(),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsServerConfigError {
    InvalidToken { field: &'static str, value: String },
    InvalidPath { field: &'static str, value: PathBuf },
}

fn validate_config_token(field: &'static str, value: &str) -> Result<(), NatsServerConfigError> {
    if value.is_empty()
        || value
            .chars()
            .any(|character| matches!(character, '"' | '\'' | '{' | '}' | '\n' | '\r'))
    {
        return Err(NatsServerConfigError::InvalidToken {
            field,
            value: value.to_owned(),
        });
    }

    Ok(())
}

fn validate_config_path(field: &'static str, value: &Path) -> Result<(), NatsServerConfigError> {
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

struct NatsConfigRenderer<'a> {
    config: &'a NatsServerConfig,
}

impl<'a> NatsConfigRenderer<'a> {
    const fn new(config: &'a NatsServerConfig) -> Self {
        Self { config }
    }

    fn render(&self) -> String {
        let store_dir = quote_nats_string(&self.config.jetstream_store_dir.to_string_lossy());
        format!(
            "server_name: {}\nhost: {}\nport: {}\njetstream {{\n  store_dir: {}\n}}\n",
            self.config.server_name, self.config.host, self.config.port, store_dir
        )
    }
}

fn quote_nats_string(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            _ => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}
