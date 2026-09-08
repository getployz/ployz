//! Attempt-local build inputs isolate Docker execution from later source edits.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Read as _},
    os::unix::{
        ffi::OsStringExt as _,
        fs::{DirBuilderExt as _, PermissionsExt as _, symlink},
    },
    path::{Path, PathBuf},
};

use base64::Engine as _;
use sha2::{Digest as _, Sha256};

use super::ComposeError;

/// Private, attempt-scoped copies. Never persisted as deployment history.
pub(super) struct BuildInputs {
    root: PathBuf,
    captures: BTreeMap<Input, CapturedInput>,
}

#[derive(Eq, PartialEq, Ord, PartialOrd)]
enum Input {
    File(PathBuf),
    Context {
        path: PathBuf,
        dockerfile: Option<PathBuf>,
    },
}

struct CapturedInput {
    path: PathBuf,
    fingerprint: Vec<u8>,
}

struct Selection {
    paths: BTreeSet<PathBuf>,
    ignore: Option<String>,
}

impl Input {
    fn path(&self) -> &Path {
        match self {
            Self::File(path) | Self::Context { path, .. } => path,
        }
    }

    fn selection(&self) -> Result<Option<Selection>, ComposeError> {
        let Self::Context { path, dockerfile } = self else {
            return Ok(None);
        };
        #[derive(serde::Deserialize)]
        struct Response {
            paths: Vec<String>,
            ignore: Option<String>,
        }
        let response: Response = super::loader::helper(&serde_json::json!({
            "version": 1, "build_context": { "path": path, "dockerfile": dockerfile }
        }))?;
        let paths = response
            .paths
            .into_iter()
            .map(|path| {
                base64::engine::general_purpose::STANDARD
                    .decode(path)
                    .map(|bytes| PathBuf::from(std::ffi::OsString::from_vec(bytes)))
                    .map_err(|error| ComposeError::Io(format!("decode context path: {error}")))
            })
            .collect::<Result<_, _>>()?;
        Ok(Some(Selection {
            paths,
            ignore: response.ignore,
        }))
    }

    fn fingerprint(&self) -> Result<Vec<u8>, ComposeError> {
        fingerprint(self.path(), self.selection()?.as_ref()).map_err(input_error)
    }
}

impl BuildInputs {
    /// Allocate a private directory removed when this capture is dropped.
    ///
    /// # Errors
    /// Returns an I/O error if the private directory cannot be created.
    pub(super) fn new() -> Result<Self, ComposeError> {
        let root = std::env::temp_dir().join(format!("ployz-build-{}", uuid::Uuid::new_v4()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .map_err(input_error)?;
        Ok(Self {
            root,
            captures: BTreeMap::new(),
        })
    }

    /// Copy a source once, rejecting edits observed during the copy.
    ///
    /// # Errors
    /// Rejects unreadable, unstable, recursive, or unsupported filesystem inputs.
    pub(super) fn capture(&mut self, path: &Path) -> Result<PathBuf, ComposeError> {
        self.capture_input(Input::File(path.canonicalize().map_err(input_error)?))
    }

    /// Capture only the context entries selected by Docker's effective ignore file.
    ///
    /// # Errors
    /// Rejects invalid ignore patterns and unreadable or unstable included inputs.
    pub(super) fn context(
        &mut self,
        path: &Path,
        dockerfile: Option<&Path>,
    ) -> Result<PathBuf, ComposeError> {
        self.capture_input(Input::Context {
            path: path.canonicalize().map_err(input_error)?,
            dockerfile: dockerfile.map(Path::to_path_buf),
        })
    }

    fn capture_input(&mut self, input: Input) -> Result<PathBuf, ComposeError> {
        if let Some(captured) = self.captures.get(&input) {
            return Ok(captured.path.clone());
        }
        let source = input.path();
        if self.root.starts_with(source) {
            return Err(ComposeError::Invalid(
                "build context contains its capture directory".into(),
            ));
        }
        let target = self.root.join(self.captures.len().to_string());
        let selection = input.selection()?;
        let before = fingerprint(source, selection.as_ref()).map_err(input_error)?;
        copy(source, &target, source, selection.as_ref()).map_err(input_error)?;
        if before != input.fingerprint()?
            || before != fingerprint(&target, selection.as_ref()).map_err(input_error)?
        {
            return Err(ComposeError::Invalid(
                "build inputs changed during capture; retry when the source is stable".into(),
            ));
        }
        self.captures.insert(
            input,
            CapturedInput {
                path: target.clone(),
                fingerprint: before,
            },
        );
        Ok(target)
    }

    /// Check that all original inputs still match their captured fingerprints.
    ///
    /// # Errors
    /// Fails if a source changed or can no longer be fingerprinted.
    pub(super) fn verify(&self) -> Result<(), ComposeError> {
        for (input, captured) in &self.captures {
            if input.fingerprint()? != captured.fingerprint {
                return Err(ComposeError::Invalid(
                    "build inputs changed during capture; retry when the source is stable".into(),
                ));
            }
        }
        Ok(())
    }

    /// Capture a Dockerfile together with its adjacent ignore file, when present.
    ///
    /// # Errors
    /// Propagates capture failures and errors copying the ignore file.
    pub(super) fn dockerfile(&mut self, source: &Path) -> Result<PathBuf, ComposeError> {
        let captured = self.capture(source)?;
        let mut ignore = source.as_os_str().to_os_string();
        ignore.push(".dockerignore");
        let ignore = PathBuf::from(ignore);
        if ignore.exists() {
            let captured_ignore = self.capture(&ignore)?;
            let mut target = captured.as_os_str().to_os_string();
            target.push(".dockerignore");
            fs::copy(captured_ignore, target).map_err(input_error)?;
        }
        Ok(captured)
    }

    /// Write one resolved secret to an owner-readable file for Docker.
    ///
    /// # Errors
    /// Fails if the index is already used or the private file cannot be written.
    pub(super) fn secret(&self, index: usize, value: &str) -> Result<PathBuf, ComposeError> {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let path = self.root.join(format!("secret-{index}"));
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .and_then(|mut file| file.write_all(value.as_bytes()))
            .map_err(input_error)?;
        Ok(path)
    }
}

impl Drop for BuildInputs {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn input_error(error: io::Error) -> ComposeError {
    ComposeError::Io(format!("capture build inputs: {error}"))
}

fn entries(path: &Path, root: &Path, selection: Option<&Selection>) -> io::Result<Vec<PathBuf>> {
    let mut paths = fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    if let Some(selection) = selection {
        paths.retain(|path| {
            selection
                .paths
                .contains(path.strip_prefix(root).expect("entry is under root"))
        });
    }
    paths.sort();
    Ok(paths)
}

fn fingerprint(path: &Path, selection: Option<&Selection>) -> io::Result<Vec<u8>> {
    fn visit(
        path: &Path,
        root: &Path,
        selection: Option<&Selection>,
        digest: &mut Sha256,
    ) -> io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        digest.update(metadata.permissions().mode().to_le_bytes());
        if metadata.is_symlink() {
            digest.update(b"link");
            digest.update(fs::read_link(path)?.as_os_str().as_encoded_bytes());
        } else if metadata.is_dir() {
            digest.update(b"directory");
            for entry in entries(path, root, selection)? {
                let name = entry
                    .file_name()
                    .expect("directory entry")
                    .as_encoded_bytes();
                digest.update(name.len().to_le_bytes());
                digest.update(name);
                visit(&entry, root, selection, digest)?;
            }
        } else if metadata.is_file() {
            digest.update(b"file");
            digest.update(metadata.len().to_le_bytes());
            let mut file = fs::File::open(path)?;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                digest.update(buffer.get(..read).expect("read fits the supplied buffer"));
            }
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "build inputs contain a socket or special file",
            ));
        }
        Ok(())
    }
    let mut digest = Sha256::new();
    if let Some(ignore) = selection.and_then(|selection| selection.ignore.as_ref()) {
        digest.update(ignore.len().to_le_bytes());
        digest.update(ignore.as_bytes());
    }
    visit(path, path, selection, &mut digest)?;
    Ok(digest.finalize().to_vec())
}

fn copy(
    source: &Path,
    target: &Path,
    root: &Path,
    selection: Option<&Selection>,
) -> io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.is_symlink() {
        symlink(fs::read_link(source)?, target)
    } else if metadata.is_dir() {
        fs::create_dir(target)?;
        for entry in entries(source, root, selection)? {
            copy(
                &entry,
                &target.join(entry.file_name().expect("directory entry")),
                root,
                selection,
            )?;
        }
        fs::set_permissions(target, metadata.permissions())
    } else if metadata.is_file() {
        fs::copy(source, target).map(|_| ())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "build inputs contain a socket or special file",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn ignored_special_files_do_not_enter_capture() {
        let fixture = BuildInputs::new().unwrap();
        let source = fixture.root.join("source");
        fs::create_dir_all(source.join("ignored")).unwrap();
        fs::write(source.join(".dockerignore"), "ignored\n").unwrap();
        fs::write(source.join("included"), "original").unwrap();
        let _socket = UnixListener::bind(source.join("ignored/socket")).unwrap();
        rustix::fs::mknodat(
            rustix::fs::CWD,
            source.join("ignored/fifo"),
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::RUSR,
            0,
        )
        .unwrap();
        fs::write(source.join("ignored/unreadable"), "private").unwrap();
        fs::set_permissions(
            source.join("ignored/unreadable"),
            fs::Permissions::from_mode(0o0),
        )
        .unwrap();
        fs::create_dir(source.join("ignored/locked")).unwrap();
        fs::set_permissions(
            source.join("ignored/locked"),
            fs::Permissions::from_mode(0o0),
        )
        .unwrap();
        let mut inputs = BuildInputs::new().unwrap();
        let target = inputs.context(&source, None).unwrap();
        assert!(!target.join("ignored").exists());
        assert_eq!(
            fs::read_to_string(target.join("included")).unwrap(),
            "original"
        );
        fs::write(source.join("ignored/new"), "ignored edit").unwrap();
        inputs.verify().unwrap();
        fs::write(source.join("included"), "changed").unwrap();
        let error = inputs.verify().unwrap_err().to_string();
        assert!(error.contains("retry when the source is stable"), "{error}");
        assert!(!error.contains("correct the reported value"), "{error}");
        assert!(!error.contains("remove the unsupported setting"), "{error}");
        fs::set_permissions(
            source.join("ignored/locked"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }

    #[test]
    fn context_rules_reinclude_files_and_keep_dockerfiles_separate() {
        let fixture = BuildInputs::new().unwrap();
        let source = &fixture.root;
        fs::create_dir(source.join("cache")).unwrap();
        fs::write(source.join("cache/keep"), "keep").unwrap();
        fs::write(source.join("cache/drop"), "drop").unwrap();
        let first = source.join("first.Dockerfile");
        let second = source.join("second.Dockerfile");
        fs::write(&first, "FROM scratch").unwrap();
        fs::write(&second, "FROM scratch").unwrap();
        fs::write(source.join(".dockerignore"), "cache\n").unwrap();
        fs::write(
            source.join("first.Dockerfile.dockerignore"),
            "\u{feff}# comment\n /cache/ \n!**/keep\n",
        )
        .unwrap();
        // An empty Dockerfile-specific file overrides the root ignore file too.
        fs::write(source.join("second.Dockerfile.dockerignore"), "").unwrap();
        let mut inputs = BuildInputs::new().unwrap();
        let one = inputs.context(source, Some(&first)).unwrap();
        let two = inputs.context(source, Some(&second)).unwrap();
        let root = inputs.context(source, None).unwrap();
        assert_ne!(one, two);
        assert!(one.join("cache/keep").exists());
        assert!(!one.join("cache/drop").exists());
        assert!(two.join("cache/drop").exists());
        assert!(!root.join("cache").exists());
        inputs.verify().unwrap();
        fs::write(source.join("first.Dockerfile.dockerignore"), "cache\n").unwrap();
        assert!(inputs.verify().is_err());
    }

    #[test]
    fn included_special_files_and_invalid_patterns_still_fail() {
        let fixture = BuildInputs::new().unwrap();
        let source = &fixture.root;
        let _socket = UnixListener::bind(source.join("socket")).unwrap();
        let mut inputs = BuildInputs::new().unwrap();
        assert!(
            inputs
                .context(source, None)
                .unwrap_err()
                .to_string()
                .contains("special file")
        );
        fs::write(source.join(".dockerignore"), "socket\n!socket\n").unwrap();
        assert!(
            inputs
                .context(source, None)
                .unwrap_err()
                .to_string()
                .contains("special file")
        );
        fs::write(source.join(".dockerignore"), "[\n").unwrap();
        assert!(inputs.context(source, None).is_err());
    }
}
