//! Bounded, private receiver for a captured Build. No input is runnable before Finish.

use crate::remote::InputError;
use crate::{
    Admission, BuildError, BuiltImage, Progress, Request,
    remote::{CHUNK_SIZE, Definition, Input, Kind},
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::{self, File},
    io::{self, Read, Write},
    os::unix::{
        ffi::OsStringExt,
        fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink},
    },
    path::{Component, Path, PathBuf},
};

// ponytail: fixed per-attempt ceilings; move these to Machine-local policy if
// legitimate captures exceed 2 GiB source, 16 MiB private material, or 100k entries.
const SOURCE_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
const PRIVATE_LIMIT: u64 = 16 * 1024 * 1024;
const ENTRY_LIMIT: usize = 100_000;

/// Private receiver state. Its path is never exposed to callers, and only a
/// complete, validated upload can reach the execution host.
pub struct Upload {
    root: PathBuf,
    paths: BTreeSet<PathBuf>,
    directories: Vec<(PathBuf, u32)>,
    links: BTreeMap<PathBuf, PathBuf>,
    file: Option<(File, u64)>,
    source_bytes: u64,
    private_bytes: u64,
    metadata_bytes: usize,
    state: UploadState,
    retain: bool,
}

enum UploadState {
    Receiving,
    Failed,
    Finished,
}

/// A finished capture; only this type can execute.
pub struct CompletedUpload(Upload);

impl Upload {
    /// Create private staging for one bounded upload.
    /// # Errors
    /// Reports failure to create protected staging.
    pub fn new() -> Result<Self, InputError> {
        let root =
            std::env::temp_dir().join(format!("ployz-received-build-{}", uuid::Uuid::new_v4()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .map_err(io_error)?;
        Ok(Self {
            root,
            paths: BTreeSet::new(),
            directories: Vec::new(),
            links: BTreeMap::new(),
            file: None,
            source_bytes: 0,
            private_bytes: 0,
            metadata_bytes: 0,
            state: UploadState::Receiving,
            retain: false,
        })
    }

    /// A malformed frame permanently invalidates this upload; callers cannot
    /// repair a rejected prefix and accidentally execute partial inputs.
    /// # Errors
    /// Rejects malformed, oversized, repeated, or out-of-order entries.
    pub fn accept(&mut self, frame: Input) -> Result<(), InputError> {
        if !matches!(self.state, UploadState::Receiving) {
            return Err("Build upload is already closed".into());
        }
        let result = self.accept_next(frame);
        if result.is_err() {
            self.state = UploadState::Failed;
        }
        result
    }

    fn accept_next(&mut self, frame: Input) -> Result<(), InputError> {
        if let Input::Data(bytes) = frame {
            if bytes.is_empty() || bytes.len() > CHUNK_SIZE {
                return Err("invalid Build data frame size".into());
            }
            let Some((file, remaining)) = &mut self.file else {
                return Err("Build data has no file header".into());
            };
            *remaining = remaining
                .checked_sub(bytes.len() as u64)
                .ok_or("Build file exceeds declared size")?;
            return file.write_all(&bytes).map_err(io_error);
        }
        if self
            .file
            .as_ref()
            .is_some_and(|(_, remaining)| *remaining != 0)
        {
            return Err("Build upload contains an incomplete file".into());
        }
        self.file.take();
        match frame {
            Input::Entry { path, kind, mode } => {
                self.metadata_bytes = self
                    .metadata_bytes
                    .checked_add(
                        path.len()
                            + match &kind {
                                Kind::Link { target } => target.len(),
                                Kind::Directory | Kind::File { .. } => 0,
                            },
                    )
                    .filter(|bytes| *bytes <= PRIVATE_LIMIT as usize)
                    .ok_or("Build entry metadata exceeds byte limit")?;
                let relative = received_path(path)?;
                let source = relative.starts_with("source");
                if !source
                    && !relative.starts_with("private")
                    && relative != Path::new("compose.yaml")
                {
                    return Err("Build entry is outside source and private material".into());
                }
                if mode & !0o777 != 0 {
                    return Err("Build entry has special permission bits".into());
                }
                if self.paths.len() >= ENTRY_LIMIT || !self.paths.insert(relative.clone()) {
                    return Err("Build upload exceeds entry limit or repeats a path".into());
                }
                let path = self.root.join(&relative);
                let parent = path.parent().ok_or("Build entry has no parent")?;
                if !fs::symlink_metadata(parent).map_err(io_error)?.is_dir() {
                    return Err("Build entry parent is not a directory".into());
                }
                match kind {
                    Kind::Directory => {
                        if relative == Path::new("compose.yaml") {
                            return Err("Build recipe must be a regular file".into());
                        }
                        fs::DirBuilder::new()
                            .mode(0o700)
                            .create(&path)
                            .map_err(io_error)?;
                        self.directories
                            .push((relative, if source { mode } else { 0o700 }));
                    }
                    Kind::File { size } => {
                        let (total, limit) = if source {
                            (&mut self.source_bytes, SOURCE_LIMIT)
                        } else {
                            (&mut self.private_bytes, PRIVATE_LIMIT)
                        };
                        *total = total
                            .checked_add(size)
                            .filter(|total| *total <= limit)
                            .ok_or("Build upload exceeds byte limit")?;
                        let file = fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .mode(if source { mode } else { 0o600 })
                            .open(path)
                            .map_err(io_error)?;
                        if source {
                            file.set_permissions(fs::Permissions::from_mode(mode))
                                .map_err(io_error)?;
                        }
                        self.file = Some((file, size));
                    }
                    Kind::Link { target } => {
                        if !source
                            || target.is_empty()
                            || target.len() > 4096
                            || target.contains(&0)
                        {
                            return Err("invalid Build source link".into());
                        }
                        self.links.insert(
                            relative,
                            PathBuf::from(std::ffi::OsString::from_vec(target)),
                        );
                    }
                }
                Ok(())
            }
            Input::Finish => {
                for path in self.links.keys() {
                    validate_link(path, &self.links)?;
                }
                for (path, target) in &self.links {
                    symlink(target, self.root.join(path)).map_err(io_error)?;
                }
                self.state = UploadState::Finished;
                Ok(())
            }
            Input::Start(_) | Input::Cancel | Input::Data(_) => {
                Err("unexpected Build upload frame".into())
            }
        }
    }

    /// Seal a successfully received capture for execution.
    /// # Errors
    /// Rejects incomplete or previously rejected uploads.
    pub fn complete(self) -> Result<CompletedUpload, InputError> {
        if !matches!(self.state, UploadState::Finished) {
            return Err("Build upload was not completed".into());
        }
        Ok(CompletedUpload(self))
    }
}

impl CompletedUpload {
    /// Consume only a completed capture. The remote host supplies its own PATH
    /// and Docker socket; client environment and plugin paths are never used.
    /// # Errors
    /// Returns recipe validation or host execution failure.
    pub fn execute(
        self,
        definition: &Definition,
        admission: Admission,
        docker: Option<&Path>,
        progress: &(dyn Fn(Progress) + Sync),
    ) -> Result<Vec<BuiltImage>, BuildError> {
        let mut upload = self.0;
        crate::received_recipe::validate_capture(&upload.root, definition)
            .map_err(|error| BuildError::Request(error.to_string()))?;
        for (path, mode) in upload.directories.iter().rev() {
            fs::set_permissions(upload.root.join(path), fs::Permissions::from_mode(*mode))
                .map_err(|error| BuildError::Request(io_error(error).to_string()))?;
        }
        let environment = BTreeMap::from([
            (
                "PATH".into(),
                std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into()),
            ),
            (
                "HOME".into(),
                upload.root.join("private").to_string_lossy().into_owned(),
            ),
            (
                "DOCKER_CONFIG".into(),
                upload
                    .root
                    .join("private/docker")
                    .to_string_lossy()
                    .into_owned(),
            ),
        ]);
        let result = crate::execute_admitted(
            &Request {
                compose_file: Path::new("compose.yaml"),
                working_dir: &upload.root,
                environment: &environment,
                docker,
                targets: &definition.targets,
                build_args: &[],
                output: definition.output,
                no_cache: definition.no_cache,
                pull: definition.pull,
            },
            admission,
            progress,
        );
        // Unconfirmed processes may still read private attempt files. Keep them
        // protected alongside quarantined builder ownership until safe cleanup.
        upload.retain = result.as_ref().is_err_and(|error| error.is_unknown());
        result
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        if !self.retain {
            let _ = make_removable(&self.root);
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

fn make_removable(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        for entry in fs::read_dir(path)? {
            make_removable(&entry?.path())?;
        }
    }
    Ok(())
}

fn received_path(bytes: Vec<u8>) -> Result<PathBuf, InputError> {
    if bytes.is_empty() || bytes.len() > 4096 || bytes.contains(&0) {
        return Err("invalid Build entry path".into());
    }
    let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
    if path.components().count() > 128
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("Build entry path escapes staging".into());
    }
    Ok(path)
}

/// Follow each link hop inside source, including links through other uploaded
/// links. Dangling links are allowed, but even dangling targets cannot escape.
fn validate_link(path: &Path, links: &BTreeMap<PathBuf, PathBuf>) -> Result<(), InputError> {
    let mut pending = VecDeque::from([path.to_owned()]);
    let mut resolved = PathBuf::new();
    let mut followed = 0;
    while let Some(next) = pending.pop_front() {
        let mut parts = next.components();
        while let Some(part) = parts.next() {
            match part {
                Component::CurDir => continue,
                Component::Normal(name) => resolved.push(name),
                Component::ParentDir if resolved != Path::new("source") && resolved.pop() => {
                    continue;
                }
                Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                    return Err("Build link escapes source staging".into());
                }
            }
            if !resolved.starts_with("source") {
                return Err("Build link escapes source staging".into());
            }
            if let Some(target) = links.get(&resolved) {
                followed += 1;
                if followed > 40 {
                    return Err("Build source has a cyclic link".into());
                }
                resolved.pop();
                pending.push_front(parts.as_path().to_owned());
                pending.push_front(target.clone());
                break;
            }
        }
    }
    Ok(())
}

fn io_error(error: io::Error) -> InputError {
    format!("stage Build input: {}", error.kind()).into()
}

/// Stream an owned capture with bounded buffering. Only the dispatch adapter
/// calls this, after receiving admission for its pinned Machine.
/// # Errors
/// Reports filesystem failures, unsupported input kinds, and sender rejection.
pub fn upload(
    root: &Path,
    mut send: impl FnMut(Input) -> Result<(), InputError>,
) -> Result<(), InputError> {
    fn visit(
        root: &Path,
        relative: &Path,
        send: &mut impl FnMut(Input) -> Result<(), InputError>,
    ) -> Result<(), InputError> {
        let path = root.join(relative);
        let metadata = fs::symlink_metadata(&path).map_err(io_error)?;
        let kind = if metadata.is_symlink() {
            Kind::Link {
                target: fs::read_link(&path)
                    .map_err(io_error)?
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec(),
            }
        } else if metadata.is_dir() {
            Kind::Directory
        } else if metadata.is_file() {
            Kind::File {
                size: metadata.len(),
            }
        } else {
            return Err("Build source contains a special file".into());
        };
        send(Input::Entry {
            path: relative.as_os_str().as_encoded_bytes().to_vec(),
            kind,
            mode: metadata.permissions().mode() & 0o777,
        })?;
        if metadata.is_dir() {
            for entry in fs::read_dir(&path).map_err(io_error)? {
                let entry = entry.map_err(io_error)?;
                visit(root, &relative.join(entry.file_name()), send)?;
            }
        } else if metadata.is_file() {
            let mut file = File::open(path).map_err(io_error)?;
            let mut bytes = [0; CHUNK_SIZE];
            loop {
                let n = file.read(&mut bytes).map_err(io_error)?;
                if n == 0 {
                    break;
                }
                send(Input::Data(bytes.split_at(n).0.to_vec()))?;
            }
        }
        Ok(())
    }
    // Credential material uses its own namespace and is never a source context.
    for path in ["source", "private", "compose.yaml"] {
        visit(root, Path::new(path), &mut send)?;
    }
    send(Input::Finish)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_finished_uploads_can_become_executable() {
        assert!(Upload::new().unwrap().complete().is_err());
        let mut upload = Upload::new().unwrap();
        assert!(upload.accept(Input::Data(vec![1])).is_err());
        assert!(upload.accept(Input::Finish).is_err());
        assert!(upload.complete().is_err());
        let mut upload = Upload::new().unwrap();
        upload.accept(Input::Finish).unwrap();
        assert!(upload.complete().is_ok());
    }

    #[test]
    fn links_cannot_cross_into_private_material_or_escape_through_another_link() {
        for target in ["../private/key", "/etc/passwd", "../../source/again"] {
            let mut upload = Upload::new().unwrap();
            upload
                .accept(Input::Entry {
                    path: b"source".to_vec(),
                    kind: Kind::Directory,
                    mode: 0o755,
                })
                .unwrap();
            upload
                .accept(Input::Entry {
                    path: b"source/link".to_vec(),
                    kind: Kind::Link {
                        target: target.as_bytes().to_vec(),
                    },
                    mode: 0o777,
                })
                .unwrap();
            assert!(upload.accept(Input::Finish).is_err(), "{target}");
        }
        let mut upload = Upload::new().unwrap();
        upload
            .accept(Input::Entry {
                path: b"source".to_vec(),
                kind: Kind::Directory,
                mode: 0o755,
            })
            .unwrap();
        for (path, target) in [("source/a", "b/../../private"), ("source/b", ".")] {
            upload
                .accept(Input::Entry {
                    path: path.as_bytes().to_vec(),
                    kind: Kind::Link {
                        target: target.as_bytes().to_vec(),
                    },
                    mode: 0o777,
                })
                .unwrap();
        }
        assert!(upload.accept(Input::Finish).is_err());
    }

    #[test]
    fn oversized_source_and_private_entries_are_rejected_before_writing() {
        for (path, size) in [
            ("source/file", SOURCE_LIMIT + 1),
            ("private/key", PRIVATE_LIMIT + 1),
        ] {
            let mut upload = Upload::new().unwrap();
            let area = path.split('/').next().unwrap();
            upload
                .accept(Input::Entry {
                    path: area.as_bytes().to_vec(),
                    kind: Kind::Directory,
                    mode: 0o700,
                })
                .unwrap();
            assert!(
                upload
                    .accept(Input::Entry {
                        path: path.as_bytes().to_vec(),
                        kind: Kind::File { size },
                        mode: 0o600
                    })
                    .is_err()
            );
            assert!(!upload.root.join(path).exists());
        }
        assert!(
            crate::remote::decode::<Input>(&ployz_core::OpaquePayload::new(vec![
                b' ';
                crate::remote::FRAME_LIMIT
                    + 1
            ]))
            .is_err()
        );
        assert!(
            crate::remote::decode::<Input>(&ployz_core::OpaquePayload::new(
                br#"{"Data":[1],"Finish":null}"#.to_vec()
            ))
            .is_err()
        );
    }

    #[test]
    fn received_source_rejects_escape_duplicate_and_incomplete_files() {
        for path in [
            b"../outside".as_slice(),
            b"/tmp/outside",
            b"source/../private/key",
        ] {
            let mut upload = Upload::new().unwrap();
            assert!(
                upload
                    .accept(Input::Entry {
                        path: path.to_vec(),
                        kind: Kind::File { size: 1 },
                        mode: 0o644
                    })
                    .is_err()
            );
        }
        let mut upload = Upload::new().unwrap();
        upload
            .accept(Input::Entry {
                path: b"source".to_vec(),
                kind: Kind::Directory,
                mode: 0o755,
            })
            .unwrap();
        upload
            .accept(Input::Entry {
                path: b"source/file".to_vec(),
                kind: Kind::File { size: 2 },
                mode: 0o644,
            })
            .unwrap();
        upload.accept(Input::Data(vec![1])).unwrap();
        assert!(upload.accept(Input::Finish).is_err());
        assert!(upload.accept(Input::Data(vec![2, 3])).is_err());
        let mut upload = Upload::new().unwrap();
        let entry = Input::Entry {
            path: b"source".to_vec(),
            kind: Kind::Directory,
            mode: 0o755,
        };
        upload.accept(entry.clone()).unwrap();
        assert!(upload.accept(entry).is_err());
    }
}
