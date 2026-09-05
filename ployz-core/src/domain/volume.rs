//! Service storage declarations and machine-local Docker Volume observations.

use std::{
    collections::BTreeMap,
    fmt::{self, Display, Formatter},
    num::NonZeroU64,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};

use crate::{
    BindPropagation, BindRecursive, ContainerPath, DockerVolumeId, DockerVolumeName, MANAGED_LABEL,
    MachinePath, PROJECT_NAME_LABEL, ProjectName, ServiceVolumeReference, ValueError,
};

/// A storage source declared under a service-local reference.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceVolume {
    pub reference: ServiceVolumeReference,
    pub source: VolumeSource,
}

/// A container mount that refers to a declared Service Volume by its local name.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceMount {
    /// Service-local Volume Reference to mount.
    pub volume: ServiceVolumeReference,
    /// Absolute path inside the container.
    pub target: ContainerPath,
    /// Mount the source read-only.
    #[serde(default)]
    pub read_only: bool,
    /// Disable Docker's initial copy into a named Volume for this mount.
    #[serde(default)]
    pub no_copy: bool,
    /// Mount only this Volume subdirectory.
    #[serde(default)]
    pub subpath: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum VolumeSource {
    Bind {
        machine_path: MachinePath,
        #[serde(default)]
        create_machine_path: bool,
        #[serde(default)]
        propagation: Option<BindPropagation>,
        #[serde(default)]
        recursive: Option<BindRecursive>,
    },
    /// A Docker Volume managed outside Ployz.
    External { name: DockerVolumeName },
    /// An ordinary Docker Volume managed by Ployz.
    Ordinary {
        name: DockerVolumeName,
        driver: VolumeDriver,
        #[serde(default)]
        labels: BTreeMap<String, String>,
    },
    /// A bounded Docker Volume backed by a Machine Pool.
    Provisioned {
        /// Machine-local Docker Volume name after Project scoping.
        name: DockerVolumeName,
        /// Required positive storage maximum.
        maximum_bytes: ProvisionedVolumeMaximumBytes,
        /// Labels applied when the Docker Volume is created.
        #[serde(default)]
        labels: BTreeMap<String, String>,
    },
    Tmpfs {
        #[serde(default)]
        size_bytes: Option<u64>,
        #[serde(default)]
        mode: Option<u32>,
        #[serde(default)]
        options: Vec<Vec<String>>,
    },
}

impl VolumeSource {
    /// Borrow the machine-local Docker Volume name for a named source.
    ///
    /// Bind Mounts and Tmpfs Mounts do not address Docker Volumes and return `None`.
    #[must_use]
    pub fn docker_volume_name(&self) -> Option<&DockerVolumeName> {
        match self {
            Self::External { name }
            | Self::Ordinary { name, .. }
            | Self::Provisioned { name, .. } => Some(name),
            Self::Bind { .. } | Self::Tmpfs { .. } => None,
        }
    }

    /// Bind an ordinary or Provisioned Volume to `project`: physical name and ownership labels.
    pub fn scope_to_project(&mut self, project: &ProjectName) {
        let (name, labels) = match self {
            Self::Ordinary { name, labels, .. } | Self::Provisioned { name, labels, .. } => {
                (name, labels)
            }
            Self::External { .. } | Self::Bind { .. } | Self::Tmpfs { .. } => return,
        };
        if labels.contains_key(PROJECT_NAME_LABEL) {
            // Already bound: scale from a Resolved Service Spec, or a volume that
            // already carries ownership. Do not prefix again or rewrite a foreign owner.
            return;
        }
        *name = project.volume_name(name);
        labels.insert(MANAGED_LABEL.into(), String::new());
        labels.insert(PROJECT_NAME_LABEL.into(), project.to_string());
    }

    /// Whether an observed Docker Volume exactly matches this managed source.
    ///
    /// Requested labels must match; extra observed labels are accepted. External
    /// Volumes, Bind Mounts, and Tmpfs are not managed shapes and return false.
    #[must_use]
    pub fn matches_managed_volume(&self, observed: &DockerVolume) -> bool {
        let Some(expected) = self.to_create_volume_request() else {
            return false;
        };
        observed.id.name == expected.name
            && observed.driver() == expected.driver
            && observed.options == expected.options
            && expected
                .labels
                .iter()
                .all(|(key, value)| observed.labels.get(key) == Some(value))
            && match (self, &observed.storage) {
                (Self::Ordinary { .. }, DockerVolumeStorageObservation::Plain { .. }) => true,
                (
                    Self::Provisioned { maximum_bytes, .. },
                    DockerVolumeStorageObservation::Provisioned { bound_bytes, .. },
                ) => bound_bytes.get() == maximum_bytes.get(),
                _ => false,
            }
    }
}

/// Reserved Docker driver used only by [`VolumeSource::Provisioned`].
pub const PROVISIONED_VOLUME_DRIVER: &str = "ployz";

/// A positive maximum byte count for one Provisioned Volume.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ProvisionedVolumeMaximumBytes(NonZeroU64);

impl ProvisionedVolumeMaximumBytes {
    /// Construct a Provisioned Volume bound from a positive byte count.
    #[must_use]
    pub const fn new(bytes: NonZeroU64) -> Self {
        Self(bytes)
    }

    /// The positive maximum byte count.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl Display for ProvisionedVolumeMaximumBytes {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Serialize for ProvisionedVolumeMaximumBytes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ProvisionedVolumeMaximumBytes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse::<NonZeroU64>()
            .map(Self)
            .map_err(D::Error::custom)
    }
}

/// A Docker Volume driver that cannot name Ployz's reserved Provisioned Volume driver.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VolumeDriver {
    name: String,
    #[serde(default)]
    options: BTreeMap<String, String>,
}

impl VolumeDriver {
    /// Parse an ordinary Docker Volume driver and its options.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] when `name` is reserved for Provisioned Volumes.
    pub fn parse(
        name: impl Into<String>,
        options: BTreeMap<String, String>,
    ) -> Result<Self, ValueError> {
        let name = name.into();
        if name == PROVISIONED_VOLUME_DRIVER {
            return Err(ValueError::new(
                "ordinary Docker Volume driver",
                name,
                "a driver other than the reserved 'ployz' driver",
            ));
        }
        Ok(Self { name, options })
    }

    /// Borrow the Docker driver name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Borrow the Docker driver options.
    #[must_use]
    pub fn options(&self) -> &BTreeMap<String, String> {
        &self.options
    }
}

impl<'de> Deserialize<'de> for VolumeDriver {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Data {
            name: String,
            #[serde(default)]
            options: BTreeMap<String, String>,
        }

        let driver = Data::deserialize(deserializer)?;
        Self::parse(driver.name, driver.options).map_err(D::Error::custom)
    }
}

/// Current storage evidence for one observed Docker Volume.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DockerVolumeStorageObservation {
    /// A Docker Volume without a Ployz-managed byte bound.
    Plain {
        /// Docker driver reported for the ordinary Volume.
        driver: String,
    },
    /// A Provisioned Volume observed through the Ployz Docker driver.
    Provisioned {
        /// Current ZFS dataset mountpoint.
        mountpoint: MachinePath,
        /// Current ZFS dataset byte bound.
        bound_bytes: NonZeroU64,
        /// Current referenced ZFS dataset bytes.
        used_bytes: u64,
    },
}

/// One Docker Volume observed on one Machine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DockerVolume {
    pub id: DockerVolumeId,
    #[serde(default)]
    pub options: BTreeMap<String, String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// Current storage kind and Provisioned Volume usage evidence.
    pub storage: DockerVolumeStorageObservation,
}

impl DockerVolume {
    /// Docker driver implied by the observed storage kind.
    #[must_use]
    pub fn driver(&self) -> &str {
        match &self.storage {
            DockerVolumeStorageObservation::Plain { driver } => driver,
            DockerVolumeStorageObservation::Provisioned { .. } => PROVISIONED_VOLUME_DRIVER,
        }
    }
}

/// Destroy these Docker Volumes. The list is the confirmation.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoveVolumesRequest {
    pub volumes: Vec<DockerVolumeId>,
    /// Force-remove an in-use Docker Volume. Defaults to false.
    #[serde(default)]
    pub force: bool,
}

/// The outcome of attempting to remove one Machine-local Docker Volume.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VolumeRemoval {
    pub id: DockerVolumeId,
    pub outcome: VolumeRemovalOutcome,
}

/// Evidence from one bounded removal attempt, never an atomicity guarantee.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum VolumeRemovalOutcome {
    /// Removed or already absent.
    Removed,
    /// The request failed. A transport error or timeout leaves completion unknown.
    Failed { error: crate::RpcError },
    /// Not attempted because the Machine was absent or did not invite RPC.
    Omitted,
}
