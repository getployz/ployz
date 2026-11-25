//! Kept Volumes: the anonymous Docker Volumes a removal left behind because no copy took
//! the removed Container's place, recorded so this Machine can list and remove them.
//!
//! The record is the only proof that Ployz kept a Volume. A removal here touches only a
//! Volume this Machine recorded, and only while no Container mounts it: checked under the
//! config lock, which every Container create and removal takes, so no create can mount it
//! between the check and Docker's removal. Docker refuses to remove a Volume any Container
//! mounts as well, which covers a Container made outside Ployz.

use std::{
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use ployz_core::{
    ContainerId, ContainerPath, DockerVolumeName, KeptVolume, KeptVolumeRemoval,
    KeptVolumeRemovalOutcome, labels::ServiceLabels,
};

use super::{
    Error, Lifecycle,
    error::{Status, classify},
    inspect::Inspected,
};

/// Docker's mark on a Volume it made for a Container's anonymous mount.
const ANONYMOUS_LABEL: &str = "com.docker.volume.anonymous";

/// How long a listing may spend measuring Kept Volumes before it reports the rest
/// unmeasured.
const MEASURE_BUDGET: Duration = Duration::from_secs(2);

const BLOCK_BYTES: u64 = 512;

impl Lifecycle {
    /// The anonymous Volumes `inspected` mounts, as Docker marked them when it made them,
    /// which removing `container_id` without its Volumes keeps. A Volume Docker can't read
    /// now, or one a Container that isn't a Service's mounts, is left out, so it is never
    /// recorded as kept.
    pub(super) async fn anonymous_volumes(
        &self,
        container_id: &ContainerId,
        inspected: &Inspected,
    ) -> Vec<KeptVolume> {
        let Some(identity) = inspected
            .labels()
            .and_then(|labels| ServiceLabels::parse(labels).ok())
            .map(|labels| labels.identity)
        else {
            return Vec::new();
        };
        let kept_at = chrono::Utc::now().timestamp();
        let mut kept = Vec::new();
        for mount in inspected.mounts.iter().flatten() {
            let (Some("volume"), Some(name), Some(path)) = (
                mount.typ.as_deref(),
                mount.name.as_deref(),
                mount.destination.as_deref(),
            ) else {
                continue;
            };
            let anonymous = match self.docker.client.inspect_volume(name).await {
                Ok(volume) => volume.labels.contains_key(ANONYMOUS_LABEL),
                Err(error) => {
                    tracing::warn!(
                        container = container_id.as_str(),
                        volume = name,
                        error = %ployz_core::error_chain::inline(&error),
                        "could not read a volume the removed container mounts; it is not recorded as kept"
                    );
                    false
                }
            };
            let (true, Ok(name), Ok(path)) = (
                anonymous,
                DockerVolumeName::parse(name),
                ContainerPath::parse(path),
            ) else {
                continue;
            };
            kept.push(KeptVolume {
                name,
                namespace: identity.namespace.clone(),
                service: identity.name.clone(),
                container: *container_id,
                path,
                kept_at,
                size_bytes: None,
            });
        }
        kept
    }

    /// This Machine's Kept Volumes that Docker still has, oldest first, each measured
    /// while [`MEASURE_BUDGET`] lasts.
    ///
    /// # Errors
    ///
    /// Returns when the record can't be read.
    pub async fn kept_volumes(&self) -> Result<Vec<KeptVolume>, Error> {
        let mut listed = Vec::new();
        let mut roots = Vec::new();
        for volume in self.specs.kept_volumes().await? {
            match self
                .docker
                .client
                .inspect_volume(volume.name.as_str())
                .await
            {
                Ok(found) => roots.push(Some(PathBuf::from(found.mountpoint))),
                Err(error) if classify(&error) == Status::Absent => continue,
                // Still recorded, so still listed; its size is unknown.
                Err(_) => roots.push(None),
            }
            listed.push(volume);
        }
        let sizes = tokio::task::spawn_blocking(move || {
            let until = Instant::now() + MEASURE_BUDGET;
            roots
                .iter()
                .map(|root| root.as_deref().and_then(|root| disk_bytes(root, until)))
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        for (volume, size) in listed.iter_mut().zip(sizes) {
            volume.size_bytes = size;
        }
        Ok(listed)
    }

    /// Removes each of `names` that this Machine recorded as kept and no Container,
    /// running or not, mounts. Never forced.
    ///
    /// # Errors
    ///
    /// Returns when the record can't be read or written, or Docker can't list who mounts
    /// a Volume. Results for the names before it stand.
    pub async fn remove_kept_volumes(
        &self,
        names: Vec<DockerVolumeName>,
    ) -> Result<Vec<KeptVolumeRemoval>, Error> {
        let mut results = Vec::with_capacity(names.len());
        for volume in names {
            let outcome = self.remove_kept_volume(&volume).await?;
            results.push(KeptVolumeRemoval { volume, outcome });
        }
        Ok(results)
    }

    async fn remove_kept_volume(
        &self,
        name: &DockerVolumeName,
    ) -> Result<KeptVolumeRemovalOutcome, Error> {
        let mut operation = self.specs.config_operation().await;
        if operation.kept_volume(name).await?.is_none() {
            return Ok(KeptVolumeRemovalOutcome::NotKept);
        }
        let holders = self.volumes.volume_holder_ids(name).await?;
        if !holders.is_empty() {
            return Ok(in_use(holders));
        }
        let outcome = match self
            .docker
            .client
            .remove_volume(
                name.as_str(),
                None::<bollard::query_parameters::RemoveVolumeOptions>,
            )
            .await
        {
            Ok(()) => KeptVolumeRemovalOutcome::Removed,
            Err(error) => match classify(&error) {
                Status::Absent => KeptVolumeRemovalOutcome::Removed,
                // A Container Ployz doesn't make, which took no config lock, mounts it.
                Status::Conflict => {
                    return Ok(in_use(self.volumes.volume_holder_ids(name).await?));
                }
                Status::Unchanged | Status::Other => {
                    return Ok(KeptVolumeRemovalOutcome::Failed {
                        message: ployz_core::error_chain::inline(&error),
                    });
                }
            },
        };
        operation.forget_kept_volume(name).await?;
        Ok(outcome)
    }
}

fn in_use(containers: Vec<ContainerId>) -> KeptVolumeRemovalOutcome {
    KeptVolumeRemovalOutcome::InUse { containers }
}

/// Bytes allocated to the files under `root`, or `None` when the walk fails or runs past
/// `until`. Symlinks are not followed.
fn disk_bytes(root: &Path, until: Instant) -> Option<u64> {
    let mut total = 0_u64;
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        if Instant::now() > until {
            return None;
        }
        for entry in std::fs::read_dir(&dir).ok()? {
            let entry = entry.ok()?;
            let metadata = entry.metadata().ok()?;
            if metadata.is_dir() {
                dirs.push(entry.path());
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.blocks().saturating_mul(BLOCK_BYTES));
            }
        }
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
    };

    use futures_util::FutureExt as _;
    use ployz_core::{ContainerKind, Namespace};

    use super::*;
    use crate::docker::{
        ContainerRuntime, VolumeDisposal,
        lifecycle::ContainerRequest,
        test_support::{
            FakeDocker, container_request, fake_runtime_with, machine, spec_with_sources,
        },
    };

    type Containers = Arc<Mutex<BTreeMap<String, serde_json::Value>>>;

    const DATA: &str = "/var/lib/postgresql/data";

    /// A Machine whose image declares a `VOLUME` at [`DATA`], so Docker gives each new
    /// Container a fresh anonymous Volume there.
    async fn machine_with_image_volume() -> (ContainerRuntime, FakeDocker, Containers) {
        let containers = Containers::default();
        let (runtime, fake) = fake_runtime_with(FakeDocker {
            named_containers: Some(containers.clone()),
            list_named: true,
            anonymous_mounts: Arc::new(Mutex::new(vec![DATA.to_owned()])),
            ..Default::default()
        })
        .await;
        (runtime, fake, containers)
    }

    async fn create(runtime: &ContainerRuntime, creation_key: &'static str) -> ContainerId {
        let namespace = Namespace::parse("app").unwrap();
        let spec = spec_with_sources(Vec::new());
        runtime
            .lifecycle()
            .create_with_admission::<_, _, Error>(
                &machine(),
                ContainerRequest {
                    creation_key,
                    ..container_request(
                        ContainerKind::ServiceContainer,
                        &namespace,
                        &spec,
                        std::future::ready(None),
                    )
                },
            )
            .await
            .unwrap()
            .container_id
    }

    /// The anonymous Volume Docker gave `container` at [`DATA`].
    fn data_volume(containers: &Containers, container: &ContainerId) -> DockerVolumeName {
        let containers = containers.lock().unwrap();
        let mounted = containers
            .values()
            .find(|found| {
                found.get("Id").and_then(serde_json::Value::as_str) == Some(container.as_str())
            })
            .and_then(|found| found.get("Mounts"))
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .find(|mount| {
                mount.get("Destination").and_then(serde_json::Value::as_str) == Some(DATA)
            })
            .and_then(|mount| mount.get("Name").and_then(serde_json::Value::as_str))
            .unwrap_or_else(|| panic!("{container} mounts an anonymous Volume at {DATA}"));
        DockerVolumeName::parse(mounted).unwrap()
    }

    /// A Container Docker has made, not started, mounting `volume`.
    fn mount_elsewhere(containers: &Containers, volume: &DockerVolumeName) -> ContainerId {
        let id = "c".repeat(64);
        containers.lock().unwrap().insert(
            "made-by-hand".to_owned(),
            serde_json::json!({
                "Id": id,
                "Name": "/made-by-hand",
                "Config": {"Labels": {}},
                "State": {"Status": "created"},
                "Mounts": [{"Type": "volume", "Name": volume.as_str(), "Destination": "/data"}],
            }),
        );
        ContainerId::parse(id).unwrap()
    }

    fn exists(fake: &FakeDocker, volume: &DockerVolumeName) -> bool {
        fake.volumes.lock().unwrap().contains_key(volume.as_str())
    }

    async fn prune(
        runtime: &ContainerRuntime,
        volume: &DockerVolumeName,
    ) -> KeptVolumeRemovalOutcome {
        let mut results = runtime
            .lifecycle()
            .remove_kept_volumes(vec![volume.clone()])
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        results.remove(0).outcome
    }

    /// Removes a Container no copy replaces, as a Drain or scale-down does, keeping its
    /// anonymous Volume.
    async fn scale_down(
        runtime: &ContainerRuntime,
        containers: &Containers,
        creation_key: &'static str,
    ) -> (ContainerId, DockerVolumeName) {
        let container = create(runtime, creation_key).await;
        let volume = data_volume(containers, &container);
        runtime
            .lifecycle()
            .remove(&container, VolumeDisposal::Keep, true)
            .await
            .unwrap();
        (container, volume)
    }

    #[tokio::test]
    async fn a_removal_no_copy_replaces_records_the_volume_it_keeps() {
        let (runtime, fake, containers) = machine_with_image_volume().await;
        let (container, volume) = scale_down(&runtime, &containers, "plan:0").await;

        assert!(exists(&fake, &volume), "the removal keeps the data");
        let kept = runtime.lifecycle().kept_volumes().await.unwrap();
        let [listed] = kept.as_slice() else {
            panic!("the one kept Volume is listed once: {kept:?}");
        };
        assert_eq!(
            listed,
            &KeptVolume {
                name: volume,
                namespace: Namespace::parse("app").unwrap(),
                service: ployz_core::ServiceName::parse("api").unwrap(),
                container,
                path: ContainerPath::parse(DATA).unwrap(),
                kept_at: listed.kept_at,
                size_bytes: None,
            },
            "listed with where it came from; a size it couldn't measure is unknown, not zero"
        );
    }

    #[tokio::test]
    async fn prune_refuses_a_volume_this_machine_did_not_keep() {
        let (runtime, fake, containers) = machine_with_image_volume().await;
        let live = create(&runtime, "plan:0").await;
        let live_data = data_volume(&containers, &live);
        containers.lock().unwrap().clear();

        assert_eq!(
            prune(&runtime, &live_data).await,
            KeptVolumeRemovalOutcome::NotKept,
            "an anonymous Volume no removal kept is not Ployz's to delete, mounted or not"
        );
        assert!(exists(&fake, &live_data));
    }

    #[tokio::test]
    async fn prune_refuses_a_kept_volume_a_container_mounts_until_it_is_gone() {
        let (runtime, fake, containers) = machine_with_image_volume().await;
        let (_, volume) = scale_down(&runtime, &containers, "plan:0").await;
        let holder = mount_elsewhere(&containers, &volume);

        assert_eq!(
            prune(&runtime, &volume).await,
            KeptVolumeRemovalOutcome::InUse {
                containers: vec![holder]
            },
            "a Container that is made but not running still holds the data"
        );
        assert!(exists(&fake, &volume));
        assert_eq!(runtime.lifecycle().kept_volumes().await.unwrap().len(), 1);

        containers.lock().unwrap().remove("made-by-hand");
        assert_eq!(
            prune(&runtime, &volume).await,
            KeptVolumeRemovalOutcome::Removed
        );
        assert!(!exists(&fake, &volume));
        assert!(runtime.lifecycle().kept_volumes().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn no_create_lands_between_prunes_check_and_its_removal() {
        let (runtime, fake, containers) = machine_with_image_volume().await;
        let (_, volume) = scale_down(&runtime, &containers, "plan:0").await;
        // Prune lists who mounts only a Volume it kept, where this test holds it.
        assert_eq!(runtime.lifecycle().kept_volumes().await.unwrap().len(), 1);
        let checked = Arc::new(tokio::sync::Barrier::new(2));
        *fake.listed_hold.lock().unwrap() = Some(checked.clone());

        let pruning = tokio::spawn({
            let runtime = runtime.clone();
            let volume = volume.clone();
            async move { prune(&runtime, &volume).await }
        });
        // Docker has answered prune that no Container mounts the Volume.
        checked.wait().await;
        assert!(
            runtime
                .lifecycle()
                .specs
                .config_operation()
                .now_or_never()
                .is_none(),
            "a Container create waits for the config lock prune holds from its check to its removal"
        );
        checked.wait().await;

        assert_eq!(pruning.await.unwrap(), KeptVolumeRemovalOutcome::Removed);
        assert!(!exists(&fake, &volume));
        assert!(
            runtime
                .lifecycle()
                .specs
                .config_operation()
                .now_or_never()
                .is_some()
        );
    }
}
