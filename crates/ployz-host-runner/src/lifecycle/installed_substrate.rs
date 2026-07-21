use std::path::Path;

use ployz_core::install::MachineJoinSubstrateRelease;
use ployz_core::operation::FailureMessage;
use serde::{Deserialize, Serialize};

use crate::execution::{FileMode, write_durable_file};
use crate::release_manifest::ReleaseManifest;

pub(crate) const INSTALLED_SUBSTRATE_RELEASE_FILE: &str = "installed-substrate-release.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum InstalledSubstrateManifestSource {
    PublicExactRelease,
    PersistedManifest { contents: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstalledSubstrateRelease {
    pub release: MachineJoinSubstrateRelease,
    pub manifest_source: InstalledSubstrateManifestSource,
}

impl InstalledSubstrateRelease {
    #[must_use]
    pub(crate) const fn public_exact(release: MachineJoinSubstrateRelease) -> Self {
        Self {
            release,
            manifest_source: InstalledSubstrateManifestSource::PublicExactRelease,
        }
    }

    pub(crate) fn persisted_manifest(
        release: MachineJoinSubstrateRelease,
        contents: String,
    ) -> Result<Self, String> {
        let manifest = ReleaseManifest::parse(&contents).map_err(|error| error.to_string())?;
        if manifest.version() != &release.version {
            return Err(format!(
                "persisted manifest release {} does not match installed substrate release {}",
                manifest.version().as_str(),
                release.version.as_str()
            ));
        }
        Ok(Self {
            release,
            manifest_source: InstalledSubstrateManifestSource::PersistedManifest { contents },
        })
    }

    fn validate(self) -> Result<Self, String> {
        match &self.manifest_source {
            InstalledSubstrateManifestSource::PublicExactRelease => Ok(self),
            InstalledSubstrateManifestSource::PersistedManifest { contents } => {
                Self::persisted_manifest(self.release, contents.clone())
            }
        }
    }
}

pub(crate) fn store_installed_substrate_release(
    state_dir: &Path,
    release: &InstalledSubstrateRelease,
) -> Result<(), FailureMessage> {
    let contents = serde_json::to_vec(release).map_err(|error| {
        failure_message(format!(
            "failed to encode installed substrate release: {error}"
        ))
    })?;
    write_durable_file(
        state_dir,
        INSTALLED_SUBSTRATE_RELEASE_FILE,
        FileMode::Plain,
        &contents,
    )
}

pub(crate) fn load_installed_substrate_release(
    state_dir: &Path,
) -> Result<InstalledSubstrateRelease, String> {
    let path = state_dir.join(INSTALLED_SUBSTRATE_RELEASE_FILE);
    let contents = std::fs::read(&path).map_err(|error| {
        format!(
            "failed to read installed substrate release {}: {error}",
            path.display()
        )
    })?;
    serde_json::from_slice::<InstalledSubstrateRelease>(&contents)
        .map_err(|error| {
            format!(
                "failed to decode installed substrate release {}: {error}",
                path.display()
            )
        })?
        .validate()
        .map_err(|error| {
            format!(
                "installed substrate release {} is invalid: {error}",
                path.display()
            )
        })
}

fn failure_message(message: String) -> FailureMessage {
    FailureMessage::try_new(message).expect("installed substrate release failure is non-empty")
}

#[cfg(test)]
mod tests {
    use ployz_core::install::{ExactPloyzVersion, MachineJoinSubstrateRelease};

    use super::{
        InstalledSubstrateManifestSource, InstalledSubstrateRelease,
        load_installed_substrate_release, store_installed_substrate_release,
    };

    #[test]
    fn installed_substrate_release_round_trips_canonical_exact_identity() {
        let root = tempfile::tempdir().expect("tempdir");
        let release = InstalledSubstrateRelease::public_exact(MachineJoinSubstrateRelease {
            version: ExactPloyzVersion::try_new("v0.1.0").expect("exact release"),
        });

        store_installed_substrate_release(root.path(), &release).expect("release stores");

        assert_eq!(
            load_installed_substrate_release(root.path()).expect("release loads"),
            release
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("installed-substrate-release.json"))
                .expect("stored identity reads"),
            r#"{"release":{"version":"0.1.0"},"manifest_source":{"kind":"public_exact_release"}}"#
        );
    }

    #[test]
    fn persisted_manifest_is_atomic_with_exact_identity() {
        let release = MachineJoinSubstrateRelease {
            version: ExactPloyzVersion::try_new("dev-aabbccdd").expect("exact release"),
        };
        let sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let contents = format!(
            "PLOYZ_VERSION=dev-aabbccdd\n\
             PLOYZ_RELEASE_PLATFORM=linux-amd64\n\
             PLOYZD_URL=/var/lib/ployz/dev/ployzd\n\
             PLOYZD_SHA256={sha}\n\
             PLOYZ_EBPF_TC_URL=/var/lib/ployz/dev/ployz-ebpf-tc\n\
             PLOYZ_EBPF_TC_SHA256={sha}\n\
             PLOYZ_EBPF_CTL_URL=/var/lib/ployz/dev/ployz-ebpf-ctl\n\
             PLOYZ_EBPF_CTL_SHA256={sha}\n\
             PLOYZ_RAILPACK_VERSION=v0.31.0\n\
             PLOYZ_RAILPACK_URL=https://example.test/railpack\n\
             PLOYZ_RAILPACK_SHA256={sha}\n"
        );
        let installed =
            InstalledSubstrateRelease::persisted_manifest(release.clone(), contents.clone())
                .expect("matching manifest persists");

        assert_eq!(installed.release, release);
        assert_eq!(
            installed.manifest_source,
            InstalledSubstrateManifestSource::PersistedManifest { contents }
        );
    }
}
