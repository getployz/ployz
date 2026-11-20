//! A Volume's birth as the approver's client mints it, and the line a person reads for
//! each mount before signing.
//!
//! The Config Store lists which Volumes a plan's specs mount for the first time. Only the
//! signer mints, and only it says "a new, empty volume": a birth Cloud relays in the spec
//! reads as the data it names, whatever Cloud claims about it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use ts_rs::TS;

use super::{canonical::Timestamp, shown::Shown};
use crate::{
    BirthNonce, DockerVolumeName, Namespace, RawVolumeSource, RequestedServiceSpec, ServiceName,
    VolumeBirth, VolumeDriver,
};

/// A Volume a plan's specs mount, as the Config Store names it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PlannedVolume {
    /// What Ployz Cloud calls it now.
    pub name: String,
    /// No approval has mounted it and no Server can hold its data yet, so the approval
    /// that first mounts it mints its birth.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub unborn: bool,
}

/// Each Volume a plan mounts, by the name its specs declare it under.
pub type PlannedVolumes = BTreeMap<DockerVolumeName, PlannedVolume>;

/// The births one approval minted, by the name the specs declare each Volume under.
pub type Births = BTreeMap<DockerVolumeName, VolumeBirth>;

/// Why a spec can't be signed with the births it carries.
#[derive(Debug, thiserror::Error)]
pub enum BirthError {
    #[error("{0} mounts a volume from another environment. Nothing signed.")]
    OtherEnvironment(ServiceName),
}

/// Give every mount in `target`'s specs of a Volume `volumes` lists as unborn, and whose
/// spec carries no birth for it, a birth minted now: one per Volume, named as `volumes` names
/// it and made in `namespace`, with the nonce `nonce` gives it.
///
/// # Errors
/// Returns [`BirthError::OtherEnvironment`] for a spec carrying a birth from another
/// Namespace, which every Server refuses.
pub fn mint<'a>(
    target: impl IntoIterator<Item = &'a mut RequestedServiceSpec>,
    (namespace, volumes): (&Namespace, &PlannedVolumes),
    made_at: Timestamp,
    mut nonce: impl FnMut(&DockerVolumeName) -> BirthNonce,
) -> Result<Births, BirthError> {
    let mut births = Births::new();
    for spec in target {
        if !spec.volume_graph().born_in(namespace) {
            return Err(BirthError::OtherEnvironment(spec.name.clone()));
        }
        spec.mount_graph = spec.mount_graph.with_births(|name| {
            let planned = volumes.get(name).filter(|planned| planned.unborn)?;
            let birth = births.entry(name.clone()).or_insert_with(|| VolumeBirth {
                namespace: namespace.clone(),
                name: planned.name.clone(),
                made_at,
                nonce: nonce(name),
            });
            Some(birth.clone())
        });
    }
    Ok(births)
}

/// The nonce minted for the Volume declared as `name` from `seed`, 16 random bytes one
/// approval draws once for all its births.
#[must_use]
pub fn nonce(seed: &[u8; 16], name: &DockerVolumeName) -> BirthNonce {
    let mut hash = Sha256::new();
    hash.update(b"ployz.volume.birth.v1\0");
    hash.update(seed);
    hash.update(name.as_str().as_bytes());
    let digest: [u8; 32] = hash.finalize().into();
    let mut nonce = [0; 16];
    nonce.copy_from_slice(&digest[..16]);
    BirthNonce::from_bytes(nonce)
}

/// 16 random bytes for [`nonce`].
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn seed() -> [u8; 16] {
    use rand_core::{OsRng, RngCore as _, TryRngCore as _};
    let mut seed = [0; 16];
    OsRng.unwrap_err().fill_bytes(&mut seed);
    seed
}

/// The births of `minted` that `spec` carries, which its signed version posts.
#[must_use]
pub fn carried(spec: &RequestedServiceSpec, minted: &Births) -> Births {
    spec.volume_graph()
        .volumes()
        .iter()
        .filter_map(|volume| {
            let name = volume.source.docker_volume_name()?;
            let birth = volume.source.birth()?;
            (minted.get(name) == Some(birth)).then(|| (name.clone(), birth.clone()))
        })
        .collect()
}

/// What the approver's client knows about the Volumes a spec mounts beyond the spec.
#[derive(Clone, Copy, Debug)]
pub struct Mounts<'a> {
    /// The Namespace the approval signs for, which names each Volume on its Servers.
    pub namespace: &'a Namespace,
    pub volumes: &'a PlannedVolumes,
    /// The births this approval minted, the only ones that read as new.
    pub minted: &'a Births,
    pub now: Timestamp,
}

impl Mounts<'_> {
    /// One line for each mount of `spec` that a managed or Docker volume backs, such as
    /// `mounts managed volume previews at /srv/previews, read-write: a new, empty volume`,
    /// and the targets of those mounts. Each line names the data its Servers check: the
    /// birth, or for data from before births, the name the Volume has on its Servers. It
    /// carries every other setting the mount signs. Every other mount shows with the
    /// spec's other settings.
    #[must_use]
    pub fn lines(&self, spec: &RequestedServiceSpec) -> (Vec<Shown>, Vec<String>) {
        let graph = spec.volume_graph();
        let mut lines = Vec::new();
        let mut shown = Vec::new();
        for mount in graph.mounts() {
            let Some(volume) = Lined::of(graph.volume_for(mount).source.kind()) else {
                continue;
            };
            let called = self
                .volumes
                .get(volume.name)
                .map(|planned| planned.name.as_str());
            let line = Shown::words(match volume.driver {
                None => "mounts managed volume ",
                Some(_) => "mounts Docker volume ",
            });
            let line = match volume.birth {
                Some(birth) => line.and_text(&birth.name),
                None => line.and_text(self.namespace.volume_name(volume.name).as_str()),
            };
            let mut line = line.and_words(" at ").and_text(mount.target.as_str());
            if let Some(subpath) = &mount.subpath {
                line = line.and_words(", subpath ").and_text(subpath);
            }
            if mount.no_copy {
                line = line.and_words(", no initial copy");
            }
            if !volume.labels.is_empty() {
                line = line.and_words(", labels ").and_value(volume.labels);
            }
            if let Some(driver) = volume
                .driver
                .filter(|driver| driver.name() != "local" || !driver.options().is_empty())
            {
                line = line.and_words(", driver ").and_value(driver);
            }
            let line = line.and_words(match mount.read_only {
                true => ", read-only: ",
                false => ", read-write: ",
            });
            lines.push(line.and(match volume.birth {
                Some(birth) if self.minted.get(volume.name) == Some(birth) => {
                    Shown::words("a new, empty volume")
                }
                Some(birth) => self.made(birth, called),
                None => calls(
                    Shown::words(
                        "data from before this version of Ployz recorded where data began",
                    ),
                    called,
                ),
            }));
            shown.push(mount.target.as_str().to_owned());
        }
        (lines, shown)
    }

    /// `data made as previews, 3 Nov 14:02 UTC, 16 days ago`, naming what Cloud calls it
    /// now when that differs.
    fn made(&self, birth: &VolumeBirth, called: Option<&str>) -> Shown {
        let mut made = Shown::words("data made as ")
            .and_text(&birth.name)
            .and_words(", ");
        for (index, part) in birth.made_at.day_and_time(self.now).split(' ').enumerate() {
            if index > 0 {
                made = made.and_words(" ");
            }
            made = made.and_text(part);
        }
        let ago = self
            .now
            .as_millis()
            .checked_sub(birth.made_at.as_millis())
            .map(|millis| millis / 86_400_000);
        made = match ago {
            Some(0) => made.and_words(", today"),
            Some(1) => made.and_words(", 1 day ago"),
            Some(days) => made
                .and_words(", ")
                .and_text(&days.to_string())
                .and_words(" days ago"),
            None => made,
        };
        calls(made, called.filter(|called| *called != birth.name))
    }
}

/// `shown`, then the name Ployz Cloud gives the Volume, when it gives one.
fn calls(shown: Shown, called: Option<&str>) -> Shown {
    match called {
        Some(called) => shown
            .and_words(" (Ployz Cloud calls it ")
            .and_text(called)
            .and_words(")"),
        None => shown,
    }
}

/// A managed or Docker volume a mount's line names, with the settings it signs.
struct Lined<'a> {
    name: &'a DockerVolumeName,
    birth: Option<&'a VolumeBirth>,
    labels: &'a BTreeMap<String, String>,
    /// A Docker volume's driver; `None` for a managed volume.
    driver: Option<&'a VolumeDriver>,
}

impl<'a> Lined<'a> {
    fn of(source: &'a RawVolumeSource) -> Option<Self> {
        match source {
            RawVolumeSource::Ordinary {
                name,
                driver,
                labels,
                birth,
            } => Some(Self {
                name,
                birth: birth.as_ref(),
                labels,
                driver: Some(driver),
            }),
            RawVolumeSource::Provisioned {
                name,
                labels,
                birth,
                ..
            } => Some(Self {
                name,
                birth: birth.as_ref(),
                labels,
                driver: None,
            }),
            RawVolumeSource::External { .. }
            | RawVolumeSource::Bind { .. }
            | RawVolumeSource::Tmpfs { .. } => None,
        }
    }
}

#[cfg(test)]
#[path = "birth_tests.rs"]
mod tests;
