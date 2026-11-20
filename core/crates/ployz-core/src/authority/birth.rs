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
    BirthNonce, DockerVolumeName, Namespace, RawVolumeSource, RequestedServiceSpec, ServiceMount,
    ServiceName, VolumeBirth,
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
    #[error(
        "{0} mounts a volume another environment began, and no server runs that here. \
         Nothing signed."
    )]
    OtherEnvironment(ServiceName),
}

/// Give every mount in `target` of a Volume `volumes` lists as unborn, and whose spec
/// carries no birth for it, a birth minted now: one per Volume, named as `volumes` names
/// it and made in `namespace`, with the nonce `nonce` gives it.
///
/// # Errors
/// Returns [`BirthError::OtherEnvironment`] for a spec carrying a birth from another
/// Namespace, which every Server refuses.
pub fn mint(
    target: &mut [RequestedServiceSpec],
    (namespace, volumes): (&Namespace, &PlannedVolumes),
    made_at: Timestamp,
    mut nonce: impl FnMut(&DockerVolumeName) -> BirthNonce,
) -> Result<Births, BirthError> {
    let mut births = Births::new();
    for spec in target.iter_mut() {
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
    pub volumes: &'a PlannedVolumes,
    /// The births this approval minted, the only ones that read as new.
    pub minted: &'a Births,
    pub now: Timestamp,
}

static NONE_PLANNED: PlannedVolumes = BTreeMap::new();
static NONE_MINTED: Births = BTreeMap::new();

impl Mounts<'_> {
    /// A spec's mounts with no names from the Store and no birth minted.
    #[must_use]
    pub fn unnamed(now: Timestamp) -> Self {
        Mounts {
            volumes: &NONE_PLANNED,
            minted: &NONE_MINTED,
            now,
        }
    }

    /// One line for each mount of `spec` that a plain managed Volume backs, such as
    /// `mounts previews at /srv/previews, read-write: a new, empty volume`, and the targets
    /// of those mounts. Every other mount shows with the spec's other settings.
    #[must_use]
    pub fn lines(&self, spec: &RequestedServiceSpec) -> (Vec<Shown>, Vec<String>) {
        let graph = spec.volume_graph();
        let mut lines = Vec::new();
        let mut shown = Vec::new();
        for mount in graph.mounts() {
            let Some((declared, birth)) = plain(graph.volume_for(mount).source.kind(), mount)
            else {
                continue;
            };
            let planned = self
                .volumes
                .get(declared)
                .map(|planned| planned.name.as_str());
            let called = planned.unwrap_or(declared.as_str());
            let name = birth.map_or(called, |birth| birth.name.as_str());
            let line = Shown::words("mounts ")
                .and_text(name)
                .and_words(" at ")
                .and_text(mount.target.as_str())
                .and_words(match mount.read_only {
                    true => ", read-only: ",
                    false => ", read-write: ",
                });
            lines.push(line.and(match birth {
                Some(birth) if self.minted.get(declared) == Some(birth) => {
                    Shown::words("a new, empty volume")
                }
                Some(birth) => self.made(birth, planned),
                None => {
                    Shown::words("data from before this version of Ployz recorded where data began")
                }
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
        match called {
            Some(called) if called != birth.name => made
                .and_words(" (Ployz Cloud calls it ")
                .and_text(called)
                .and_words(")"),
            _ => made,
        }
    }
}

/// The declared name and birth of a mount whose line says all it signs: a managed Volume
/// with no labels, Docker's plain `local` driver for an ordinary one, and a mount of the
/// whole Volume with Docker's first copy on.
fn plain<'a>(
    source: &'a RawVolumeSource,
    mount: &ServiceMount,
) -> Option<(&'a DockerVolumeName, Option<&'a VolumeBirth>)> {
    if mount.subpath.is_some() || mount.no_copy {
        return None;
    }
    match source {
        RawVolumeSource::Ordinary {
            name,
            driver,
            labels,
            birth,
        } if labels.is_empty() && driver.name() == "local" && driver.options().is_empty() => {
            Some((name, birth.as_ref()))
        }
        RawVolumeSource::Provisioned {
            name,
            labels,
            birth,
            ..
        } if labels.is_empty() => Some((name, birth.as_ref())),
        RawVolumeSource::Ordinary { .. }
        | RawVolumeSource::Provisioned { .. }
        | RawVolumeSource::External { .. }
        | RawVolumeSource::Bind { .. }
        | RawVolumeSource::Tmpfs { .. } => None,
    }
}

#[cfg(test)]
#[path = "birth_tests.rs"]
mod tests;
