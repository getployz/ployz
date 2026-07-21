use std::path::Path;

use ployz_core::install::MachineJoinSubstrateRelease;
use ployz_core::operation::FailureMessage;

use crate::execution::{FileMode, write_durable_file};

pub(crate) const INSTALLED_SUBSTRATE_RELEASE_FILE: &str = "installed-substrate-release.json";

pub(crate) fn store_installed_substrate_release(
    state_dir: &Path,
    release: &MachineJoinSubstrateRelease,
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
) -> Result<MachineJoinSubstrateRelease, String> {
    let path = state_dir.join(INSTALLED_SUBSTRATE_RELEASE_FILE);
    let contents = std::fs::read(&path).map_err(|error| {
        format!(
            "failed to read installed substrate release {}: {error}",
            path.display()
        )
    })?;
    serde_json::from_slice(&contents).map_err(|error| {
        format!(
            "failed to decode installed substrate release {}: {error}",
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

    use super::{load_installed_substrate_release, store_installed_substrate_release};

    #[test]
    fn installed_substrate_release_round_trips_canonical_exact_identity() {
        let root = tempfile::tempdir().expect("tempdir");
        let release = MachineJoinSubstrateRelease {
            version: ExactPloyzVersion::try_new("v0.1.0").expect("exact release"),
        };

        store_installed_substrate_release(root.path(), &release).expect("release stores");

        assert_eq!(
            load_installed_substrate_release(root.path()).expect("release loads"),
            release
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("installed-substrate-release.json"))
                .expect("stored identity reads"),
            r#"{"version":"0.1.0"}"#
        );
    }
}
