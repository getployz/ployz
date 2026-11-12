//! The ZFS Volume bytes each container is booked, from the objset kstats
//! [`zfs_io`] reads.

use std::collections::{BTreeMap, HashMap, hash_map::Entry};

use ployz_core::{ContainerId, DockerVolumeName, ServiceId};

use super::zfs_io::DatasetIo;
use crate::dataset_layout::Place;

/// The Volumes one running container mounts.
pub(super) struct Mounts<'a> {
    pub(super) id: ContainerId,
    pub(super) service_id: Option<ServiceId>,
    pub(super) volumes: &'a [DockerVolumeName],
}

/// Volume bytes as each container's own running total. A kstat counts from
/// its dataset's mount, so booking it whole would jump whenever another
/// replica takes a Volume over or a kstat misses a tick. Only the growth
/// between two readings of a dataset is booked.
#[derive(Debug, Default)]
pub(super) struct VolumeLedger {
    seen: HashMap<String, DatasetIo>,
    booked: HashMap<ContainerId, DatasetIo>,
}

impl VolumeLedger {
    /// What `container` has booked so far.
    pub(super) fn booked(&self, container: &ContainerId) -> Option<DatasetIo> {
        self.booked.get(container).copied()
    }

    pub(super) fn book(&mut self, datasets: BTreeMap<String, DatasetIo>, live: &[Mounts<'_>]) {
        let grown: BTreeMap<String, DatasetIo> = datasets
            .into_iter()
            .map(|(dataset, now)| {
                let grown = self.seen.insert(dataset.clone(), now).map_or_else(
                    DatasetIo::default,
                    |last| DatasetIo {
                        read_bytes: growth(last.read_bytes, now.read_bytes),
                        write_bytes: growth(last.write_bytes, now.write_bytes),
                    },
                );
                (dataset, grown)
            })
            .collect();
        for (container, io) in volume_io(&grown, live) {
            let total = self.booked.entry(container).or_default();
            total.read_bytes = total.read_bytes.saturating_add(io.read_bytes);
            total.write_bytes = total.write_bytes.saturating_add(io.write_bytes);
        }
        self.booked
            .retain(|id, _| live.iter().any(|container| container.id == *id));
    }
}

/// A counter below its last reading restarted, as a remount resets it.
fn growth(last: u64, now: u64) -> u64 {
    now.checked_sub(last).unwrap_or(now)
}

/// Each Volume's ZFS bytes, booked once per Service on the lowest container
/// id that mounts it. Replicas sharing a Volume cannot be told apart, and
/// booking it on each would count it once per replica.
fn volume_io(
    datasets: &BTreeMap<String, DatasetIo>,
    containers: &[Mounts<'_>],
) -> HashMap<ContainerId, DatasetIo> {
    let by_volume: HashMap<&str, DatasetIo> = datasets
        .iter()
        .filter_map(|(dataset, io)| {
            Place::in_any_pool(dataset)
                .writer_root()
                .map(|volume| (volume, *io))
        })
        .collect();
    let mut owners: HashMap<(Option<ServiceId>, &str), ContainerId> = HashMap::new();
    for container in containers {
        for volume in container.volumes {
            if !by_volume.contains_key(volume.as_str()) {
                continue;
            }
            match owners.entry((container.service_id, volume.as_str())) {
                Entry::Occupied(mut owner) if container.id < *owner.get() => {
                    owner.insert(container.id);
                }
                Entry::Occupied(_) => {}
                Entry::Vacant(owner) => {
                    owner.insert(container.id);
                }
            }
        }
    }
    let mut booked: HashMap<ContainerId, DatasetIo> = HashMap::new();
    for ((_, volume), owner) in owners {
        let Some(io) = by_volume.get(volume) else {
            continue;
        };
        let total = booked.entry(owner).or_default();
        total.read_bytes = total.read_bytes.saturating_add(io.read_bytes);
        total.write_bytes = total.write_bytes.saturating_add(io.write_bytes);
    }
    booked
}
