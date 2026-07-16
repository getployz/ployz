//! Volume declarations and durable storage naming contracts.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "Brand<string, \"VolumeName\">"))]
#[serde(try_from = "String", into = "String")]
pub struct VolumeName(String);

impl VolumeName {
    pub fn try_new(value: impl Into<String>) -> Result<Self, VolumeNameError> {
        let value = value.into();
        if value.is_empty() {
            return Err(VolumeNameError::Empty);
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(VolumeNameError::InvalidCharacter { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn stable_storage_name(&self, namespace_id: &NamespaceId) -> String {
        let namespace = namespace_id.as_str();
        format!(
            "ployz-n{}-{namespace}-v{}-{}",
            namespace.len(),
            self.0.len(),
            self.0
        )
    }
}

impl TryFrom<String> for VolumeName {
    type Error = VolumeNameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<VolumeName> for String {
    fn from(value: VolumeName) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VolumeNameError {
    #[error("volume name is empty")]
    Empty,
    #[error("volume name contains invalid characters: {value}")]
    InvalidCharacter { value: String },
}

const ZFS_DATASET_NAME_MAX_BYTES: usize = 255;

/// One physical ZFS pool component; it cannot name an arbitrary dataset root.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "Brand<string, \"ZfsPoolName\">"))]
#[serde(try_from = "String", into = "String")]
pub struct ZfsPoolName(String);

impl ZfsPoolName {
    pub fn try_new(value: impl Into<String>) -> Result<Self, ZfsPoolNameError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ZfsPoolNameError::Empty);
        }
        if value.contains('/') {
            return Err(ZfsPoolNameError::Hierarchical { value });
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
        {
            return Err(ZfsPoolNameError::InvalidCharacter { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ZfsPoolName {
    type Error = ZfsPoolNameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<ZfsPoolName> for String {
    fn from(value: ZfsPoolName) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ZfsPoolNameError {
    #[error("ZFS pool name is empty")]
    Empty,
    #[error("ZFS pool name must be one path component: {value}")]
    Hierarchical { value: String },
    #[error("ZFS pool name contains an invalid character: {value}")]
    InvalidCharacter { value: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "Brand<string, \"DatasetName\">"))]
#[serde(try_from = "String", into = "String")]
pub struct DatasetName(String);

impl DatasetName {
    pub fn try_new(value: impl Into<String>) -> Result<Self, DatasetNameError> {
        let value = value.into();
        if value.is_empty() {
            return Err(DatasetNameError::Empty);
        }
        if value.len() > ZFS_DATASET_NAME_MAX_BYTES {
            return Err(DatasetNameError::NameBudgetExceeded {
                bytes: value.len(),
                maximum: ZFS_DATASET_NAME_MAX_BYTES,
            });
        }
        if value.split('/').any(str::is_empty) {
            return Err(DatasetNameError::EmptyComponent);
        }
        if !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':' | b'/')
        }) {
            return Err(DatasetNameError::InvalidCharacter { value });
        }
        if !is_canonical_provisioned_dataset_name(&value) {
            return Err(DatasetNameError::NonCanonical { value });
        }
        Ok(Self(value))
    }

    /// Constructs the only dataset namespace used for Provisioned Volumes.
    pub fn for_volume(
        pool: &ZfsPoolName,
        namespace_id: &NamespaceId,
        volume_name: &VolumeName,
    ) -> Result<Self, DatasetNameError> {
        Self::try_new(format!(
            "{}/ployz/volumes/{}",
            pool.as_str(),
            volume_name.stable_storage_name(namespace_id)
        ))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn matches_volume(&self, namespace_id: &NamespaceId, volume_name: &VolumeName) -> bool {
        self.0
            .rsplit_once('/')
            .is_some_and(|(_, leaf)| leaf == volume_name.stable_storage_name(namespace_id))
    }
}

impl TryFrom<String> for DatasetName {
    type Error = DatasetNameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<DatasetName> for String {
    fn from(value: DatasetName) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DatasetNameError {
    #[error("dataset name is empty")]
    Empty,
    #[error("dataset name contains an empty path component")]
    EmptyComponent,
    #[error("dataset name contains an invalid character: {value}")]
    InvalidCharacter { value: String },
    #[error("dataset name is outside the canonical Ployz provisioned-volume namespace: {value}")]
    NonCanonical { value: String },
    #[error("dataset name is {bytes} bytes; maximum is {maximum}")]
    NameBudgetExceeded { bytes: usize, maximum: usize },
}

fn is_canonical_provisioned_dataset_name(value: &str) -> bool {
    let mut components = value.split('/');
    let (Some(pool), Some("ployz"), Some("volumes"), Some(leaf), None) = (
        components.next(),
        components.next(),
        components.next(),
        components.next(),
        components.next(),
    ) else {
        return false;
    };
    if ZfsPoolName::try_new(pool).is_err() {
        return false;
    }
    let Some(framed) = leaf.strip_prefix("ployz-n") else {
        return false;
    };
    let Some((namespace_length, framed)) = take_decimal_prefix(framed) else {
        return false;
    };
    let Some(framed) = framed.strip_prefix('-') else {
        return false;
    };
    let Some(namespace) = framed.get(..namespace_length) else {
        return false;
    };
    let Some(framed) = framed.get(namespace_length..) else {
        return false;
    };
    let Some(framed) = framed.strip_prefix("-v") else {
        return false;
    };
    let Some((volume_length, framed)) = take_decimal_prefix(framed) else {
        return false;
    };
    let Some(volume) = framed.strip_prefix('-') else {
        return false;
    };
    if volume.len() != volume_length {
        return false;
    }
    let Ok(namespace_id) = NamespaceId::try_new(namespace) else {
        return false;
    };
    let Ok(volume_name) = VolumeName::try_new(volume) else {
        return false;
    };
    volume_name.stable_storage_name(&namespace_id) == leaf
}

fn take_decimal_prefix(value: &str) -> Option<(usize, &str)> {
    let digits = value.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    value
        .get(..digits)?
        .parse::<usize>()
        .ok()
        .map(|length| (length, &value[digits..]))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "SafeInteger<\"VolumeMaxSizeBytes\">")
)]
#[serde(try_from = "u64", into = "u64")]
pub struct VolumeMaxSizeBytes(NonZeroU64);

impl VolumeMaxSizeBytes {
    pub fn try_new(value: u64) -> Result<Self, VolumeMaxSizeError> {
        let Some(value) = NonZeroU64::new(value) else {
            return Err(VolumeMaxSizeError::Zero);
        };
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<u64> for VolumeMaxSizeBytes {
    type Error = VolumeMaxSizeError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<VolumeMaxSizeBytes> for u64 {
    fn from(value: VolumeMaxSizeBytes) -> Self {
        value.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VolumeMaxSizeError {
    #[error("volume maximum size must be greater than zero")]
    Zero,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum VolumeSpec {
    Plain,
    Provisioned { max_size_bytes: VolumeMaxSizeBytes },
}
