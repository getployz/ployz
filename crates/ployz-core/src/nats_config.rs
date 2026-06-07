//! NATS server config policy.

use std::path::{Path, PathBuf};

use crate::ids::NodeId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsServerConfig {
    host: String,
    port: u16,
    server_name: NodeId,
    jetstream_store_dir: PathBuf,
}

impl NatsServerConfig {
    pub fn single_node(
        node_id: NodeId,
        jetstream_store_dir: PathBuf,
    ) -> Result<Self, NatsServerConfigError> {
        let config = Self {
            host: "127.0.0.1".to_owned(),
            port: 4222,
            server_name: node_id,
            jetstream_store_dir,
        };
        config.validate()?;
        Ok(config)
    }

    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    #[must_use]
    pub fn render(&self) -> String {
        let store_dir = self
            .jetstream_store_dir
            .to_str()
            .expect("validated nats store dir is UTF-8");
        let store_dir = quote_nats_string(store_dir);
        format!(
            "server_name: {}\nhost: {}\nport: {}\njetstream {{\n  store_dir: {}\n}}\n",
            self.server_name.as_str(),
            self.host,
            self.port,
            store_dir
        )
    }

    fn validate(&self) -> Result<(), NatsServerConfigError> {
        validate_config_token("host", &self.host)?;
        validate_config_path("jetstream_store_dir", &self.jetstream_store_dir)?;
        Ok(())
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
        || !value.is_absolute()
        || value.to_str().is_none()
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
