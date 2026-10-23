//! Opened Sealed values live as 0600 files for one solve and reach BuildKit
//! only as secret mounts. The recipe names the files; the Builder writes them.

use crate::BuildError;
use std::{
    collections::BTreeMap,
    fs, io,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub(crate) const DIRECTORY: &str = "private/sealed";

/// Where one Sealed value opens for BuildKit, relative to the capture root.
#[must_use]
pub fn sealed_file(name: &str) -> PathBuf {
    Path::new(DIRECTORY).join(name)
}

/// A name that can be both a BuildKit secret id and a file name.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_+-".contains(&byte))
}

/// Opened values written for one solve. Dropping removes them on every
/// outcome, including a retained Unknown root and a dropped cancelled Build.
pub(crate) struct OpenedFiles {
    directory: Option<PathBuf>,
    files: Vec<PathBuf>,
}

impl OpenedFiles {
    pub(crate) fn write(
        root: &Path,
        opened: &BTreeMap<String, String>,
    ) -> Result<Self, BuildError> {
        let mut files = Self {
            directory: None,
            files: Vec::new(),
        };
        if opened.is_empty() {
            return Ok(files);
        }
        let directory = root.join(DIRECTORY);
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(io_error)?;
        files.directory = Some(directory);
        for (name, value) in opened {
            if !valid_name(name) {
                return Err(BuildError::Request(format!(
                    "{name:?} cannot be a Build secret name"
                )));
            }
            let path = root.join(sealed_file(name));
            files.files.push(path.clone());
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .and_then(|mut file| io::Write::write_all(&mut file, value.as_bytes()))
                .map_err(io_error)?;
        }
        Ok(files)
    }
}

impl Drop for OpenedFiles {
    fn drop(&mut self) {
        for file in &self.files {
            let _ = fs::remove_file(file);
        }
        if let Some(directory) = &self.directory {
            let _ = fs::remove_dir(directory);
        }
    }
}

fn io_error(error: io::Error) -> BuildError {
    BuildError::Request(format!("stage opened Build secrets: {}", error.kind()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Root(PathBuf);
    impl Root {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("ployz-sealed-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(root.join("private")).unwrap();
            Self(root)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn opened_files_are_private_and_vanish_when_the_guard_drops() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = Root::new();
        let opened = BTreeMap::from([
            ("NPM_TOKEN".to_owned(), "opened-token".to_owned()),
            ("OTHER".to_owned(), "two".to_owned()),
        ]);
        let guard = OpenedFiles::write(root.path(), &opened).unwrap();
        let token = root.path().join(sealed_file("NPM_TOKEN"));
        assert_eq!(fs::read_to_string(&token).unwrap(), "opened-token");
        assert_eq!(
            fs::metadata(&token).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(root.path().join(DIRECTORY))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        drop(guard);
        assert!(!root.path().join(DIRECTORY).exists());
        assert!(root.path().join("private").exists());
    }

    #[test]
    fn no_opened_values_leave_private_material_untouched() {
        let root = Root::new();
        let _guard = OpenedFiles::write(root.path(), &BTreeMap::new()).unwrap();
        assert_eq!(
            fs::read_dir(root.path().join("private")).unwrap().count(),
            0
        );
    }

    #[test]
    fn names_that_cannot_be_files_or_secret_ids_are_refused() {
        let root = Root::new();
        for name in ["", "../escape", "a/b", "with space", "a=b"] {
            let opened = BTreeMap::from([(name.to_owned(), "x".to_owned())]);
            assert!(
                OpenedFiles::write(root.path(), &opened).is_err(),
                "{name:?}"
            );
        }
        assert!(!root.path().join(DIRECTORY).exists());
    }
}
