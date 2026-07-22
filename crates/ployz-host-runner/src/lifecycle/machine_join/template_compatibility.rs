//! Private compatibility for promoting stored machine-join templates.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use atomic_write_file::AtomicWriteFile;
use ployz_core::install::{ExactPloyzVersion, MachineJoinTemplate};
use ployz_core::operation::FailureMessage;

#[derive(Debug)]
pub(in crate::lifecycle) struct PreparedMachineJoinTemplateReleasePromotion {
    staged: Option<StagedTemplate>,
}

#[derive(Debug)]
struct StagedTemplate {
    path: PathBuf,
    file: AtomicWriteFile,
}

impl PreparedMachineJoinTemplateReleasePromotion {
    pub(in crate::lifecycle) fn commit(self) -> Result<(), FailureMessage> {
        let Some(staged) = self.staged else {
            return Ok(());
        };
        staged.file.commit().map_err(|error| {
            failure_message(format!(
                "failed to commit atomic file {}: {error}",
                staged.path.display()
            ))
        })
    }
}

pub(in crate::lifecycle) fn prepare_machine_join_template_release_promotion(
    path: &Path,
    target_version: &ExactPloyzVersion,
) -> Result<PreparedMachineJoinTemplateReleasePromotion, FailureMessage> {
    let contents = match std::fs::read(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PreparedMachineJoinTemplateReleasePromotion { staged: None });
        }
        Err(error) => {
            return Err(failure_message(format!(
                "failed to read machine join template {}: {error}",
                path.display()
            )));
        }
    };
    let mut template =
        serde_json::from_slice::<MachineJoinTemplate>(&contents).map_err(|error| {
            failure_message(format!(
                "machine join template {} is invalid: {error}",
                path.display()
            ))
        })?;
    template.join_bundle.material.substrate_release.version = target_version.clone();
    stage_template(path, &template).map(|staged| PreparedMachineJoinTemplateReleasePromotion {
        staged: Some(staged),
    })
}

fn stage_template(
    path: &Path,
    template: &MachineJoinTemplate,
) -> Result<StagedTemplate, FailureMessage> {
    let mut rendered = serde_json::to_vec_pretty(template).map_err(|error| {
        failure_message(format!(
            "failed to encode promoted machine join template {}: {error}",
            path.display()
        ))
    })?;
    rendered.push(b'\n');
    if path.parent().is_none() {
        return Err(failure_message(format!(
            "machine join template path {} has no parent directory",
            path.display()
        )));
    }
    if path.file_name().and_then(|name| name.to_str()).is_none() {
        return Err(failure_message(format!(
            "machine join template path {} has no UTF-8 file name",
            path.display()
        )));
    }
    let mut file = AtomicWriteFile::open(path).map_err(|error| {
        failure_message(format!(
            "failed to create atomic file {}: {error}",
            path.display()
        ))
    })?;
    file.write_all(&rendered).map_err(|error| {
        failure_message(format!(
            "failed to write atomic file {}: {error}",
            path.display()
        ))
    })?;
    Ok(StagedTemplate {
        path: path.to_path_buf(),
        file,
    })
}

fn failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message).expect("machine join template failure message is non-empty")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ployz_core::install::{ExactPloyzVersion, MachineJoinTemplate};

    use super::prepare_machine_join_template_release_promotion;

    #[test]
    fn absent_machine_join_template_is_not_a_core_and_is_ignored() {
        let root = tempfile::tempdir().expect("tempdir");
        let version = ExactPloyzVersion::try_new("v0.0.2-alpha.88").expect("exact version");

        prepare_machine_join_template_release_promotion(
            &root.path().join("missing.json"),
            &version,
        )
        .expect("missing template prepares as a no-op")
        .commit()
        .expect("missing template commit is a no-op");
    }

    #[test]
    fn current_machine_join_template_promotes_only_its_release_identity() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("machine-join-template.json");
        let original = ployz_test_support::fixtures::machine_join_template();
        fs::write(
            &path,
            serde_json::to_vec_pretty(&original).expect("template serializes"),
        )
        .expect("write template");
        let version = ExactPloyzVersion::try_new("v0.0.2-alpha.88").expect("exact version");

        let original_bytes = fs::read(&path).expect("read original template");
        let prepared = prepare_machine_join_template_release_promotion(&path, &version)
            .expect("template promotion prepares");
        assert_eq!(
            fs::read(&path).expect("template remains readable before commit"),
            original_bytes
        );
        prepared.commit().expect("template promotion commits");

        let promoted: MachineJoinTemplate =
            serde_json::from_slice(&fs::read(&path).expect("read promoted template"))
                .expect("promoted template is strict current shape");
        let mut expected = original;
        expected.join_bundle.material.substrate_release.version = version;
        assert_eq!(promoted, expected);
    }

    #[test]
    fn atomic_commit_failure_is_returned_after_successful_preparation() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("machine-join-template.json");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&ployz_test_support::fixtures::machine_join_template())
                .expect("template serializes"),
        )
        .expect("write template");
        let version = ExactPloyzVersion::try_new("v0.0.2-alpha.88").expect("exact version");
        let prepared = prepare_machine_join_template_release_promotion(&path, &version)
            .expect("template promotion prepares");
        fs::remove_file(&path).expect("remove target after preparation");
        fs::create_dir(&path).expect("replace target with a directory");

        let error = prepared.commit().expect_err("atomic replacement fails");

        assert!(error.as_str().contains("failed to commit atomic file"));
    }

    #[test]
    fn dropping_prepared_promotion_preserves_the_authoritative_template() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("machine-join-template.json");
        let original =
            serde_json::to_vec_pretty(&ployz_test_support::fixtures::machine_join_template())
                .expect("template serializes");
        fs::write(&path, &original).expect("write template");
        let version = ExactPloyzVersion::try_new("v0.0.2-alpha.88").expect("exact version");

        let prepared = prepare_machine_join_template_release_promotion(&path, &version)
            .expect("template promotion prepares");
        drop(prepared);

        assert_eq!(fs::read(path).expect("read original template"), original);
    }

    #[test]
    fn artifact_bearing_machine_join_template_is_rejected_without_replacing_the_file() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("machine-join-template.json");
        let mut legacy =
            serde_json::to_value(ployz_test_support::fixtures::machine_join_template())
                .expect("current template serializes");
        legacy
            .pointer_mut("/join_bundle/material")
            .and_then(serde_json::Value::as_object_mut)
            .expect("fixture carries join material")
            .insert(
                "ployzd".to_owned(),
                serde_json::json!({
                    "version": "v0.0.2-alpha.86",
                    "source": "https://example.invalid/ployzd",
                    "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "install_path": "/usr/local/bin/ployzd"
                }),
            );
        let original = serde_json::to_vec_pretty(&legacy).expect("legacy template serializes");
        fs::write(&path, &original).expect("write legacy template");
        let version = ExactPloyzVersion::try_new("v0.0.2-alpha.88").expect("exact version");

        let error = prepare_machine_join_template_release_promotion(&path, &version)
            .expect_err("artifact-bearing template fails during preparation");

        assert!(error.as_str().contains("unknown field `ployzd`"));
        assert_eq!(fs::read(path).expect("read unchanged template"), original);
    }
}
