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
        let command = match build_command(&request, &invocation) {
            Ok(command) => command,
            Err(error) => return self.err("BUILD_LOCAL_INPUT_UNSUPPORTED", error),
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
        if let Err(error) = operation_store.update_stage(&mut operation, "running build command") {
            return self.fail_build_local_operation(
                &operation_store,
                &mut operation,
                "BUILD_LOCAL_OPERATION_FAILED",
                error,
            );
        }
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
        let output = match runner.run(&command, &context_dir).await {
            Ok(output) if output.status_success => output,
            Ok(output) => {
                let message = build_command_failure_message(&command, &output);
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_COMMAND_FAILED",
                    message,
                );
            }
            Err(error) => {
                let error = command.redact_text(&error);
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

        let message = render_build_result(&operation.id, &record, output, &command);
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
    secret_env: Vec<(String, String)>,
    docker_build_args: Vec<(String, String)>,
    buildkit_secret_env: Vec<String>,
    railpack_prepare_env: Vec<(String, String)>,
    railpack_secret_cache_token: Option<String>,
}

impl std::fmt::Debug for BuildInvocationPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let env = self
            .env
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
            .field("secret_env", &secret_env)
            .field("docker_build_args", &docker_build_args)
            .field("buildkit_secret_env", &self.buildkit_secret_env)
            .field("railpack_prepare_env", &"<redacted>")
            .field(
                "railpack_secret_cache_token",
                &self
                    .railpack_secret_cache_token
                    .as_ref()
                    .map(|_value| "<redacted>"),
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
    let mut secret_env = Vec::new();
    let mut secret_material = Vec::new();
    let mut secret_names = Vec::new();

    for (key, value) in &inputs.env {
        validate_build_input_key(key)?;
        match value {
            BuildEnvValue::Plain { value } => {
                summary.env.push(key.clone());
                env.push((key.clone(), value.clone()));
            }
            BuildEnvValue::Secret { value, fingerprint } => {
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
    secret_env.sort_by(|left, right| left.0.cmp(&right.0));
    secret_names.sort();
    docker_build_args.sort_by(|left, right| left.0.cmp(&right.0));
    secret_material.sort_by(|left, right| left.0.cmp(&right.0));

    let railpack_prepare_env = env.clone();
    let railpack_secret_cache_token = match (method, secret_material.is_empty()) {
        (BuildMethod::Railpack, false) => Some(secret_cache_token(&secret_material)),
        (BuildMethod::Dockerfile | BuildMethod::Railpack, true)
        | (BuildMethod::Dockerfile, false) => None,
    };

    Ok(BuildInvocationPlan {
        summary,
        env,
        secret_env,
        docker_build_args,
        buildkit_secret_env: secret_names,
        railpack_prepare_env,
        railpack_secret_cache_token,
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

fn secret_cache_token(secret_material: &[(String, String)]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"ployz:railpack-secrets:v1\0");
    for (key, value) in secret_material {
        update_length_prefixed(&mut hasher, key.as_bytes());
        update_length_prefixed(&mut hasher, value.as_bytes());
    }
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

fn build_command(
    request: &BuildLocalRequest,
    invocation: &BuildInvocationPlan,
) -> Result<BuildCommand, String> {
    let (program, mut args) = match request.method {
        BuildMethod::Dockerfile => (
            "docker",
            vec!["build".into(), "-t".into(), request.image_name.clone()],
        ),
        BuildMethod::Railpack => (
            "railpack",
            vec!["build".into(), "--name".into(), request.image_name.clone()],
        ),
    };
    if let Some(platform) = &request.platform {
        args.push("--platform".into());
        args.push(format_platform(platform));
    }
    match request.method {
        BuildMethod::Dockerfile => {
            for (key, value) in &invocation.docker_build_args {
                args.push("--build-arg".into());
                args.push(format!("{key}={value}"));
            }
            for key in &invocation.buildkit_secret_env {
                args.push("--secret".into());
                args.push(format!("id={key},env={key}"));
            }
        }
        BuildMethod::Railpack => {
            if invocation.railpack_secret_cache_token.is_some() {
                return Err(
                    "railpack secret env requires the Railpack frontend executor; the current railpack CLI build path cannot pass secrets without exposing values in process arguments"
                        .into(),
                );
            }
            for (key, value) in &invocation.railpack_prepare_env {
                if invocation
                    .summary
                    .secrets
                    .iter()
                    .any(|secret| secret.name == *key)
                {
                    continue;
                }
                args.push("--env".into());
                args.push(format!("{key}={value}"));
            }
        }
    }
    args.push(".".into());
    Ok(BuildCommand {
        program,
        args,
        env: match request.method {
            BuildMethod::Dockerfile => invocation.secret_env.clone(),
            BuildMethod::Railpack => Vec::new(),
        },
        redaction_values: invocation
            .env
            .iter()
            .chain(invocation.docker_build_args.iter())
            .map(|(_key, value)| value.clone())
            .collect(),
    })
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
        digest: artifact.image.digest.clone(),
        presence: ImagePresence::Present {
            artifact,
            recorded_at: now,
            source_operation_id: Some(operation_id.into()),
        },
        updated_at: now,
    }
}

fn image_ref_from_tag(reference: &str, digest: ImageDigest) -> ImageRef {
    let (repository, tag) = if reference.starts_with("sha256:") {
        (None, None)
    } else {
        let last_slash = reference.rfind('/');
        match reference
            .rfind(':')
            .filter(|index| last_slash.is_none_or(|slash| *index > slash))
        {
            Some(index) if index + 1 < reference.len() => {
                let (repository, tag) = reference.split_at(index);
                (Some(repository.to_string()), Some(tag[1..].to_string()))
            }
            _ => (Some(reference.to_string()), None),
        }
    };
    ImageRef {
        repository,
        tag,
        digest,
    }
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
        let identity = Identity::generate(MachineId("founder".into()), [11; 32]);
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
                MachineId("founder".into()),
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
        let command = build_command(&request, &invocation).expect("command");

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
    fn railpack_command_uses_name_and_platform_variant() {
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
        let command = build_command(&request, &invocation).expect("command");

        assert_eq!(command.program, "railpack");
        assert_eq!(
            command.args,
            vec![
                "build",
                "--name",
                "example/app:latest",
                "--platform",
                "linux/arm64/v8",
                "."
            ]
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
        let command = build_command(&request, &invocation).expect("command");

        assert_eq!(
            command.args,
            vec![
                "build",
                "-t",
                "example/app:latest",
                "--build-arg",
                "PUBLIC_COMMIT=abc123",
                "--secret",
                "id=SENTRY_AUTH_TOKEN,env=SENTRY_AUTH_TOKEN",
                "."
            ]
        );
        assert_eq!(
            command.env,
            vec![("SENTRY_AUTH_TOKEN".into(), "super-secret-token".into())]
        );
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
    fn railpack_secret_inputs_are_planned_but_not_supported_by_local_cli_command() {
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
        let error = build_command(&request, &invocation)
            .expect_err("railpack CLI command cannot safely carry secrets");

        assert!(error.contains("Railpack frontend executor"));
        assert!(!error.contains("master-key"));
        assert!(invocation.railpack_secret_cache_token.is_some());
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
        let first = plan_build_invocation(BuildMethod::Railpack, &inputs("one"))
            .expect("first plan")
            .railpack_secret_cache_token
            .expect("first token");
        let second = plan_build_invocation(BuildMethod::Railpack, &inputs("two"))
            .expect("second plan")
            .railpack_secret_cache_token
            .expect("second token");

        assert_ne!(first, second);
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

        assert_eq!(artifact.image.repository.as_deref(), Some("example/app"));
        assert_eq!(artifact.image.tag.as_deref(), Some("latest"));
        assert_eq!(artifact.image.digest, image_digest);
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
            image: ImageRef {
                repository: Some("example/app".into()),
                tag: Some("latest".into()),
                digest: digest('b'),
            },
            platform: None,
            provenance: ImageArtifactProvenance::Build {
                method: BuildMethod::Railpack,
                location: BuildLocation::Local,
                source_digest: None,
            },
            created_at: 1,
        };

        let record = present_build_availability(&MachineId("founder".into()), artifact, "build-1");

        assert_eq!(record.machine_id.0, "founder");
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

        assert_eq!(
            image.repository.as_deref(),
            Some("localhost:5000/example/app")
        );
        assert_eq!(image.tag.as_deref(), Some("latest"));

        let untagged = image_ref_from_tag("localhost:5000/example/app", digest('d'));
        assert_eq!(
            untagged.repository.as_deref(),
            Some("localhost:5000/example/app")
        );
        assert_eq!(untagged.tag, None);
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

        assert!(response.ok, "{}", response.message);
        let Some(DaemonPayload::BuildResult(payload)) = response.payload else {
            panic!("expected build result payload");
        };
        assert_eq!(payload.artifact.digest(), &image_digest);
        let operation = state
            .build_operation_store()
            .load(&payload.operation_id)
            .expect("load operation")
            .expect("operation exists");
        assert_eq!(operation.status, OperationStatus::Succeeded);
        assert_eq!(
            operation.artifact.expect("artifact").digest(),
            &image_digest
        );
        let record = state
            .active
            .as_ref()
            .expect("active")
            .mesh
            .store
            .get_image_availability(&MachineId("founder".into()), &image_digest)
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

        assert!(response.ok, "{}", response.message);
        let Some(DaemonPayload::BuildResult(payload)) = response.payload else {
            panic!("expected build result payload");
        };
        let operation = state
            .build_operation_store()
            .load(&payload.operation_id)
            .expect("load operation")
            .expect("operation exists");
        assert_eq!(operation.inputs.env, vec!["NODE_ENV"]);
        assert_eq!(operation.inputs.secrets[0].name, "SENTRY_AUTH_TOKEN");
        assert_eq!(
            operation.inputs.secrets[0].fingerprint.as_deref(),
            Some("sentry-v1")
        );
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

        assert!(!response.ok);
        assert_eq!(response.code, "BUILD_LOCAL_INPUT_INVALID");
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

        assert!(!response.ok);
        let Some(DaemonPayload::BuildOperation(payload)) = response.payload else {
            panic!("expected build operation payload");
        };
        assert_eq!(payload.operation.status, OperationStatus::Failed);
        assert!(
            state
                .active
                .as_ref()
                .expect("active")
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

        assert!(!response.ok);
        assert!(!response.message.contains("production"));
        assert!(!response.message.contains("abc123"));
        assert!(!response.message.contains("super-secret-token"));
        let Some(DaemonPayload::BuildOperation(payload)) = response.payload else {
            panic!("expected build operation payload");
        };
        assert!(
            !payload
                .operation
                .last_error
                .as_deref()
                .expect("last error")
                .contains("super-secret-token")
        );
        let persisted = state
            .build_operation_store()
            .load(&payload.operation.id)
            .expect("load operation")
            .expect("operation exists");
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

        assert!(!response.ok);
        assert!(!response.message.contains("super-secret-token"));
        assert!(!response.message.contains("abc123"));
        let Some(DaemonPayload::BuildOperation(payload)) = response.payload else {
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

        assert!(response.ok, "{}", response.message);
        assert!(!response.message.contains("production"));
        assert!(!response.message.contains("abc123"));
        assert!(!response.message.contains("super-secret-token"));
    }

    #[tokio::test]
    async fn build_local_rejects_railpack_secrets_before_operation_record() {
        let state = active_state().await;
        let context_dir = temp_dir("ployz-build-local-context");
        std::fs::create_dir_all(&context_dir).expect("create context");
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

        assert!(!response.ok);
        assert_eq!(response.code, "BUILD_LOCAL_INPUT_UNSUPPORTED");
        assert!(
            state
                .build_operation_store()
                .list()
                .expect("list operations")
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

        assert!(!response.ok);
        assert_eq!(response.code, "BUILD_LOCAL_IMAGE_BUSY");
        let Some(DaemonPayload::BuildOperation(payload)) = response.payload else {
            panic!("expected build operation payload");
        };
        assert_eq!(payload.operation.status, OperationStatus::Failed);
    }
}
