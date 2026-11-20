//! The Containers that mount a Volume name, read from Docker in the same call that renames
//! a copy under the name (promote, restore, seed landing). Every one of them must expect
//! the origin the copy carries, running or stopped. Each Container's `ployz.origins` label
//! is fixed at create from its approved spec, so a kept start, or Docker restarting it on
//! its own, opens only data its approval names. A Container without the label predates
//! origins and expects Legacy.
//!
//! The plugin runs before Docker and outlives it, so Docker may be down. Then the rename
//! waits for another call. The list reads Docker's view of its Containers, which takes no
//! Container's lock, so a start that waits on this plugin's Mount does not hold it up. The
//! call is bounded all the same.

use std::{collections::HashMap, time::Duration};

use ployz_core::{DockerVolumeName, MountOrigins, RpcError, VolumeOrigin, labels};

use super::{VolumeStorage, origin::Origin};

/// How long a rename waits for Docker to list a name's Containers before it is refused.
const LIST_BOUND: Duration = Duration::from_secs(10);

/// Where the plugin lists Containers.
#[derive(Clone)]
pub(super) enum Holders {
    /// The local Docker daemon, reached afresh by each list.
    Docker,
    /// A list a test sets: the Containers each name has, or Docker's failure.
    #[cfg(test)]
    Fixed(std::sync::Arc<std::sync::Mutex<std::result::Result<Vec<Holder>, String>>>),
}

/// One Container that mounts the name.
#[derive(Clone, Debug)]
pub(super) struct Holder {
    /// Its `ployz.origins` label, absent on a Container from before origins.
    pub(super) origins: Option<String>,
}

/// Why a copy may not go under a name.
#[derive(Debug)]
pub(super) enum Unheld {
    /// A Container that mounts the name expects other data.
    Foreign(String),
    /// Docker could not say which Containers mount it.
    Unlisted(RpcError),
}

impl Holders {
    async fn list(&self, name: &DockerVolumeName) -> std::result::Result<Vec<Holder>, String> {
        match self {
            Self::Docker => {
                let docker = bollard::Docker::connect_with_defaults()
                    .map_err(|error| ployz_core::error_chain::inline(&error))?
                    .with_timeout(LIST_BOUND);
                let filters = HashMap::from([("volume", vec![name.as_str()])]);
                let listed = docker
                    .list_containers(Some(
                        bollard::query_parameters::ListContainersOptionsBuilder::default()
                            .all(true)
                            .filters(&filters)
                            .build(),
                    ))
                    .await
                    .map_err(|error| ployz_core::error_chain::inline(&error))?;
                Ok(listed
                    .into_iter()
                    .map(|container| Holder {
                        origins: container
                            .labels
                            .and_then(|mut labels| labels.remove(labels::VOLUME_ORIGINS)),
                    })
                    .collect())
            }
            #[cfg(test)]
            Self::Fixed(listed) => listed
                .lock()
                .expect("the fixed list is never poisoned")
                .clone(),
        }
    }
}

#[cfg(test)]
impl Holders {
    /// No Container mounts any name.
    pub(super) fn none() -> Self {
        Self::Fixed(std::sync::Arc::new(std::sync::Mutex::new(Ok(Vec::new()))))
    }

    /// Every later list answers `listed`.
    pub(super) fn set(&self, listed: std::result::Result<Vec<Holder>, String>) {
        match self {
            Self::Fixed(fixed) => *fixed.lock().expect("the fixed list is never poisoned") = listed,
            Self::Docker => panic!("only a fixed list is set"),
        }
    }
}

impl VolumeStorage {
    /// Admits a copy carrying `carried` under `name` only when every Container that
    /// mounts `name` expects it. A copy without an origin is Legacy, keyed by `name`.
    pub(super) async fn admit_holders(
        &self,
        name: &DockerVolumeName,
        carried: Origin,
    ) -> std::result::Result<(), Unheld> {
        let holders = self.holders.list(name).await.map_err(|error| {
            Unheld::Unlisted(RpcError::unavailable(format!(
                "Docker is not answering on this Machine, so Volume {name} keeps its copies as they are until it does: {error}"
            )))
        })?;
        let carried = carried.unwrap_or_else(|| VolumeOrigin::legacy(name));
        for holder in holders {
            let expected = match holder.origins.as_deref() {
                None => VolumeOrigin::legacy(name),
                Some(label) => MountOrigins::parse(label)
                    .ok()
                    .and_then(|origins| origins.get(name))
                    .ok_or_else(|| mounted_for_other_data(name))?,
            };
            if expected != carried {
                return Err(mounted_for_other_data(name));
            }
        }
        Ok(())
    }
}

fn mounted_for_other_data(name: &DockerVolumeName) -> Unheld {
    Unheld::Foreign(format!(
        "Volume {name} is mounted by a Container approved for other data, so this copy stays where it is"
    ))
}
