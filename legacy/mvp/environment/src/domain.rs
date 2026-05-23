use std::fmt::{self, Display, Formatter};

use serde::{Deserialize, Serialize};

use crate::{EnvironmentError, EnvironmentResult};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EnvironmentId(String);

impl EnvironmentId {
    pub fn parse(value: impl Into<String>) -> EnvironmentResult<Self> {
        let value = value.into();
        validate_identifier("environment id", &value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for EnvironmentId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EnvironmentBranchId(String);

impl EnvironmentBranchId {
    pub fn parse(value: impl Into<String>) -> EnvironmentResult<Self> {
        let value = value.into();
        validate_identifier("environment branch id", &value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for EnvironmentBranchId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EnvironmentCommandId(String);

impl EnvironmentCommandId {
    pub fn parse(value: impl Into<String>) -> EnvironmentResult<Self> {
        let value = value.into();
        validate_identifier("environment command id", &value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for EnvironmentCommandId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EnvironmentHeadId(String);

impl EnvironmentHeadId {
    pub fn parse(value: impl Into<String>) -> EnvironmentResult<Self> {
        let value = value.into();
        validate_identifier("environment head id", &value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for EnvironmentHeadId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EnvironmentVolumeRef(String);

impl EnvironmentVolumeRef {
    pub fn parse(value: impl Into<String>) -> EnvironmentResult<Self> {
        let value = value.into();
        validate_identifier("environment volume ref", &value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for EnvironmentVolumeRef {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EnvironmentRouteRef(String);

impl EnvironmentRouteRef {
    pub fn parse(value: impl Into<String>) -> EnvironmentResult<Self> {
        let value = value.into();
        validate_identifier("environment route ref", &value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for EnvironmentRouteRef {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EnvironmentEpoch(u64);

impl EnvironmentEpoch {
    pub fn new(value: u64) -> EnvironmentResult<Self> {
        if value == 0 {
            return Err(EnvironmentError::InvalidEpoch { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }

    pub fn next(self) -> EnvironmentResult<Self> {
        Self::new(
            self.0
                .checked_add(1)
                .ok_or(EnvironmentError::EpochOverflow { epoch: self })?,
        )
    }
}

impl Display for EnvironmentEpoch {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

fn validate_identifier(label: &'static str, value: &str) -> EnvironmentResult<()> {
    if value.is_empty() || value.trim().is_empty() {
        return Err(EnvironmentError::InvalidIdentifier {
            label,
            value: value.to_owned(),
        });
    }
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Ok(());
    }
    Err(EnvironmentError::InvalidIdentifier {
        label,
        value: value.to_owned(),
    })
}
