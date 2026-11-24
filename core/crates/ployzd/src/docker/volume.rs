use ployz_core::labels;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use bollard::{
    Docker,
    models::{Mount, MountType, MountVolumeOptionsDriverConfig, Volume, VolumeCreateRequest},
    query_parameters::{ListContainersOptionsBuilder, RemoveVolumeOptionsBuilder},
};
use futures_util::{StreamExt, stream};
use ployz_core::{
    ContainerId, CreateVolumeRequest, DockerVolume, DockerVolumeId, DockerVolumeName,
    DockerVolumeStorageObservation, MachineId, Namespace, QualifiedService, ResolvedServiceSpec,
    RpcError, ServiceName, VolumeInventory, VolumeObservationFailure, VolumeSource,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Error, LocalDocker, Status, classify, some_map};
use crate::VolumePluginStatus;

const VOLUME_INSPECTION_CONCURRENCY: usize = 8;

/// Successful Docker mutation followed by its independent verification outcome.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verification", rename_all = "snake_case")]
pub enum CreateVolumeReport {
    Verified { volume: DockerVolume },
    Unverified { id: DockerVolumeId, error: RpcError },
}

impl CreateVolumeReport {
    /// Return the verified observation or the created identity and verification failure.
    ///
    /// # Errors
    ///
    /// Returns the created identity and verification error when creation succeeded but
    /// its follow-up observation failed.
    #[expect(
        clippy::result_large_err,
        reason = "both outcomes are public wire values; boxing would leak allocation into the contract"
    )]
    pub fn into_observation(self) -> Result<DockerVolume, VolumeObservationFailure> {
        match self {
            Self::Verified { volume } => Ok(volume),
            Self::Unverified { id, error } => Err(VolumeObservationFailure { id, error }),
        }
    }
}

/// Docker Volumes on this Machine: mounting, creating, listing, inspecting and removing.
#[derive(Clone)]
pub struct Volumes {
    docker: LocalDocker,
}

impl Volumes {
    pub(super) fn new(docker: LocalDocker) -> Self {
        Self { docker }
    }

    /// Ensure every mounted Docker Volume exists and every managed source matches exactly.
    ///
    /// # Errors
    ///
    /// Returns when an external Volume is absent or a managed Volume cannot be created or
    /// verified without mutation.
    pub(super) async fn ensure_mounted_volumes(
        &self,
        machine_id: &MachineId,
        spec: &ResolvedServiceSpec,
    ) -> Result<(), Error> {
        let mut mounted = BTreeMap::<&DockerVolumeName, &VolumeSource>::new();
        for volume in spec.volume_graph().mounted_volumes() {
            let source = &volume.source;
            let Some(name) = source.docker_volume_name() else {
                continue;
            };
            mounted.entry(name).or_insert(source);
        }
        for source in mounted.values() {
            self.ensure_volume_source(machine_id, source).await?;
        }
        Ok(())
    }

    pub(crate) async fn ensure_volume_source(
        &self,
        machine_id: &MachineId,
        source: &VolumeSource,
    ) -> Result<(), Error> {
        let Some(name) = source.docker_volume_name() else {
            return Ok(());
        };
        if matches!(source.kind(), ployz_core::RawVolumeSource::External { .. }) {
            return match ensure_volume_exists(&self.docker.client, name.as_str()).await {
                Err(error) if volume_not_found(&error) => {
                    Err(Error::ExternalVolumeNotFound(name.clone()))
                }
                result => result,
            };
        }

        match self.inspect_volume(machine_id, name).await {
            Ok(volume) => verify_volume(source, &volume),
            Err(error) if volume_not_found(&error) => {
                let request = source
                    .to_create_volume_request()
                    .expect("managed Docker Volume sources have creation requests");
                match self.create_volume(machine_id, request).await {
                    Ok(CreateVolumeReport::Verified { volume }) => verify_volume(source, &volume),
                    Ok(CreateVolumeReport::Unverified { id, error }) => {
                        Err(Error::VolumeCreatedButUnverified {
                            id,
                            error: Box::new(error),
                        })
                    }
                    Err(error) if volume_conflict(&error) => {
                        verify_volume(source, &self.inspect_volume(machine_id, name).await?)
                    }
                    Err(error) if relays(&error, crate::volume_copy::DOCKER_NOT_ANSWERING) => {
                        Err(Error::VolumeHoldersUnlisted(name.clone()))
                    }
                    Err(error) if relays(&error, crate::volume_copy::HELD_FOR_OTHER_DATA) => {
                        Err(Error::ForeignVolume {
                            name: name.clone(),
                            found: format!(
                                "{}, so no new data is made under its name until that Container is removed",
                                crate::volume_copy::HELD_FOR_OTHER_DATA
                            ),
                        })
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    /// Refuses unless each mounted managed Volume carries the origin `spec` expects of it:
    /// an Ordinary one by its `ployz.origin` label, a Provisioned one by its dataset, which
    /// the plugin reads. A Volume that carries none is Legacy, which only a mount with no
    /// birth expects.
    ///
    /// # Errors
    /// Returns [`Error::ForeignVolume`] naming the first Volume that differs, or the error
    /// that kept its origin unread.
    pub(super) async fn require_origins(
        &self,
        machine_id: &MachineId,
        spec: &ResolvedServiceSpec,
        plugin: &crate::storage::Plugin,
    ) -> Result<(), Error> {
        let mut checked = BTreeSet::new();
        for volume in spec.volume_graph().mounted_volumes() {
            let source = &volume.source;
            let Some(name) = source.docker_volume_name() else {
                continue;
            };
            if !checked.insert(name) {
                continue;
            }
            let carried = match source.kind() {
                ployz_core::RawVolumeSource::Ordinary { .. } => {
                    let observed = self.inspect_volume(machine_id, name).await?;
                    match observed.labels.get(labels::VOLUME_ORIGIN) {
                        None => None,
                        Some(label) => {
                            Some(ployz_core::VolumeOrigin::parse(label).map_err(|_| {
                                Error::ForeignVolume {
                                    name: name.clone(),
                                    found: format!("has an unreadable origin label {label:?}"),
                                }
                            })?)
                        }
                    }
                }
                ployz_core::RawVolumeSource::Provisioned { .. } => {
                    provisioned_origin(plugin, name).await?
                }
                ployz_core::RawVolumeSource::External { .. }
                | ployz_core::RawVolumeSource::Bind { .. }
                | ployz_core::RawVolumeSource::Tmpfs { .. } => continue,
            };
            let expected =
                ployz_core::VolumeOrigin::expected(source.birth(), name).map_err(|error| {
                    Error::InvalidContainerConfig(format!("mount origins: {error}"))
                })?;
            let carried = carried.unwrap_or_else(|| ployz_core::VolumeOrigin::legacy(name));
            if carried != expected {
                return Err(other_data(name));
            }
        }
        Ok(())
    }

    /// Refuse an External mount of a Volume Ployz made, early, before an image pull.
    /// [`Self::pin_external_drivers`] repeats it where it counts, right before Docker's create.
    ///
    /// # Errors
    /// Returns the refusal of [`external_driver`] naming the Volume, or the inspection's error.
    pub(crate) async fn refuse_managed_external(
        &self,
        machine_id: &MachineId,
        spec: &ResolvedServiceSpec,
    ) -> Result<(), Error> {
        for volume in spec.volume_graph().mounted_volumes() {
            let ployz_core::RawVolumeSource::External { name } = volume.source.kind() else {
                continue;
            };
            // A missing one is the mount check's to name, after this.
            match self.inspect_volume(machine_id, name).await {
                Ok(observed) => external_driver(name, &observed).map(drop)?,
                Err(error) if volume_not_found(&error) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Names in each External mount the driver Docker has for its Volume now, refusing one
    /// Ployz made. Docker searches every driver for a mount that names none, and the volume
    /// plugin answers for a root whose labels Docker lost, so an unpinned External mount
    /// could open Provisioned data. A pinned one opens only the driver named here, at create
    /// and at every later start, or Docker refuses the name.
    ///
    /// # Errors
    /// Returns [`Error::ExternalVolumeNotFound`], the refusal of [`external_driver`], or the
    /// inspection's error.
    pub(super) async fn pin_external_drivers(
        &self,
        machine_id: &MachineId,
        spec: &ResolvedServiceSpec,
        mounts: &mut [Mount],
    ) -> Result<(), Error> {
        let mut pinned = BTreeSet::new();
        for volume in spec.volume_graph().mounted_volumes() {
            let ployz_core::RawVolumeSource::External { name } = volume.source.kind() else {
                continue;
            };
            if !pinned.insert(name) {
                continue;
            }
            let observed = match self.inspect_volume(machine_id, name).await {
                Ok(observed) => observed,
                Err(error) if volume_not_found(&error) => {
                    return Err(Error::ExternalVolumeNotFound(name.clone()));
                }
                Err(error) => return Err(error),
            };
            let driver = external_driver(name, &observed)?;
            for mount in mounts.iter_mut().filter(|mount| {
                mount.typ == Some(MountType::VOLUME)
                    && mount.source.as_deref() == Some(name.as_str())
            }) {
                mount.volume_options.get_or_insert_default().driver_config =
                    Some(MountVolumeOptionsDriverConfig {
                        name: Some(driver.to_owned()),
                        options: None,
                    });
            }
        }
        Ok(())
    }

    /// Validate every existing provisioned Volume before allocating any new storage.
    pub(crate) async fn validate_provisioned_volumes(
        &self,
        machine_id: &MachineId,
        specs: &[ployz_core::ServiceStorageSpec],
    ) -> Result<(), Error> {
        let mut definitions = BTreeMap::new();
        for spec in specs {
            for volume in spec.volume_graph().mounted_provisioned_volumes() {
                let name = volume
                    .source
                    .docker_volume_name()
                    .expect("provisioned Volume has a name");
                if let Some(existing) = definitions.insert(name, &volume.source) {
                    if existing != &volume.source {
                        return Err(Error::VolumeShapeMismatch {
                            name: name.clone(),
                            reason: "batch contains conflicting definitions".into(),
                        });
                    }
                    continue;
                }
                match self.inspect_volume(machine_id, name).await {
                    Ok(existing) => verify_volume(&volume.source, &existing)?,
                    Err(error) if volume_not_found(&error) => {}
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(())
    }

    /// Finish Docker metadata for storage already secured by the volume plugin.
    pub(crate) async fn ensure_provisioned_volumes(
        &self,
        machine_id: &MachineId,
        specs: &[ployz_core::ServiceStorageSpec],
    ) -> Result<(), Error> {
        for spec in specs {
            for volume in spec.volume_graph().mounted_provisioned_volumes() {
                self.ensure_volume_source(machine_id, &volume.source)
                    .await?;
            }
        }
        Ok(())
    }

    /// Create a Docker Volume and verify its observable state when possible.
    ///
    /// # Errors
    ///
    /// Returns an error when Docker rejects the create request. A successful create whose
    /// verification fails is returned as [`CreateVolumeReport::Unverified`].
    pub async fn create_volume(
        &self,
        machine_id: &MachineId,
        request: CreateVolumeRequest,
    ) -> Result<CreateVolumeReport, Error> {
        let docker_name = request.name.to_string();
        let id = DockerVolumeId {
            machine_id: *machine_id,
            name: request.name,
        };
        decode_volume(
            self.docker
                .client
                .create_volume(VolumeCreateRequest {
                    name: Some(docker_name),
                    driver: Some(request.driver),
                    driver_opts: some_map(request.options),
                    labels: some_map(request.labels),
                    ..Default::default()
                })
                .await,
        )?;
        Ok(match self.inspect_volume(machine_id, &id.name).await {
            Ok(volume) => CreateVolumeReport::Verified { volume },
            Err(error) => CreateVolumeReport::Unverified {
                id,
                error: (&error).into(),
            },
        })
    }

    /// List Docker Volumes, retaining individual detail failures alongside healthy observations.
    ///
    /// # Errors
    ///
    /// Returns an error when Docker rejects the collection request or listed inventory is invalid.
    pub async fn list_volumes(&self, machine_id: &MachineId) -> Result<VolumeInventory, Error> {
        let listed = decode_volume_list(
            self.docker
                .client
                .list_volumes(None::<bollard::query_parameters::ListVolumesOptions>)
                .await,
        )?;
        let warnings = listed.warnings.unwrap_or_default();
        // Every Get after a failed ployz List hits the same broken plugin, and Docker retries
        // each hang-up for about 15 seconds, overrunning the caller's read timeout.
        let ployz_list_failure = warnings
            .iter()
            .find_map(|warning| warning.strip_prefix("list ployz: "));
        let mut inventory = VolumeInventory::default();
        let volumes = listed.volumes.unwrap_or_default();
        let mut observations = stream::iter(volumes.into_iter().map(|volume| async move {
            let id = docker_volume_id(machine_id, &volume.name)?;
            let observation = if volume.driver == "ployz" {
                match ployz_list_failure {
                    Some(message) => Err(Error::VolumePluginListFailed(message.to_owned())),
                    None => self.inspect_volume(machine_id, &id.name).await,
                }
            } else {
                docker_volume(machine_id, volume)
            };
            Ok::<_, Error>((id, observation))
        }))
        .buffered(VOLUME_INSPECTION_CONCURRENCY);
        while let Some(observation) = observations.next().await {
            let (id, observation) = observation?;
            match observation {
                Ok(volume) => inventory.volumes.push(volume),
                Err(error) => inventory.failures.push(VolumeObservationFailure {
                    id,
                    error: (&error).into(),
                }),
            }
        }
        Ok(inventory)
    }

    /// Observe one named Docker Volume directly.
    ///
    /// # Errors
    ///
    /// Returns an error when Docker inspection or decoding fails, or when Docker
    /// returns a different Volume name than requested.
    pub async fn inspect_volume(
        &self,
        machine_id: &MachineId,
        name: &DockerVolumeName,
    ) -> Result<DockerVolume, Error> {
        let volume = decode_volume(self.docker.client.inspect_volume(name.as_str()).await)?;
        if volume.name != name.as_str() {
            return Err(Error::UnexpectedVolumeName {
                requested: name.clone(),
                actual: volume.name,
            });
        }
        docker_volume(machine_id, volume)
    }

    /// Drop Docker's record of a Volume the plugin no longer serves. A name Docker
    /// never had is already forgotten.
    ///
    /// # Errors
    ///
    /// Returns when Docker keeps the name, including when a Container still mounts it.
    pub async fn forget_volume(&self, name: &DockerVolumeName) -> Result<(), Error> {
        match self.remove_volume(name, false).await {
            Err(error) if volume_not_found(&error) => Ok(()),
            result => result,
        }
    }

    /// Remove a named Docker Volume.
    ///
    /// `force` is Docker's force flag: it does not evict holders. An in-use
    /// volume still fails, and the error names labeled Services when they can
    /// be observed.
    ///
    /// # Errors
    ///
    /// Returns when Docker rejects the deletion, including when containers
    /// still mount the volume.
    pub async fn remove_volume(&self, name: &DockerVolumeName, force: bool) -> Result<(), Error> {
        match self
            .docker
            .client
            .remove_volume(
                name.as_str(),
                Some(RemoveVolumeOptionsBuilder::default().force(force).build()),
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(error) => {
                let mapped = Error::from(error);
                if !volume_conflict(&mapped) {
                    return Err(mapped);
                }
                match self.volume_holders(name).await {
                    Ok(holders) if !holders.is_empty() => Err(volume_in_use(holders)),
                    _ => Err(mapped),
                }
            }
        }
    }

    /// Refuse a new Service Container on Docker Volume `name` while another Container
    /// runs on it. See [`super::holder::refuse_beside`].
    ///
    /// # Errors
    /// [`Error::VolumeHeld`] naming the running holder, or the error of a read that failed.
    pub(crate) async fn refuse_beside(&self, name: &str) -> Result<(), Error> {
        super::holder::refuse_beside(&self.docker.client, None, name).await
    }

    /// The Docker name of a Container, other than `me` when given, that runs on the
    /// Docker Volume `name`, whatever made it, or `None`.
    pub(crate) async fn running_beside(
        &self,
        name: &DockerVolumeName,
        me: Option<&ContainerId>,
    ) -> Result<Option<String>, Error> {
        super::holder::running_beside(
            &self.docker.client,
            me.map(ContainerId::as_str),
            name.as_str(),
        )
        .await
    }

    /// Every Container, running or not, that mounts the Docker Volume `name`.
    pub(crate) async fn volume_holder_ids(
        &self,
        name: &DockerVolumeName,
    ) -> Result<Vec<ContainerId>, Error> {
        let filters = HashMap::from([("volume", vec![name.as_str()])]);
        self.docker
            .client
            .list_containers(Some(
                ListContainersOptionsBuilder::default()
                    .all(true)
                    .filters(&filters)
                    .build(),
            ))
            .await?
            .into_iter()
            .filter_map(|container| container.id)
            .map(|id| {
                ContainerId::parse(id).map_err(|source| Error::InvalidValue {
                    field: "container ID",
                    source,
                })
            })
            .collect()
    }

    async fn volume_holders(
        &self,
        name: &DockerVolumeName,
    ) -> Result<Vec<Option<QualifiedService>>, Error> {
        let filters = HashMap::from([("volume", vec![name.as_str()])]);
        let holders = self
            .docker
            .client
            .list_containers(Some(
                ListContainersOptionsBuilder::default()
                    .all(true)
                    .filters(&filters)
                    .build(),
            ))
            .await?
            .into_iter()
            .map(|container| holder_service(container.labels.as_ref()))
            .collect();
        Ok(holders)
    }
}

fn holder_service(labels: Option<&HashMap<String, String>>) -> Option<QualifiedService> {
    let labels = labels?;
    let namespace = Namespace::parse(labels.get(labels::NAMESPACE)?).ok()?;
    let name = ServiceName::parse(labels.get(labels::SERVICE_NAME)?).ok()?;
    Some(QualifiedService::new(namespace, name))
}

fn volume_in_use(holders: Vec<Option<QualifiedService>>) -> Error {
    let count = holders.len();
    let named = holders
        .into_iter()
        .flatten()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let containers = if count == 1 {
        "1 container".to_owned()
    } else {
        format!("{count} containers")
    };
    let names = named
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let message = if named.is_empty() {
        format!("volume is in use ({containers})")
    } else {
        format!("volume is in use by {names} ({containers})")
    };
    Error::VolumeInUse {
        message,
        services: named,
    }
}

/// The origin a Provisioned Volume's writable root carries, `None` for Legacy. A name with
/// no root here has no data a Container could open, so it is refused as well.
async fn provisioned_origin(
    plugin: &crate::storage::Plugin,
    name: &DockerVolumeName,
) -> Result<Option<ployz_core::VolumeOrigin>, Error> {
    let copy = plugin
        .call::<Option<ployz_core::VolumeCopy>>(
            crate::volume_copy::INSPECT_ROUTE,
            &crate::volume_copy::CopyRequest { name: name.clone() },
        )
        .await
        .map_err(|source| Error::VolumeOriginUnread {
            name: name.clone(),
            source: Box::new(source),
        })?;
    match copy {
        Some(ployz_core::VolumeCopy::Root { origin, .. }) => Ok(origin),
        Some(ployz_core::VolumeCopy::Slot { .. }) | None => {
            Err(Error::VolumeNotWritable(name.clone()))
        }
    }
}

/// The driver an External mount may open `observed` through. External means made outside
/// Ployz. Under Device enforcement the only named Volumes an Operator can make come from
/// `PrepareVolumes`, which labels them, so a labelled one may hold options nobody approved.
/// One the Provisioned plugin serves holds Ployz's own data whatever labels Docker kept, and
/// only a Provisioned mount, whose origin the plugin checks, opens it.
fn external_driver<'a>(
    name: &DockerVolumeName,
    observed: &'a DockerVolume,
) -> Result<&'a str, Error> {
    if observed.labels.contains_key(ployz_core::labels::MANAGED) {
        return Err(Error::VolumeShapeMismatch {
            name: name.clone(),
            reason: "an External mount names a Volume Ployz manages".into(),
        });
    }
    match observed.driver() {
        ployz_core::PROVISIONED_VOLUME_DRIVER => Err(Error::ForeignVolume {
            name: name.clone(),
            found: "is a Ployz Provisioned Volume, and an External mount opens only a Volume made outside Ployz".to_owned(),
        }),
        driver => Ok(driver),
    }
}

pub(super) fn other_data(name: &DockerVolumeName) -> Error {
    Error::ForeignVolume {
        name: name.clone(),
        found: "holds another Volume's data under the same name, not the data this Service was approved for".to_owned(),
    }
}

/// Whether Docker refused a create with the volume plugin's `words`, which Docker relays
/// only as text.
fn relays(error: &Error, words: &str) -> bool {
    matches!(
        error,
        Error::Docker(bollard::errors::Error::DockerResponseServerError { message, .. })
            if message.contains(words)
    )
}

fn verify_volume(source: &VolumeSource, observed: &DockerVolume) -> Result<(), Error> {
    let expected = source
        .to_create_volume_request()
        .expect("only managed Docker Volume sources are verified");
    // An Ordinary Volume's origin is its label, so one that carries another origin than a
    // born source names holds other data, which is no shape problem.
    if matches!(source.kind(), ployz_core::RawVolumeSource::Ordinary { .. })
        && let Some(origin) = expected.labels.get(labels::VOLUME_ORIGIN)
        && observed.labels.get(labels::VOLUME_ORIGIN) != Some(origin)
    {
        return Err(other_data(&expected.name));
    }
    if !source.matches_managed_volume(observed) {
        return Err(Error::VolumeShapeMismatch {
            name: expected.name,
            reason: "requested and observed managed shapes differ".into(),
        });
    }
    Ok(())
}

fn volume_not_found(error: &Error) -> bool {
    volume_status(error) == Status::Absent
}

fn volume_conflict(error: &Error) -> bool {
    volume_status(error) == Status::Conflict
}

fn volume_status(error: &Error) -> Status {
    if let Error::Docker(error) = error {
        classify(error)
    } else {
        Status::Other
    }
}

// Bollard models Volume Status as key-only data, so valid plugin values use raw JSON recovery.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawVolume {
    name: String,
    driver: String,
    #[serde(default)]
    mountpoint: String,
    status: Option<Value>,
    options: Option<HashMap<String, String>>,
    labels: Option<HashMap<String, String>>,
}

impl From<Volume> for RawVolume {
    fn from(volume: Volume) -> Self {
        Self {
            name: volume.name,
            driver: volume.driver,
            mountpoint: volume.mountpoint,
            status: None,
            options: Some(volume.options),
            labels: Some(volume.labels),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawVolumeList {
    volumes: Option<Vec<RawVolume>>,
    warnings: Option<Vec<String>>,
}

fn decode_volume(result: Result<Volume, bollard::errors::Error>) -> Result<RawVolume, Error> {
    match result {
        Ok(volume) => Ok(volume.into()),
        Err(bollard::errors::Error::JsonDataError { contents, .. }) => {
            Ok(serde_json::from_str(&contents)?)
        }
        Err(error) => Err(error.into()),
    }
}

/// Confirm that a named Docker Volume exists and is decodable.
///
/// # Errors
///
/// Returns an error when Docker rejects the inspection or its response cannot be decoded.
pub(super) async fn ensure_volume_exists(docker: &Docker, name: &str) -> Result<(), Error> {
    decode_volume(docker.inspect_volume(name).await).map(drop)
}

fn decode_volume_list(
    result: Result<bollard::models::VolumeListResponse, bollard::errors::Error>,
) -> Result<RawVolumeList, Error> {
    match result {
        Ok(response) => Ok(RawVolumeList {
            volumes: response
                .volumes
                .map(|volumes| volumes.into_iter().map(Into::into).collect()),
            warnings: response.warnings,
        }),
        Err(bollard::errors::Error::JsonDataError { contents, .. }) => {
            Ok(serde_json::from_str(&contents)?)
        }
        Err(error) => Err(error.into()),
    }
}

fn docker_volume(machine_id: &MachineId, volume: RawVolume) -> Result<DockerVolume, Error> {
    let storage = if volume.driver == "ployz" {
        let status = volume
            .status
            .ok_or_else(|| Error::InvalidVolumeStatus("Provisioned Volume status is missing"))?;
        let status = serde_json::from_value::<VolumePluginStatus>(status)
            .map_err(|_| Error::InvalidVolumeStatus("Provisioned Volume status is malformed"))?;
        let bound_bytes = std::num::NonZeroU64::new(status.bound_bytes).ok_or_else(|| {
            Error::InvalidVolumeStatus("Provisioned Volume status has no positive bound_bytes")
        })?;
        let mountpoint = ployz_core::MachinePath::parse(volume.mountpoint).map_err(|_| {
            Error::InvalidVolumeStatus("Provisioned Volume status has an invalid mountpoint")
        })?;
        DockerVolumeStorageObservation::provisioned(
            mountpoint,
            bound_bytes,
            status.used_bytes,
            None,
        )
    } else {
        DockerVolumeStorageObservation::Plain {
            driver: volume.driver,
        }
    };
    Ok(DockerVolume {
        id: docker_volume_id(machine_id, &volume.name)?,
        options: volume.options.unwrap_or_default().into_iter().collect(),
        labels: volume.labels.unwrap_or_default().into_iter().collect(),
        storage,
    })
}

fn docker_volume_id(machine_id: &MachineId, name: &str) -> Result<DockerVolumeId, Error> {
    Ok(DockerVolumeId {
        machine_id: *machine_id,
        name: DockerVolumeName::parse(name).map_err(|source| Error::InvalidValue {
            field: "Docker Volume name",
            source,
        })?,
    })
}

#[cfg(test)]
mod tests;
