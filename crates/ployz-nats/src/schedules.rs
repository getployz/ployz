//! NATS server version parsing for bootstrap checks.

use semver::Version;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct NatsServerVersion {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

impl NatsServerVersion {
    #[must_use]
    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    pub fn parse(value: &str) -> Result<Self, NatsServerVersionParseError> {
        if value.is_empty() {
            return Err(NatsServerVersionParseError::Empty);
        }
        let version = Version::parse(value).map_err(|_| NatsServerVersionParseError::Invalid {
            value: value.to_owned(),
        })?;
        Ok(Self {
            major: u16::try_from(version.major).map_err(|_| {
                NatsServerVersionParseError::Invalid {
                    value: value.to_owned(),
                }
            })?,
            minor: u16::try_from(version.minor).map_err(|_| {
                NatsServerVersionParseError::Invalid {
                    value: value.to_owned(),
                }
            })?,
            patch: u16::try_from(version.patch).map_err(|_| {
                NatsServerVersionParseError::Invalid {
                    value: value.to_owned(),
                }
            })?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NatsServerVersionParseError {
    #[error("NATS server version is empty")]
    Empty,
    #[error("NATS server version {value:?} is invalid")]
    Invalid { value: String },
}

#[cfg(test)]
mod tests {
    use super::NatsServerVersion;

    #[test]
    fn nats_server_version_keeps_core_semver_numbers() {
        assert_eq!(
            NatsServerVersion::parse("2.14.1-beta.1+build.7").expect("version parses"),
            NatsServerVersion::new(2, 14, 1)
        );
        assert!(NatsServerVersion::parse("2.14").is_err());
    }
}
