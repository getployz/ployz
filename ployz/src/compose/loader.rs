use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::OpenOptionsExt as _,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

use super::{
    configs::short_config,
    convert::convert_raw_project,
    model::{ComposeError, ComposeProject, RawProject},
    mounts::relative_bind_source,
};

const COMPOSE_INSTALL_URL: &str = "https://docs.docker.com/compose/install/";
const SECRET_SOURCE_DIAGNOSTIC: &str = ": one of file|environment must be set";

#[derive(Clone, Debug)]
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
    let docker = options
        .docker
        .as_deref()
        .unwrap_or_else(|| Path::new("docker"));
    let (raw, recovered_secrets) = normalized_project(docker, options)?;
    let environment_override = (!recovered_secrets.is_empty())
        .then(|| TemporaryComposeFile::new(&secret_overrides(&recovered_secrets)))
        .transpose()?;
    let environment = parse_environment(
        &checked_config(
            docker,
            options,
            environment_override.as_ref(),
            &["--environment"],
        )?
        .stdout,
    );
    convert_raw_project(
        raw,
        project_working_dir(options)?,
        environment,
        &recovered_secrets,
    )
}

fn normalized_project(
    docker: &Path,
    options: &LoadOptions,
) -> Result<(RawProject, BTreeSet<String>), ComposeError> {
    let mut recovered = BTreeSet::new();
    loop {
        let override_file = (!recovered.is_empty())
            .then(|| TemporaryComposeFile::new(&secret_overrides(&recovered)))
            .transpose()?;
        let output = config_output(
            docker,
            options,
            override_file.as_ref(),
            &["--format", "yaml"],
        )?;
        if output.status.success() {
            let raw = parse_project(&output)?;
            let classifier = checked_config(
                docker,
                options,
                override_file.as_ref(),
                &[
                    "--no-consistency",
                    "--no-normalize",
                    "--no-path-resolution",
                    "--format",
                    "yaml",
                ],
            )?;
            validate_source_forms(&parse_project(&classifier)?)?;
            return Ok((raw, recovered));
        }
        let diagnostic = String::from_utf8_lossy(&output.stderr).into_owned();
        if let Some(name) = missing_source_secret(&diagnostic)
            && recovered.insert(name)
        {
            continue;
        }
        return Err(classify_config_failure(docker, options, diagnostic));
    }
}

fn checked_config(
    docker: &Path,
    options: &LoadOptions,
    override_file: Option<&TemporaryComposeFile>,
    args: &[&str],
) -> Result<Output, ComposeError> {
    let output = config_output(docker, options, override_file, args)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(classify_config_failure(
            docker,
            options,
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    }
}

fn config_output(
    docker: &Path,
    options: &LoadOptions,
    override_file: Option<&TemporaryComposeFile>,
    args: &[&str],
) -> Result<Output, ComposeError> {
    compose_command(docker, options, override_file)?
        .arg("config")
        .args(args)
        .output()
        .map_err(|error| classify_spawn_failure(docker, options, error))
}

fn parse_project(output: &Output) -> Result<RawProject, ComposeError> {
    serde_norway::from_slice(&output.stdout)
        .map_err(|error| ComposeError::Invalid(error.to_string()))
}

fn validate_source_forms(project: &RawProject) -> Result<(), ComposeError> {
    for (service, raw) in &project.services {
        if let Some(config) = short_config(raw) {
            return Err(ComposeError::Invalid(format!(
                "service '{service}': short-syntax config '{config}' is not supported"
            )));
        }
        if let Some(source) = relative_bind_source(raw) {
            return Err(ComposeError::Invalid(format!(
                "service '{service}': bind mount source '{source}' is relative"
            )));
        }
    }
    Ok(())
}

fn missing_source_secret(diagnostic: &str) -> Option<String> {
    diagnostic.lines().find_map(|line| {
        line.strip_prefix("secrets.")?
            .strip_suffix(SECRET_SOURCE_DIAGNOSTIC)
            .map(|name| name.trim_matches('"').to_owned())
    })
}

fn secret_overrides(names: &BTreeSet<String>) -> String {
    let entries = names
        .iter()
        .map(|name| format!("  {name:?}:\n    external: true\n"))
        .collect::<String>();
    format!("secrets:\n{entries}")
}

fn project_working_dir(options: &LoadOptions) -> Result<PathBuf, ComposeError> {
    let file = options
        .files
        .first()
        .cloned()
        .or_else(first_compose_file_from_environment);
    let file = match file {
        Some(file) if file.is_absolute() => file,
        Some(file) => options
            .working_dir
            .as_deref()
            .unwrap_or_else(|| Path::new("."))
            .join(file),
        None => discover_default_compose_file(options)?,
    };
    Ok(file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .or_else(|| options.working_dir.clone())
        .unwrap_or_else(|| PathBuf::from(".")))
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
            let mut files = std::env::var_os("COMPOSE_FILE").ok_or_else(|| {
                ComposeError::Invalid(
                    "x-command secret loading with default Compose file discovery is unsupported; use --file or COMPOSE_FILE".into(),
                )
            })?;
            files.push(compose_path_separator());
            files.push(&override_file.path);
            command.env("COMPOSE_FILE", files);
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
    let files = std::env::var_os("COMPOSE_FILE")?;
    files
        .to_string_lossy()
        .split(compose_path_separator().to_string_lossy().as_ref())
        .find(|file| !file.is_empty())
        .map(PathBuf::from)
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

fn classify_config_failure(
    docker: &Path,
    options: &LoadOptions,
    diagnostic: String,
) -> ComposeError {
    match compose_version(docker, options) {
        Ok(status) if status.success() => ComposeError::Compose(diagnostic),
        _ => plugin_prerequisite(options),
    }
}

fn compose_version(
    docker: &Path,
    options: &LoadOptions,
) -> std::io::Result<std::process::ExitStatus> {
    let mut command = Command::new(docker);
    command
        .args(["compose", "version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(directory) = &options.working_dir {
        command.current_dir(directory);
    }
    command.status()
}

fn classify_spawn_failure(
    docker: &Path,
    options: &LoadOptions,
    error: std::io::Error,
) -> ComposeError {
    let _ = compose_version(docker, options);
    ComposeError::Prerequisite(format!(
        "ployz {} requires the Docker CLI and Docker Compose plugin on the client ({error}); install Compose: {COMPOSE_INSTALL_URL}",
        options.command
    ))
}

fn plugin_prerequisite(options: &LoadOptions) -> ComposeError {
    ComposeError::Prerequisite(format!(
        "ployz {} requires the Docker Compose plugin on the client; install Compose: {COMPOSE_INSTALL_URL}",
        options.command
    ))
}

fn parse_environment(bytes: &[u8]) -> BTreeMap<String, String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

pub(super) struct TemporaryComposeFile {
    pub(super) path: PathBuf,
}

impl TemporaryComposeFile {
    pub(super) fn new(content: &str) -> Result<Self, ComposeError> {
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
                .mode(0o600)
                .open(&path)
            {
                Ok(mut file) => {
                    use std::io::Write as _;
                    file.write_all(content.as_bytes()).map_err(|error| {
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
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    #[test]
    fn temporary_compose_files_are_private() {
        let file = TemporaryComposeFile::new("services: {}\n").unwrap();
        let mode = fs::metadata(&file.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
