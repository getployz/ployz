//! Preview Namespaces, which a pull request's preview deploys into, and what a Server admits
//! in one. The Namespace names the one Server that may hold its Containers and data, so no
//! second Server ever holds the same preview.

use sha2::{Digest as _, Sha256};

use crate::{
    Admission, ContainerPath, DockerVolumeName, MachineId, Namespace, RawVolumeSource,
    ServiceVolumeGraph, VolumeBirth,
    authority::canonical::Timestamp,
    separation::{PREVIEW_BIRTH, PREVIEW_HOME},
    value::{hex_newtype, is_lower_hex},
};

hex_newtype!(
    /// The Server a preview Namespace names: the first 4 bytes of a hash of its Machine ID.
    PreviewHome, "preview home", 4, ""
);

impl PreviewHome {
    #[must_use]
    pub fn of(machine: &MachineId) -> Self {
        let digest: [u8; 32] = Sha256::new()
            .chain_update(PREVIEW_HOME)
            .chain_update(machine.as_str())
            .finalize()
            .into();
        let (home, _) = digest.split_first_chunk().expect("a digest holds 4 bytes");
        Self::from_bytes(*home)
    }
}

/// A Namespace of the exact form `pv-<16 hex rule>-<8 hex home>-<8 hex nonce>`. Any other
/// Namespace, one that only starts with `pv-` included, is an ordinary one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreviewNamespace {
    namespace: Namespace,
    home: PreviewHome,
}

impl PreviewNamespace {
    /// `namespace` as a preview Namespace, or `None` when it isn't of the exact form.
    #[must_use]
    pub fn of(namespace: &Namespace) -> Option<Self> {
        let mut parts = namespace.as_str().strip_prefix("pv-")?.split('-');
        let (rule, home, nonce) = (parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() || !is_lower_hex(rule, 16) || !is_lower_hex(nonce, 8) {
            return None;
        }
        Some(Self {
            namespace: namespace.clone(),
            home: PreviewHome::parse(home).ok()?,
        })
    }

    /// Refuse this preview on any Server but its home.
    ///
    /// # Errors
    /// Returns [`PreviewRefusal::Elsewhere`] on a Server the Namespace doesn't name.
    pub fn admit_on(&self, machine: &MachineId) -> Result<(), PreviewRefusal> {
        if self.home == PreviewHome::of(machine) {
            Ok(())
        } else {
            Err(PreviewRefusal::Elsewhere {
                namespace: self.namespace.clone(),
            })
        }
    }

    /// The birth of the preview's Volume `physical`: derived from the two names alone, so
    /// every Deploy of the preview mounts the same empty Volume, and no signer's random
    /// nonce ever matches it.
    #[must_use]
    pub fn birth(&self, physical: &DockerVolumeName) -> VolumeBirth {
        let digest: [u8; 32] = Sha256::new()
            .chain_update(PREVIEW_BIRTH)
            .chain_update(self.namespace.as_str())
            .chain_update(b"\0")
            .chain_update(physical.as_str())
            .finalize()
            .into();
        let (nonce, _) = digest.split_first_chunk().expect("a digest holds 16 bytes");
        VolumeBirth {
            namespace: self.namespace.clone(),
            name: physical.as_str().to_owned(),
            made_at: Timestamp::from_millis(0).expect("zero is a time"),
            nonce: crate::BirthNonce::from_bytes(*nonce),
        }
    }

    /// Whether `physical` is a Docker Volume of this Namespace.
    fn holds(&self, physical: &DockerVolumeName) -> bool {
        physical
            .as_str()
            .strip_prefix(self.namespace.as_str())
            .and_then(|declared| declared.strip_prefix('_'))
            .is_some_and(|declared| !declared.is_empty())
    }
}

impl VolumeBirth {
    /// Whether this is the birth a preview Namespace derives for its own Volume.
    #[must_use]
    pub fn is_preview(&self) -> bool {
        PreviewNamespace::of(&self.namespace).is_some_and(|preview| {
            DockerVolumeName::parse(self.name.as_str())
                .is_ok_and(|physical| preview.birth(&physical) == *self)
        })
    }
}

/// Why a Server refused a Container or data in a preview Namespace.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PreviewRefusal {
    #[error("Preview {namespace} runs on another Server, so this one holds none of it.")]
    Elsewhere { namespace: Namespace },
    #[error("A preview runs only signed Containers.")]
    Unsigned,
    #[error(
        "A preview mounts only its own Volumes and tmpfs, and what it mounts at {target} is neither."
    )]
    Mount { target: ContainerPath },
}

/// Refuse a Container in a preview Namespace unless this Server is its home, a person's
/// signature or rule admitted it, and it mounts only tmpfs and the preview's own
/// Provisioned Volumes, each with its preview birth. A Container in any other Namespace
/// passes.
///
/// # Errors
/// Returns the first [`PreviewRefusal`] that holds.
pub fn admit_preview(
    machine: &MachineId,
    namespace: &Namespace,
    admission: Admission,
    volumes: &ServiceVolumeGraph,
) -> Result<(), PreviewRefusal> {
    let Some(preview) = PreviewNamespace::of(namespace) else {
        return Ok(());
    };
    preview.admit_on(machine)?;
    if admission != Admission::Signed {
        return Err(PreviewRefusal::Unsigned);
    }
    for mount in volumes.mounts() {
        let own = match volumes.volume_for(mount).source.kind() {
            RawVolumeSource::Tmpfs { .. } => true,
            RawVolumeSource::Provisioned {
                name,
                birth: Some(birth),
                ..
            } => preview.holds(name) && *birth == preview.birth(name),
            RawVolumeSource::Provisioned { birth: None, .. }
            | RawVolumeSource::Bind { .. }
            | RawVolumeSource::External { .. }
            | RawVolumeSource::Ordinary { .. } => false,
        };
        if !own {
            return Err(PreviewRefusal::Mount {
                target: mount.target.clone(),
            });
        }
    }
    Ok(())
}

/// Refuse data arriving into the Docker Volume `physical` when it belongs to a preview
/// whose home is another Server.
///
/// # Errors
/// Returns [`PreviewRefusal::Elsewhere`] for another Server's preview.
pub fn admit_preview_arrival(
    machine: &MachineId,
    physical: &DockerVolumeName,
) -> Result<(), PreviewRefusal> {
    let preview = physical
        .as_str()
        .split_once('_')
        .and_then(|(namespace, _)| Namespace::parse(namespace).ok())
        .and_then(|namespace| PreviewNamespace::of(&namespace));
    preview.map_or(Ok(()), |preview| preview.admit_on(machine))
}

#[cfg(test)]
#[path = "preview_tests.rs"]
mod tests;
