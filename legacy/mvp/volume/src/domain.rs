use std::fmt::{self, Display, Formatter};

use mvp_lease::LeaseResource;
use serde::{Deserialize, Serialize};

use crate::error::{VolumeError, VolumeResult};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct VolumeNamespace(String);

impl VolumeNamespace {
    pub fn parse(value: impl Into<String>) -> VolumeResult<Self> {
        let value = value.into();
        validate_identifier("volume namespace", &value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for VolumeNamespace {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct VolumeId(String);

impl VolumeId {
    pub fn parse(value: impl Into<String>) -> VolumeResult<Self> {
        let value = value.into();
        validate_identifier("volume id", &value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn lease_resource(&self, namespace: &VolumeNamespace) -> LeaseResource {
        LeaseResource::from_segments(["volume", namespace.as_str(), self.as_str()])
    }
}

impl Display for VolumeId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct VolumeTransferId(String);

impl VolumeTransferId {
    pub fn parse(value: impl Into<String>) -> VolumeResult<Self> {
        let value = value.into();
        validate_identifier("volume transfer id", &value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for VolumeTransferId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct VolumeSnapshotId(String);

impl VolumeSnapshotId {
    pub fn parse(value: impl Into<String>) -> VolumeResult<Self> {
        let value = value.into();
        validate_identifier("volume snapshot id", &value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for VolumeSnapshotId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct VolumeSnapshotGuid(String);

impl VolumeSnapshotGuid {
    pub fn parse(value: impl Into<String>) -> VolumeResult<Self> {
        let value = value.into();
        validate_identifier("volume snapshot guid", &value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for VolumeSnapshotGuid {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn validate_identifier(label: &'static str, value: &str) -> VolumeResult<()> {
    if value.is_empty() || value.trim().is_empty() {
        return Err(VolumeError::InvalidIdentifier {
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
    Err(VolumeError::InvalidIdentifier {
        label,
        value: value.to_owned(),
    })
}
