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

    pub fn parse(value: &str) -> Result<Self, NatsServerVersionParseError> {
        let version = value
            .split(['-', '+'])
            .next()
            .ok_or(NatsServerVersionParseError::Empty)?;
        if version.is_empty() {
            return Err(NatsServerVersionParseError::Empty);
        }

        let mut parts = version.split('.');
        let Some(major) = parts.next() else {
            return Err(NatsServerVersionParseError::MissingPart {
                value: value.to_owned(),
            });
        };
        let Some(minor) = parts.next() else {
            return Err(NatsServerVersionParseError::MissingPart {
                value: value.to_owned(),
            });
        };
        let Some(patch) = parts.next() else {
            return Err(NatsServerVersionParseError::MissingPart {
                value: value.to_owned(),
            });
        };
        if parts.next().is_some() {
            return Err(NatsServerVersionParseError::TooManyParts {
                value: value.to_owned(),
            });
        }

        Ok(Self {
            major: parse_version_part(major, value)?,
            minor: parse_version_part(minor, value)?,
            patch: parse_version_part(patch, value)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsServerVersionParseError {
    Empty,
    MissingPart { value: String },
    TooManyParts { value: String },
    InvalidPart { value: String },
}

fn parse_version_part(part: &str, full_value: &str) -> Result<u16, NatsServerVersionParseError> {
    part.parse::<u16>()
        .map_err(|_| NatsServerVersionParseError::InvalidPart {
            value: full_value.to_owned(),
        })
}
