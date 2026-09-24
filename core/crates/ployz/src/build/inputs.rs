//! Attempt-local build inputs isolate Docker execution from later source edits.

use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read as _},
    os::unix::fs::{DirBuilderExt as _, PermissionsExt as _, symlink},
    path::{Path, PathBuf},
};

use sha2::{Digest as _, Sha256};

use super::{
    Error,
    ignore::{Rules, Selection, own_ignore, select},
};

/// Private, attempt-scoped copies. Never persisted as deployment history.
pub(super) struct BuildInputs {
    root: PathBuf,
    captures: BTreeMap<Input, CapturedInput>,
}

#[derive(Eq, PartialEq, Ord, PartialOrd)]
enum Input {
    File(PathBuf),
    Context { path: PathBuf, rules: Rules },
}

struct CapturedInput {
    path: PathBuf,
    fingerprint: Vec<u8>,
}

impl Input {
    fn path(&self) -> &Path {
        match self {
            Self::File(path) | Self::Context { path, .. } => path,
        }
    }

    fn selection(&self) -> Result<Option<Selection>, Error> {
        let Self::Context { path, rules } = self else {
            return Ok(None);
        };
        select(path, rules).map(Some)
    }

    fn fingerprint(&self) -> Result<Vec<u8>, Error> {
        fingerprint(self.path(), self.selection()?.as_ref()).map_err(input_error)
    }
}

impl BuildInputs {
    /// Allocate a private directory removed when this capture is dropped.
    ///
    /// # Errors
    /// Returns an I/O error if the private directory cannot be created.
    pub(super) fn new() -> Result<Self, Error> {
        let root = std::env::temp_dir().join(format!("ployz-build-{}", uuid::Uuid::new_v4()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .map_err(input_error)?;
        for directory in ["source", "private", "private/docker"] {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(root.join(directory))
                .map_err(input_error)?;
        }
        let inputs = Self {
            root,
            captures: BTreeMap::new(),
        };
        // The Build Machine requires registry configuration; these Builds carry no credentials.
        inputs.private(&inputs.root.join("private/docker/config.json"), b"{}")?;
        Ok(inputs)
    }

    /// Copy a source once, rejecting edits observed during the copy.
    ///
    /// # Errors
    /// Rejects unreadable, unstable, recursive, or unsupported filesystem inputs.
    fn capture(&mut self, path: &Path) -> Result<PathBuf, Error> {
        self.capture_input(Input::File(path.canonicalize().map_err(input_error)?))
    }

    /// Capture only the context entries selected by Docker's effective ignore file.
    ///
    /// # Errors
    /// Rejects invalid ignore patterns and unreadable or unstable included inputs.
    pub(super) fn context(&mut self, path: &Path, dockerfile: &Path) -> Result<PathBuf, Error> {
        self.capture_input(Input::Context {
            path: path.canonicalize().map_err(input_error)?,
            rules: Rules::Dockerfile(dockerfile.to_owned()),
        })
    }

    /// Railpack's merged exclusions are resolved inside capture, before any
    /// source reaches the execution host. The config remains a preparation input.
    ///
    /// # Errors
    /// Rejects invalid exclusions and unreadable or unstable included inputs.
    pub(super) fn railpack_context(
        &mut self,
        path: &Path,
        variables: &BTreeMap<String, String>,
    ) -> Result<PathBuf, Error> {
        let config = variables
            .get("RAILPACK_CONFIG_FILE")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        self.capture_input(Input::Context {
            path: path.canonicalize().map_err(input_error)?,
            rules: Rules::Railpack(config),
        })
    }

    /// Paths in the capture describe its layout, never its temporary location.
    pub(super) fn relative(&self, path: &Path) -> PathBuf {
        path.strip_prefix(&self.root)
            .expect("owned capture path")
            .to_owned()
    }

    fn capture_input(&mut self, input: Input) -> Result<PathBuf, Error> {
        if let Some(captured) = self.captures.get(&input) {
            return Ok(captured.path.clone());
        }
        let source = input.path();
        if self.root.starts_with(source) {
            return Err(Error::Invalid(
                "build context contains its capture directory".into(),
            ));
        }
        let target = self
            .root
            .join("source")
            .join(self.captures.len().to_string());
        if !matches!(input, Input::Context { .. })
            && !fs::metadata(source).map_err(input_error)?.is_file()
        {
            return Err(Error::Invalid("Dockerfile must be a regular file".into()));
        }
        let selection = input.selection()?;
        let before = fingerprint(source, selection.as_ref()).map_err(input_error)?;
        copy(source, &target, source, selection.as_ref()).map_err(input_error)?;
        if before != input.fingerprint()?
            || before != fingerprint(&target, selection.as_ref()).map_err(input_error)?
        {
            return Err(Error::Invalid(
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

    /// The private directory holding every captured input for this attempt.
    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    /// Check that all original inputs still match their captured fingerprints.
    ///
    /// # Errors
    /// Fails if a source changed or can no longer be fingerprinted.
    pub(super) fn verify(&self) -> Result<(), Error> {
        for (input, captured) in &self.captures {
            if input.fingerprint()? != captured.fingerprint {
                return Err(Error::Invalid(
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
    pub(super) fn dockerfile(&mut self, source: &Path) -> Result<PathBuf, Error> {
        let captured = self.capture(source)?;
        let ignore = own_ignore(source);
        if ignore.exists() {
            let captured_ignore = self.capture(&ignore)?;
            fs::copy(captured_ignore, own_ignore(&captured)).map_err(input_error)?;
        }
        Ok(captured)
    }

    /// Keep Railpack recipe metadata and variables in private upload material.
    ///
    /// # Errors
    /// Fails if the private file cannot be written.
    pub(super) fn railpack(&self, recipe: &ployz_build::Railpack) -> Result<(), Error> {
        // The Build Machine reads a list of recipes.
        let bytes =
            serde_json::to_vec(&[recipe]).map_err(|error| input_error(io::Error::other(error)))?;
        self.private(&self.root.join("private/railpack.json"), &bytes)
    }

    /// Write the captured Buildx file beside its sources.
    ///
    /// # Errors
    /// Fails if the private file cannot be written.
    pub(super) fn recipe(&self, yaml: &str) -> Result<(), Error> {
        self.private(&self.root.join("compose.yaml"), yaml.as_bytes())
    }

    fn private(&self, path: &Path, content: &[u8]) -> Result<(), Error> {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .and_then(|mut file| file.write_all(content))
            .map_err(input_error)
    }
}

impl Drop for BuildInputs {
    fn drop(&mut self) {
        // Source directory modes belong to the payload, but read-only source
        // must not prevent removal of this attempt's private material.
        let _ = make_removable(&self.root);
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn make_removable(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(metadata.permissions().mode() | 0o700),
        )?;
        for entry in fs::read_dir(path)? {
            make_removable(&entry?.path())?;
        }
    }
    Ok(())
}

fn input_error(error: io::Error) -> Error {
    Error::Io(format!("capture build inputs: {error}"))
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
            let mut file = source_file(path, root)?;
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
    if let Some(selection) = selection {
        digest.update(selection.ignore.len().to_le_bytes());
        digest.update(&selection.ignore);
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
        validate_link(source, root, selection)?;
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
        let mut input = source_file(source, root)?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?;
        io::copy(&mut input, &mut output)?;
        fs::set_permissions(target, metadata.permissions())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "build inputs contain a socket or special file",
        ))
    }
}

// Open every component without following links, so a source edit cannot turn a
// file or its parent into a route to uncaptured host bytes during copy/hash.
fn source_file(path: &Path, root: &Path) -> io::Result<fs::File> {
    use rustix::fs::{Mode, OFlags, open, openat};
    let root = if path == root {
        root.parent().expect("file has parent")
    } else {
        root
    };
    let mut fd = open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW,
        Mode::empty(),
    )?;
    let relative = path.strip_prefix(root).map_err(|_| link_error())?;
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(link_error());
        };
        fd = openat(
            &fd,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::empty(),
        )?;
    }
    let file = fs::File::from(fd);
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "build source is not a regular file",
        ));
    }
    Ok(file)
}

fn link_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "build symlink escapes or refers to uncaptured source",
    )
}

// Resolve one hop at a time inside the selected context. Checking only a final
// canonical path would admit links that leave the context and come back in.
fn validate_link(path: &Path, root: &Path, selection: Option<&Selection>) -> io::Result<()> {
    use std::{collections::VecDeque, path::Component};
    let mut pending = VecDeque::from([path
        .strip_prefix(root)
        .map_err(|_| link_error())?
        .to_owned()]);
    let mut resolved = PathBuf::new();
    let mut links = 0;
    while let Some(next) = pending.pop_front() {
        let mut components = next.components();
        while let Some(component) = components.next() {
            match component {
                Component::CurDir => continue,
                Component::ParentDir if resolved.pop() => continue,
                Component::Normal(name) => resolved.push(name),
                Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                    return Err(link_error());
                }
            }
            if selection.is_some_and(|selection| !selection.paths.contains(&resolved)) {
                // This target is absent after filtering. Preserve the dangling
                // link, but still check its remaining components for escapes.
                continue;
            }
            let candidate = root.join(&resolved);
            if fs::symlink_metadata(&candidate)
                .map_err(|_| link_error())?
                .is_symlink()
            {
                links += 1;
                if links > 40 {
                    return Err(link_error());
                }
                let target = fs::read_link(candidate)?;
                resolved.pop();
                pending.push_front(components.as_path().to_owned());
                pending.push_front(target);
                break;
            }
        }
    }
    Ok(())
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
        assert!(
            std::process::Command::new("mkfifo")
                .arg(source.join("ignored/fifo"))
                .status()
                .unwrap()
                .success()
        );
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
        let target = inputs.context(&source, &source.join("Dockerfile")).unwrap();
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
        let one = inputs.context(source, &first).unwrap();
        let two = inputs.context(source, &second).unwrap();
        let root = inputs.context(source, &source.join("Dockerfile")).unwrap();
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
    fn the_captured_recipe_stays_private() {
        let inputs = BuildInputs::new().unwrap();
        inputs.recipe("services: {}\n").unwrap();
        let recipe = inputs.root().join("compose.yaml");
        let mode = fs::metadata(&recipe).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(fs::read_to_string(&recipe).unwrap(), "services: {}\n");
        let root = inputs.root().to_owned();
        drop(inputs);
        assert!(!root.exists());
    }

    #[test]
    fn included_special_files_and_invalid_patterns_still_fail() {
        let fixture = BuildInputs::new().unwrap();
        let source = &fixture.root;
        let _socket = UnixListener::bind(source.join("socket")).unwrap();
        let mut inputs = BuildInputs::new().unwrap();
        assert!(
            inputs
                .context(source, &source.join("Dockerfile"))
                .unwrap_err()
                .to_string()
                .contains("special file")
        );
        fs::write(source.join(".dockerignore"), "socket\n!socket\n").unwrap();
        assert!(
            inputs
                .context(source, &source.join("Dockerfile"))
                .unwrap_err()
                .to_string()
                .contains("special file")
        );
        fs::write(source.join(".dockerignore"), "[\n").unwrap();
        assert!(inputs.context(source, &source.join("Dockerfile")).is_err());
    }
}
