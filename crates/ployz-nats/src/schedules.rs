//! NATS server version parsing for bootstrap checks.

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

    /// Parses `major.minor.patch` with any semver pre-release or build
    /// suffix (`-...`, `+...`) ignored. Exactly three numeric parts are
    /// required; the parts must fit in `u16`.
    pub fn parse(value: &str) -> Result<Self, NatsServerVersionParseError> {
        if value.is_empty() {
            return Err(NatsServerVersionParseError::Empty);
        }
        let invalid = || NatsServerVersionParseError::Invalid {
            value: value.to_owned(),
        };
        let Some(core) = value.split(['-', '+']).next() else {
            return Err(invalid());
        };
        let parts = core.split('.').collect::<Vec<_>>();
        let [major, minor, patch] = parts.as_slice() else {
            return Err(invalid());
        };
        Ok(Self {
            major: major.parse().map_err(|_| invalid())?,
            minor: minor.parse().map_err(|_| invalid())?,
            patch: patch.parse().map_err(|_| invalid())?,
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
