//! The Containers that mount a Volume name, read from Docker in the same call that puts a
//! root under the name: a fresh create, or a promote, restore or seed landing. Every one
//! of them must expect the origin the root carries, running or stopped. Each Container's
//! `ployz.origins` label is fixed at create from its approved spec, so a kept start, or
//! Docker restarting it on its own, opens only data its approval names. A Container
//! without the label predates origins and expects Legacy.
//!
//! The plugin runs before Docker and outlives it, so Docker may be down. Then the root
//! waits for another call. The list reads Docker's view of its Containers, which takes no
//! Container's lock, so a start that waits on this plugin's Mount does not hold it up. The
//! call is bounded all the same.

use std::{collections::HashMap, time::Duration};

use ployz_core::{DockerVolumeName, MountOrigins, VolumeOrigin, labels};
use ployzd::volume_copy::HELD_FOR_OTHER_DATA;

use super::{VolumeStorage, origin::Origin};

/// How long a root waits for Docker to list a name's Containers, response body included,
/// before it is refused. The CLI and Cloud give a copy or seed call 10 s, and the ZFS work
/// before the list spends part of that, so the refusal reaches them before they give up.
const LIST_BOUND: Duration = Duration::from_secs(3);

/// Where the plugin lists Containers.
#[derive(Clone)]
pub(super) enum Holders {
    /// The local Docker daemon, reached afresh by each list.
    Docker,
    /// A Docker a test serves on this Unix socket.
    #[cfg(test)]
    Socket(std::path::PathBuf),
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

/// Why a root may not go under a name.
#[derive(Debug)]
pub(super) enum Unheld {
    /// A Container that mounts the name expects other data.
    Foreign,
    /// Docker could not say which Containers mount it, in words for the caller.
    Unlisted(String),
}

impl Holders {
    async fn list(&self, name: &DockerVolumeName) -> std::result::Result<Vec<Holder>, String> {
        let docker = match self {
            Self::Docker => bollard::Docker::connect_with_defaults(),
            #[cfg(test)]
            Self::Socket(path) => bollard::Docker::connect_with_unix(
                &path.to_string_lossy(),
                LIST_BOUND.as_secs(),
                bollard::API_DEFAULT_VERSION,
            ),
            #[cfg(test)]
            Self::Fixed(listed) => {
                return listed
                    .lock()
                    .expect("the fixed list is never poisoned")
                    .clone();
            }
        }
        .map_err(|error| ployz_core::error_chain::inline(&error))?;
        // Bollard's own timeout ends at the response headers, and it reads the body after.
        tokio::time::timeout(LIST_BOUND, mounting(&docker, name))
            .await
            .map_err(|_| format!("no list within {} s", LIST_BOUND.as_secs()))?
    }
}

async fn mounting(
    docker: &bollard::Docker,
    name: &DockerVolumeName,
) -> std::result::Result<Vec<Holder>, String> {
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
impl Holders {
    /// No Container mounts any name.
    pub(super) fn none() -> Self {
        Self::Fixed(std::sync::Arc::new(std::sync::Mutex::new(Ok(Vec::new()))))
    }

    /// Every later list answers `listed`.
    pub(super) fn set(&self, listed: std::result::Result<Vec<Holder>, String>) {
        match self {
            Self::Fixed(fixed) => *fixed.lock().expect("the fixed list is never poisoned") = listed,
            Self::Docker | Self::Socket(_) => panic!("only a fixed list is set"),
        }
    }
}

impl VolumeStorage {
    /// Admits a root carrying `carried` under `name` only when every Container that
    /// mounts `name` expects it. A root without an origin is Legacy, keyed by `name`.
    pub(super) async fn admit_holders(
        &self,
        name: &DockerVolumeName,
        carried: Origin,
    ) -> std::result::Result<(), Unheld> {
        let holders = self.holders.list(name).await.map_err(|error| {
            Unheld::Unlisted(format!(
                "Docker is not answering on this Machine, so Volume {name} keeps its copies as they are until it does: {error}"
            ))
        })?;
        let carried = carried.unwrap_or_else(|| VolumeOrigin::legacy(name));
        for holder in holders {
            let expected = match holder.origins.as_deref() {
                None => VolumeOrigin::legacy(name),
                Some(label) => MountOrigins::parse(label)
                    .ok()
                    .and_then(|origins| origins.get(name))
                    .ok_or(Unheld::Foreign)?,
            };
            if expected != carried {
                return Err(Unheld::Foreign);
            }
        }
        Ok(())
    }
}

/// The refusal of a copy that stays in its slot because a holder of `name` expects other
/// data.
pub(super) fn copy_held_for_other_data(name: &DockerVolumeName) -> String {
    format!("Volume {name} {HELD_FOR_OTHER_DATA}, so this copy stays where it is")
}
