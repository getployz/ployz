use std::fs::OpenOptions;
use std::io::{Read as _, Write as _};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ployz_api::{
    BuildEnvValue, BuildInputs, BuildLocalRequest, BuildOperationPayload, BuildResultPayload,
    DaemonPayload,
};
use ployz_runtime_api::{RuntimeImage, RuntimeImageBackend, RuntimeImageError};
use ployz_store_api::ImageAvailabilityStore;
#[cfg(test)]
use ployz_store_memory::StoreDriverMemoryExt as _;
use ployz_types::model::{
    BuildInputSummary, BuildLocation, BuildMethod, BuildOperationKind, BuildSecretSummary,
    ImageArtifact, ImageArtifactProvenance, ImageAvailabilityRecord, ImageDigest, ImagePlatform,
    ImagePresence, ImageRef, OperationStatus,
};
use ployz_types::time::now_unix_secs;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::time::timeout;

use crate::daemon::DaemonState;

const BUILD_COMMAND_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const BUILD_OUTPUT_TAIL_BYTES: usize = 64 * 1024;
const DOCKER_BUILDKIT_ENV: &str = "DOCKER_BUILDKIT";
const RAILPACK_FRONTEND: &str = "ghcr.io/railwayapp/railpack-frontend";
const RAILPACK_SECRET_PLACEHOLDER: &str = "__PLOYZ_BUILDKIT_SECRET__";
const BUILD_CACHE_KEY_RETRY_ATTEMPTS: usize = 20;
const BUILD_CACHE_KEY_RETRY_DELAY: Duration = Duration::from_millis(10);
const BUILD_CACHE_KEY_REPAIR_LOCK_STALE: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BuildCommandStepKind {
    RailpackPrepare,
    ImageBuild,
}

impl BuildCommandStepKind {
    fn stage(self) -> &'static str {
        match self {
            Self::RailpackPrepare => "preparing railpack build plan",
            Self::ImageBuild => "running image build command",
        }
    }
}

#[derive(Debug)]
struct BuildCommandStep {
    kind: BuildCommandStepKind,
    command: BuildCommand,
}

#[derive(Debug)]
struct BuildCommandPlan {
    pre_build_steps: Vec<BuildCommandStep>,
    image_build: BuildCommandStep,
    cleanup_dirs: Vec<PathBuf>,
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

    fn steps(&self) -> impl Iterator<Item = &BuildCommandStep> {
        self.pre_build_steps
            .iter()
            .chain(std::iter::once(&self.image_build))
    }

    fn image_build_command(&self) -> &BuildCommand {
        &self.image_build.command
    }

    fn redact_text(&self, text: &str) -> String {
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

struct BuildCommand {
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

    fn redact_text(&self, text: &str) -> String {
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
struct BuildCommandOutput {
    status_success: bool,
    timed_out: bool,
    stdout: String,
    stderr: String,
}

#[async_trait]
trait BuildCommandRunner: Send + Sync {
    async fn run(
        &self,
        command: &BuildCommand,
        current_dir: &Path,
    ) -> Result<BuildCommandOutput, String>;
}

struct TokioBuildCommandRunner;

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

impl DaemonState {
    pub(crate) async fn handle_build_local(
        &self,
        request: &BuildLocalRequest,
    ) -> ployz_api::DaemonResponse {
        let runner = TokioBuildCommandRunner;
        self.handle_build_local_with_runner_and_backend(request, &runner, None)
            .await
    }

    async fn handle_build_local_with_runner_and_backend(
        &self,
        request: &BuildLocalRequest,
        runner: &dyn BuildCommandRunner,
        backend_result: Option<Result<Arc<dyn RuntimeImageBackend>, String>>,
    ) -> ployz_api::DaemonResponse {
        let active = match self.require_active(
            "BUILD_LOCAL_INACTIVE",
            "build local requires a running mesh",
        ) {
            Ok(active) => active,
            Err(response) => return *response,
        };
        let invocation = match plan_build_invocation(request.method, &request.inputs) {
            Ok(invocation) => invocation,
            Err(error) => return self.err("BUILD_LOCAL_INPUT_INVALID", error),
        };
        if request.push_target.is_some() || !request.distribute_targets.is_empty() {
            return self.err(
                "BUILD_LOCAL_IMAGE_MOVEMENT_UNSUPPORTED",
                "build local does not support push_target or distribute_targets in this release; run image push or image distribute explicitly after the build",
            );
        }
        let context_dir = PathBuf::from(&request.context_dir);
        if !context_dir.is_dir() {
            return self.err(
                "BUILD_LOCAL_CONTEXT_NOT_FOUND",
                format!(
                    "build context '{}' is not a directory",
                    context_dir.display()
                ),
            );
        }
        let image_name = normalize_build_image_name(&request.image_name);
        let request = if image_name == request.image_name {
            request.clone()
        } else {
            BuildLocalRequest {
                image_name,
                ..request.clone()
            }
        };
        let operation_store = self.build_operation_store();
        let mut operation = match operation_store.begin_with_input_summary(
            BuildOperationKind::Local,
            request.method,
            BuildLocation::Local,
            "waiting for image build lock",
            invocation.summary.clone(),
        ) {
            Ok(operation) => operation,
            Err(error) => return self.err("BUILD_LOCAL_OPERATION_FAILED", error),
        };
        let command_paths = match prepare_build_command_paths(
            &self.data_dir,
            request.method,
            &operation.id,
            &invocation,
        ) {
            Ok(paths) => paths,
            Err(error) => {
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_INPUT_UNSUPPORTED",
                    error,
                );
            }
        };
        let command_plan = match build_command_plan(&request, &invocation, command_paths) {
            Ok(command_plan) => command_plan,
            Err(error) => {
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_INPUT_UNSUPPORTED",
                    error,
                );
            }
        };

        let build_lock = self.local_build_lock(&request.image_name).await;
        let Ok(_build_guard) = build_lock.try_lock() else {
            let message = format!(
                "another local build for image '{}' is already running",
                request.image_name
            );
            return self.fail_build_local_operation(
                &operation_store,
                &mut operation,
                "BUILD_LOCAL_IMAGE_BUSY",
                message,
            );
        };
        let backend = match match backend_result {
            Some(result) => result,
            None => self.runtime_image_backend().await,
        } {
            Ok(backend) => backend,
            Err(error) => {
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_RUNTIME_UNAVAILABLE",
                    error,
                );
            }
        };
        for step in &command_plan.pre_build_steps {
            if let Err(error) = operation_store.update_stage(&mut operation, step.kind.stage()) {
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_OPERATION_FAILED",
                    error,
                );
            }
            match runner.run(&step.command, &context_dir).await {
                Ok(output) if output.status_success => {}
                Ok(output) => {
                    let message = build_command_failure_message(&step.command, &output);
                    return self.fail_build_local_operation(
                        &operation_store,
                        &mut operation,
                        "BUILD_LOCAL_COMMAND_FAILED",
                        message,
                    );
                }
                Err(error) => {
                    let error = command_plan.redact_text(&error);
                    return self.fail_build_local_operation(
                        &operation_store,
                        &mut operation,
                        "BUILD_LOCAL_COMMAND_FAILED",
                        error,
                    );
                }
            }
        }
        let image_build = &command_plan.image_build;
        if let Err(error) = operation_store.update_stage(&mut operation, image_build.kind.stage()) {
            return self.fail_build_local_operation(
                &operation_store,
                &mut operation,
                "BUILD_LOCAL_OPERATION_FAILED",
                error,
            );
        }
        let output = match runner.run(&image_build.command, &context_dir).await {
            Ok(output) if output.status_success => output,
            Ok(output) => {
                let message = build_command_failure_message(&image_build.command, &output);
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_COMMAND_FAILED",
                    message,
                );
            }
            Err(error) => {
                let error = command_plan.redact_text(&error);
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_COMMAND_FAILED",
                    error,
                );
            }
        };

        if let Err(error) = operation_store.update_stage(&mut operation, "inspecting built image") {
            return self.fail_build_local_operation(
                &operation_store,
                &mut operation,
                "BUILD_LOCAL_OPERATION_FAILED",
                error,
            );
        }
        let image = match backend.as_ref().inspect_image(&request.image_name).await {
            Ok(Some(image)) => image,
            Ok(None) => {
                let message = format!("built image '{}' was not found", request.image_name);
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_IMAGE_NOT_FOUND",
                    message,
                );
            }
            Err(error) => {
                let message = format!("inspect built image '{}': {error}", request.image_name);
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_INSPECT_FAILED",
                    message,
                );
            }
        };
        let artifact = match build_image_artifact(&request, &image) {
            Ok(artifact) => artifact,
            Err(error) => {
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_MISSING_DIGEST",
                    error,
                );
            }
        };
        let record =
            present_build_availability(&self.identity.machine_id, artifact.clone(), &operation.id);

        if let Err(error) = operation_store.update_artifact(&mut operation, artifact.clone()) {
            return self.fail_build_local_operation_with_result(
                &operation_store,
                &mut operation,
                "BUILD_LOCAL_OPERATION_FAILED",
                error,
                artifact,
                None,
            );
        }
        if let Err(error) = active.mesh.store.upsert_image_availability(&record).await {
            let message = format!("record built image availability: {error}");
            return self.fail_build_local_operation_with_result(
                &operation_store,
                &mut operation,
                "BUILD_LOCAL_AVAILABILITY_FAILED",
                message,
                artifact,
                None,
            );
        }
        if let Err(error) =
            operation_store.update_status(&mut operation, OperationStatus::Succeeded, None)
        {
            return self.fail_build_local_operation_with_result(
                &operation_store,
                &mut operation,
                "BUILD_LOCAL_OPERATION_FAILED",
                error,
                artifact,
                Some(record),
            );
        }

        let image_build_command = command_plan.image_build_command();
        let message = render_build_result(&operation.id, &record, output, image_build_command);
        self.ok_with_payload(
            message,
            Some(DaemonPayload::BuildResult(BuildResultPayload {
                operation_id: operation.id,
                artifact,
                availability: Some(record),
            })),
        )
    }

    fn fail_build_local_operation(
        &self,
        operation_store: &super::operations::BuildOperationStore,
        operation: &mut ployz_types::model::BuildOperationRecord,
        code: &str,
        message: String,
    ) -> ployz_api::DaemonResponse {
        let _ = operation_store.update_status(
            operation,
            OperationStatus::Failed,
            Some(message.clone()),
        );
        self.err_with_payload(
            code,
            message,
            Some(DaemonPayload::BuildOperation(BuildOperationPayload {
                operation: operation.clone(),
            })),
        )
    }

    fn fail_build_local_operation_with_result(
        &self,
        operation_store: &super::operations::BuildOperationStore,
        operation: &mut ployz_types::model::BuildOperationRecord,
        code: &str,
        message: String,
        artifact: ImageArtifact,
        availability: Option<ImageAvailabilityRecord>,
    ) -> ployz_api::DaemonResponse {
        let _ = operation_store.update_status(
            operation,
            OperationStatus::Failed,
            Some(message.clone()),
        );
        self.err_with_payload(
            code,
            message,
            Some(DaemonPayload::BuildResult(BuildResultPayload {
                operation_id: operation.id.clone(),
                artifact,
                availability,
            })),
        )
    }
}

pub(super) struct BuildInvocationPlan {
    pub(super) summary: BuildInputSummary,
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

pub(super) fn plan_build_invocation(
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

struct BuildCommandPaths {
    cleanup_dirs: Vec<PathBuf>,
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

fn prepare_build_command_paths(
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

fn build_command_plan(
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

fn normalize_build_image_name(reference: &str) -> String {
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

fn build_image_artifact(
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

fn present_build_availability(
    machine_id: &ployz_types::model::MachineId,
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

fn build_command_failure_message(command: &BuildCommand, output: &BuildCommandOutput) -> String {
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

fn render_build_result(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::{ActiveMesh, RetainedSubnet};
    use crate::mesh_state::network::{DEFAULT_CLUSTER_CIDR, NetworkConfig};
    use ployz_api::{BuildEnvValue, BuildInputs};
    use ployz_orchestrator::Mesh;
    use ployz_orchestrator::mesh::driver::WireguardDriver;
    use ployz_runtime_api::{Identity, NoopRuntimeHandle};
    use ployz_store_api::{ImageAvailabilityStore, MachineMembershipStore, StoreDriver};
    use ployz_types::model::{
        ImagePlatform, MachineId, MachineMembership, NetworkLifecycle, NetworkName, OverlayIp,
        PublicKey,
    };
    use std::collections::BTreeMap;
    use std::sync::{Barrier, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn digest(ch: char) -> ImageDigest {
        ImageDigest::try_new(format!("sha256:{}", ch.to_string().repeat(64))).expect("digest")
    }

    struct FakeRunner {
        output: BuildCommandOutput,
    }

    #[async_trait]
    impl BuildCommandRunner for FakeRunner {
        async fn run(
            &self,
            _command: &BuildCommand,
            _current_dir: &Path,
        ) -> Result<BuildCommandOutput, String> {
            Ok(BuildCommandOutput {
                status_success: self.output.status_success,
                timed_out: self.output.timed_out,
                stdout: self.output.stdout.clone(),
                stderr: self.output.stderr.clone(),
            })
        }
    }

    struct FailingRunner {
        error: String,
    }

    #[async_trait]
    impl BuildCommandRunner for FailingRunner {
        async fn run(
            &self,
            _command: &BuildCommand,
            _current_dir: &Path,
        ) -> Result<BuildCommandOutput, String> {
            Err(self.error.clone())
        }
    }

    struct RecordingRunner {
        output: BuildCommandOutput,
        commands: Mutex<Vec<&'static str>>,
    }

    impl RecordingRunner {
        fn new(output: BuildCommandOutput) -> Self {
            Self {
                output,
                commands: Mutex::new(Vec::new()),
            }
        }

        fn programs(&self) -> Vec<&'static str> {
            self.commands.lock().expect("commands lock").clone()
        }
    }

    struct SequencedRunner {
        outputs: Mutex<Vec<Result<BuildCommandOutput, String>>>,
        commands: Mutex<Vec<&'static str>>,
    }

    impl SequencedRunner {
        fn new(outputs: Vec<Result<BuildCommandOutput, String>>) -> Self {
            Self {
                outputs: Mutex::new(outputs),
                commands: Mutex::new(Vec::new()),
            }
        }

        fn programs(&self) -> Vec<&'static str> {
            self.commands.lock().expect("commands lock").clone()
        }
    }

    #[async_trait]
    impl BuildCommandRunner for SequencedRunner {
        async fn run(
            &self,
            command: &BuildCommand,
            _current_dir: &Path,
        ) -> Result<BuildCommandOutput, String> {
            self.commands
                .lock()
                .expect("commands lock")
                .push(command.program);
            let mut outputs = self.outputs.lock().expect("outputs lock");
            if outputs.is_empty() {
                return Err("unexpected extra build command".into());
            }
            outputs.remove(0)
        }
    }

    #[async_trait]
    impl BuildCommandRunner for RecordingRunner {
        async fn run(
            &self,
            command: &BuildCommand,
            _current_dir: &Path,
        ) -> Result<BuildCommandOutput, String> {
            self.commands
                .lock()
                .expect("commands lock")
                .push(command.program);
            Ok(BuildCommandOutput {
                status_success: self.output.status_success,
                timed_out: self.output.timed_out,
                stdout: self.output.stdout.clone(),
                stderr: self.output.stderr.clone(),
            })
        }
    }

    struct FakeBackend {
        image: Option<RuntimeImage>,
    }

    #[async_trait]
    impl RuntimeImageBackend for FakeBackend {
        async fn inspect_image(
            &self,
            _reference: &str,
        ) -> Result<Option<RuntimeImage>, RuntimeImageError> {
            Ok(self.image.clone())
        }
    }

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()))
    }

    async fn active_state() -> DaemonState {
        let data_dir = temp_dir("ployz-build-local-test");
        std::fs::create_dir_all(&data_dir).expect("create data dir");
        let identity = Identity::generate(MachineId::new("founder"), [11; 32]);
        let mut config = NetworkConfig::new(
            NetworkName("alpha".into()),
            &identity.public_key,
            DEFAULT_CLUSTER_CIDR,
            "10.210.0.0/24".parse().expect("valid subnet"),
        );
        config.lifecycle = NetworkLifecycle::Running;
        let store = StoreDriver::memory();
        store
            .upsert_self_machine(&MachineMembership::seed(
                MachineId::new("founder"),
                PublicKey([11; 32]),
                OverlayIp("fd00::11".parse().expect("valid overlay")),
                None,
                Vec::new(),
            ))
            .await
            .expect("insert founder");
        let mesh = Mesh::new(
            WireguardDriver::memory(),
            store,
            None,
            identity.machine_id.clone(),
            51820,
        );
        let mut state = DaemonState::new_for_tests(
            &data_dir,
            identity,
            DEFAULT_CLUSTER_CIDR.into(),
            24,
            4319,
            "127.0.0.1:0".into(),
            None,
            1,
        );
        state.active = Some(ActiveMesh {
            retained_subnet: RetainedSubnet::from_running_config(config.subnet),
            config,
            mesh,
            nats_control: Box::new(NoopRuntimeHandle),
            zfs_transfer: Box::new(NoopRuntimeHandle),
            image_receiver: Box::new(NoopRuntimeHandle),
            image_receiver_bind_addr: None,
            gateway: Box::new(NoopRuntimeHandle),
            dns: Box::new(NoopRuntimeHandle),
            certificate_renewal: None,
            bootstrap_peer_seed: None,
        });
        state
    }

    fn build_request(context_dir: &Path) -> BuildLocalRequest {
        BuildLocalRequest {
            method: BuildMethod::Dockerfile,
            context_dir: context_dir.display().to_string(),
            image_name: "example/app:latest".into(),
            platform: None,
            push_target: None,
            distribute_targets: Vec::new(),
            inputs: BuildInputs::default(),
        }
    }

    fn command_paths_for(
        method: BuildMethod,
        invocation: &BuildInvocationPlan,
    ) -> (BuildCommandPaths, PathBuf) {
        let data_dir = temp_dir("ployz-build-command-plan");
        let paths = prepare_build_command_paths(&data_dir, method, "build-op", invocation)
            .expect("command paths");
        (paths, data_dir)
    }

    #[test]
    fn dockerfile_command_tags_platform_and_context() {
        let request = BuildLocalRequest {
            method: BuildMethod::Dockerfile,
            context_dir: "/tmp/context".into(),
            image_name: "example/app:latest".into(),
            platform: Some(ImagePlatform {
                os: "linux".into(),
                architecture: "amd64".into(),
                variant: None,
            }),
            push_target: None,
            distribute_targets: Vec::new(),
            inputs: BuildInputs::default(),
        };

        let invocation = plan_build_invocation(request.method, &request.inputs).expect("plan");
        let (paths, _data_dir) = command_paths_for(request.method, &invocation);
        let plan = build_command_plan(&request, &invocation, paths).expect("command plan");
        let command = plan.image_build_command();

        assert_eq!(command.program, "docker");
        assert_eq!(
            command.args,
            vec![
                "build",
                "-t",
                "example/app:latest",
                "--platform",
                "linux/amd64",
                "."
            ]
        );
    }

    #[test]
    fn railpack_command_prepares_plan_and_uses_frontend_with_platform_variant() {
        let request = BuildLocalRequest {
            method: BuildMethod::Railpack,
            context_dir: "/tmp/context".into(),
            image_name: "example/app:latest".into(),
            platform: Some(ImagePlatform {
                os: "linux".into(),
                architecture: "arm64".into(),
                variant: Some("v8".into()),
            }),
            push_target: None,
            distribute_targets: Vec::new(),
            inputs: BuildInputs::default(),
        };

        let invocation = plan_build_invocation(request.method, &request.inputs).expect("plan");
        let (paths, data_dir) = command_paths_for(request.method, &invocation);
        let plan_path = railpack_metadata_dir(&data_dir, "build-op").join("railpack-plan.json");
        let info_path = railpack_metadata_dir(&data_dir, "build-op").join("railpack-info.json");
        let plan = build_command_plan(&request, &invocation, paths).expect("command plan");
        let [prepare_step] = plan.pre_build_steps.as_slice() else {
            panic!("expected one railpack prepare step");
        };
        let build_step = &plan.image_build;

        assert_eq!(prepare_step.kind, BuildCommandStepKind::RailpackPrepare);
        assert_eq!(prepare_step.command.program, "railpack");
        assert_eq!(
            prepare_step.command.args,
            vec![
                "prepare",
                "--plan-out",
                plan_path.display().to_string().as_str(),
                "--info-out",
                info_path.display().to_string().as_str(),
                "."
            ]
        );
        assert_eq!(build_step.kind, BuildCommandStepKind::ImageBuild);
        assert_eq!(build_step.command.program, "docker");
        assert_eq!(
            build_step.command.args,
            vec![
                "buildx",
                "build",
                "-t",
                "example/app:latest",
                "--build-arg",
                format!("BUILDKIT_SYNTAX={RAILPACK_FRONTEND}").as_str(),
                "-f",
                plan_path.display().to_string().as_str(),
                "--platform",
                "linux/arm64/v8",
                "--load",
                "."
            ]
        );
        assert_eq!(
            build_step.command.env,
            vec![("DOCKER_BUILDKIT".into(), "1".into())]
        );
    }

    #[test]
    fn dockerfile_inputs_plan_build_args_and_secret_env_mounts() {
        let request = BuildLocalRequest {
            method: BuildMethod::Dockerfile,
            context_dir: "/tmp/context".into(),
            image_name: "example/app:latest".into(),
            platform: None,
            push_target: None,
            distribute_targets: Vec::new(),
            inputs: BuildInputs {
                env: BTreeMap::from([
                    (
                        "NODE_ENV".into(),
                        BuildEnvValue::Plain {
                            value: "production".into(),
                        },
                    ),
                    (
                        "SENTRY_AUTH_TOKEN".into(),
                        BuildEnvValue::Secret {
                            value: "super-secret-token".into(),
                            fingerprint: Some("sentry-v1".into()),
                        },
                    ),
                ]),
                docker_build_args: BTreeMap::from([("PUBLIC_COMMIT".into(), "abc123".into())]),
            },
        };

        let invocation = plan_build_invocation(request.method, &request.inputs).expect("plan");
        let (paths, _data_dir) = command_paths_for(request.method, &invocation);
        let plan = build_command_plan(&request, &invocation, paths).expect("command plan");
        let command = plan.image_build_command();

        assert_eq!(
            command.args,
            vec![
                "build",
                "-t",
                "example/app:latest",
                "--build-arg",
                "NODE_ENV=production",
                "--build-arg",
                "PUBLIC_COMMIT=abc123",
                "--secret",
                command
                    .args
                    .iter()
                    .find(|arg| arg.starts_with("id=SENTRY_AUTH_TOKEN,src="))
                    .expect("secret src arg")
                    .as_str(),
                "."
            ]
        );
        assert_eq!(command.env, vec![("DOCKER_BUILDKIT".into(), "1".into())]);
        assert_eq!(invocation.summary.env, vec!["NODE_ENV"]);
        assert_eq!(invocation.summary.secrets[0].name, "SENTRY_AUTH_TOKEN");
        assert_eq!(
            invocation.summary.secrets[0].fingerprint.as_deref(),
            Some("sentry-v1")
        );
        let rendered = format!("{command:?}");
        assert!(!rendered.contains("super-secret-token"));
        assert!(
            !build_command_failure_message(
                &command,
                &BuildCommandOutput {
                    status_success: false,
                    timed_out: false,
                    stdout: String::new(),
                    stderr: String::new(),
                },
            )
            .contains("super-secret-token")
        );
    }

    #[test]
    fn secret_env_rejects_reserved_docker_client_env_key() {
        let error = plan_build_invocation(
            BuildMethod::Dockerfile,
            &BuildInputs {
                env: BTreeMap::from([(
                    "DOCKER_BUILDKIT".into(),
                    BuildEnvValue::Secret {
                        value: "not-the-client-toggle".into(),
                        fingerprint: None,
                    },
                )]),
                docker_build_args: BTreeMap::new(),
            },
        )
        .expect_err("reserved client env");

        assert_eq!(
            error,
            "build env secret cannot use reserved Docker client env key 'DOCKER_BUILDKIT'"
        );
    }

    #[test]
    fn captured_output_with_sensitive_values_is_omitted() {
        let command = BuildCommand {
            program: "docker",
            args: vec!["build".into()],
            env: vec![("SENTRY_AUTH_TOKEN".into(), "super-secret-token".into())],
            redaction_values: vec!["abc123".into()],
        };

        let redacted = command.redact_captured_output("tail starts inside super-sec");

        assert_eq!(
            redacted,
            "[output omitted because build inputs contain redacted values]"
        );
    }

    #[test]
    fn railpack_secret_inputs_use_frontend_secrets_and_cache_token() {
        let request = BuildLocalRequest {
            method: BuildMethod::Railpack,
            context_dir: "/tmp/context".into(),
            image_name: "example/app:latest".into(),
            platform: None,
            push_target: None,
            distribute_targets: Vec::new(),
            inputs: BuildInputs {
                env: BTreeMap::from([
                    (
                        "NODE_ENV".into(),
                        BuildEnvValue::Plain {
                            value: "production".into(),
                        },
                    ),
                    (
                        "RAILS_MASTER_KEY".into(),
                        BuildEnvValue::Secret {
                            value: "master-key".into(),
                            fingerprint: None,
                        },
                    ),
                ]),
                docker_build_args: BTreeMap::new(),
            },
        };

        let invocation = plan_build_invocation(request.method, &request.inputs).expect("plan");
        let (paths, _data_dir) = command_paths_for(request.method, &invocation);
        let Some(token) = paths.railpack_secret_cache_token.as_deref() else {
            panic!("expected secret cache token");
        };
        let token = token.to_string();
        let plan = build_command_plan(&request, &invocation, paths).expect("command plan");
        let [prepare_step] = plan.pre_build_steps.as_slice() else {
            panic!("expected one railpack prepare step");
        };
        let build_step = &plan.image_build;

        assert!(prepare_step.command.args.contains(&"--env".into()));
        assert!(
            prepare_step
                .command
                .args
                .contains(&"NODE_ENV=production".into())
        );
        assert!(
            prepare_step
                .command
                .args
                .contains(&format!("RAILS_MASTER_KEY={RAILPACK_SECRET_PLACEHOLDER}"))
        );
        assert!(
            build_step
                .command
                .args
                .contains(&"NODE_ENV=production".into())
        );
        assert!(
            build_step
                .command
                .args
                .iter()
                .any(|arg| arg.starts_with("id=RAILS_MASTER_KEY,src="))
        );
        assert!(
            build_step
                .command
                .args
                .contains(&format!("secrets-hash={token}"))
        );
        assert_eq!(
            build_step.command.env,
            vec![("DOCKER_BUILDKIT".into(), "1".into())]
        );
        let rendered = format!("{plan:?}");
        assert!(!rendered.contains("master-key"));
        assert!(!rendered.contains("production"));
        assert!(!rendered.contains(&token));
    }

    #[test]
    fn railpack_rejects_docker_build_args() {
        let error = plan_build_invocation(
            BuildMethod::Railpack,
            &BuildInputs {
                env: BTreeMap::new(),
                docker_build_args: BTreeMap::from([("PUBLIC_COMMIT".into(), "abc123".into())]),
            },
        )
        .expect_err("railpack should reject docker args");

        assert!(error.contains("railpack builds do not accept"));
    }

    #[test]
    fn docker_build_args_reject_secret_like_names() {
        let error = plan_build_invocation(
            BuildMethod::Dockerfile,
            &BuildInputs {
                env: BTreeMap::new(),
                docker_build_args: BTreeMap::from([("API_TOKEN".into(), "secret".into())]),
            },
        )
        .expect_err("secret-like build arg should fail");

        assert!(error.contains("looks secret-bearing"));
    }

    #[test]
    fn docker_build_args_reject_duplicate_env_keys() {
        let error = plan_build_invocation(
            BuildMethod::Dockerfile,
            &BuildInputs {
                env: BTreeMap::from([(
                    "PUBLIC_COMMIT".into(),
                    BuildEnvValue::Plain {
                        value: "abc123".into(),
                    },
                )]),
                docker_build_args: BTreeMap::from([("PUBLIC_COMMIT".into(), "def456".into())]),
            },
        )
        .expect_err("duplicate build arg should fail");

        assert!(error.contains("duplicates a build env key"));
    }

    #[test]
    fn secret_fingerprints_reject_raw_hashes() {
        let error = plan_build_invocation(
            BuildMethod::Dockerfile,
            &BuildInputs {
                env: BTreeMap::from([(
                    "SECRET_VALUE".into(),
                    BuildEnvValue::Secret {
                        value: "secret".into(),
                        fingerprint: Some("a".repeat(64)),
                    },
                )]),
                docker_build_args: BTreeMap::new(),
            },
        )
        .expect_err("raw hash fingerprint should fail");

        assert!(error.contains("raw hash"));
    }

    #[test]
    fn railpack_secret_cache_token_changes_with_secret_values() {
        let inputs = |value: &str| BuildInputs {
            env: BTreeMap::from([(
                "SECRET_VALUE".into(),
                BuildEnvValue::Secret {
                    value: value.into(),
                    fingerprint: None,
                },
            )]),
            docker_build_args: BTreeMap::new(),
        };
        let data_dir = temp_dir("ployz-build-cache-token");
        let first_invocation =
            plan_build_invocation(BuildMethod::Railpack, &inputs("one")).expect("first plan");
        let first_paths = prepare_build_command_paths(
            &data_dir,
            BuildMethod::Railpack,
            "build-one",
            &first_invocation,
        )
        .expect("first paths");
        let Some(first) = first_paths.railpack_secret_cache_token else {
            panic!("expected first token");
        };
        let second_invocation =
            plan_build_invocation(BuildMethod::Railpack, &inputs("two")).expect("second plan");
        let second_paths = prepare_build_command_paths(
            &data_dir,
            BuildMethod::Railpack,
            "build-two",
            &second_invocation,
        )
        .expect("second paths");
        let Some(second) = second_paths.railpack_secret_cache_token else {
            panic!("expected second token");
        };

        assert_ne!(first, second);
    }

    #[test]
    fn railpack_secret_cache_key_creation_handles_concurrent_builds() {
        let data_dir = temp_dir("ployz-build-cache-key-race");
        let inputs = BuildInputs {
            env: BTreeMap::from([(
                "SECRET_VALUE".into(),
                BuildEnvValue::Secret {
                    value: "secret".into(),
                    fingerprint: None,
                },
            )]),
            docker_build_args: BTreeMap::new(),
        };
        let workers = 16;
        let barrier = Arc::new(Barrier::new(workers));
        let handles = (0..workers)
            .map(|index| {
                let data_dir = data_dir.clone();
                let inputs = inputs.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let invocation = plan_build_invocation(BuildMethod::Railpack, &inputs)
                        .expect("railpack invocation plan");
                    prepare_build_command_paths(
                        &data_dir,
                        BuildMethod::Railpack,
                        &format!("build-{index}"),
                        &invocation,
                    )
                    .map(|paths| paths.railpack_secret_cache_token)
                })
            })
            .collect::<Vec<_>>();

        let mut tokens = Vec::new();
        for handle in handles {
            let result = handle.join().expect("worker should not panic");
            let Ok(token) = result else {
                panic!("concurrent cache key creation should succeed");
            };
            let Some(token) = token else {
                panic!("expected concurrent cache token");
            };
            tokens.push(token);
        }
        let [first, rest @ ..] = tokens.as_slice() else {
            panic!("expected concurrent cache tokens");
        };
        assert!(rest.iter().all(|token| token == first));

        let invocation = plan_build_invocation(BuildMethod::Railpack, &inputs)
            .expect("railpack invocation plan");
        let later_paths = prepare_build_command_paths(
            &data_dir,
            BuildMethod::Railpack,
            "build-later",
            &invocation,
        )
        .expect("later paths");
        let Some(later) = later_paths.railpack_secret_cache_token else {
            panic!("expected later cache token");
        };
        assert_eq!(first, &later);
    }

    #[test]
    fn railpack_secret_cache_key_retries_short_reads() {
        let data_dir = temp_dir("ployz-build-cache-key-short-read");
        let key_dir = data_dir.join("build-work");
        create_private_dir(&key_dir).expect("create key dir");
        let key_path = key_dir.join("cache-token-key");
        write_private_file(&key_path, &[]).expect("create incomplete key file");
        let expected_key = [9_u8; 32];
        let writer_path = key_path.clone();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(BUILD_CACHE_KEY_RETRY_DELAY * 2);
            std::fs::write(writer_path, expected_key).expect("replace incomplete key");
        });

        let key = load_or_create_build_cache_key(&data_dir).expect("read retried cache key");
        handle.join().expect("writer should not panic");

        assert_eq!(key, expected_key);
    }

    #[test]
    fn railpack_secret_cache_key_repairs_stale_short_key() {
        let data_dir = temp_dir("ployz-build-cache-key-repair");
        let key_dir = data_dir.join("build-work");
        create_private_dir(&key_dir).expect("create key dir");
        let key_path = key_dir.join("cache-token-key");
        write_private_file(&key_path, b"short").expect("create incomplete key file");

        let key = load_or_create_build_cache_key(&data_dir).expect("repair stale cache key");
        let repaired = std::fs::read(&key_path).expect("read repaired key");

        assert_eq!(repaired, key);
        assert_eq!(repaired.len(), 32);
    }

    #[test]
    fn railpack_secret_cache_key_repairs_oversized_key() {
        let data_dir = temp_dir("ployz-build-cache-key-oversized-repair");
        let key_dir = data_dir.join("build-work");
        create_private_dir(&key_dir).expect("create key dir");
        let key_path = key_dir.join("cache-token-key");
        write_private_file(&key_path, &[7_u8; 33]).expect("create oversized key file");

        let key = load_or_create_build_cache_key(&data_dir).expect("repair oversized cache key");
        let repaired = std::fs::read(&key_path).expect("read repaired key");

        assert_eq!(repaired, key);
        assert_eq!(repaired.len(), 32);
    }

    #[test]
    fn railpack_secret_cache_key_repair_is_serialized() {
        let data_dir = temp_dir("ployz-build-cache-key-concurrent-repair");
        let key_dir = data_dir.join("build-work");
        create_private_dir(&key_dir).expect("create key dir");
        let key_path = key_dir.join("cache-token-key");
        write_private_file(&key_path, b"short").expect("create incomplete key file");
        let workers = 16;
        let barrier = Arc::new(Barrier::new(workers));
        let handles = (0..workers)
            .map(|_| {
                let data_dir = data_dir.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    load_or_create_build_cache_key(&data_dir)
                })
            })
            .collect::<Vec<_>>();

        let mut keys = Vec::new();
        for handle in handles {
            keys.push(
                handle
                    .join()
                    .expect("worker should not panic")
                    .expect("repair should succeed"),
            );
        }
        let final_key = std::fs::read(&key_path).expect("read repaired key");

        assert_eq!(final_key.len(), 32);
        assert!(keys.iter().all(|key| key.as_slice() == final_key));
    }

    #[test]
    fn repair_lock_guard_preserves_successor_lock() {
        let lock_dir = temp_dir("ployz-build-cache-key-repair-lock");
        std::fs::create_dir(&lock_dir).expect("create first lock");
        write_repair_lock_token(&lock_dir, "first").expect("write first token");
        let first = RepairLockGuard {
            path: lock_dir.clone(),
            token: "first".into(),
        };
        std::fs::remove_dir_all(&lock_dir).expect("remove stale lock");
        std::fs::create_dir(&lock_dir).expect("create successor lock");
        write_repair_lock_token(&lock_dir, "second").expect("write second token");
        let second = RepairLockGuard {
            path: lock_dir.clone(),
            token: "second".into(),
        };

        drop(first);
        assert!(repair_lock_token_matches(&lock_dir, "second").expect("read second token"));

        drop(second);
        assert!(!lock_dir.exists());
    }

    #[test]
    fn build_command_path_preparation_cleans_secret_files_on_error() {
        let data_dir = temp_dir("ployz-build-command-path-cleanup");
        let operation_id = "build-op";
        let metadata_dir = railpack_metadata_dir(&data_dir, operation_id);
        let secret_path = metadata_dir.join("secrets").join("API_TOKEN");
        std::fs::create_dir_all(&secret_path).expect("create conflicting secret path");
        let inputs = BuildInputs {
            env: BTreeMap::from([(
                "API_TOKEN".into(),
                BuildEnvValue::Secret {
                    value: "secret".into(),
                    fingerprint: None,
                },
            )]),
            docker_build_args: BTreeMap::new(),
        };
        let invocation =
            plan_build_invocation(BuildMethod::Dockerfile, &inputs).expect("invocation plan");

        let result = prepare_build_command_paths(
            &data_dir,
            BuildMethod::Dockerfile,
            operation_id,
            &invocation,
        );

        assert!(result.is_err());
        assert!(!metadata_dir.exists());
    }

    #[test]
    fn build_artifact_uses_image_id_and_build_provenance() {
        let image_digest = digest('a');
        let request = BuildLocalRequest {
            method: BuildMethod::Dockerfile,
            context_dir: "/tmp/context".into(),
            image_name: "example/app:latest".into(),
            platform: None,
            push_target: None,
            distribute_targets: Vec::new(),
            inputs: BuildInputs::default(),
        };
        let image = RuntimeImage {
            reference: "example/app:latest".into(),
            id: Some(image_digest.as_str().into()),
            repo_digests: Vec::new(),
            platform: Some(ImagePlatform {
                os: "linux".into(),
                architecture: "amd64".into(),
                variant: None,
            }),
            size_bytes: None,
        };

        let artifact = build_image_artifact(&request, &image).expect("artifact");

        assert_eq!(artifact.image.repository(), Some("example/app"));
        assert_eq!(artifact.image.tag(), Some("latest"));
        assert_eq!(artifact.image.digest(), &image_digest);
        assert_eq!(
            artifact.provenance,
            ImageArtifactProvenance::Build {
                method: BuildMethod::Dockerfile,
                location: BuildLocation::Local,
                source_digest: None
            }
        );
        assert_eq!(artifact.platform.expect("platform").architecture, "amd64");
    }

    #[test]
    fn present_availability_points_at_build_operation() {
        let artifact = ImageArtifact {
            image: ImageRef::repository_digest("example/app", Some("latest".into()), digest('b')),
            platform: None,
            provenance: ImageArtifactProvenance::Build {
                method: BuildMethod::Railpack,
                location: BuildLocation::Local,
                source_digest: None,
            },
            created_at: 1,
        };

        let record = present_build_availability(&MachineId::new("founder"), artifact, "build-1");

        assert_eq!(record.machine_id.as_str(), "founder");
        let ImagePresence::Present {
            source_operation_id,
            ..
        } = record.presence
        else {
            panic!("expected present record");
        };
        assert_eq!(source_operation_id.as_deref(), Some("build-1"));
    }

    #[test]
    fn image_ref_parses_registry_port_without_confusing_tag() {
        let image = image_ref_from_tag("localhost:5000/example/app:latest", digest('c'));

        assert_eq!(image.repository(), Some("localhost:5000/example/app"));
        assert_eq!(image.tag(), Some("latest"));

        let untagged = image_ref_from_tag("localhost:5000/example/app", digest('d'));
        assert_eq!(untagged.repository(), Some("localhost:5000/example/app"));
        assert_eq!(untagged.tag(), None);
    }

    #[test]
    fn normalize_build_image_name_defaults_missing_tag_to_latest() {
        assert_eq!(
            normalize_build_image_name("example/app"),
            "example/app:latest"
        );
        assert_eq!(
            normalize_build_image_name("localhost:5000/example/app"),
            "localhost:5000/example/app:latest"
        );
        assert_eq!(
            normalize_build_image_name("localhost:5000/example/app:v1"),
            "localhost:5000/example/app:v1"
        );
    }

    #[tokio::test]
    async fn build_local_success_persists_operation_and_local_availability() {
        let state = active_state().await;
        let context_dir = temp_dir("ployz-build-local-context");
        std::fs::create_dir_all(&context_dir).expect("create context");
        let image_digest = digest('e');
        let response = state
            .handle_build_local_with_runner_and_backend(
                &build_request(&context_dir),
                &FakeRunner {
                    output: BuildCommandOutput {
                        status_success: true,
                        timed_out: false,
                        stdout: String::new(),
                        stderr: String::new(),
                    },
                },
                Some(Ok(Arc::new(FakeBackend {
                    image: Some(RuntimeImage {
                        reference: "example/app:latest".into(),
                        id: Some(image_digest.as_str().into()),
                        repo_digests: Vec::new(),
                        platform: None,
                        size_bytes: None,
                    }),
                }))),
            )
            .await;

        assert!(response.is_ok(), "{}", response.message());
        let Some(DaemonPayload::BuildResult(payload)) = response.payload() else {
            panic!("expected build result payload");
        };
        assert_eq!(payload.artifact.digest(), &image_digest);
        let operation = state
            .build_operation_store()
            .load(&payload.operation_id)
            .expect("load operation")
            .expect("operation exists");
        assert_eq!(operation.status(), OperationStatus::Succeeded);
        assert_eq!(
            operation.artifact().expect("artifact").digest(),
            &image_digest
        );
        let active = state.active.as_ref().expect("active daemon state");
        let record = active
            .mesh
            .store
            .get_image_availability(&MachineId::new("founder"), &image_digest)
            .await
            .expect("read availability")
            .expect("availability exists");
        assert!(matches!(record.presence, ImagePresence::Present { .. }));
    }

    #[tokio::test]
    async fn build_local_persists_redacted_input_summary_without_secret_values() {
        let state = active_state().await;
        let context_dir = temp_dir("ployz-build-local-context");
        std::fs::create_dir_all(&context_dir).expect("create context");
        let image_digest = digest('f');
        let mut request = build_request(&context_dir);
        request.inputs = BuildInputs {
            env: BTreeMap::from([
                (
                    "NODE_ENV".into(),
                    BuildEnvValue::Plain {
                        value: "production".into(),
                    },
                ),
                (
                    "SENTRY_AUTH_TOKEN".into(),
                    BuildEnvValue::Secret {
                        value: "super-secret-token".into(),
                        fingerprint: Some("sentry-v1".into()),
                    },
                ),
            ]),
            docker_build_args: BTreeMap::from([("PUBLIC_COMMIT".into(), "abc123".into())]),
        };

        let response = state
            .handle_build_local_with_runner_and_backend(
                &request,
                &FakeRunner {
                    output: BuildCommandOutput {
                        status_success: true,
                        timed_out: false,
                        stdout: String::new(),
                        stderr: String::new(),
                    },
                },
                Some(Ok(Arc::new(FakeBackend {
                    image: Some(RuntimeImage {
                        reference: "example/app:latest".into(),
                        id: Some(image_digest.as_str().into()),
                        repo_digests: Vec::new(),
                        platform: None,
                        size_bytes: None,
                    }),
                }))),
            )
            .await;

        assert!(response.is_ok(), "{}", response.message());
        let Some(DaemonPayload::BuildResult(payload)) = response.payload() else {
            panic!("expected build result payload");
        };
        let operation = state
            .build_operation_store()
            .load(&payload.operation_id)
            .expect("load operation");
        let Some(operation) = operation else {
            panic!("expected operation");
        };
        assert_eq!(operation.inputs.env, vec!["NODE_ENV"]);
        let [secret] = operation.inputs.secrets.as_slice() else {
            panic!("expected one secret summary");
        };
        assert_eq!(secret.name, "SENTRY_AUTH_TOKEN");
        assert_eq!(secret.fingerprint.as_deref(), Some("sentry-v1"));
        assert_eq!(operation.inputs.docker_build_args, vec!["PUBLIC_COMMIT"]);
        let operation_json = serde_json::to_string(&operation).expect("serialize operation");
        assert!(!operation_json.contains("super-secret-token"));
        assert!(!operation_json.contains("production"));
        assert!(!operation_json.contains("abc123"));
    }

    #[tokio::test]
    async fn build_local_rejects_invalid_inputs_before_operation_record() {
        let state = active_state().await;
        let context_dir = temp_dir("ployz-build-local-context");
        std::fs::create_dir_all(&context_dir).expect("create context");
        let mut request = build_request(&context_dir);
        request.inputs = BuildInputs {
            env: BTreeMap::from([(
                "BAD-NAME".into(),
                BuildEnvValue::Plain { value: "x".into() },
            )]),
            docker_build_args: BTreeMap::new(),
        };

        let response = state
            .handle_build_local_with_runner_and_backend(
                &request,
                &FakeRunner {
                    output: BuildCommandOutput {
                        status_success: true,
                        timed_out: false,
                        stdout: String::new(),
                        stderr: String::new(),
                    },
                },
                Some(Ok(Arc::new(FakeBackend { image: None }))),
            )
            .await;

        assert!(!response.is_ok());
        assert_eq!(response.code(), "BUILD_LOCAL_INPUT_INVALID");
        assert!(
            state
                .build_operation_store()
                .list()
                .expect("list operations")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn build_local_command_failure_marks_operation_failed_without_availability() {
        let state = active_state().await;
        let context_dir = temp_dir("ployz-build-local-context");
        std::fs::create_dir_all(&context_dir).expect("create context");
        let response = state
            .handle_build_local_with_runner_and_backend(
                &build_request(&context_dir),
                &FakeRunner {
                    output: BuildCommandOutput {
                        status_success: false,
                        timed_out: false,
                        stdout: String::new(),
                        stderr: "bad dockerfile".into(),
                    },
                },
                Some(Ok(Arc::new(FakeBackend { image: None }))),
            )
            .await;

        assert!(!response.is_ok());
        let Some(DaemonPayload::BuildOperation(payload)) = response.payload() else {
            panic!("expected build operation payload");
        };
        assert_eq!(payload.operation.status(), OperationStatus::Failed);
        let active = state.active.as_ref().expect("active daemon state");
        assert!(
            active
                .mesh
                .store
                .list_image_availability()
                .await
                .expect("list availability")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn build_local_command_failure_redacts_input_values_from_persisted_error() {
        let state = active_state().await;
        let context_dir = temp_dir("ployz-build-local-context");
        std::fs::create_dir_all(&context_dir).expect("create context");
        let mut request = build_request(&context_dir);
        request.inputs = BuildInputs {
            env: BTreeMap::from([
                (
                    "NODE_ENV".into(),
                    BuildEnvValue::Plain {
                        value: "production".into(),
                    },
                ),
                (
                    "SENTRY_AUTH_TOKEN".into(),
                    BuildEnvValue::Secret {
                        value: "super-secret-token".into(),
                        fingerprint: None,
                    },
                ),
            ]),
            docker_build_args: BTreeMap::from([("PUBLIC_COMMIT".into(), "abc123".into())]),
        };

        let response = state
            .handle_build_local_with_runner_and_backend(
                &request,
                &FakeRunner {
                    output: BuildCommandOutput {
                        status_success: false,
                        timed_out: false,
                        stdout: "env production commit abc123".into(),
                        stderr: "secret super-secret-token".into(),
                    },
                },
                Some(Ok(Arc::new(FakeBackend { image: None }))),
            )
            .await;

        assert!(!response.is_ok());
        assert!(!response.message().contains("production"));
        assert!(!response.message().contains("abc123"));
        assert!(!response.message().contains("super-secret-token"));
        let Some(DaemonPayload::BuildOperation(payload)) = response.payload() else {
            panic!("expected build operation payload");
        };
        assert!(
            !payload
                .operation
                .last_error()
                .expect("last error")
                .contains("super-secret-token")
        );
        let persisted = state
            .build_operation_store()
            .load(&payload.operation.id)
            .expect("load operation");
        let Some(persisted) = persisted else {
            panic!("expected operation");
        };
        let operation_json = serde_json::to_string(&persisted).expect("serialize operation");
        assert!(!operation_json.contains("production"));
        assert!(!operation_json.contains("abc123"));
        assert!(!operation_json.contains("super-secret-token"));
    }

    #[tokio::test]
    async fn build_local_runner_error_redacts_input_values_from_persisted_error() {
        let state = active_state().await;
        let context_dir = temp_dir("ployz-build-local-context");
        std::fs::create_dir_all(&context_dir).expect("create context");
        let mut request = build_request(&context_dir);
        request.inputs = BuildInputs {
            env: BTreeMap::from([(
                "SENTRY_AUTH_TOKEN".into(),
                BuildEnvValue::Secret {
                    value: "super-secret-token".into(),
                    fingerprint: None,
                },
            )]),
            docker_build_args: BTreeMap::from([("PUBLIC_COMMIT".into(), "abc123".into())]),
        };

        let response = state
            .handle_build_local_with_runner_and_backend(
                &request,
                &FailingRunner {
                    error: "spawn failed with super-secret-token and abc123".into(),
                },
                Some(Ok(Arc::new(FakeBackend { image: None }))),
            )
            .await;

        assert!(!response.is_ok());
        assert!(!response.message().contains("super-secret-token"));
        assert!(!response.message().contains("abc123"));
        let Some(DaemonPayload::BuildOperation(payload)) = response.payload() else {
            panic!("expected build operation payload");
        };
        let operation_json = serde_json::to_string(&payload.operation).expect("serialize");
        assert!(!operation_json.contains("super-secret-token"));
        assert!(!operation_json.contains("abc123"));
    }

    #[tokio::test]
    async fn build_local_success_redacts_input_values_from_response_stdout() {
        let state = active_state().await;
        let context_dir = temp_dir("ployz-build-local-context");
        std::fs::create_dir_all(&context_dir).expect("create context");
        let image_digest = digest('9');
        let mut request = build_request(&context_dir);
        request.inputs = BuildInputs {
            env: BTreeMap::from([
                (
                    "NODE_ENV".into(),
                    BuildEnvValue::Plain {
                        value: "production".into(),
                    },
                ),
                (
                    "SENTRY_AUTH_TOKEN".into(),
                    BuildEnvValue::Secret {
                        value: "super-secret-token".into(),
                        fingerprint: None,
                    },
                ),
            ]),
            docker_build_args: BTreeMap::from([("PUBLIC_COMMIT".into(), "abc123".into())]),
        };

        let response = state
            .handle_build_local_with_runner_and_backend(
                &request,
                &FakeRunner {
                    output: BuildCommandOutput {
                        status_success: true,
                        timed_out: false,
                        stdout: "built with production abc123 super-secret-token".into(),
                        stderr: String::new(),
                    },
                },
                Some(Ok(Arc::new(FakeBackend {
                    image: Some(RuntimeImage {
                        reference: "example/app:latest".into(),
                        id: Some(image_digest.as_str().into()),
                        repo_digests: Vec::new(),
                        platform: None,
                        size_bytes: None,
                    }),
                }))),
            )
            .await;

        assert!(response.is_ok(), "{}", response.message());
        assert!(!response.message().contains("production"));
        assert!(!response.message().contains("abc123"));
        assert!(!response.message().contains("super-secret-token"));
    }

    #[tokio::test]
    async fn build_local_railpack_secret_build_succeeds_without_persisting_secret_values() {
        let state = active_state().await;
        let context_dir = temp_dir("ployz-build-local-context");
        std::fs::create_dir_all(&context_dir).expect("create context");
        let image_digest = digest('8');
        let mut request = build_request(&context_dir);
        request.method = BuildMethod::Railpack;
        request.inputs = BuildInputs {
            env: BTreeMap::from([(
                "RAILS_MASTER_KEY".into(),
                BuildEnvValue::Secret {
                    value: "master-key".into(),
                    fingerprint: None,
                },
            )]),
            docker_build_args: BTreeMap::new(),
        };
        let runner = RecordingRunner::new(BuildCommandOutput {
            status_success: true,
            timed_out: false,
            stdout: "built without leaking master-key".into(),
            stderr: String::new(),
        });

        let response = state
            .handle_build_local_with_runner_and_backend(
                &request,
                &runner,
                Some(Ok(Arc::new(FakeBackend {
                    image: Some(RuntimeImage {
                        reference: "example/app:latest".into(),
                        id: Some(image_digest.as_str().into()),
                        repo_digests: Vec::new(),
                        platform: None,
                        size_bytes: None,
                    }),
                }))),
            )
            .await;

        assert!(response.is_ok(), "{}", response.message());
        assert!(!response.message().contains("master-key"));
        assert_eq!(runner.programs(), vec!["railpack", "docker"]);
        let Some(DaemonPayload::BuildResult(payload)) = response.payload() else {
            panic!("expected build result payload");
        };
        assert_eq!(payload.artifact.digest(), &image_digest);
        let operation = state
            .build_operation_store()
            .load(&payload.operation_id)
            .expect("load operation");
        let Some(operation) = operation else {
            panic!("expected operation");
        };
        let [secret] = operation.inputs.secrets.as_slice() else {
            panic!("expected one secret summary");
        };
        assert_eq!(secret.name, "RAILS_MASTER_KEY");
        let operation_json = serde_json::to_string(&operation).expect("serialize operation");
        assert!(!operation_json.contains("master-key"));
    }

    #[tokio::test]
    async fn build_local_railpack_prepare_failure_stops_before_image_build() {
        let state = active_state().await;
        let context_dir = temp_dir("ployz-build-local-context");
        std::fs::create_dir_all(&context_dir).expect("create context");
        let mut request = build_request(&context_dir);
        request.method = BuildMethod::Railpack;
        let runner = RecordingRunner::new(BuildCommandOutput {
            status_success: false,
            timed_out: false,
            stdout: String::new(),
            stderr: "prepare failed".into(),
        });

        let response = state
            .handle_build_local_with_runner_and_backend(
                &request,
                &runner,
                Some(Ok(Arc::new(FakeBackend {
                    image: Some(RuntimeImage {
                        reference: "example/app:latest".into(),
                        id: Some(digest('7').as_str().into()),
                        repo_digests: Vec::new(),
                        platform: None,
                        size_bytes: None,
                    }),
                }))),
            )
            .await;

        assert!(!response.is_ok());
        assert_eq!(response.code(), "BUILD_LOCAL_COMMAND_FAILED");
        assert_eq!(runner.programs(), vec!["railpack"]);
        assert!(
            {
                let Some(active) = state.active.as_ref() else {
                    panic!("expected active daemon state");
                };
                active
                    .mesh
                    .store
                    .list_image_availability()
                    .await
                    .expect("list availability")
            }
            .is_empty()
        );
    }

    #[tokio::test]
    async fn build_local_railpack_image_build_failure_redacts_inputs() {
        let state = active_state().await;
        let context_dir = temp_dir("ployz-build-local-context");
        std::fs::create_dir_all(&context_dir).expect("create context");
        let mut request = build_request(&context_dir);
        request.method = BuildMethod::Railpack;
        request.inputs = BuildInputs {
            env: BTreeMap::from([
                (
                    "NODE_ENV".into(),
                    BuildEnvValue::Plain {
                        value: "production".into(),
                    },
                ),
                (
                    "RAILS_MASTER_KEY".into(),
                    BuildEnvValue::Secret {
                        value: "master-key".into(),
                        fingerprint: None,
                    },
                ),
            ]),
            docker_build_args: BTreeMap::new(),
        };
        let runner = SequencedRunner::new(vec![
            Ok(BuildCommandOutput {
                status_success: true,
                timed_out: false,
                stdout: String::new(),
                stderr: String::new(),
            }),
            Ok(BuildCommandOutput {
                status_success: false,
                timed_out: false,
                stdout: "using production".into(),
                stderr: "failed with master-key".into(),
            }),
        ]);

        let response = state
            .handle_build_local_with_runner_and_backend(
                &request,
                &runner,
                Some(Ok(Arc::new(FakeBackend {
                    image: Some(RuntimeImage {
                        reference: "example/app:latest".into(),
                        id: Some(digest('6').as_str().into()),
                        repo_digests: Vec::new(),
                        platform: None,
                        size_bytes: None,
                    }),
                }))),
            )
            .await;

        assert!(!response.is_ok());
        assert_eq!(response.code(), "BUILD_LOCAL_COMMAND_FAILED");
        assert_eq!(runner.programs(), vec!["railpack", "docker"]);
        assert!(!response.message().contains("production"));
        assert!(!response.message().contains("master-key"));
        let Some(DaemonPayload::BuildOperation(payload)) = response.payload() else {
            panic!("expected build operation payload");
        };
        let operation_last_error = payload.operation.last_error();
        let Some(last_error) = operation_last_error.as_deref() else {
            panic!("expected persisted last error");
        };
        assert!(!last_error.contains("production"));
        assert!(!last_error.contains("master-key"));
        assert!(
            {
                let Some(active) = state.active.as_ref() else {
                    panic!("expected active daemon state");
                };
                active
                    .mesh
                    .store
                    .list_image_availability()
                    .await
                    .expect("list availability")
            }
            .is_empty()
        );
    }

    #[tokio::test]
    async fn build_local_rejects_overlapping_build_for_same_image() {
        let state = active_state().await;
        let context_dir = temp_dir("ployz-build-local-context");
        std::fs::create_dir_all(&context_dir).expect("create context");
        let mut request = build_request(&context_dir);
        request.image_name = "example/app".into();
        let build_lock = state.local_build_lock("example/app:latest").await;
        let _build_guard = build_lock.lock().await;

        let response = state
            .handle_build_local_with_runner_and_backend(
                &request,
                &FakeRunner {
                    output: BuildCommandOutput {
                        status_success: true,
                        timed_out: false,
                        stdout: String::new(),
                        stderr: String::new(),
                    },
                },
                Some(Ok(Arc::new(FakeBackend { image: None }))),
            )
            .await;

        assert!(!response.is_ok());
        assert_eq!(response.code(), "BUILD_LOCAL_IMAGE_BUSY");
        let Some(DaemonPayload::BuildOperation(payload)) = response.payload() else {
            panic!("expected build operation payload");
        };
        assert_eq!(payload.operation.status(), OperationStatus::Failed);
    }
}
