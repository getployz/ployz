//! Preview Namespaces, which a pull request's preview deploys into, and what a Server admits
//! in one. The Namespace names the one Server that may hold its Containers and data, so no
//! second Server ever holds the same preview.

use sha2::{Digest as _, Sha256};

use crate::{
    Admission, ContainerPath, DockerVolumeName, MachineId, Namespace, RawVolumeSource,
    RequestedServiceSpec, VolumeBirth,
    authority::{canonical::Timestamp, deploy::host_reach},
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

    /// The preview whose Docker Volume `physical` is, if any.
    #[must_use]
    pub fn holding(physical: &DockerVolumeName) -> Option<Self> {
        let (namespace, _) = physical.as_str().split_once('_')?;
        Self::of(&Namespace::parse(namespace).ok()?)
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

    /// The birth of the preview's Volume `logical`, as its drafts name it: derived from the
    /// two names alone, so every Deploy of the preview mounts the same empty Volume, and no
    /// signer's random nonce ever matches it. It has no time of its own, so it is shown as
    /// the preview's own data rather than by when it was made.
    #[must_use]
    pub fn birth(&self, logical: &str) -> VolumeBirth {
        let digest: [u8; 32] = Sha256::new()
            .chain_update(PREVIEW_BIRTH)
            .chain_update(self.namespace.as_str())
            .chain_update(b"\0")
            .chain_update(logical)
            .finalize()
            .into();
        let (nonce, _) = digest.split_first_chunk().expect("a digest holds 16 bytes");
        VolumeBirth {
            namespace: self.namespace.clone(),
            name: logical.to_owned(),
            made_at: Timestamp::from_millis(0).expect("zero is a time"),
            nonce: crate::BirthNonce::from_bytes(*nonce),
        }
    }

    /// The name the preview's drafts give `physical`, when it is a Docker Volume of this
    /// Namespace.
    fn logical<'a>(&self, physical: &'a DockerVolumeName) -> Option<&'a str> {
        physical
            .as_str()
            .strip_prefix(self.namespace.as_str())
            .and_then(|declared| declared.strip_prefix('_'))
            .filter(|declared| !declared.is_empty())
    }
}

impl VolumeBirth {
    /// Whether this is the birth a preview Namespace derives for its own Volume.
    #[must_use]
    pub fn is_preview(&self) -> bool {
        PreviewNamespace::of(&self.namespace)
            .is_some_and(|preview| preview.birth(&self.name) == *self)
    }
}

/// Why a Server refused a Container or data in a preview Namespace.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PreviewRefusal {
    #[error("Preview {namespace} runs on another Server, so this one holds none of it.")]
    Elsewhere { namespace: Namespace },
    #[error("A preview runs only signed Containers.")]
    Unsigned,
    #[error("A preview can't reach its Server, and this one asks for {what}.")]
    HostReach { what: String },
    #[error("A preview mounts no data from outside it, and what it mounts at {target} is.")]
    Outside { target: ContainerPath },
    #[error(
        "A preview mounts only Volumes it made, and the one at {target} is a Volume this preview didn't make."
    )]
    NotItsOwn { target: ContainerPath },
    #[error("The image keeps data at {target}, so a preview mounts its own Volume or tmpfs there.")]
    ImageVolume { target: String },
}

/// Refuse a Container in a preview Namespace unless this Server is its home, a person's
/// signature or rule admitted it, it reaches nothing of its Server, and it mounts only tmpfs
/// and the preview's own Provisioned Volumes, each with its preview birth. A Container in
/// any other Namespace passes.
///
/// Config mounts are the one exception: each binds read only a file this Server wrote from
/// the signed spec itself, so it carries nothing from outside the preview.
///
/// # Errors
/// Returns the first [`PreviewRefusal`] that holds.
pub fn admit_preview(
    machine: &MachineId,
    namespace: &Namespace,
    admission: Admission,
    spec: &RequestedServiceSpec,
) -> Result<(), PreviewRefusal> {
    let Some(preview) = PreviewNamespace::of(namespace) else {
        return Ok(());
    };
    preview.admit_on(machine)?;
    if admission != Admission::Signed {
        return Err(PreviewRefusal::Unsigned);
    }
    let volumes = spec.volume_graph();
    for mount in volumes.mounts() {
        let target = || mount.target.clone();
        match volumes.volume_for(mount).source.kind() {
            RawVolumeSource::Tmpfs { .. } => {}
            RawVolumeSource::Provisioned {
                name,
                birth: Some(birth),
                ..
            } if preview
                .logical(name)
                .is_some_and(|logical| *birth == preview.birth(logical)) => {}
            RawVolumeSource::Bind { .. } | RawVolumeSource::External { .. } => {
                return Err(PreviewRefusal::Outside { target: target() });
            }
            RawVolumeSource::Provisioned { .. } | RawVolumeSource::Ordinary { .. } => {
                return Err(PreviewRefusal::NotItsOwn { target: target() });
            }
        }
    }
    match host_reach(spec).first() {
        Some(what) => Err(PreviewRefusal::HostReach {
            what: what.to_string(),
        }),
        None => Ok(()),
    }
}

/// Refuse a preview's Container whose image declares a `VOLUME` that no mount of `spec`
/// covers, since Docker would make an anonymous Volume there that no preview birth names.
/// `declared` is the image's `VOLUME` paths. A Container in any other Namespace passes.
///
/// # Errors
/// Returns [`PreviewRefusal::ImageVolume`] for the first path no mount covers.
pub fn admit_preview_image<'a>(
    namespace: &Namespace,
    spec: &RequestedServiceSpec,
    declared: impl IntoIterator<Item = &'a str>,
) -> Result<(), PreviewRefusal> {
    if PreviewNamespace::of(namespace).is_none() {
        return Ok(());
    }
    let mounted = spec
        .mounts()
        .iter()
        .map(|mount| &mount.target)
        .chain(
            spec.config_mounts()
                .iter()
                .filter_map(|mount| mount.target.as_ref()),
        )
        .collect::<Vec<_>>();
    for path in declared {
        let covered = ContainerPath::parse(path.trim_end_matches('/'))
            .is_ok_and(|path| mounted.contains(&&path));
        if !covered {
            return Err(PreviewRefusal::ImageVolume {
                target: path.to_owned(),
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
    PreviewNamespace::holding(physical).map_or(Ok(()), |preview| preview.admit_on(machine))
}

#[cfg(test)]
#[path = "preview_tests.rs"]
mod tests;
