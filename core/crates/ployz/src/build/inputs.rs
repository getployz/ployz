//! Attempt-local build inputs isolate Docker execution from later source edits.

use std::{
    collections::{BTreeMap, BTreeSet},
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

    /// Keep the Build's Sealed values, still sealed, in private upload material.
    ///
    /// # Errors
    /// Fails if the private file cannot be written.
    pub(super) fn sealed(&self, capture: &super::sealed::SealedCapture<'_>) -> Result<(), Error> {
        let bytes =
            serde_json::to_vec(capture).map_err(|error| input_error(io::Error::other(error)))?;
        self.private(&self.root.join("private/sealed.json"), &bytes)
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
            return Err(special_file(path, root));
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
        Err(special_file(source, root))
    }
}

fn special_file(path: &Path, root: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "build inputs contain a socket or special file at {:?}",
            path.strip_prefix(root).expect("input is under its root")
        ),
    )
}

/// What an upload of a directory holds, listed once so its digest and its archive
/// always cover the same paths.
pub struct UploadListing {
    root: PathBuf,
    paths: BTreeSet<PathBuf>,
}

impl UploadListing {
    /// List `root`. In a Git checkout that is what Git keeps, tracked or untracked but
    /// not ignored, and an error when Git can't say. Elsewhere it is every path.
    ///
    /// # Errors
    /// Fails on unreadable entries, and in a Git checkout when `git ls-files` fails.
    pub fn read(root: &Path) -> Result<Self, Error> {
        let read = || -> io::Result<_> {
            for dir in root.canonicalize()?.ancestors() {
                match fs::symlink_metadata(dir.join(".git")) {
                    Ok(_) => return git_kept_paths(root),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(_) => return Ok(None),
                }
            }
            let mut paths = BTreeSet::new();
            walk(root, root, &mut paths)?;
            Ok(Some(paths))
        };
        let paths = read()
            .map_err(|error| Error::Io(format!("read uploaded source: {error}")))?
            .ok_or_else(|| Error::Io(GIT_UNREADABLE.into()))?;
        Ok(Self {
            root: root.to_owned(),
            paths,
        })
    }

    /// Whether the listing holds no path at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// The Uploaded Source's content digest: lowercase hex sha256 over every listed
    /// path, byte, mode and link.
    ///
    /// # Errors
    /// Fails on unreadable entries and sockets or special files.
    pub fn digest(&self) -> Result<String, Error> {
        let digest = fingerprint(
            &self.root,
            Some(&Selection {
                paths: self.paths.clone(),
                ignore: Vec::new(),
            }),
        )
        .map_err(|error| Error::Io(format!("read uploaded source: {error}")))?;
        Ok(hex::encode(digest))
    }

    /// The upload as Cloud takes it: a gzipped tar of the listed paths, under one
    /// top-level `source` directory, with modes and links as they are.
    ///
    /// # Errors
    /// Fails on unreadable entries.
    pub(crate) fn archive(&self) -> Result<Vec<u8>, Error> {
        (|| {
            let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            let mut archive = tar::Builder::new(gzip);
            archive.follow_symlinks(false);
            archive.append_path_with_name(&self.root, "source")?;
            for path in &self.paths {
                archive
                    .append_path_with_name(self.root.join(path), Path::new("source").join(path))?;
            }
            archive.into_inner()?.finish()
        })()
        .map_err(|error| Error::Io(format!("archive uploaded source: {error}")))
    }
}

/// The content digest of an upload of `root`.
///
/// # Errors
/// Fails as [`UploadListing::read`] and [`UploadListing::digest`] do.
pub fn content_digest(root: &Path) -> Result<String, Error> {
    UploadListing::read(root)?.digest()
}

const GIT_UNREADABLE: &str =
    "Couldn't list files with git. Install git, or run ployz up outside the Git checkout.";

fn walk(path: &Path, root: &Path, paths: &mut BTreeSet<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?.path();
        paths.insert(
            entry
                .strip_prefix(root)
                .expect("entry is under root")
                .to_owned(),
        );
        if fs::symlink_metadata(&entry)?.is_dir() {
            walk(&entry, root, paths)?;
        }
    }
    Ok(())
}

/// The files `git ls-files` keeps under `root` and their directories, or `None` when
/// Git can't run or fails.
fn git_kept_paths(root: &Path) -> io::Result<Option<BTreeSet<PathBuf>>> {
    use std::os::unix::ffi::OsStrExt as _;
    let output = match std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()
    {
        Ok(output) if output.status.success() => output,
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut paths = BTreeSet::new();
    for file in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|file| !file.is_empty())
    {
        let file = Path::new(std::ffi::OsStr::from_bytes(file));
        // `--cached` still lists a tracked file deleted from the working tree.
        match fs::symlink_metadata(root.join(file)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
            Ok(_) => {}
        }
        paths.extend(
            file.ancestors()
                .filter(|path| !path.as_os_str().is_empty())
                .map(Path::to_owned),
        );
    }
    Ok(Some(paths))
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
    fn content_digest_covers_paths_bytes_modes_and_links() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/app"), "one").unwrap();
        symlink("src/app", root.join("link")).unwrap();
        let digest = content_digest(root).unwrap();
        assert!(ployz_core::is_lower_hex(&digest, 64), "{digest}");
        let changed = |edit: &dyn Fn()| {
            edit();
            content_digest(root).unwrap()
        };
        let mut seen = BTreeSet::from([digest]);
        let edits: [&dyn Fn(); 5] = [
            &|| fs::write(root.join("src/app"), "two").unwrap(),
            &|| {
                fs::set_permissions(root.join("src/app"), fs::Permissions::from_mode(0o755))
                    .unwrap()
            },
            &|| {
                fs::remove_file(root.join("link")).unwrap();
                symlink("src", root.join("link")).unwrap();
            },
            &|| fs::rename(root.join("src/app"), root.join("src/main")).unwrap(),
            &|| fs::write(root.join("src/.git"), "").unwrap(),
        ];
        for edit in edits {
            assert!(seen.insert(changed(edit)), "every edit changes the digest");
        }
    }

    #[test]
    fn an_upload_archive_unpacks_to_the_same_digest() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        fs::create_dir(root.join("bin")).unwrap();
        fs::write(root.join("bin/run"), "#!/bin/sh").unwrap();
        fs::set_permissions(root.join("bin/run"), fs::Permissions::from_mode(0o750)).unwrap();
        fs::set_permissions(root.join("bin"), fs::Permissions::from_mode(0o770)).unwrap();
        symlink("bin/run", root.join("run")).unwrap();
        let archive = UploadListing::read(root).unwrap().archive().unwrap();
        let unpacked = tempfile::tempdir().unwrap();
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive.as_slice()));
        tar.set_preserve_permissions(true);
        tar.unpack(unpacked.path()).unwrap();
        let source = unpacked.path().join("source");
        assert_eq!(
            content_digest(&source).unwrap(),
            content_digest(root).unwrap()
        );
    }

    #[test]
    fn an_upload_of_a_git_checkout_holds_only_what_git_keeps() {
        let checkout = tempfile::tempdir().unwrap();
        let root = checkout.path();
        let git = |args: &[&str]| git(root, args);
        git(&["init", "--quiet"]);
        fs::write(root.join(".gitignore"), "node_modules/\n.env\ndist/\n").unwrap();
        fs::write(root.join(".env"), "SECRET=1").unwrap();
        fs::create_dir_all(root.join("node_modules/left-pad")).unwrap();
        fs::write(root.join("node_modules/left-pad/index.js"), "").unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/app.js"), "tracked").unwrap();
        fs::write(root.join("notes.txt"), "untracked").unwrap();
        fs::create_dir(root.join("dist")).unwrap();
        fs::write(root.join("dist/vendor.js"), "forced").unwrap();
        fs::write(root.join("dist/build.js"), "ignored").unwrap();
        git(&["add", "src/app.js"]);
        git(&["add", "--force", "dist/vendor.js"]);
        let unpacked = unpack(root);
        let source = unpacked.path().join("source");
        assert_eq!(
            files(&source, &source),
            [".gitignore", "dist/vendor.js", "notes.txt", "src/app.js"].map(PathBuf::from),
            "Git's kept files, tracked or untracked, and nothing it ignores"
        );
        assert_eq!(
            content_digest(&source).unwrap(),
            content_digest(root).unwrap(),
            "the digest leaves out what the archive leaves out"
        );
    }

    #[test]
    fn an_upload_of_a_checkout_git_cannot_read_is_refused() {
        let checkout = tempfile::tempdir().unwrap();
        let root = checkout.path();
        git(root, &["init", "--quiet"]);
        fs::write(root.join(".gitignore"), ".env\n").unwrap();
        fs::write(root.join(".env"), "SECRET=1").unwrap();
        fs::write(root.join(".git/index"), "not an index").unwrap();
        assert_eq!(
            UploadListing::read(root).err().unwrap().to_string(),
            GIT_UNREADABLE,
            "nothing is archived when Git can't say what it ignores"
        );
    }

    #[test]
    fn an_upload_with_a_dangling_git_link_is_refused() {
        let checkout = tempfile::tempdir().unwrap();
        let root = checkout.path();
        fs::write(root.join("app"), "").unwrap();
        symlink("/missing-gitdir", root.join(".git")).unwrap();
        assert_eq!(
            UploadListing::read(root).err().unwrap().to_string(),
            GIT_UNREADABLE,
            "a .git that points nowhere is a checkout Git can't read"
        );
    }

    #[test]
    fn an_upload_archives_the_files_its_digest_listed() {
        let checkout = tempfile::tempdir().unwrap();
        let root = checkout.path();
        git(root, &["init", "--quiet"]);
        fs::write(root.join("app.js"), "listed").unwrap();
        let listing = UploadListing::read(root).unwrap();
        let digest = listing.digest().unwrap();
        fs::write(root.join("late.js"), "written after the listing").unwrap();
        let archive = listing.archive().unwrap();
        let unpacked = tempfile::tempdir().unwrap();
        tar::Archive::new(flate2::read::GzDecoder::new(archive.as_slice()))
            .unpack(unpacked.path())
            .unwrap();
        assert_eq!(
            content_digest(&unpacked.path().join("source")).unwrap(),
            digest
        );
    }

    fn git(root: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn an_upload_outside_a_git_checkout_holds_every_file() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        fs::write(root.join(".gitignore"), "node_modules/\n.env\n").unwrap();
        fs::write(root.join(".env"), "SECRET=1").unwrap();
        fs::create_dir(root.join("node_modules")).unwrap();
        fs::write(root.join("node_modules/index.js"), "").unwrap();
        let unpacked = unpack(root);
        let source = unpacked.path().join("source");
        assert_eq!(
            files(&source, &source),
            [".env", ".gitignore", "node_modules/index.js"].map(PathBuf::from)
        );
    }

    fn unpack(root: &Path) -> tempfile::TempDir {
        let unpacked = tempfile::tempdir().unwrap();
        tar::Archive::new(flate2::read::GzDecoder::new(
            UploadListing::read(root)
                .unwrap()
                .archive()
                .unwrap()
                .as_slice(),
        ))
        .unpack(unpacked.path())
        .unwrap();
        unpacked
    }

    fn files(dir: &Path, root: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                found.extend(files(&path, root));
            } else {
                found.push(path.strip_prefix(root).unwrap().to_owned());
            }
        }
        found.sort();
        found
    }

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
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path();
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
        fs::remove_file(source.join(".dockerignore")).unwrap();
        fs::remove_file(source.join("socket")).unwrap();
        fs::create_dir(source.join("cache")).unwrap();
        let _named_socket = UnixListener::bind(source.join("cache/run\nsocket")).unwrap();
        for error in [
            content_digest(source).unwrap_err(),
            inputs
                .context(source, &source.join("Dockerfile"))
                .unwrap_err(),
        ] {
            let message = error.to_string();
            assert!(message.contains("cache/run\\nsocket"), "{message}");
            assert!(!message.contains('\n'), "filename controls must be escaped");
            assert!(!message.contains(source.to_str().unwrap()));
        }
    }
}
