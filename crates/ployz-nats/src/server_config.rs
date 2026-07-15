//! Concrete NATS server configuration rendering.

use std::path::{Path, PathBuf};

use ployz_core::ids::MachineId;

/// Where the NATS listener binds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsListener {
    Loopback,
    External { advertise_host: NatsAdvertisedHost },
}

/// TLS certificate and key paths rendered into the server configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsServerTlsFiles {
    pub cert_file: PathBuf,
    pub key_file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsServerConfig {
    listener: NatsListener,
    port: u16,
    server_name: MachineId,
    tls: NatsServerTlsFiles,
    authorized_users_include: PathBuf,
}

impl NatsServerConfig {
    pub fn single_machine(
        machine_id: MachineId,
        listener: NatsListener,
        tls: NatsServerTlsFiles,
        authorized_users_include: PathBuf,
    ) -> Result<Self, NatsServerConfigError> {
        let config = Self {
            listener,
            port: 4222,
            server_name: machine_id,
            tls,
            authorized_users_include,
        };
        config.validate()?;
        Ok(config)
    }

    #[must_use]
    pub fn client_host(&self) -> &str {
        match &self.listener {
            NatsListener::Loopback => "127.0.0.1",
            NatsListener::External { advertise_host } => advertise_host.as_str(),
        }
    }

    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    #[must_use]
    pub fn render(&self) -> String {
        let cert_file = quote_nats_string(
            self.tls
                .cert_file
                .to_str()
                .expect("validated nats tls cert path is UTF-8"),
        );
        let key_file = quote_nats_string(
            self.tls
                .key_file
                .to_str()
                .expect("validated nats tls key path is UTF-8"),
        );
        let include_path = quote_nats_string(
            self.authorized_users_include
                .to_str()
                .expect("validated authorized-users include path is UTF-8"),
        );
        let host = match &self.listener {
            NatsListener::Loopback => "127.0.0.1",
            NatsListener::External { .. } => "0.0.0.0",
        };

        let mut rendered = format!(
            "server_name: {}\nhost: {}\nport: {}\n",
            self.server_name.as_str(),
            host,
            self.port,
        );
        if let NatsListener::External { advertise_host } = &self.listener {
            let client_advertise =
                quote_nats_string(&format!("{}:{}", advertise_host.as_str(), self.port));
            rendered.push_str(&format!("client_advertise: {client_advertise}\n"));
        }
        rendered.push_str(&format!(
            "tls {{\n  cert_file: {cert_file}\n  key_file: {key_file}\n}}\njetstream: disabled\ninclude {include_path}\n"
        ));
        rendered
    }

    fn validate(&self) -> Result<(), NatsServerConfigError> {
        validate_config_path("tls.cert_file", &self.tls.cert_file)?;
        validate_config_path("tls.key_file", &self.tls.key_file)?;
        validate_include_path(&self.authorized_users_include)?;
        Ok(())
    }
}

/// The host an externally reachable listener advertises to clients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsAdvertisedHost(String);

impl NatsAdvertisedHost {
    pub fn try_new(value: impl Into<String>) -> Result<Self, NatsServerConfigError> {
        let value = value.into();
        if !is_valid_host_syntax(&value) {
            return Err(NatsServerConfigError::InvalidAdvertisedHost { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[must_use]
pub fn is_valid_host_syntax(value: &str) -> bool {
    if let Some(bracketed) = value.strip_prefix('[') {
        let Some(address) = bracketed.strip_suffix(']') else {
            return false;
        };
        return address.parse::<std::net::Ipv6Addr>().is_ok();
    }
    if value.parse::<std::net::Ipv4Addr>().is_ok() {
        return true;
    }
    is_hostname_syntax(value)
}

fn is_hostname_syntax(value: &str) -> bool {
    if value.is_empty() || value.len() > 253 {
        return false;
    }
    value.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-')
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NatsServerConfigError {
    #[error("NATS config path {field} {} is not a valid path", value.display())]
    InvalidPath { field: &'static str, value: PathBuf },
    #[error("NATS advertised host {value:?} must be a hostname, IPv4, or bracketed IPv6 address")]
    InvalidAdvertisedHost { value: String },
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

fn validate_include_path(value: &Path) -> Result<(), NatsServerConfigError> {
    let rendered = value.to_string_lossy();
    if rendered.is_empty()
        || value.to_str().is_none()
        || rendered
            .chars()
            .any(|character| matches!(character, '\n' | '\r' | '\0'))
    {
        return Err(NatsServerConfigError::InvalidPath {
            field: "authorized_users_include",
            value: value.to_path_buf(),
        });
    }
    Ok(())
}

pub(crate) fn quote_nats_string(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}
