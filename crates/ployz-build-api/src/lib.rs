use std::collections::BTreeSet;
use std::path::PathBuf;

use async_trait::async_trait;
use ployz_model::{
    BuildInputSummary, BuildInputs, BuildMethod, ImageDigest, ImagePlatform, ImageRef,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, BuildBackendError>;

pub const DOCKER_BUILDKIT_ENV: &str = "DOCKER_BUILDKIT";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildBackendKind {
    Dockerfile,
    Railpack,
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildCapability {
    LocalContext,
    Dockerfile,
    RailpackPlan,
    BuildKitSecrets,
    DockerBuildArgs,
    CacheKey,
    MultiPlatform,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildCapabilities {
    supported: BTreeSet<BuildCapability>,
}

impl BuildCapabilities {
    #[must_use]
    pub fn new(supported: impl IntoIterator<Item = BuildCapability>) -> Self {
        Self {
            supported: supported.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn dockerfile() -> Self {
        Self::new([
            BuildCapability::LocalContext,
            BuildCapability::Dockerfile,
            BuildCapability::BuildKitSecrets,
            BuildCapability::DockerBuildArgs,
            BuildCapability::CacheKey,
            BuildCapability::MultiPlatform,
        ])
    }

    #[must_use]
    pub fn railpack() -> Self {
        Self::new([
            BuildCapability::LocalContext,
            BuildCapability::RailpackPlan,
            BuildCapability::BuildKitSecrets,
            BuildCapability::DockerBuildArgs,
            BuildCapability::CacheKey,
            BuildCapability::MultiPlatform,
        ])
    }

    #[must_use]
    pub fn contains(&self, capability: BuildCapability) -> bool {
        self.supported.contains(&capability)
    }

    pub fn require(&self, backend: &str, capability: BuildCapability) -> Result<()> {
        if self.contains(capability) {
            Ok(())
        } else {
            Err(BuildBackendError::UnsupportedCapability {
                backend: backend.to_string(),
                capability,
            })
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildBackendInfo {
    pub id: String,
    pub kind: BuildBackendKind,
    pub capabilities: BuildCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildRequest {
    pub operation_id: String,
    pub method: BuildMethod,
    pub context_dir: PathBuf,
    pub image_name: String,
    pub platform: Option<ImagePlatform>,
    pub inputs: BuildInputs,
    pub cache_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildOutput {
    pub image: ImageRef,
    pub digest: ImageDigest,
    pub metadata: BuildOutputMetadata,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildOutputMetadata {
    pub source_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BuildBackendError {
    #[error("build backend '{backend}' does not support method {method:?}")]
    UnsupportedMethod {
        backend: String,
        method: BuildMethod,
    },
    #[error("build backend '{backend}' does not support {capability:?}")]
    UnsupportedCapability {
        backend: String,
        capability: BuildCapability,
    },
    #[error("invalid build request field '{field}': {message}")]
    InvalidRequest { field: String, message: String },
    #[error("build backend '{backend}' failed during {stage}: {message}")]
    Failed {
        backend: String,
        stage: String,
        message: String,
    },
}

#[async_trait]
pub trait BuildBackend: Send + Sync {
    fn info(&self) -> BuildBackendInfo;

    async fn build(&self, request: BuildRequest) -> Result<BuildOutput>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildCommandStepKind {
    RailpackPrepare,
    ImageBuild,
}

impl BuildCommandStepKind {
    #[must_use]
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
    #[must_use]
    pub fn new(
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

    #[must_use]
    pub fn image_build_command(&self) -> &BuildCommand {
        &self.image_build.command
    }

    #[must_use]
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
    pub program: &'static str,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub redaction_values: Vec<String>,
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
    #[must_use]
    pub fn redacted_args(&self) -> Vec<String> {
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

    #[must_use]
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

    #[must_use]
    pub fn redact_captured_output(&self, text: &str) -> String {
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

pub struct BuildInvocationPlan {
    pub summary: BuildInputSummary,
    pub env: Vec<(String, String)>,
    pub plain_env: Vec<(String, String)>,
    pub secret_env: Vec<(String, String)>,
    pub docker_build_args: Vec<(String, String)>,
    pub buildkit_secret_env: Vec<String>,
    pub railpack_secret_cache_required: bool,
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

pub struct BuildCommandPaths {
    pub cleanup_dirs: Vec<PathBuf>,
    pub railpack_plan_path: Option<PathBuf>,
    pub railpack_info_path: Option<PathBuf>,
    pub buildkit_secret_files: Vec<(String, PathBuf)>,
    pub railpack_secret_cache_token: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dockerfile_and_railpack_capabilities_are_distinct() {
        let dockerfile = BuildCapabilities::dockerfile();
        let railpack = BuildCapabilities::railpack();

        assert!(dockerfile.contains(BuildCapability::Dockerfile));
        assert!(!dockerfile.contains(BuildCapability::RailpackPlan));
        assert!(railpack.contains(BuildCapability::RailpackPlan));
        assert!(!railpack.contains(BuildCapability::Dockerfile));
    }

    #[test]
    fn unsupported_build_capability_is_structured() {
        let capabilities = BuildCapabilities::dockerfile();

        let error = capabilities
            .require("dockerfile", BuildCapability::RailpackPlan)
            .expect_err("dockerfile backend should not advertise railpack plan generation");

        assert_eq!(
            error,
            BuildBackendError::UnsupportedCapability {
                backend: "dockerfile".to_string(),
                capability: BuildCapability::RailpackPlan,
            }
        );
    }
}
