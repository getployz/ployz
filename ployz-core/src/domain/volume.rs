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
pub enum RawVolumeSource {
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
        /// Logical declaration name; immutable scoped views expose the physical name.
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

/// A source admitted from a raw declaration, or retained from a resolved observation.
/// Its scoping state is private and cannot be asserted by a user-supplied label.
/// Scoped observations serialize only through [`ResolvedVolumeSource`], never as raw input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VolumeSource {
    source: RawVolumeSource,
    scope: Option<ScopedVolumeSource>,
}

/// Checked Project and logical identity from which a physical name and owner labels derive.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopedVolumeSource {
    project: ProjectName,
    logical_name: DockerVolumeName,
}

impl ScopedVolumeSource {
    fn physical_name(&self) -> DockerVolumeName {
        self.project.volume_name(&self.logical_name)
    }
}

impl TryFrom<RawVolumeSource> for VolumeSource {
    type Error = ValueError;
    fn try_from(source: RawVolumeSource) -> Result<Self, Self::Error> {
        if let RawVolumeSource::Ordinary { labels, .. }
        | RawVolumeSource::Provisioned { labels, .. } = &source
            && let Some(key) = labels
                .keys()
                .find(|key| key.as_str() == MANAGED_LABEL || key.as_str() == PROJECT_NAME_LABEL)
        {
            return Err(ValueError::new(
                "volume label",
                key.clone(),
                "a non-reserved user label",
            ));
        }
        Ok(Self {
            source,
            scope: None,
        })
    }
}

impl Serialize for VolumeSource {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.scope.is_some() {
            return Err(serde::ser::Error::custom(
                "scoped observations cannot be serialized as raw volume declarations",
            ));
        }
        self.source.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for VolumeSource {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Request {
            #[serde(flatten)]
            source: RawVolumeSource,
            #[serde(default, rename = "scope", deserialize_with = "reject_scope")]
            _scope: (),
        }
        fn reject_scope<'de, D: Deserializer<'de>>(_: D) -> Result<(), D::Error> {
            Err(D::Error::custom(
                "raw volume declarations cannot assert observed scope",
            ))
        }
        Self::try_from(Request::deserialize(deserializer)?.source).map_err(D::Error::custom)
    }
}

impl VolumeSource {
    /// Read the source's current physical shape without granting mutation of provenance.
    #[must_use]
    pub fn kind(&self) -> &RawVolumeSource {
        &self.source
    }

    #[must_use]
    pub fn docker_volume_name(&self) -> Option<&DockerVolumeName> {
        match self.kind() {
            RawVolumeSource::External { name }
            | RawVolumeSource::Ordinary { name, .. }
            | RawVolumeSource::Provisioned { name, .. } => Some(name),
            RawVolumeSource::Bind { .. } | RawVolumeSource::Tmpfs { .. } => None,
        }
    }

    /// Scope admitted declarations once; preserve imported observations' exact identity.
    pub fn scope_to_project(&mut self, project: &ProjectName) {
        if self.scope.is_some() {
            return;
        }
        let name = match &mut self.source {
            RawVolumeSource::Ordinary { name, .. } | RawVolumeSource::Provisioned { name, .. } => {
                name
            }
            RawVolumeSource::External { .. }
            | RawVolumeSource::Bind { .. }
            | RawVolumeSource::Tmpfs { .. } => return,
        };
        let logical_name = name.clone();
        *name = project.volume_name(&logical_name);
        self.scope = Some(ScopedVolumeSource {
            project: project.clone(),
            logical_name,
        });
    }

    pub(crate) fn is_resolved(&self) -> bool {
        !matches!(
            self.source,
            RawVolumeSource::Ordinary { .. } | RawVolumeSource::Provisioned { .. }
        ) || self.scope.is_some()
    }

    /// Ownership labels are derived, never taken from user declarations.
    #[must_use]
    pub fn creation_labels(&self) -> BTreeMap<String, String> {
        let mut labels = match self.kind() {
            RawVolumeSource::Ordinary { labels, .. }
            | RawVolumeSource::Provisioned { labels, .. } => labels.clone(),
            RawVolumeSource::External { .. }
            | RawVolumeSource::Bind { .. }
            | RawVolumeSource::Tmpfs { .. } => return BTreeMap::new(),
        };
        if let Some(scope) = &self.scope {
            labels.insert(MANAGED_LABEL.into(), String::new());
            labels.insert(PROJECT_NAME_LABEL.into(), scope.project.to_string());
        }
        labels
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
            && match (self.kind(), &observed.storage) {
                (
                    RawVolumeSource::Ordinary { .. },
                    DockerVolumeStorageObservation::Plain { .. },
                ) => true,
                (
                    RawVolumeSource::Provisioned { maximum_bytes, .. },
                    DockerVolumeStorageObservation::Provisioned { bound_bytes, .. },
                ) => bound_bytes.get() == maximum_bytes.get(),
                _ => false,
            }
    }
}

/// Reserved Docker driver used only by [`RawVolumeSource::Provisioned`].
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

/// Wire source in a Resolved Service Spec. Import checks physical identity correspondence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    try_from = "ResolvedVolumeSourceWire",
    into = "ResolvedVolumeSourceWire"
)]
pub struct ResolvedVolumeSource(VolumeSource);

#[derive(Serialize, Deserialize)]
struct ResolvedVolumeSourceWire {
    #[serde(flatten)]
    source: RawVolumeSource,
    #[serde(default)]
    scope: Option<ScopedVolumeSource>,
}

impl TryFrom<ResolvedVolumeSourceWire> for ResolvedVolumeSource {
    type Error = ValueError;
    fn try_from(wire: ResolvedVolumeSourceWire) -> Result<Self, Self::Error> {
        let mut source = VolumeSource::try_from(wire.source)?;
        if let Some(scope) = &wire.scope
            && (!matches!(
                source.kind(),
                RawVolumeSource::Ordinary { .. } | RawVolumeSource::Provisioned { .. }
            ) || source.docker_volume_name() != Some(&scope.physical_name()))
        {
            return Err(ValueError::new(
                "resolved volume",
                "scope",
                "ownership matching the managed physical source",
            ));
        }
        source.scope = wire.scope;
        Self::try_from(source)
    }
}
impl From<ResolvedVolumeSource> for ResolvedVolumeSourceWire {
    fn from(value: ResolvedVolumeSource) -> Self {
        Self {
            source: value.0.source,
            scope: value.0.scope,
        }
    }
}
impl TryFrom<VolumeSource> for ResolvedVolumeSource {
    type Error = ValueError;
    fn try_from(source: VolumeSource) -> Result<Self, Self::Error> {
        if !source.is_resolved() {
            return Err(ValueError::new(
                "resolved volume",
                "unscoped",
                "a Project-scoped managed source",
            ));
        }
        Ok(Self(source))
    }
}
impl ResolvedVolumeSource {
    /// Consume this source while retaining its Project ownership.
    #[must_use]
    pub fn into_requested(self) -> VolumeSource {
        self.0
    }
}

impl RawVolumeSource {
    /// Admit a raw source, rejecting reserved ownership labels.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] if user labels assert Ployz ownership.
    pub fn admit(self) -> Result<VolumeSource, ValueError> {
        self.try_into()
    }
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
