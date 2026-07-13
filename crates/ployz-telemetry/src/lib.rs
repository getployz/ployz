#![forbid(unsafe_code)]

//! Shared opt-out telemetry configuration and sink wiring.

use std::env;
use std::error::Error;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use atomic_write_file::AtomicWriteFile;
use posthog_rs::{Client as PosthogClient, ClientOptionsBuilder, Event};
use sentry::types::Dsn;
use sentry::{ClientInitGuard, ClientOptions};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Cli,
    Daemon,
}

impl Surface {
    fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Daemon => "daemon",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistedTelemetry {
    Enabled,
    Disabled,
    Unset,
    Corrupt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    Enabled,
    Disabled,
}

impl Consent {
    #[must_use]
    pub fn from_environment(persisted: PersistedTelemetry) -> Self {
        Self::resolve(
            env::var("PLOYZ_TELEMETRY").ok().as_deref(),
            env::var("DO_NOT_TRACK").ok().as_deref(),
            persisted,
        )
    }

    #[must_use]
    pub fn resolve(
        ployz_telemetry: Option<&str>,
        do_not_track: Option<&str>,
        persisted: PersistedTelemetry,
    ) -> Self {
        match ployz_telemetry {
            Some("1") => return Self::Enabled,
            Some("0") => return Self::Disabled,
            Some(_) | None => {}
        }
        if do_not_track == Some("1") {
            return Self::Disabled;
        }
        match persisted {
            PersistedTelemetry::Enabled | PersistedTelemetry::Unset => Self::Enabled,
            PersistedTelemetry::Disabled | PersistedTelemetry::Corrupt => Self::Disabled,
        }
    }

    #[must_use]
    pub fn is_enabled(self) -> bool {
        self == Self::Enabled
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("home directory is unavailable")]
    HomeUnavailable,
    #[error("could not create telemetry config directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not read telemetry config {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not serialize telemetry config: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("could not write telemetry config {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredConfig {
    install_id: Uuid,
    telemetry: Option<bool>,
    cli_notice_seen: bool,
    daemon_notice_seen: bool,
}

impl StoredConfig {
    fn fresh() -> Self {
        Self {
            install_id: Uuid::new_v4(),
            telemetry: None,
            cli_notice_seen: false,
            daemon_notice_seen: false,
        }
    }
}

#[derive(Debug)]
pub struct ConfigFile {
    path: PathBuf,
    stored: StoredConfig,
    corrupt: bool,
    diagnostic: Option<String>,
}

impl ConfigFile {
    pub fn load_or_create_default() -> Result<Self, ConfigError> {
        let home = env::var_os("HOME").ok_or(ConfigError::HomeUnavailable)?;
        Self::load_or_create(PathBuf::from(home).join(".ployz/config"))
    }

    pub fn load_or_create(path: impl Into<PathBuf>) -> Result<Self, ConfigError> {
        let path = path.into();
        match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(stored) => Ok(Self {
                    path,
                    stored,
                    corrupt: false,
                    diagnostic: None,
                }),
                Err(error) => Ok(Self {
                    diagnostic: Some(format!(
                        "could not parse telemetry config {}: {error}",
                        path.display()
                    )),
                    path,
                    stored: StoredConfig::fresh(),
                    corrupt: true,
                }),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let config = Self {
                    path,
                    stored: StoredConfig::fresh(),
                    corrupt: false,
                    diagnostic: None,
                };
                config.save()?;
                Ok(config)
            }
            Err(source) => Err(ConfigError::Read { path, source }),
        }
    }

    #[must_use]
    pub fn install_id(&self) -> Uuid {
        self.stored.install_id
    }

    #[must_use]
    pub fn persisted_telemetry(&self) -> PersistedTelemetry {
        if self.corrupt {
            return PersistedTelemetry::Corrupt;
        }
        match self.stored.telemetry {
            Some(true) => PersistedTelemetry::Enabled,
            Some(false) => PersistedTelemetry::Disabled,
            None => PersistedTelemetry::Unset,
        }
    }

    #[must_use]
    pub fn diagnostic(&self) -> Option<&str> {
        self.diagnostic.as_deref()
    }

    #[must_use]
    pub fn notice_needed(&self, surface: Surface) -> bool {
        match surface {
            Surface::Cli => !self.stored.cli_notice_seen,
            Surface::Daemon => !self.stored.daemon_notice_seen,
        }
    }

    pub fn mark_notice_seen(&mut self, surface: Surface) -> Result<(), ConfigError> {
        match surface {
            Surface::Cli => self.stored.cli_notice_seen = true,
            Surface::Daemon => self.stored.daemon_notice_seen = true,
        }
        self.corrupt = false;
        self.diagnostic = None;
        self.save()
    }

    pub fn set_telemetry(&mut self, enabled: bool) -> Result<(), ConfigError> {
        self.stored.telemetry = Some(enabled);
        self.corrupt = false;
        self.diagnostic = None;
        self.save()
    }

    fn save(&self) -> Result<(), ConfigError> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|source| ConfigError::CreateDirectory {
            path: parent.to_path_buf(),
            source,
        })?;
        let bytes = serde_json::to_vec_pretty(&self.stored)?;
        let mut file = AtomicWriteFile::options()
            .open(&self.path)
            .map_err(|source| ConfigError::Write {
                path: self.path.clone(),
                source,
            })?;
        file.write_all(&bytes)
            .map_err(|source| ConfigError::Write {
                path: self.path.clone(),
                source,
            })?;
        file.commit().map_err(|source| ConfigError::Write {
            path: self.path.clone(),
            source,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOutcome {
    Succeeded,
    Failed,
}

impl CommandOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Error)]
pub enum TelemetryError {
    #[error("invalid Sentry DSN: {0}")]
    InvalidSentryDsn(String),
    #[error("could not configure PostHog: {0}")]
    Posthog(String),
}

pub struct Telemetry {
    enabled: bool,
    surface: Surface,
    version: &'static str,
    sentry: Option<ClientInitGuard>,
    posthog: Option<PosthogClient>,
    distinct_id: Option<String>,
}

impl Telemetry {
    pub fn start(
        surface: Surface,
        version: &'static str,
        consent: Consent,
        sentry_dsn: &str,
    ) -> Result<Self, TelemetryError> {
        let enabled = consent.is_enabled();
        let sentry = if enabled && !sentry_dsn.trim().is_empty() {
            let dsn = sentry_dsn
                .trim()
                .parse::<Dsn>()
                .map_err(|error| TelemetryError::InvalidSentryDsn(error.to_string()))?;
            let guard = sentry::init(ClientOptions {
                dsn: Some(dsn),
                release: Some(version.into()),
                shutdown_timeout: SHUTDOWN_TIMEOUT,
                ..ClientOptions::default()
            });
            sentry::configure_scope(|scope| {
                scope.set_tag("surface", surface.as_str());
                scope.set_tag("version", version);
            });
            Some(guard)
        } else {
            None
        };
        Ok(Self {
            enabled,
            surface,
            version,
            sentry,
            posthog: None,
            distinct_id: None,
        })
    }

    pub fn attach_posthog(
        &mut self,
        api_key: &str,
        distinct_id: impl Into<String>,
    ) -> Result<(), TelemetryError> {
        if !self.enabled || api_key.trim().is_empty() {
            return Ok(());
        }
        let mut builder = ClientOptionsBuilder::default();
        builder
            .api_key(api_key.trim().to_owned())
            .request_timeout_seconds(SHUTDOWN_TIMEOUT.as_secs())
            .shutdown_timeout_ms(SHUTDOWN_TIMEOUT.as_millis() as u64)
            .is_server(self.surface == Surface::Daemon);
        let options = builder
            .build()
            .map_err(|error| TelemetryError::Posthog(error.to_string()))?;
        self.posthog = Some(posthog_rs::client(options));
        self.distinct_id = Some(distinct_id.into());
        Ok(())
    }

    pub fn capture_command(&self, command: &str, outcome: CommandOutcome, duration: Duration) {
        let Some((client, distinct_id)) = self.posthog_with_id() else {
            return;
        };
        let mut event = self.event("command_invoked", distinct_id);
        let _ = event.insert_prop("command", command);
        let _ = event.insert_prop("outcome", outcome.as_str());
        let _ = event.insert_prop("duration", duration.as_millis() as u64);
        client.capture(event);
    }

    pub fn capture_daemon_started(&self) {
        let Some((client, distinct_id)) = self.posthog_with_id() else {
            return;
        };
        client.capture(self.event("daemon_started", distinct_id));
    }

    pub fn capture_error(&self, error: &(dyn Error + 'static), failure_tag: &str) {
        if self.sentry.is_none() {
            return;
        }
        sentry::with_scope(
            |scope| scope.set_tag("failure", failure_tag),
            || {
                sentry::capture_error(error);
            },
        );
    }

    pub fn shutdown(self) {
        if let Some(client) = &self.posthog {
            client.shutdown();
        }
    }

    fn posthog_with_id(&self) -> Option<(&PosthogClient, &str)> {
        Some((self.posthog.as_ref()?, self.distinct_id.as_deref()?))
    }

    fn event(&self, name: &str, distinct_id: &str) -> Event {
        let mut event = Event::new(name.to_owned(), distinct_id.to_owned());
        let _ = event.insert_prop("version", self.version);
        let _ = event.insert_prop("os", env::consts::OS);
        let _ = event.insert_prop("arch", env::consts::ARCH);
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_uses_documented_precedence() {
        assert_eq!(
            Consent::resolve(Some("1"), Some("1"), PersistedTelemetry::Disabled),
            Consent::Enabled
        );
        assert_eq!(
            Consent::resolve(Some("invalid"), Some("1"), PersistedTelemetry::Enabled),
            Consent::Disabled
        );
        assert_eq!(
            Consent::resolve(None, None, PersistedTelemetry::Unset),
            Consent::Enabled
        );
        assert_eq!(
            Consent::resolve(None, None, PersistedTelemetry::Corrupt),
            Consent::Disabled
        );
    }

    #[test]
    fn config_persists_identity_preference_and_notices() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/config");
        let mut first = ConfigFile::load_or_create(&path).unwrap();
        let install_id = first.install_id();
        first.set_telemetry(false).unwrap();
        first.mark_notice_seen(Surface::Cli).unwrap();

        let second = ConfigFile::load_or_create(&path).unwrap();
        assert_eq!(second.install_id(), install_id);
        assert_eq!(second.persisted_telemetry(), PersistedTelemetry::Disabled);
        assert!(!second.notice_needed(Surface::Cli));
        assert!(second.notice_needed(Surface::Daemon));
    }

    #[test]
    fn corrupt_config_is_diagnostic_and_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config");
        fs::write(&path, b"not json").unwrap();

        let config = ConfigFile::load_or_create(&path).unwrap();
        assert_eq!(config.persisted_telemetry(), PersistedTelemetry::Corrupt);
        assert!(config.diagnostic().is_some());
        assert_eq!(fs::read(&path).unwrap(), b"not json");
    }

    #[test]
    fn blank_sink_keys_are_inert() {
        let mut telemetry = Telemetry::start(Surface::Cli, "test", Consent::Enabled, " ").unwrap();
        telemetry.attach_posthog("", "install-id").unwrap();
        telemetry.capture_command("version", CommandOutcome::Succeeded, Duration::ZERO);
        telemetry.shutdown();
    }
}
