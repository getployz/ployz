//! Opened Sealed values live as 0600 files for one solve and reach BuildKit
//! as secret mounts and declared Dockerfile ARGs. The Builder writes the files.

use crate::{BuildError, Target};
use ployz_core::OpenedSealed;
use std::{
    collections::BTreeMap,
    fs, io,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
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

/// Where one attempt stages its opened Sealed values: its own capture's `private/sealed`,
/// which the attempt's owner can take back while the solve still runs. The execution writes
/// each value under the lock [`Self::revoke`] takes, so a value is either written first and
/// revoked, or never written.
#[derive(Clone)]
pub struct SealedStaging(Arc<Staging>);

struct Staging {
    root: PathBuf,
    revoked: Mutex<bool>,
}

impl SealedStaging {
    pub(crate) fn under(root: &Path) -> Self {
        Self(Arc::new(Staging {
            root: root.to_owned(),
            revoked: Mutex::new(false),
        }))
    }

    /// Remove every value staged here and stage none again, so a solve still running reads
    /// none of them again. The directory itself stays for [`Staged`]'s own drop.
    ///
    /// # Errors
    /// A staged file that could not be removed.
    pub fn revoke(&self) -> io::Result<()> {
        let mut revoked = self.0.revoked.lock().expect("staging lock");
        *revoked = true;
        let entries = match fs::read_dir(self.0.root.join(DIRECTORY)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        for entry in entries {
            match fs::remove_file(entry?.path()) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                Ok(()) | Err(_) => {}
            }
        }
        Ok(())
    }

    /// Run `write` unless [`Self::revoke`] ran, under its lock.
    fn write<T>(&self, write: impl FnOnce() -> io::Result<T>) -> Result<T, BuildError> {
        let revoked = self.0.revoked.lock().expect("staging lock");
        if *revoked {
            return Err(BuildError::Cancelled);
        }
        write().map_err(io_error)
    }
}

/// Opened values written for one solve. Dropping removes them on every
/// outcome, including a retained Unknown root and a dropped cancelled Build.
pub struct Staged {
    directory: Option<PathBuf>,
    files: Vec<PathBuf>,
    compose_override: Option<PathBuf>,
}

impl Staged {
    /// Write `opened` into `staging`: a file per value, and the Compose override that
    /// passes the declared Dockerfile ARGs of `targets`.
    ///
    /// # Errors
    /// Fails on a name that can't be a secret id, an ARG nothing opened, a write, or a
    /// staging its owner revoked.
    pub fn stage(
        staging: &SealedStaging,
        opened: &OpenedSealed,
        targets: &[Target],
    ) -> Result<Self, BuildError> {
        let mut staged = Self::write(staging, opened)?;
        staged.compose_override = staged.args(staging, targets, opened)?;
        Ok(staged)
    }

    /// The Compose file that passes opened values as ARGs.
    #[must_use]
    pub fn compose_override(&self) -> Option<&Path> {
        self.compose_override.as_deref()
    }

    fn args(
        &mut self,
        staging: &SealedStaging,
        targets: &[Target],
        opened: &OpenedSealed,
    ) -> Result<Option<PathBuf>, BuildError> {
        let root = &staging.0.root;
        let bytes = match fs::read(crate::capture::sealed_args(root)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error(error)),
        };
        let declared: BTreeMap<String, Vec<String>> = serde_json::from_slice(&bytes)
            .map_err(|_| BuildError::Request("invalid Sealed Build argument names".into()))?;
        let mut services = BTreeMap::new();
        for (target, names) in declared {
            if !targets.iter().any(|admitted| admitted.name == target) {
                return Err(BuildError::Request(
                    "Sealed arguments name an unadmitted target".into(),
                ));
            }
            let mut args = BTreeMap::new();
            for name in names {
                let value = opened.get(&name).ok_or_else(|| {
                    BuildError::Request("Sealed Build argument was not opened".into())
                })?;
                args.insert(name, value);
            }
            if !args.is_empty() {
                services.insert(target, serde_json::json!({"build": {"args": args}}));
            }
        }
        if services.is_empty() {
            return Ok(None);
        }
        let path = root.join(DIRECTORY).join("args.compose.yaml");
        let contents = serde_json::to_string(&serde_json::json!({"services": services}))
            .expect("Compose arguments encode")
            .replace('$', "$$");
        self.files.push(path.clone());
        staging.write(|| private_file(&path, contents.as_bytes()))?;
        Ok(Some(path))
    }

    fn write(staging: &SealedStaging, opened: &OpenedSealed) -> Result<Self, BuildError> {
        let mut files = Self {
            directory: None,
            files: Vec::new(),
            compose_override: None,
        };
        if opened.is_empty() {
            return Ok(files);
        }
        let root = &staging.0.root;
        let directory = root.join(DIRECTORY);
        staging.write(|| fs::DirBuilder::new().mode(0o700).create(&directory))?;
        files.directory = Some(directory);
        for (name, value) in opened.values() {
            if !valid_name(name) {
                return Err(BuildError::Request(format!(
                    "{name:?} cannot be a Build secret name"
                )));
            }
            let path = root.join(sealed_file(name));
            files.files.push(path.clone());
            staging.write(|| private_file(&path, value.as_bytes()))?;
        }
        Ok(files)
    }
}

/// A new file only its owner reads, holding `contents`.
fn private_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut file| io::Write::write_all(&mut file, contents))
}

impl Drop for Staged {
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

    fn opened(values: BTreeMap<String, String>) -> OpenedSealed {
        OpenedSealed::new(values, Vec::new())
    }

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
        let values = BTreeMap::from([
            ("NPM_TOKEN".to_owned(), "opened-token".to_owned()),
            ("OTHER".to_owned(), "two".to_owned()),
        ]);
        let guard = Staged::write(&SealedStaging::under(root.path()), &opened(values)).unwrap();
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
        let _guard =
            Staged::write(&SealedStaging::under(root.path()), &opened(BTreeMap::new())).unwrap();
        assert_eq!(
            fs::read_dir(root.path().join("private")).unwrap().count(),
            0
        );
    }

    fn targets() -> Vec<Target> {
        vec![Target {
            name: "api".into(),
            platforms: Vec::new(),
        }]
    }

    #[test]
    #[expect(clippy::indexing_slicing, reason = "fixed Compose fixture")]
    fn declared_args_get_private_values_and_share_secret_cleanup() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = Root::new();
        fs::write(
            root.path().join("private/sealed-args.json"),
            r#"{"api":["NPM_TOKEN"]}"#,
        )
        .unwrap();
        let value = "literal $TOKEN ${HOME} $$ \\\"\nsecond line";
        let opened = opened(BTreeMap::from([
            ("NPM_TOKEN".into(), value.into()),
            ("MOUNT_ONLY".into(), "mount-only-value".into()),
        ]));
        let guard = Staged::stage(&SealedStaging::under(root.path()), &opened, &targets()).unwrap();
        let file = guard.compose_override().unwrap().to_owned();
        let encoded = fs::read_to_string(&file).unwrap();
        let compose: serde_json::Value = serde_json::from_str(&encoded.replace("$$", "$")).unwrap();
        assert_eq!(
            compose["services"]["api"]["build"]["args"]["NPM_TOKEN"],
            value
        );
        assert!(!encoded.contains("mount-only-value"));
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::read_to_string(root.path().join(sealed_file("MOUNT_ONLY"))).unwrap(),
            "mount-only-value"
        );
        drop(guard);
        assert!(!file.exists());
        assert!(!root.path().join(DIRECTORY).exists());
    }

    #[test]
    fn invalid_arg_material_fails_without_leaving_opened_files() {
        let root = Root::new();
        let opened = opened(BTreeMap::from([("NPM_TOKEN".into(), "dummy".into())]));
        for contents in [
            "bad json",
            r#"{"other":["NPM_TOKEN"]}"#,
            r#"{"api":["MISSING"]}"#,
        ] {
            fs::write(root.path().join("private/sealed-args.json"), contents).unwrap();
            assert!(
                Staged::stage(&SealedStaging::under(root.path()), &opened, &targets()).is_err()
            );
            assert!(!root.path().join(DIRECTORY).exists());
        }
    }

    #[test]
    fn names_that_cannot_be_files_or_secret_ids_are_refused() {
        let root = Root::new();
        for name in ["", "../escape", "a/b", "with space", "a=b"] {
            let opened = opened(BTreeMap::from([(name.to_owned(), "x".to_owned())]));
            assert!(
                Staged::write(&SealedStaging::under(root.path()), &opened).is_err(),
                "{name:?}"
            );
        }
        assert!(!root.path().join(DIRECTORY).exists());
    }
}
