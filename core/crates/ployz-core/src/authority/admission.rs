//! Which specs a Machine runs only signed: the ones that reach data.
//!
//! A spec reaches data when its Container would open a Sealed value, mount a Volume,
//! reach its Machine beyond its own Environment, or run an image pulled with a login.
//! Anything else gets nothing Cloud does not already hold, so Cloud may run it alone.

use std::fmt;

use super::deploy::{Creation, host_reach};
use crate::{EnvValue, RawVolumeSource};

/// What a spec reaches that makes a Machine run it only signed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataReach {
    /// It opens a Sealed value.
    Sealed,
    /// It mounts a Volume.
    Volume,
    /// It reaches its Machine beyond its own Environment.
    Host,
    /// Its image is pulled with a registry login.
    ImageLogin,
}

impl fmt::Display for DataReach {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Sealed => "opens a secret",
            Self::Volume => "mounts a volume",
            Self::Host => "reaches its server",
            Self::ImageLogin => "pulls its image with a login",
        })
    }
}

/// What `creation` reaches that needs a signature, or `None` when it reaches no data.
/// `image_login` says the request carries a registry login for its image.
///
/// A hook's Sealed values count for the Service's own Container too, and a Volume counts
/// whether or not the Container mounts it: the spec is signed whole.
#[must_use]
pub fn needs_signature(creation: Creation<'_>, image_login: bool) -> Option<DataReach> {
    let spec = creation.spec;
    let hook = spec.pre_deploy.as_ref();
    let sealed = spec
        .container
        .environment
        .values()
        .chain(hook.into_iter().flat_map(|hook| hook.environment.values()))
        .any(EnvValue::is_sealed);
    if sealed {
        return Some(DataReach::Sealed);
    }
    let requested = spec.to_requested();
    let volume = requested
        .volume_graph()
        .volumes()
        .iter()
        .any(|volume| !matches!(volume.source.kind(), RawVolumeSource::Tmpfs { .. }));
    if volume {
        return Some(DataReach::Volume);
    }
    if !host_reach(&requested).is_empty() {
        return Some(DataReach::Host);
    }
    image_login.then_some(DataReach::ImageLogin)
}

#[cfg(test)]
#[path = "admission_tests.rs"]
mod tests;
