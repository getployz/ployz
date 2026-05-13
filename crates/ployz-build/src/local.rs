use std::fs::OpenOptions;
use std::io::{Read as _, Write as _};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use ployz_api::{BuildEnvValue, BuildInputs, BuildLocalRequest};
use ployz_model::{
    BuildInputSummary, BuildLocation, BuildMethod, BuildSecretSummary, ImageArtifact,
    ImageArtifactProvenance, ImageAvailabilityRecord, ImageDigest, ImagePlatform, ImagePresence,
    ImageRef,
};
use ployz_runtime_api::{RuntimeImage, RuntimeImageError};
use ployz_time::now_unix_secs;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::time::timeout;

const BUILD_COMMAND_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const BUILD_OUTPUT_TAIL_BYTES: usize = 64 * 1024;
const DOCKER_BUILDKIT_ENV: &str = "DOCKER_BUILDKIT";
const RAILPACK_FRONTEND: &str = "ghcr.io/railwayapp/railpack-frontend";
const RAILPACK_SECRET_PLACEHOLDER: &str = "__PLOYZ_BUILDKIT_SECRET__";
const BUILD_CACHE_KEY_RETRY_ATTEMPTS: usize = 20;
const BUILD_CACHE_KEY_RETRY_DELAY: Duration = Duration::from_millis(10);
const BUILD_CACHE_KEY_REPAIR_LOCK_STALE: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildCommandStepKind {
    RailpackPrepare,
    ImageBuild,
}

impl BuildCommandStepKind {
    pub fn stage(self) -> &'static str {
        match self {
            Self::RailpackPrepare => "preparing railpack build plan",
            Self::ImageBuild => "running image build command",
        }
    }
}

#[derive(Debug)]
pub struct BuildCommandStep {
    pub kind: BuildCommandStepKind,
    pub command: BuildCommand,
}

#[derive(Debug)]
pub struct BuildCommandPlan {
    pub pre_build_steps: Vec<BuildCommandStep>,
    pub image_build: BuildCommandStep,
    pub cleanup_dirs: Vec<PathBuf>,
}

impl BuildCommandPlan {
    fn new(
        pre_build_steps: Vec<BuildCommandStep>,
        image_build: BuildCommandStep,
        cleanup_dirs: Vec<PathBuf>,
    ) -> Self {
        Self {
            pre_build_steps,
            image_build,
            cleanup_dirs,
        }
    }

    pub fn steps(&self) -> impl Iterator<Item = &BuildCommandStep> {
        self.pre_build_steps
            .iter()
            .chain(std::iter::once(&self.image_build))
    }

    pub fn image_build_command(&self) -> &BuildCommand {
        &self.image_build.command
    }

    pub fn redact_text(&self, text: &str) -> String {
        self.steps().fold(text.to_string(), |text, step| {
            step.command.redact_text(&text)
        })
    }

    fn cleanup(&self) {
        for path in &self.cleanup_dirs {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

impl Drop for BuildCommandPlan {
    fn drop(&mut self) {
        self.cleanup();
    }
}

pub struct BuildCommand {
    program: &'static str,
    args: Vec<String>,
    env: Vec<(String, String)>,
    redaction_values: Vec<String>,
}

impl std::fmt::Debug for BuildCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let env = self
            .env
            .iter()
            .map(|(key, _value)| (key, "<redacted>"))
            .collect::<Vec<_>>();
        f.debug_struct("BuildCommand")
            .field("program", &self.program)
            .field("args", &self.redacted_args())
            .field("env", &env)
            .finish()
    }
}

impl BuildCommand {
    fn redacted_args(&self) -> Vec<String> {
        let mut redacted = Vec::with_capacity(self.args.len());
        let mut redact_next = false;
        for arg in &self.args {
            if redact_next {
                let key = arg
                    .split_once('=')
                    .map_or(arg.as_str(), |(key, _value)| key);
                redacted.push(format!("{key}=<redacted>"));
                redact_next = false;
                continue;
            }
            if arg == "--build-arg" || arg == "--env" {
                redacted.push(arg.clone());
                redact_next = true;
                continue;
            }
            redacted.push(arg.clone());
        }
        redacted
    }

    pub fn redact_text(&self, text: &str) -> String {
        let mut values = self.sensitive_values();
        values.sort_by_key(|value| std::cmp::Reverse(value.len()));
        values.dedup();

        let mut redacted = text.to_string();
        for value in values {
            if !value.is_empty() {
                redacted = redacted.replace(&value, "<redacted>");
            }
        }
        redacted
    }

    fn redact_captured_output(&self, text: &str) -> String {
        if self
            .sensitive_values()
            .iter()
            .any(|value| !value.is_empty())
        {
            "[output omitted because build inputs contain redacted values]".into()
        } else {
            self.redact_text(text)
        }
    }

    fn sensitive_values(&self) -> Vec<String> {
        let mut values = self
            .env
            .iter()
            .filter(|(key, _value)| key != DOCKER_BUILDKIT_ENV)
            .map(|(_key, value)| value.clone())
            .collect::<Vec<_>>();
        values.extend(self.redaction_values.iter().cloned());
        let mut capture_next = false;
        for arg in &self.args {
            if capture_next {
                match arg.split_once('=') {
                    Some((_key, value)) => values.push(value.into()),
                    None => values.push(arg.clone()),
                }
                capture_next = false;
                continue;
            }
            if arg == "--build-arg" || arg == "--env" {
                capture_next = true;
            }
        }
        values
    }
}

#[derive(Debug)]
pub struct BuildCommandOutput {
    pub status_success: bool,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
}

#[async_trait]
pub trait BuildCommandRunner: Send + Sync {
    async fn run(
        &self,
        command: &BuildCommand,
        current_dir: &Path,
    ) -> Result<BuildCommandOutput, String>;
}

pub struct TokioBuildCommandRunner;

#[async_trait]
impl BuildCommandRunner for TokioBuildCommandRunner {
    async fn run(
        &self,
        command: &BuildCommand,
        current_dir: &Path,
    ) -> Result<BuildCommandOutput, String> {
        let mut child = Command::new(command.program)
            .args(&command.args)
            .envs(command.env.iter().map(|(key, value)| (key, value)))
            .current_dir(current_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                format!(
                    "spawn {} {}: {error}",
                    command.program,
                    command.redacted_args().join(" ")
                )
            })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("capture {} stdout", command.program))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| format!("capture {} stderr", command.program))?;
        let stdout_task = tokio::spawn(read_tail(stdout, BUILD_OUTPUT_TAIL_BYTES));
        let stderr_task = tokio::spawn(read_tail(stderr, BUILD_OUTPUT_TAIL_BYTES));
        let (status_success, timed_out) = match timeout(BUILD_COMMAND_TIMEOUT, child.wait()).await {
            Ok(Ok(status)) => (status.success(), false),
            Ok(Err(error)) => {
                return Err(format!(
                    "wait for {} {}: {error}",
                    command.program,
                    command.redacted_args().join(" ")
                ));
            }
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                (false, true)
            }
        };
        Ok(BuildCommandOutput {
            status_success,
            timed_out,
            stdout: stdout_task
                .await
                .map_err(|error| format!("read {} stdout: {error}", command.program))?,
            stderr: stderr_task
                .await
                .map_err(|error| format!("read {} stderr: {error}", command.program))?,
        })
    }
}

async fn read_tail<R>(mut reader: R, limit: usize) -> String
where
    R: AsyncRead + Unpin,
{
    let mut tail = Vec::new();
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => {
                tail.extend_from_slice(format!("\n[read error: {error}]").as_bytes());
                break;
            }
        };
        tail.extend_from_slice(&buffer[..read]);
        if tail.len() > limit {
            let excess = tail.len() - limit;
            tail.drain(0..excess);
            truncated = true;
        };
    }
    let body = String::from_utf8_lossy(&tail).to_string();
    if truncated {
        format!("[output truncated to last {limit} bytes]\n{body}")
    } else {
        body
    }
}

pub struct BuildInvocationPlan {
    pub summary: BuildInputSummary,
    env: Vec<(String, String)>,
    plain_env: Vec<(String, String)>,
    secret_env: Vec<(String, String)>,
    docker_build_args: Vec<(String, String)>,
    buildkit_secret_env: Vec<String>,
    railpack_secret_cache_required: bool,
}

impl std::fmt::Debug for BuildInvocationPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let env = self
            .env
            .iter()
            .map(|(key, _value)| (key, "<redacted>"))
            .collect::<Vec<_>>();
        let plain_env = self
            .plain_env
            .iter()
            .map(|(key, _value)| (key, "<redacted>"))
            .collect::<Vec<_>>();
        let docker_build_args = self
            .docker_build_args
            .iter()
            .map(|(key, _value)| (key, "<redacted>"))
            .collect::<Vec<_>>();
        let secret_env = self
            .secret_env
            .iter()
            .map(|(key, _value)| (key, "<redacted>"))
            .collect::<Vec<_>>();
        f.debug_struct("BuildInvocationPlan")
            .field("summary", &self.summary)
            .field("env", &env)
            .field("plain_env", &plain_env)
            .field("secret_env", &secret_env)
            .field("docker_build_args", &docker_build_args)
            .field("buildkit_secret_env", &self.buildkit_secret_env)
            .field(
                "railpack_secret_cache_required",
                &self.railpack_secret_cache_required,
            )
            .finish()
    }
}

pub fn plan_build_invocation(
    method: BuildMethod,
    inputs: &BuildInputs,
) -> Result<BuildInvocationPlan, String> {
    let mut summary = BuildInputSummary::default();
    let mut env = Vec::new();
    let mut plain_env = Vec::new();
    let mut secret_env = Vec::new();
    let mut secret_material = Vec::new();
    let mut secret_names = Vec::new();

    for (key, value) in &inputs.env {
        validate_build_input_key(key)?;
        match value {
            BuildEnvValue::Plain { value } => {
                summary.env.push(key.clone());
                env.push((key.clone(), value.clone()));
                plain_env.push((key.clone(), value.clone()));
            }
            BuildEnvValue::Secret { value, fingerprint } => {
                if key == DOCKER_BUILDKIT_ENV {
                    return Err(format!(
                        "build env secret cannot use reserved Docker client env key '{DOCKER_BUILDKIT_ENV}'"
                    ));
                }
                if let Some(fingerprint) = fingerprint {
                    validate_secret_fingerprint(fingerprint)?;
                }
                summary.secrets.push(BuildSecretSummary {
                    name: key.clone(),
                    fingerprint: fingerprint.clone(),
                });
                env.push((key.clone(), value.clone()));
                secret_env.push((key.clone(), value.clone()));
                secret_names.push(key.clone());
                secret_material.push((key.clone(), value.clone()));
            }
        }
    }

    let mut docker_build_args = Vec::new();
    for (key, value) in &inputs.docker_build_args {
        validate_build_input_key(key)?;
        if inputs.env.contains_key(key) {
            return Err(format!(
                "docker build arg '{key}' duplicates a build env key; use one input source for each key"
            ));
        }
        if secret_like_build_arg_name(key) {
            return Err(format!(
                "docker build arg '{key}' looks secret-bearing; pass it as build env secret instead"
            ));
        }
        summary.docker_build_args.push(key.clone());
        docker_build_args.push((key.clone(), value.clone()));
    }

    if method == BuildMethod::Railpack && !docker_build_args.is_empty() {
        return Err("railpack builds do not accept Dockerfile-only build args".into());
    }

    summary.env.sort();
    summary
        .secrets
        .sort_by(|left, right| left.name.cmp(&right.name));
    summary.docker_build_args.sort();
    env.sort_by(|left, right| left.0.cmp(&right.0));
    plain_env.sort_by(|left, right| left.0.cmp(&right.0));
    secret_env.sort_by(|left, right| left.0.cmp(&right.0));
    secret_names.sort();
    docker_build_args.sort_by(|left, right| left.0.cmp(&right.0));
    secret_material.sort_by(|left, right| left.0.cmp(&right.0));

    let railpack_secret_cache_required =
        method == BuildMethod::Railpack && !secret_material.is_empty();

    Ok(BuildInvocationPlan {
        summary,
        env,
        plain_env,
        secret_env,
        docker_build_args,
        buildkit_secret_env: secret_names,
        railpack_secret_cache_required,
    })
}

fn validate_build_input_key(key: &str) -> Result<(), String> {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return Err("build input key cannot be empty".into());
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return Err(format!(
            "build input key '{key}' must start with '_' or ASCII letter"
        ));
    }
    if !chars.all(|value| value == '_' || value.is_ascii_alphanumeric()) {
        return Err(format!(
            "build input key '{key}' may only contain ASCII letters, digits, and '_'"
        ));
    }
    Ok(())
}

fn validate_secret_fingerprint(fingerprint: &str) -> Result<(), String> {
    if fingerprint.is_empty() {
        return Err("secret fingerprint cannot be empty".into());
    }
    if fingerprint.len() > 128 {
        return Err("secret fingerprint cannot exceed 128 bytes".into());
    }
    if !fingerprint
        .chars()
        .all(|value| value.is_ascii_graphic() || value == ' ')
    {
        return Err("secret fingerprint must be printable ASCII".into());
    }
    if looks_like_secret_hash(fingerprint) {
        return Err("secret fingerprint must not be a raw hash of the secret value".into());
    }
    Ok(())
}

fn looks_like_secret_hash(value: &str) -> bool {
    let hash = value.strip_prefix("sha256:").unwrap_or(value);
    hash.len() == 64 && hash.chars().all(|character| character.is_ascii_hexdigit())
}

fn secret_like_build_arg_name(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    [
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "TOKEN",
        "PRIVATE_KEY",
        "ACCESS_KEY",
        "API_KEY",
        "CREDENTIAL",
    ]
    .iter()
    .any(|needle| upper.contains(needle))
}

fn secret_cache_token(key: &[u8; 32], secret_material: &[(String, String)]) -> String {
    let mut normalized_key = [0_u8; 64];
    normalized_key[..key.len()].copy_from_slice(key);
    let mut inner_key = [0_u8; 64];
    let mut outer_key = [0_u8; 64];
    for index in 0..64 {
        inner_key[index] = normalized_key[index] ^ 0x36;
        outer_key[index] = normalized_key[index] ^ 0x5c;
    }

    let mut hasher = Sha256::new();
    hasher.update(inner_key);
    hasher.update(b"ployz:railpack-secrets:v1\0");
    for (key, value) in secret_material {
        update_length_prefixed(&mut hasher, key.as_bytes());
        update_length_prefixed(&mut hasher, value.as_bytes());
    }
    let inner = hasher.finalize();

    let mut hasher = Sha256::new();
    hasher.update(outer_key);
    hasher.update(inner);
    hex_lower(&hasher.finalize())
}

fn update_length_prefixed(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(value.len().to_be_bytes());
    hasher.update(value);
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

pub struct BuildCommandPaths {
    pub cleanup_dirs: Vec<PathBuf>,
    railpack_plan_path: Option<PathBuf>,
    railpack_info_path: Option<PathBuf>,
    buildkit_secret_files: Vec<(String, PathBuf)>,
    railpack_secret_cache_token: Option<String>,
}

struct CleanupDirsOnError {
    dirs: Vec<PathBuf>,
}

impl CleanupDirsOnError {
    fn new() -> Self {
        Self { dirs: Vec::new() }
    }

    fn push(&mut self, path: PathBuf) {
        self.dirs.push(path);
    }

    fn disarm(mut self) {
        self.dirs.clear();
    }
}

impl Drop for CleanupDirsOnError {
    fn drop(&mut self) {
        for path in &self.dirs {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

pub fn prepare_build_command_paths(
    data_dir: &Path,
    method: BuildMethod,
    operation_id: &str,
    invocation: &BuildInvocationPlan,
) -> Result<BuildCommandPaths, String> {
    let mut paths = BuildCommandPaths {
        cleanup_dirs: Vec::new(),
        railpack_plan_path: None,
        railpack_info_path: None,
        buildkit_secret_files: Vec::new(),
        railpack_secret_cache_token: None,
    };
    let mut cleanup_on_error = CleanupDirsOnError::new();
    if method == BuildMethod::Railpack || !invocation.secret_env.is_empty() {
        let metadata_dir = railpack_metadata_dir(data_dir, operation_id);
        create_private_dir(&metadata_dir)?;
        paths.cleanup_dirs.push(metadata_dir.clone());
        cleanup_on_error.push(metadata_dir.clone());
        if method == BuildMethod::Railpack {
            paths.railpack_plan_path = Some(metadata_dir.join("railpack-plan.json"));
            paths.railpack_info_path = Some(metadata_dir.join("railpack-info.json"));
        }
        if !invocation.secret_env.is_empty() {
            let secret_dir = metadata_dir.join("secrets");
            create_private_dir(&secret_dir)?;
            for (key, value) in &invocation.secret_env {
                let secret_path = secret_dir.join(key);
                write_private_file(&secret_path, value.as_bytes())?;
                paths.buildkit_secret_files.push((key.clone(), secret_path));
            }
        }
    }
    if invocation.railpack_secret_cache_required {
        let key = load_or_create_build_cache_key(data_dir)?;
        paths.railpack_secret_cache_token = Some(secret_cache_token(&key, &invocation.secret_env));
    }
    cleanup_on_error.disarm();
    Ok(paths)
}

pub fn build_command_plan(
    request: &BuildLocalRequest,
    invocation: &BuildInvocationPlan,
    paths: BuildCommandPaths,
) -> Result<BuildCommandPlan, String> {
    match request.method {
        BuildMethod::Dockerfile => dockerfile_command_plan(request, invocation, paths),
        BuildMethod::Railpack => railpack_command_plan(request, invocation, paths),
    }
}

fn dockerfile_command_plan(
    request: &BuildLocalRequest,
    invocation: &BuildInvocationPlan,
    paths: BuildCommandPaths,
) -> Result<BuildCommandPlan, String> {
    let mut args = vec!["build".into(), "-t".into(), request.image_name.clone()];
    if let Some(platform) = &request.platform {
        args.push("--platform".into());
        args.push(format_platform(platform));
    };
    for (key, value) in &invocation.plain_env {
        args.push("--build-arg".into());
        args.push(format!("{key}={value}"));
    }
    for (key, value) in &invocation.docker_build_args {
        args.push("--build-arg".into());
        args.push(format!("{key}={value}"));
    }
    for (key, path) in &paths.buildkit_secret_files {
        args.push("--secret".into());
        args.push(format!("id={key},src={}", path.display()));
    }
    args.push(".".into());
    let env = if invocation.buildkit_secret_env.is_empty() {
        Vec::new()
    } else {
        vec![(DOCKER_BUILDKIT_ENV.into(), "1".into())]
    };
    Ok(BuildCommandPlan::new(
        Vec::new(),
        BuildCommandStep {
            kind: BuildCommandStepKind::ImageBuild,
            command: BuildCommand {
                program: "docker",
                args,
                env,
                redaction_values: command_redaction_values(invocation, &paths),
            },
        },
        paths.cleanup_dirs,
    ))
}

fn railpack_command_plan(
    request: &BuildLocalRequest,
    invocation: &BuildInvocationPlan,
    paths: BuildCommandPaths,
) -> Result<BuildCommandPlan, String> {
    let Some(plan_path) = paths.railpack_plan_path.as_ref() else {
        return Err("railpack command plan missing plan output path".into());
    };
    let Some(info_path) = paths.railpack_info_path.as_ref() else {
        return Err("railpack command plan missing info output path".into());
    };

    let mut prepare_args = vec![
        "prepare".into(),
        "--plan-out".into(),
        plan_path.display().to_string(),
        "--info-out".into(),
        info_path.display().to_string(),
    ];
    for (key, value) in &invocation.plain_env {
        prepare_args.push("--env".into());
        prepare_args.push(format!("{key}={value}"));
    }
    for (key, _value) in &invocation.secret_env {
        prepare_args.push("--env".into());
        prepare_args.push(format!("{key}={RAILPACK_SECRET_PLACEHOLDER}"));
    }
    prepare_args.push(".".into());

    let mut build_args = vec![
        "buildx".into(),
        "build".into(),
        "-t".into(),
        request.image_name.clone(),
        "--build-arg".into(),
        format!("BUILDKIT_SYNTAX={RAILPACK_FRONTEND}"),
        "-f".into(),
        plan_path.display().to_string(),
    ];
    if let Some(platform) = &request.platform {
        build_args.push("--platform".into());
        build_args.push(format_platform(platform));
    }
    build_args.push("--load".into());
    for (key, value) in &invocation.plain_env {
        build_args.push("--build-arg".into());
        build_args.push(format!("{key}={value}"));
    }
    for (key, path) in &paths.buildkit_secret_files {
        build_args.push("--secret".into());
        build_args.push(format!("id={key},src={}", path.display()));
    }
    if let Some(token) = &paths.railpack_secret_cache_token {
        build_args.push("--build-arg".into());
        build_args.push(format!("secrets-hash={token}"));
    }
    build_args.push(".".into());

    let docker_env = vec![(DOCKER_BUILDKIT_ENV.into(), "1".into())];
    let redaction_values = command_redaction_values(invocation, &paths);
    Ok(BuildCommandPlan::new(
        vec![BuildCommandStep {
            kind: BuildCommandStepKind::RailpackPrepare,
            command: BuildCommand {
                program: "railpack",
                args: prepare_args,
                env: Vec::new(),
                redaction_values: redaction_values.clone(),
            },
        }],
        BuildCommandStep {
            kind: BuildCommandStepKind::ImageBuild,
            command: BuildCommand {
                program: "docker",
                args: build_args,
                env: docker_env,
                redaction_values,
            },
        },
        paths.cleanup_dirs,
    ))
}

fn command_redaction_values(
    invocation: &BuildInvocationPlan,
    paths: &BuildCommandPaths,
) -> Vec<String> {
    let mut values = invocation
        .env
        .iter()
        .chain(invocation.docker_build_args.iter())
        .map(|(_key, value)| value.clone())
        .collect::<Vec<_>>();
    if let Some(token) = &paths.railpack_secret_cache_token {
        values.push(token.clone());
    }
    values
}

fn railpack_metadata_dir(data_dir: &Path, operation_id: &str) -> PathBuf {
    data_dir.join("build-work").join(operation_id)
}

fn create_private_dir(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|error| {
        format!(
            "create private build directory '{}': {error}",
            path.display()
        )
    })?;
    #[cfg(unix)]
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(|error| {
        format!(
            "set private build directory permissions '{}': {error}",
            path.display()
        )
    })?;
    Ok(())
}

fn write_private_file(path: &Path, contents: &[u8]) -> Result<(), String> {
    write_private_file_io(path, contents).map_err(|error| match error.kind() {
        std::io::ErrorKind::AlreadyExists => {
            format!("create private build secret '{}': {error}", path.display())
        }
        _ => format!("write private build secret '{}': {error}", path.display()),
    })
}

fn write_private_file_io(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path)?;
    file.write_all(contents)
}

fn load_or_create_build_cache_key(data_dir: &Path) -> Result<[u8; 32], String> {
    let key_dir = data_dir.join("build-work");
    create_private_dir(&key_dir)?;
    let key_path = key_dir.join("cache-token-key");
    let mut last_retryable_error = None;
    for _attempt in 0..BUILD_CACHE_KEY_RETRY_ATTEMPTS {
        match read_build_cache_key(&key_path) {
            Ok(key) => return Ok(key),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut key = [0_u8; 32];
                rand::fill(&mut key);
                match write_build_cache_key(&key_path, &key) {
                    Ok(()) => return Ok(key),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        last_retryable_error = Some(error);
                    }
                    Err(error) => {
                        return Err(format!(
                            "create build cache token key '{}': {error}",
                            key_path.display()
                        ));
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                last_retryable_error = Some(error);
            }
            Err(error) => {
                return Err(format!(
                    "read build cache token key '{}': {error}",
                    key_path.display()
                ));
            }
        }
        std::thread::sleep(BUILD_CACHE_KEY_RETRY_DELAY);
    }
    let reason = last_retryable_error
        .map(|error| error.to_string())
        .unwrap_or_else(|| "retry attempts exhausted".into());
    match read_build_cache_key(&key_path) {
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            return repair_build_cache_key(&key_path).map_err(|error| {
                format!(
                    "repair build cache token key '{}': {error}",
                    key_path.display()
                )
            });
        }
        Ok(key) => return Ok(key),
        Err(_error) => {}
    }
    Err(format!(
        "read build cache token key '{}' after {BUILD_CACHE_KEY_RETRY_ATTEMPTS} attempts: {reason}",
        key_path.display()
    ))
}

fn read_build_cache_key(key_path: &Path) -> std::io::Result<[u8; 32]> {
    let len = std::fs::metadata(key_path)?.len();
    if len != 32 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            format!("expected 32 byte build cache token key, found {len} bytes"),
        ));
    }
    let mut key = [0_u8; 32];
    let mut file = OpenOptions::new().read(true).open(key_path)?;
    file.read_exact(&mut key)?;
    Ok(key)
}

fn write_build_cache_key(key_path: &Path, key: &[u8; 32]) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(key_path)?;
    file.write_all(key)
}

fn repair_build_cache_key(key_path: &Path) -> std::io::Result<[u8; 32]> {
    let lock_dir = key_path.with_file_name("cache-token-key.repair-lock");
    let mut last_retryable_error = None;
    for _attempt in 0..BUILD_CACHE_KEY_RETRY_ATTEMPTS {
        match try_acquire_repair_lock(&lock_dir)? {
            Some(_guard) => {
                return match read_build_cache_key(key_path) {
                    Ok(key) => Ok(key),
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::UnexpectedEof
                        ) =>
                    {
                        replace_build_cache_key(key_path)
                    }
                    Err(error) => Err(error),
                };
            }
            None => match read_build_cache_key(key_path) {
                Ok(key) => return Ok(key),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::UnexpectedEof
                    ) =>
                {
                    last_retryable_error = Some(error);
                }
                Err(error) => return Err(error),
            },
        }
        std::thread::sleep(BUILD_CACHE_KEY_RETRY_DELAY);
    }
    match read_build_cache_key(key_path) {
        Ok(key) => Ok(key),
        Err(error) => Err(last_retryable_error.unwrap_or(error)),
    }
}

struct RepairLockGuard {
    path: PathBuf,
    token: String,
}

impl Drop for RepairLockGuard {
    fn drop(&mut self) {
        if repair_lock_token_matches(&self.path, &self.token).unwrap_or(false) {
            let _ = std::fs::remove_file(repair_lock_token_path(&self.path));
            let _ = std::fs::remove_dir(&self.path);
        }
    }
}

fn try_acquire_repair_lock(lock_dir: &Path) -> std::io::Result<Option<RepairLockGuard>> {
    let token = new_repair_lock_token();
    match std::fs::create_dir(lock_dir) {
        Ok(()) => {
            write_repair_lock_token(lock_dir, &token)?;
            Ok(Some(RepairLockGuard {
                path: lock_dir.to_path_buf(),
                token,
            }))
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if repair_lock_is_stale(lock_dir)? {
                let observed_token = read_repair_lock_token(lock_dir)?;
                if repair_lock_is_stale(lock_dir)?
                    && repair_lock_observed_token_matches(lock_dir, observed_token.as_deref())?
                {
                    let _ = std::fs::remove_dir_all(lock_dir);
                }
                return match std::fs::create_dir(lock_dir) {
                    Ok(()) => {
                        write_repair_lock_token(lock_dir, &token)?;
                        Ok(Some(RepairLockGuard {
                            path: lock_dir.to_path_buf(),
                            token,
                        }))
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
                    Err(error) => Err(error),
                };
            }
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn new_repair_lock_token() -> String {
    let mut bytes = [0_u8; 16];
    rand::fill(&mut bytes);
    hex_lower(&bytes)
}

fn write_repair_lock_token(lock_dir: &Path, token: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    std::fs::set_permissions(lock_dir, std::fs::Permissions::from_mode(0o700))?;
    write_private_file_io(&repair_lock_token_path(lock_dir), token.as_bytes())
}

fn repair_lock_token_path(lock_dir: &Path) -> PathBuf {
    lock_dir.join("owner")
}

fn read_repair_lock_token(lock_dir: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(repair_lock_token_path(lock_dir)) {
        Ok(token) => Ok(Some(token)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn repair_lock_token_matches(lock_dir: &Path, token: &str) -> std::io::Result<bool> {
    Ok(read_repair_lock_token(lock_dir)?.as_deref() == Some(token))
}

fn repair_lock_observed_token_matches(
    lock_dir: &Path,
    observed_token: Option<&str>,
) -> std::io::Result<bool> {
    Ok(read_repair_lock_token(lock_dir)?.as_deref() == observed_token)
}

fn repair_lock_is_stale(lock_dir: &Path) -> std::io::Result<bool> {
    let modified = std::fs::metadata(lock_dir)?.modified()?;
    Ok(modified
        .elapsed()
        .is_ok_and(|age| age >= BUILD_CACHE_KEY_REPAIR_LOCK_STALE))
}

fn replace_build_cache_key(key_path: &Path) -> std::io::Result<[u8; 32]> {
    let mut key = [0_u8; 32];
    rand::fill(&mut key);
    let mut suffix = [0_u8; 8];
    rand::fill(&mut suffix);
    let temp_path =
        key_path.with_file_name(format!("cache-token-key.repair-{}", hex_lower(&suffix)));
    write_build_cache_key(&temp_path, &key)?;
    let result = replace_build_cache_key_from_temp(&temp_path, key_path);
    let _ = std::fs::remove_file(&temp_path);
    result.map(|()| key)
}

#[cfg(unix)]
fn replace_build_cache_key_from_temp(temp_path: &Path, key_path: &Path) -> std::io::Result<()> {
    std::fs::rename(temp_path, key_path)
}

#[cfg(not(unix))]
fn replace_build_cache_key_from_temp(temp_path: &Path, key_path: &Path) -> std::io::Result<()> {
    let key = std::fs::read(temp_path)?;
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    let mut file = options.open(key_path)?;
    file.write_all(&key)
}

pub fn normalize_build_image_name(reference: &str) -> String {
    if reference.starts_with("sha256:") || image_reference_has_tag(reference) {
        reference.into()
    } else {
        format!("{reference}:latest")
    }
}

fn image_reference_has_tag(reference: &str) -> bool {
    let last_slash = reference.rfind('/');
    reference.rfind(':').is_some_and(|index| {
        index + 1 < reference.len() && last_slash.is_none_or(|slash| index > slash)
    })
}

fn format_platform(platform: &ImagePlatform) -> String {
    match platform.variant.as_deref() {
        Some(variant) => format!("{}/{}/{}", platform.os, platform.architecture, variant),
        None => format!("{}/{}", platform.os, platform.architecture),
    }
}

pub fn build_image_artifact(
    request: &BuildLocalRequest,
    image: &RuntimeImage,
) -> Result<ImageArtifact, String> {
    let digest = runtime_image_identity(&request.image_name, image)?;
    let now = now_unix_secs();
    Ok(ImageArtifact {
        image: image_ref_from_tag(&request.image_name, digest),
        platform: request.platform.clone().or_else(|| image.platform.clone()),
        provenance: ImageArtifactProvenance::Build {
            method: request.method,
            location: BuildLocation::Local,
            source_digest: None,
        },
        created_at: now,
    })
}

fn runtime_image_identity(reference: &str, image: &RuntimeImage) -> Result<ImageDigest, String> {
    let Some(id) = image.id.as_deref() else {
        return Err(RuntimeImageError::MissingDigest {
            reference: reference.into(),
        }
        .to_string());
    };
    ImageDigest::try_new(id).map_err(|_| {
        RuntimeImageError::MissingDigest {
            reference: reference.into(),
        }
        .to_string()
    })
}

pub fn present_build_availability(
    machine_id: &ployz_model::MachineId,
    artifact: ImageArtifact,
    operation_id: &str,
) -> ImageAvailabilityRecord {
    let now = now_unix_secs();
    ImageAvailabilityRecord {
        machine_id: machine_id.clone(),
        digest: artifact.image.digest().clone(),
        presence: ImagePresence::Present {
            artifact,
            recorded_at: now,
            source_operation_id: Some(operation_id.into()),
        },
        updated_at: now,
    }
}

fn image_ref_from_tag(reference: &str, digest: ImageDigest) -> ImageRef {
    if reference.starts_with("sha256:") {
        return ImageRef::digest_only(digest);
    }

    let (repository, tag) = {
        let last_slash = reference.rfind('/');
        match reference
            .rfind(':')
            .filter(|index| last_slash.is_none_or(|slash| *index > slash))
        {
            Some(index) if index + 1 < reference.len() => {
                let (repository, tag) = reference.split_at(index);
                (repository.to_string(), Some(tag[1..].to_string()))
            }
            _ => (reference.to_string(), None),
        }
    };
    ImageRef::repository_digest(repository, tag, digest)
}

pub fn build_command_failure_message(
    command: &BuildCommand,
    output: &BuildCommandOutput,
) -> String {
    let mut lines = vec![format!(
        "{} {} exited unsuccessfully",
        command.program,
        command.redacted_args().join(" ")
    )];
    if output.timed_out {
        lines.push(format!(
            "timed out after {} seconds",
            BUILD_COMMAND_TIMEOUT.as_secs()
        ));
    }
    if !output.stdout.trim().is_empty() {
        lines.push(format!(
            "stdout: {}",
            command.redact_captured_output(output.stdout.trim())
        ));
    }
    if !output.stderr.trim().is_empty() {
        lines.push(format!(
            "stderr: {}",
            command.redact_captured_output(output.stderr.trim())
        ));
    }
    lines.join("; ")
}

pub fn render_build_result(
    operation_id: &str,
    record: &ImageAvailabilityRecord,
    output: BuildCommandOutput,
    command: &BuildCommand,
) -> String {
    let mut message = format!(
        "{}  {}  {}  present",
        operation_id,
        record.machine_id,
        record.digest.as_str()
    );
    if !output.stdout.trim().is_empty() {
        message.push('\n');
        message.push_str(&command.redact_captured_output(output.stdout.trim()));
    }
    message
}
