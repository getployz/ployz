use std::{
    fs, io,
    os::unix::fs::OpenOptionsExt as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};

use super::model::{ComposeError, ComposeProject};

#[derive(Clone, Debug, Serialize)]
pub struct LoadOptions {
    pub command: String,
    pub files: Vec<PathBuf>,
    pub profiles: Vec<String>,
    pub all_profiles: bool,
    pub working_dir: Option<PathBuf>,
    pub docker: Option<PathBuf>,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            command: "command".into(),
            files: Vec::new(),
            profiles: Vec::new(),
            all_profiles: false,
            working_dir: None,
            docker: None,
        }
    }
}

pub fn load_project(options: &LoadOptions) -> Result<ComposeProject, ComposeError> {
    let request = serde_json::json!({
        "version": 1, "files": options.files, "profiles": options.profiles,
        "all_profiles": options.all_profiles, "working_dir": options.working_dir,
    });
    helper(&request)
}

/// Convert normalized Compose YAML into validated requested service specifications.
///
/// # Errors
/// Returns an error if the helper fails or the project contains invalid or unsupported input.
pub fn parse_normalized(
    yaml: &str,
    working_dir: impl Into<PathBuf>,
) -> Result<ComposeProject, ComposeError> {
    helper(&serde_json::json!({"version": 1, "yaml": yaml, "working_dir": working_dir.into()}))
}

pub(super) fn helper<T: serde::de::DeserializeOwned>(
    request: &serde_json::Value,
) -> Result<T, ComposeError> {
    use std::io::Write as _;
    let content = include_bytes!(concat!(env!("OUT_DIR"), "/ployz-compose"));
    // Linux temporary mounts may be noexec. Keep the executable in anonymous
    // memory, sealed against modification, until the child has finished.
    #[cfg(target_os = "linux")]
    let executable = {
        use rustix::fs::{MemfdFlags, SealFlags, fcntl_add_seals, memfd_create};
        let fd = memfd_create(
            "ployz-compose",
            MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
        )
        .map_err(|error| ComposeError::Io(format!("create Compose helper: {error}")))?;
        let mut file = fs::File::from(fd);
        file.write_all(content)
            .map_err(|error| ComposeError::Io(format!("write Compose helper: {error}")))?;
        fcntl_add_seals(
            &file,
            SealFlags::WRITE | SealFlags::GROW | SealFlags::SHRINK | SealFlags::SEAL,
        )
        .map_err(|error| ComposeError::Io(format!("seal Compose helper: {error}")))?;
        file
    };
    #[cfg(target_os = "linux")]
    let path = {
        use std::os::fd::AsRawFd as _;
        PathBuf::from(format!("/proc/self/fd/{}", executable.as_raw_fd()))
    };
    #[cfg(not(target_os = "linux"))]
    let executable = TemporaryComposeFile::create(content, 0o700)?;
    #[cfg(not(target_os = "linux"))]
    let path = &executable.path;
    let mut child = retry_executable_busy(|| {
        Command::new(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    })
    .map_err(|error| ComposeError::Io(format!("start Compose helper: {error}")))?;
    let input =
        serde_json::to_vec(request).map_err(|error| ComposeError::Invalid(error.to_string()))?;
    let write = child.stdin.take().expect("piped stdin").write_all(&input);
    let output = child
        .wait_with_output()
        .map_err(|error| ComposeError::Io(format!("wait for Compose helper: {error}")))?;
    write.map_err(|error| ComposeError::Io(format!("write Compose helper input: {error}")))?;
    if !output.status.success() {
        return Err(ComposeError::Io(format!(
            "Compose helper exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    #[derive(Deserialize)]
    struct Response {
        version: u32,
        #[serde(flatten)]
        outcome: Outcome,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Outcome {
        Result(serde_json::Value),
        Error(String),
    }
    let response: Response = serde_json::from_slice(&output.stdout)
        .map_err(|error| ComposeError::Invalid(format!("Compose helper output: {error}")))?;
    if response.version != 1 {
        return Err(ComposeError::Invalid(
            "Compose helper protocol mismatch".into(),
        ));
    }
    match response.outcome {
        Outcome::Result(value) => {
            serde_json::from_value(value).map_err(|error| ComposeError::Invalid(error.to_string()))
        }
        Outcome::Error(error) => Err(ComposeError::Invalid(error)),
    }
}

pub(super) fn compose_command(
    docker: &Path,
    options: &LoadOptions,
    override_file: Option<&TemporaryComposeFile>,
) -> Result<Command, ComposeError> {
    let mut command = Command::new(docker);
    command.args(["compose", "--all-resources"]);
    for file in &options.files {
        command.arg("--file").arg(file);
    }
    if let Some(override_file) = override_file {
        if options.files.is_empty() {
            if let Some(mut files) = std::env::var_os(crate::cli::env::COMPOSE_FILE) {
                files.push(compose_path_separator());
                files.push(&override_file.path);
                command.env(crate::cli::env::COMPOSE_FILE, files);
            } else {
                command
                    .arg("--file")
                    .arg(discover_default_compose_file(options)?)
                    .arg("--file")
                    .arg(&override_file.path);
            }
        } else {
            command.arg("--file").arg(&override_file.path);
        }
    }
    for profile in &options.profiles {
        command.arg("--profile").arg(profile);
    }
    if options.all_profiles {
        command.args(["--profile", "*"]);
    }
    if let Some(directory) = &options.working_dir {
        command.current_dir(directory);
    }
    Ok(command)
}

pub(super) fn first_compose_file_from_environment() -> Option<PathBuf> {
    compose_files_from_environment().into_iter().next()
}

fn compose_input_files(options: &LoadOptions) -> Vec<PathBuf> {
    if !options.files.is_empty() {
        return options.files.clone();
    }
    let env_files = compose_files_from_environment();
    if !env_files.is_empty() {
        return env_files;
    }
    discover_default_compose_file(options)
        .ok()
        .into_iter()
        .collect()
}

/// Compose files the user named (`--file` or `COMPOSE_FILE`), not discovered defaults.
#[must_use]
pub(crate) fn has_explicit_nondefault_compose_file(options: &LoadOptions) -> bool {
    if options.files.is_empty() {
        compose_files_from_environment()
            .iter()
            .any(|file| !is_default_compose_file_name(file))
    } else {
        options
            .files
            .iter()
            .any(|file| !is_default_compose_file_name(file))
    }
}

fn is_default_compose_file_name(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("compose.yaml" | "compose.yml" | "docker-compose.yaml" | "docker-compose.yml")
    )
}

/// Source-YAML Project identity for Compose commands. This is not `ComposeProject.name`.
#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct ComposeIdentity {
    pub name: Option<String>,
    pub directory: Option<PathBuf>,
}

pub(crate) fn compose_identity(options: &LoadOptions) -> ComposeIdentity {
    let files = compose_input_files(options);
    ComposeIdentity {
        name: top_level_compose_name(&files),
        directory: compose_directory(&files),
    }
}

fn top_level_compose_name(files: &[PathBuf]) -> Option<String> {
    let mut found = None;
    for file in files {
        let Ok(text) = fs::read_to_string(file) else {
            continue;
        };
        let Ok(parsed) = serde_norway::from_str::<NamedCompose>(&text) else {
            continue;
        };
        if let Some(name) = parsed.name {
            found = Some(name);
        }
    }
    found
}

fn compose_directory(files: &[PathBuf]) -> Option<PathBuf> {
    let file = files.first()?;
    Some(
        file.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(".")),
    )
}

#[derive(Deserialize)]
struct NamedCompose {
    name: Option<String>,
}

fn compose_files_from_environment() -> Vec<PathBuf> {
    let Some(files) = std::env::var_os(crate::cli::env::COMPOSE_FILE) else {
        return Vec::new();
    };
    files
        .to_string_lossy()
        .split(compose_path_separator().to_string_lossy().as_ref())
        .filter(|file| !file.is_empty())
        .map(PathBuf::from)
        .collect()
}

pub(super) fn discover_default_compose_file(
    options: &LoadOptions,
) -> Result<PathBuf, ComposeError> {
    let current = std::env::current_dir()
        .map_err(|error| ComposeError::Io(format!("resolve current directory: {error}")))?;
    let directory = current.join(
        options
            .working_dir
            .as_deref()
            .unwrap_or_else(|| Path::new(".")),
    );
    for directory in directory.ancestors() {
        for name in [
            "compose.yaml",
            "compose.yml",
            "docker-compose.yaml",
            "docker-compose.yml",
        ] {
            let file = directory.join(name);
            if file.is_file() {
                return Ok(file);
            }
        }
    }
    Err(ComposeError::Invalid(
        "default Compose file disappeared after project loading".into(),
    ))
}

fn compose_path_separator() -> std::ffi::OsString {
    std::env::var_os("COMPOSE_PATH_SEPARATOR")
        .filter(|separator| !separator.is_empty())
        .unwrap_or_else(|| {
            if cfg!(windows) {
                ";".into()
            } else {
                ":".into()
            }
        })
}

fn retry_executable_busy<T>(mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    for attempt in 0..4 {
        match op() {
            Err(error) if error.kind() == io::ErrorKind::ExecutableFileBusy => {
                thread::sleep(Duration::from_millis(10 << attempt));
            }
            other => return other,
        }
    }
    op()
}

pub(super) struct TemporaryComposeFile {
    pub(super) path: PathBuf,
}

impl TemporaryComposeFile {
    pub(super) fn new(content: &str) -> Result<Self, ComposeError> {
        Self::create(content.as_bytes(), 0o600)
    }

    fn create(content: &[u8], mode: u32) -> Result<Self, ComposeError> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..100 {
            let path = std::env::temp_dir().join(format!(
                "ployz-compose-{}-{}.yaml",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(mode)
                .open(&path)
            {
                Ok(mut file) => {
                    use std::io::Write as _;
                    file.write_all(content).map_err(|error| {
                        ComposeError::Io(format!("write Compose override: {error}"))
                    })?;
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(ComposeError::Io(format!(
                        "create Compose override: {error}"
                    )));
                }
            }
        }
        Err(ComposeError::Io(
            "could not allocate temporary Compose override".into(),
        ))
    }
}

impl Drop for TemporaryComposeFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io,
        os::unix::fs::PermissionsExt as _,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_helper_does_not_use_tmpdir() {
        const CHILD: &str = "PLOYZ_TEST_HELPER_CHILD";
        if std::env::var_os(CHILD).is_some() {
            helper::<ployz_core::PortPublication>(
                &serde_json::json!({"version": 1, "port": "80/http"}),
            )
            .unwrap();
            return;
        }
        // A non-directory makes any attempt to extract into TMPDIR fail without
        // needing privileged mounts or changing the parent test process's environment.
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "compose::loader::tests::linux_helper_does_not_use_tmpdir",
            ])
            .env(CHILD, "1")
            .env("TMPDIR", "/dev/null")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    #[test]
    fn temporary_compose_files_are_private() {
        let file = TemporaryComposeFile::new("services: {}\n").unwrap();
        let mode = fs::metadata(&file.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn retries_executable_file_busy_then_succeeds() {
        let mut remaining = 2;
        let result = retry_executable_busy(|| {
            if remaining > 0 {
                remaining -= 1;
                Err(io::Error::from(io::ErrorKind::ExecutableFileBusy))
            } else {
                Ok(7)
            }
        });
        assert_eq!(result.unwrap(), 7);
    }

    #[test]
    fn compose_identity_reads_the_last_source_name_and_first_file_directory() {
        let root = unique_dir();
        fs::write(root.join("a.yaml"), "name: first\nservices: {}\n").unwrap();
        fs::write(root.join("b.yaml"), "name: second\nservices: {}\n").unwrap();
        fs::write(root.join("c.yaml"), "services: {}\n").unwrap();
        let stacked = LoadOptions {
            files: vec![root.join("a.yaml"), root.join("b.yaml")],
            ..Default::default()
        };
        assert_eq!(
            compose_identity(&stacked),
            ComposeIdentity {
                name: Some("second".into()),
                directory: Some(root.clone()),
            }
        );
        let later_without_name = LoadOptions {
            files: vec![root.join("a.yaml"), root.join("c.yaml")],
            ..Default::default()
        };
        assert_eq!(
            compose_identity(&later_without_name).name.as_deref(),
            Some("first")
        );
        let relative = LoadOptions {
            files: vec![PathBuf::from("compose.yaml")],
            ..Default::default()
        };
        assert_eq!(
            compose_identity(&relative).directory.as_deref(),
            Some(Path::new("."))
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn explicit_nondefault_compose_files_are_not_discovered_defaults() {
        assert!(has_explicit_nondefault_compose_file(&LoadOptions {
            files: vec![PathBuf::from("prod.yaml")],
            ..Default::default()
        }));
        assert!(!has_explicit_nondefault_compose_file(&LoadOptions {
            files: vec![PathBuf::from("compose.yaml")],
            ..Default::default()
        }));
        assert!(!has_explicit_nondefault_compose_file(&LoadOptions {
            files: vec![PathBuf::from("docker-compose.yml")],
            ..Default::default()
        }));
        assert!(!has_explicit_nondefault_compose_file(
            &LoadOptions::default()
        ));
    }

    fn unique_dir() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ployz-compose-identity-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
