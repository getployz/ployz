//! Volume declarations and durable storage naming contracts.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Brand<string, \"VolumeName\">"))]
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
    pub fn stable_storage_name(&self, namespace_id: &CorrosionNamespaceName) -> String {
        let namespace = namespace_id.as_str();
        format!(
            "ployz-n{}-{namespace}-v{}-{}",
            namespace.len(),
            self.0.len(),
            self.0
        )
    }

    pub fn try_from_stable_storage_name(
        value: impl Into<String>,
        expected_namespace_id: &CorrosionNamespaceName,
    ) -> Result<Self, VolumeStorageNameError> {
        let value = value.into();
        let Some((namespace_id, volume_name)) = parse_stable_storage_name(&value) else {
            return Err(VolumeStorageNameError::NonCanonical { value });
        };
        if namespace_id != *expected_namespace_id {
            return Err(VolumeStorageNameError::NamespaceMismatch {
                expected: expected_namespace_id.clone(),
                actual: namespace_id,
            });
        }
        Ok(volume_name)
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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VolumeStorageNameError {
    #[error("volume storage name is not canonical Ployz storage identity: {value}")]
    NonCanonical { value: String },
    #[error(
        "volume storage name belongs to namespace {}, expected {}",
        .actual.as_str(),
        .expected.as_str()
    )]
    NamespaceMismatch {
        expected: CorrosionNamespaceName,
        actual: CorrosionNamespaceName,
    },
}

const ZFS_DATASET_NAME_MAX_BYTES: usize = 255;

/// One physical ZFS pool component; it cannot name an arbitrary dataset root.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Brand<string, \"ZfsPoolName\">"))]
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Brand<string, \"DatasetName\">"))]
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
        namespace_id: &CorrosionNamespaceName,
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
    pub fn pool(&self) -> ZfsPoolName {
        let Some((pool, _)) = self.0.split_once('/') else {
            unreachable!("validated dataset names always contain a pool component");
        };
        match ZfsPoolName::try_new(pool) {
            Ok(pool) => pool,
            Err(_) => unreachable!("validated dataset names always contain a valid pool"),
        }
    }

    #[must_use]
    pub fn matches_volume(
        &self,
        namespace_id: &CorrosionNamespaceName,
        volume_name: &VolumeName,
    ) -> bool {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
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
    parse_stable_storage_name(leaf).is_some()
}

fn parse_stable_storage_name(value: &str) -> Option<(CorrosionNamespaceName, VolumeName)> {
    let framed = value.strip_prefix("ployz-n")?;
    let (namespace_length, framed) = take_decimal_prefix(framed)?;
    let framed = framed.strip_prefix('-')?;
    let namespace = framed.get(..namespace_length)?;
    let framed = framed.get(namespace_length..)?;
    let framed = framed.strip_prefix("-v")?;
    let (volume_length, framed) = take_decimal_prefix(framed)?;
    let volume = framed.strip_prefix('-')?;
    if volume.len() != volume_length {
        return None;
    }
    let Ok(namespace_id) = CorrosionNamespaceName::try_new(namespace) else {
        return None;
    };
    let Ok(volume_name) = VolumeName::try_new(volume) else {
        return None;
    };
    (volume_name.stable_storage_name(&namespace_id) == value).then_some((namespace_id, volume_name))
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "SafeInteger<\"VolumeMaxSizeBytes\">"))]
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum VolumeSpec {
    Plain,
    Provisioned { max_size_bytes: VolumeMaxSizeBytes },
}

#[cfg(test)]
mod dataset_name_error_tests {
    use super::*;

    #[test]
    fn dataset_name_error_retains_structured_wire_evidence() {
        let error = DatasetNameError::NameBudgetExceeded {
            bytes: 300,
            maximum: ZFS_DATASET_NAME_MAX_BYTES,
        };

        let encoded = serde_json::to_value(&error).expect("serialize dataset name error");
        assert_eq!(
            encoded.get("kind").and_then(serde_json::Value::as_str),
            Some("name_budget_exceeded")
        );
        assert_eq!(
            encoded.get("bytes").and_then(serde_json::Value::as_u64),
            Some(300)
        );
        assert_eq!(
            serde_json::from_value::<DatasetNameError>(encoded)
                .expect("deserialize dataset name error"),
            error
        );
    }

    #[test]
    fn stable_storage_name_round_trips_only_for_the_expected_namespace() {
        let namespace_id = CorrosionNamespaceName::try_new("default").expect("namespace");
        let other_namespace_id = CorrosionNamespaceName::try_new("other").expect("namespace");
        let volume_name = VolumeName::try_new("postgres_data").expect("volume");
        let storage_name = volume_name.stable_storage_name(&namespace_id);

        assert_eq!(
            VolumeName::try_from_stable_storage_name(storage_name.clone(), &namespace_id),
            Ok(volume_name)
        );
        assert_eq!(
            VolumeName::try_from_stable_storage_name(storage_name, &other_namespace_id),
            Err(VolumeStorageNameError::NamespaceMismatch {
                expected: other_namespace_id,
                actual: namespace_id,
            })
        );
    }

    #[test]
    fn stable_storage_name_decoder_rejects_noncanonical_framing() {
        let namespace_id = CorrosionNamespaceName::try_new("default").expect("namespace");

        for value in [
            "postgres_data",
            "ployz-n07-default-v13-postgres_data",
            "ployz-n7-default-v013-postgres_data",
            "ployz-n7-default-v12-postgres_data",
        ] {
            assert_eq!(
                VolumeName::try_from_stable_storage_name(value, &namespace_id),
                Err(VolumeStorageNameError::NonCanonical {
                    value: value.to_owned(),
                })
            );
        }
    }
}
