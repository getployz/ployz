//! Attempt-local build inputs isolate Docker execution from later source edits.

use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read as _},
    os::unix::fs::{DirBuilderExt as _, PermissionsExt as _, symlink},
    path::{Path, PathBuf},
};

use sha2::{Digest as _, Sha256};

use super::ComposeError;

/// Private, attempt-scoped copies. Never persisted as deployment history.
pub(super) struct BuildInputs {
    root: PathBuf,
    paths: BTreeMap<PathBuf, PathBuf>,
    fingerprints: BTreeMap<PathBuf, Vec<u8>>,
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
            paths: BTreeMap::new(),
            fingerprints: BTreeMap::new(),
        })
    }

    /// Copy a source once, rejecting edits observed during the copy.
    ///
    /// # Errors
    /// Rejects unreadable, unstable, recursive, or unsupported filesystem inputs.
    pub(super) fn capture(&mut self, path: &Path) -> Result<PathBuf, ComposeError> {
        let source = path.canonicalize().map_err(input_error)?;
        if let Some(captured) = self.paths.get(&source) {
            return Ok(captured.clone());
        }
        if self.root.starts_with(&source) {
            return Err(ComposeError::Invalid(
                "build context contains its capture directory".into(),
            ));
        }
        let target = self.root.join(self.paths.len().to_string());
        // ponytail: copy the complete context, including ignored files. Apply Docker's
        // ignore rules here only if large contexts make this measured overhead matter.
        let before = fingerprint(&source).map_err(input_error)?;
        copy(&source, &target).map_err(input_error)?;
        if before != fingerprint(&source).map_err(input_error)?
            || before != fingerprint(&target).map_err(input_error)?
        {
            return Err(ComposeError::Invalid(
                "build inputs changed during capture; retry when the source is stable".into(),
            ));
        }
        self.fingerprints.insert(source.clone(), before);
        self.paths.insert(source, target.clone());
        Ok(target)
    }

    /// Check that all original inputs still match their captured fingerprints.
    ///
    /// # Errors
    /// Fails if a source changed or can no longer be fingerprinted.
    pub(super) fn verify(&self) -> Result<(), ComposeError> {
        for (source, captured) in &self.fingerprints {
            if fingerprint(source).map_err(input_error)? != *captured {
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

fn entries(path: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    paths.sort();
    Ok(paths)
}

fn fingerprint(path: &Path) -> io::Result<Vec<u8>> {
    fn visit(path: &Path, digest: &mut Sha256) -> io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        digest.update(metadata.permissions().mode().to_le_bytes());
        if metadata.is_symlink() {
            digest.update(b"link");
            digest.update(fs::read_link(path)?.as_os_str().as_encoded_bytes());
        } else if metadata.is_dir() {
            digest.update(b"directory");
            for entry in entries(path)? {
                let name = entry
                    .file_name()
                    .expect("directory entry")
                    .as_encoded_bytes();
                digest.update(name.len().to_le_bytes());
                digest.update(name);
                visit(&entry, digest)?;
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
    visit(path, &mut digest)?;
    Ok(digest.finalize().to_vec())
}

fn copy(source: &Path, target: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.is_symlink() {
        symlink(fs::read_link(source)?, target)
    } else if metadata.is_dir() {
        fs::create_dir(target)?;
        for entry in entries(source)? {
            copy(
                &entry,
                &target.join(entry.file_name().expect("directory entry")),
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
