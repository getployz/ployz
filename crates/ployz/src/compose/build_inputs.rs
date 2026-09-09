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

mod service;

/// Private, attempt-scoped copies. Never persisted as deployment history.
pub(super) struct BuildInputs {
    root: PathBuf,
    captures: BTreeMap<Input, CapturedInput>,
}

#[derive(Eq, PartialEq, Ord, PartialOrd)]
enum Input {
    File(PathBuf),
    PrivateFile(PathBuf),
    Context { path: PathBuf, recipe: Recipe },
}

#[derive(Eq, PartialEq, Ord, PartialOrd)]
enum Recipe {
    Dockerfile(Option<PathBuf>),
    Railpack(PathBuf),
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
            Self::File(path) | Self::PrivateFile(path) | Self::Context { path, .. } => path,
        }
    }

    fn selection(&self) -> Result<Option<Selection>, ComposeError> {
        let Self::Context { path, recipe } = self else {
            return Ok(None);
        };
        #[derive(serde::Deserialize)]
        struct Response {
            paths: Vec<String>,
            ignore: Option<String>,
        }
        let (dockerfile, railpack_config) = match recipe {
            Recipe::Dockerfile(path) => (path.as_ref(), None),
            Recipe::Railpack(path) => (None, Some(path)),
        };
        let response: Response = super::loader::helper(&serde_json::json!({
            "version": 1, "build_context": { "path": path, "dockerfile": dockerfile, "railpack_config": railpack_config }
        }))?;
        let paths: BTreeSet<PathBuf> = response
            .paths
            .into_iter()
            .map(|path| {
                base64::engine::general_purpose::STANDARD
                    .decode(path)
                    .map(|bytes| PathBuf::from(std::ffi::OsString::from_vec(bytes)))
                    .map_err(|error| ComposeError::Io(format!("decode context path: {error}")))
            })
            .collect::<Result<_, _>>()?;
        if paths.iter().any(|path| {
            path.components().any(|part| {
                !matches!(
                    part,
                    std::path::Component::Normal(_) | std::path::Component::CurDir
                )
            })
        }) {
            return Err(ComposeError::Invalid(
                "build context path escapes staging".into(),
            ));
        }
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
        for directory in ["source", "private"] {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(root.join(directory))
                .map_err(input_error)?;
        }
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
            recipe: Recipe::Dockerfile(dockerfile.map(Path::to_path_buf)),
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
    ) -> Result<PathBuf, ComposeError> {
        let config = variables
            .get("RAILPACK_CONFIG_FILE")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .unwrap_or("railpack.json");
        self.capture_input(Input::Context {
            path: path.canonicalize().map_err(input_error)?,
            recipe: Recipe::Railpack(config.into()),
        })
    }

    /// Freeze a regular credential file outside reusable source with mode 0600.
    ///
    /// # Errors
    /// Rejects unreadable, nonregular, or unstable inputs.
    pub(super) fn private_file(&mut self, path: &Path) -> Result<PathBuf, ComposeError> {
        self.capture_input(Input::PrivateFile(
            path.canonicalize().map_err(input_error)?,
        ))
    }

    /// Paths in the capture describe its layout, never its temporary location.
    pub(super) fn relative(&self, path: &Path) -> PathBuf {
        path.strip_prefix(&self.root)
            .expect("owned capture path")
            .to_owned()
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
        let private = matches!(input, Input::PrivateFile(_));
        let target = self
            .root
            .join(if private { "private" } else { "source" })
            .join(self.captures.len().to_string());
        if !matches!(input, Input::Context { .. })
            && !fs::metadata(source).map_err(input_error)?.is_file()
        {
            return Err(ComposeError::Invalid("build credential or Dockerfile must be a regular file; SSH agent sockets are unsupported".into()));
        }
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
        if private {
            fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).map_err(input_error)?;
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

    /// Keep Railpack recipe metadata and variables in private upload material.
    ///
    /// # Errors
    /// Fails if the private file cannot be written.
    pub(super) fn railpack(&self, recipes: &[ployz_build::Railpack]) -> Result<(), ComposeError> {
        if recipes.is_empty() {
            return Ok(());
        }
        let bytes =
            serde_json::to_vec(recipes).map_err(|error| input_error(io::Error::other(error)))?;
        self.private(&self.root.join("private/railpack.json"), &bytes)
    }

    /// Write the captured Compose file beside its sources.
    ///
    /// # Errors
    /// Fails if the private file cannot be written.
    pub(super) fn compose(&self, yaml: &str) -> Result<PathBuf, ComposeError> {
        let path = self.root.join("compose.yaml");
        self.private(&path, yaml.as_bytes())?;
        Ok(path)
    }

    /// Write one resolved secret to an owner-readable file for Docker.
    ///
    /// # Errors
    /// Fails if the index is already used or the private file cannot be written.
    pub(super) fn secret(&self, index: usize, value: &str) -> Result<PathBuf, ComposeError> {
        let path = self.root.join("private").join(format!("secret-{index}"));
        self.private(&path, value.as_bytes())?;
        Ok(path)
    }

    /// Snapshot explicitly supplied registry auth and proxies. Never import the host's
    /// default Docker login or credential helpers. Keep CLI plugin discovery
    /// pointed at the caller's tool installation, just like PATH.
    ///
    /// # Errors
    /// Rejects unreadable/invalid configuration and host-specific helpers/contexts, or
    /// fails when private configuration cannot be staged.
    pub(super) fn docker_config(
        &mut self,
        environment: &BTreeMap<String, String>,
        directory: &Path,
    ) -> Result<(), ComposeError> {
        let mut config = serde_json::Map::new();
        let mut plugin_dirs = Vec::<PathBuf>::new();
        let explicit_config = environment
            .get("DOCKER_CONFIG")
            .filter(|path| !path.is_empty());
        let original_config = explicit_config
            .map(|path| directory.join(path))
            .or_else(|| {
                environment
                    .get("HOME")
                    .map(|home| directory.join(home).join(".docker"))
            });
        if let Some(path) = &original_config {
            let path = path.join("config.json");
            let supplied: serde_json::Value = if path.try_exists().map_err(input_error)? {
                let path = if explicit_config.is_some() {
                    self.private_file(&path)?
                } else {
                    path
                };
                serde_json::from_slice(&fs::read(path).map_err(input_error)?)
                    .map_err(|_| ComposeError::Invalid("Docker config.json is invalid".into()))?
            } else {
                serde_json::json!({})
            };
            if supplied
                .get("currentContext")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|context| !matches!(context, "" | "default"))
                && !environment
                    .get("DOCKER_HOST")
                    .is_some_and(|host| !host.is_empty())
            {
                return Err(ComposeError::Invalid(
                    "Docker currentContext is host-specific; supply DOCKER_HOST explicitly".into(),
                ));
            }
            if explicit_config.is_some() {
                if supplied
                    .get("credsStore")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|value| !value.is_empty())
                    || supplied
                        .get("credHelpers")
                        .and_then(serde_json::Value::as_object)
                        .is_some_and(|value| !value.is_empty())
                {
                    return Err(ComposeError::Invalid("DOCKER_CONFIG credential helpers are host-specific; supply explicit registry auths".into()));
                }
                for key in ["auths", "proxies"] {
                    if let Some(value) = supplied.get(key) {
                        config.insert(key.into(), value.clone());
                    }
                }
                if let Some(dirs) = supplied.get("cliPluginsExtraDirs") {
                    let dirs: Vec<PathBuf> =
                        serde_json::from_value(dirs.clone()).map_err(|_| {
                            ComposeError::Invalid(
                                "DOCKER_CONFIG cliPluginsExtraDirs must be an array of paths"
                                    .into(),
                            )
                        })?;
                    plugin_dirs.extend(dirs.into_iter().map(|path| directory.join(path)));
                }
            }
        }
        if let Some(original_config) = original_config {
            plugin_dirs.push(original_config.join("cli-plugins"));
        }
        config.insert("cliPluginsExtraDirs".into(), serde_json::json!(plugin_dirs));
        let directory = self.root.join("private/docker");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(input_error)?;
        self.private(
            &directory.join("config.json"),
            &serde_json::to_vec(&config)
                .map_err(|_| ComposeError::Invalid("invalid registry auths".into()))?,
        )
    }

    fn private(&self, path: &Path, content: &[u8]) -> Result<(), ComposeError> {
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
    fn default_docker_context_is_checked_without_importing_credentials() {
        let fixture = BuildInputs::new().unwrap();
        fs::create_dir(fixture.root.join(".docker")).unwrap();
        for (context, host, accepted) in [
            ("remote", "", false),
            ("remote", "ssh://builder@host", true),
            ("default", "", true),
            ("", "", true),
        ] {
            fs::write(
                fixture.root.join(".docker/config.json"),
                serde_json::json!({
                    "currentContext": context,
                    "credsStore": "desktop",
                    "auths": {"example.test": {"auth": "ambient-token"}},
                    "proxies": {"default": {"httpProxy": "http://ambient-proxy"}}
                })
                .to_string(),
            )
            .unwrap();
            let environment = BTreeMap::from([
                ("HOME".into(), fixture.root.to_string_lossy().into_owned()),
                ("DOCKER_HOST".into(), host.into()),
            ]);
            let mut inputs = BuildInputs::new().unwrap();
            let result = inputs.docker_config(&environment, &fixture.root);
            assert_eq!(result.is_ok(), accepted, "{context}, {host}");
            if let Err(error) = result {
                assert!(error.to_string().contains("currentContext"));
            } else {
                let staged: serde_json::Value = serde_json::from_slice(
                    &fs::read(inputs.root.join("private/docker/config.json")).unwrap(),
                )
                .unwrap();
                for key in ["auths", "proxies", "credsStore", "currentContext"] {
                    assert!(staged.get(key).is_none(), "imported {key}");
                }
            }
        }
    }

    #[test]
    fn selected_docker_context_is_not_silently_discarded() {
        let fixture = BuildInputs::new().unwrap();
        let config = fixture.root.join("config.json");
        for (context, host, accepted) in [
            ("remote", "", false),
            ("remote", "ssh://builder@host", true),
            ("default", "", true),
            ("", "", true),
        ] {
            fs::write(
                &config,
                serde_json::json!({"currentContext": context}).to_string(),
            )
            .unwrap();
            let environment = BTreeMap::from([
                (
                    "DOCKER_CONFIG".into(),
                    fixture.root.to_string_lossy().into_owned(),
                ),
                ("DOCKER_HOST".into(), host.into()),
            ]);
            let mut inputs = BuildInputs::new().unwrap();
            let result = inputs.docker_config(&environment, &fixture.root);
            assert_eq!(result.is_ok(), accepted, "{context}, {host}");
            if let Err(error) = result {
                assert!(error.to_string().contains("currentContext"));
            }
        }
    }

    #[test]
    fn docker_plugin_search_paths_survive_private_config_staging() {
        let fixture = BuildInputs::new().unwrap();
        fs::write(
            fixture.root.join("config.json"),
            r#"{"cliPluginsExtraDirs":["extra-plugins"]}"#,
        )
        .unwrap();
        for (explicit, present) in [(false, true), (true, true), (true, false)] {
            if !present {
                fs::remove_file(fixture.root.join("config.json")).unwrap();
            }
            let mut environment =
                BTreeMap::from([("HOME".into(), fixture.root.to_string_lossy().into_owned())]);
            if explicit {
                environment.insert(
                    "DOCKER_CONFIG".into(),
                    fixture.root.to_string_lossy().into_owned(),
                );
            }
            let mut inputs = BuildInputs::new().unwrap();
            inputs.docker_config(&environment, &fixture.root).unwrap();
            let config: serde_json::Value = serde_json::from_slice(
                &fs::read(inputs.root.join("private/docker/config.json")).unwrap(),
            )
            .unwrap();
            let expected = if explicit && present {
                vec![
                    fixture.root.join("extra-plugins"),
                    fixture.root.join("cli-plugins"),
                ]
            } else if explicit {
                vec![fixture.root.join("cli-plugins")]
            } else {
                vec![fixture.root.join(".docker/cli-plugins")]
            };
            assert_eq!(
                config.get("cliPluginsExtraDirs"),
                Some(&serde_json::json!(expected))
            );
        }
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
    fn the_captured_compose_file_and_secrets_stay_private() {
        let inputs = BuildInputs::new().unwrap();
        let compose = inputs.compose("services: {}\n").unwrap();
        let secret = inputs.secret(0, "private-token").unwrap();
        for path in [&compose, &secret] {
            let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{}", path.display());
        }
        assert_eq!(fs::read_to_string(&compose).unwrap(), "services: {}\n");
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
