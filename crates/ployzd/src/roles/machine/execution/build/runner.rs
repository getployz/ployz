use super::ValidatedOciLayout;
use super::lifecycle::{
    BUILDER_LABEL, CACHE_VOLUME, DOCKER_COMMAND_TIMEOUT, PinnedImageKind, await_buildkit,
    builder_name, create_builder, prune_buildkit_cache, pull_exact_image, remove_builder,
    require_success, run_bounded, run_buildctl, run_prepare,
};
use super::logs::{BuildLogProgress, PublishedLogs};
use super::oci::{OciLayoutError, OciValidationControl, validate_oci_layout};
use super::plan::{BuildAdapterToolchain, lower_build_adapter, toolchain_for_platform};
use super::source::checkout_git_source;
use super::workspace::{
    clean_failed_workspace, prepare_private_directory, prepare_workspace, remove_workspace_tree,
    verify_helper,
};
use crate::roles::machine::facts::read_disk_space;
use crate::roles::machine::protocol::BuildLogSummary;
use ployz_core::build::{BuildAdapter, GitSource};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::image::OciPlatform;
use ployz_core::operation::{
    BuildCachePruneEvidence, BuildPlatformFailure, BuildToolchainEvidence, FailureMessage,
};
use ployz_nats::service_runtime::NatsClient;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::time::Instant;

const GIB: u64 = 1024 * 1024 * 1024;
const CACHE_RESERVED_BYTES: u64 = 512 * 1024 * 1024;
const CACHE_MAX_BYTES: u64 = 10 * GIB;
const REQUIRED_FREE_BYTES: u64 = 5 * GIB;
const DISK_PERCENT: u64 = 20;
const BUILDKIT_CONFIG: &str = "buildkitd.toml";

#[derive(Clone)]
pub struct DockerBuildExecutor {
    machine_id: MachineId,
    client: NatsClient,
    workspace_root: PathBuf,
    effect_guard: BuildEffectGuard,
}

impl DockerBuildExecutor {
    #[must_use]
    pub fn new(machine_id: MachineId, client: NatsClient, workspace_root: PathBuf) -> Self {
        Self {
            machine_id,
            client,
            workspace_root,
            effect_guard: BuildEffectGuard::default(),
        }
    }

    pub async fn recover_orphans(&self) -> Result<(), BuildExecutionError> {
        let filter = format!("label={BUILDER_LABEL}");
        let output = run_bounded(
            "docker",
            ["ps", "-aq", "--filter", filter.as_str()],
            DOCKER_COMMAND_TIMEOUT,
        )
        .await?;
        require_success("list orphaned builders", &output)?;
        for id in String::from_utf8_lossy(&output.stdout).lines() {
            remove_builder(id).await?;
        }
        remove_workspace_tree(&self.workspace_root).await?;
        Ok(())
    }

    pub async fn force_cleanup(
        &self,
        operation_id: &OperationId,
        platform: &OciPlatform,
    ) -> Result<(), BuildExecutionError> {
        let _effect_guard = self.effect_guard.acquire().await?;
        let builder = builder_name(operation_id, platform);
        let builder_result = remove_builder(&builder).await;
        let workspace_result = remove_workspace_tree(&self.workspace(operation_id, platform)).await;
        builder_result.and(workspace_result)
    }

    pub(crate) async fn prune_cache(&self) -> Result<BuildCachePruneEvidence, BuildExecutionError> {
        let _effect_guard = self.effect_guard.acquire().await?;
        prepare_private_directory(&self.workspace_root).await?;
        let before = build_disk_space(&self.workspace_root)?;
        prune_buildkit_cache().await?;
        let after = build_disk_space(&self.workspace_root)?;
        Ok(BuildCachePruneEvidence {
            before_available_bytes: before.available_bytes,
            reclaimed_bytes: after.available_bytes.saturating_sub(before.available_bytes),
            after_available_bytes: after.available_bytes,
        })
    }

    pub async fn execute(
        &self,
        request: BuildExecutionRequest<'_>,
        mut cancelled: watch::Receiver<bool>,
        log_progress: BuildLogProgress,
        deadline: Instant,
    ) -> Result<BuildExecutionResult, BuildExecutionError> {
        let effect_guard = self.effect_guard.acquire().await?;
        let workspace = self.workspace(request.operation_id, request.platform);
        prepare_private_directory(&self.workspace_root).await?;
        let Some(operation_workspace) = workspace.parent() else {
            return Err(infrastructure(
                "prepare build workspace",
                "operation workspace has no parent",
            ));
        };
        prepare_private_directory(operation_workspace).await?;
        prepare_workspace(&workspace).await?;
        let result = self
            .execute_in_workspace(
                request,
                &mut cancelled,
                log_progress,
                deadline,
                effect_guard,
            )
            .await;
        clean_failed_workspace(&workspace, result).await
    }

    async fn execute_in_workspace(
        &self,
        request: BuildExecutionRequest<'_>,
        cancelled: &mut watch::Receiver<bool>,
        log_progress: BuildLogProgress,
        deadline: Instant,
        effect_guard: OwnedSemaphorePermit,
    ) -> Result<BuildExecutionResult, BuildExecutionError> {
        let BuildExecutionRequest {
            operation_id,
            source,
            adapter,
            platform,
        } = request;
        let workspace = self.workspace(operation_id, platform);
        check_cancelled(cancelled)?;
        let toolchain = toolchain_for_platform(platform, adapter).map_err(|error| {
            platform_failure(BuildPlatformFailure::AdapterFailed {
                message: failure(error.to_string()),
            })
        })?;
        let buildkit_config = self.ensure_host_disk_capacity(&workspace).await?;
        let checkout = checkout_git_source(source, &workspace)
            .await
            .map_err(|error| {
                platform_failure(BuildPlatformFailure::SourceFetchFailed {
                    message: failure(error.to_string()),
                })
            })?;
        verify_helper(&toolchain.adapter).await?;
        let plan = lower_build_adapter(&checkout, adapter, platform, &workspace, &toolchain)
            .map_err(|error| {
                platform_failure(BuildPlatformFailure::AdapterFailed {
                    message: failure(error.to_string()),
                })
            })?;
        if let Some(prepare) = &plan.prepare {
            run_prepare(prepare, source, cancelled).await?;
        }
        tokio::fs::create_dir_all(&plan.oci_layout)
            .await
            .map_err(|error| infrastructure("create OCI output", error.to_string()))?;

        pull_exact_image(
            &toolchain.buildkit_reference,
            &toolchain.buildkit_manifest_digest,
            platform,
            PinnedImageKind::Buildkit,
        )
        .await?;
        if let BuildAdapterToolchain::Railpack {
            frontend_reference,
            frontend_manifest_digest,
            ..
        } = &toolchain.adapter
        {
            pull_exact_image(
                frontend_reference,
                frontend_manifest_digest,
                platform,
                PinnedImageKind::Frontend,
            )
            .await?;
        }
        let volume = run_bounded(
            "docker",
            ["volume", "create", CACHE_VOLUME],
            DOCKER_COMMAND_TIMEOUT,
        )
        .await?;
        require_success("ensure BuildKit cache volume", &volume)?;

        let builder = builder_name(operation_id, platform);
        remove_builder(&builder).await?;
        create_builder(
            &builder,
            operation_id,
            platform,
            &workspace,
            &buildkit_config,
            &toolchain.buildkit_reference,
        )
        .await?;
        let build = async {
            let started = run_bounded(
                "docker",
                ["start", builder.as_str()],
                DOCKER_COMMAND_TIMEOUT,
            )
            .await?;
            require_success("start BuildKit", &started)?;
            await_buildkit(&builder, cancelled).await?;
            run_buildctl(
                &builder,
                &plan,
                source,
                cancelled,
                &self.client,
                &self.machine_id,
                operation_id,
                platform,
                log_progress,
            )
            .await
        }
        .await;
        let cleanup = remove_builder(&builder).await;
        let logs = match (build, cleanup) {
            (Ok(logs), Ok(())) => logs,
            (Err(error), _) | (Ok(_), Err(error)) => return Err(error),
        };
        let layout_path = plan.oci_layout;
        let platform_for_validation = platform.clone();
        let validation_control = OciValidationControl::new(deadline, cancelled.clone());
        let log_summary = BuildLogSummary::new(logs.final_sequence, logs.omitted_bytes);
        let layout = tokio::task::spawn_blocking(move || {
            let _effect_guard = effect_guard;
            validate_oci_layout(&layout_path, &platform_for_validation, &validation_control)
        })
        .await
        .map_err(|error| infrastructure("join OCI validation", error.to_string()))?
        .map_err(|error| validation_failure(error, log_summary))?;
        Ok(BuildExecutionResult {
            layout,
            verified_commit: checkout.commit().clone(),
            toolchain: toolchain.evidence(),
            log_summary,
        })
    }

    fn workspace(&self, operation_id: &OperationId, platform: &OciPlatform) -> PathBuf {
        self.workspace_root
            .join(operation_id.as_str())
            .join(format!("{}-{}", platform.os(), platform.architecture()))
    }

    async fn ensure_host_disk_capacity(
        &self,
        workspace: &Path,
    ) -> Result<PathBuf, BuildExecutionError> {
        let mut capacity = build_disk_space(workspace)?;
        let policy = BuildDiskPolicy::for_capacity(capacity);
        let config = workspace.join(BUILDKIT_CONFIG);
        write_buildkit_config(&config, policy).await?;
        if capacity.available_bytes < policy.required_free_bytes {
            prune_buildkit_cache().await?;
            capacity = build_disk_space(workspace)?;
        }
        if capacity.available_bytes < policy.required_free_bytes {
            return Err(platform_failure(
                BuildPlatformFailure::InsufficientHostDisk {
                    available_bytes: capacity.available_bytes,
                    required_free_bytes: policy.required_free_bytes,
                },
            ));
        }
        Ok(config)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BuildDiskPolicy {
    cache_reserved_bytes: u64,
    cache_max_bytes: u64,
    cache_min_free_bytes: u64,
    required_free_bytes: u64,
}

impl BuildDiskPolicy {
    fn for_capacity(capacity: ployz_core::machine::runtime::MachineDiskSpace) -> Self {
        let disk_share = capacity.total_bytes.saturating_mul(DISK_PERCENT) / 100;
        let cache_max_bytes = disk_share.min(CACHE_MAX_BYTES);
        Self {
            cache_reserved_bytes: CACHE_RESERVED_BYTES.min(cache_max_bytes),
            cache_max_bytes,
            cache_min_free_bytes: REQUIRED_FREE_BYTES.max(disk_share),
            required_free_bytes: REQUIRED_FREE_BYTES,
        }
    }
}

fn build_disk_space(
    path: &Path,
) -> Result<ployz_core::machine::runtime::MachineDiskSpace, BuildExecutionError> {
    read_disk_space(path)
        .map_err(|error| infrastructure("read build host disk capacity", error.to_string()))
}

async fn write_buildkit_config(
    path: &Path,
    policy: BuildDiskPolicy,
) -> Result<(), BuildExecutionError> {
    tokio::fs::write(path, render_buildkit_config(policy))
        .await
        .map_err(|error| infrastructure("write BuildKit configuration", error.to_string()))
}

fn render_buildkit_config(policy: BuildDiskPolicy) -> String {
    format!(
        "[worker.oci]\ngc = true\nreservedSpace = {}\nmaxUsedSpace = {}\nminFreeSpace = {}\n",
        policy.cache_reserved_bytes, policy.cache_max_bytes, policy.cache_min_free_bytes
    )
}

#[derive(Clone, Copy)]
pub(crate) struct BuildExecutionRequest<'a> {
    operation_id: &'a OperationId,
    source: &'a GitSource,
    adapter: &'a BuildAdapter,
    platform: &'a OciPlatform,
}

impl<'a> BuildExecutionRequest<'a> {
    #[must_use]
    pub(crate) const fn new(
        operation_id: &'a OperationId,
        source: &'a GitSource,
        adapter: &'a BuildAdapter,
        platform: &'a OciPlatform,
    ) -> Self {
        Self {
            operation_id,
            source,
            adapter,
            platform,
        }
    }
}

#[derive(Clone)]
struct BuildEffectGuard {
    semaphore: Arc<Semaphore>,
}

impl Default for BuildEffectGuard {
    fn default() -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(1)),
        }
    }
}

impl BuildEffectGuard {
    async fn acquire(&self) -> Result<OwnedSemaphorePermit, BuildExecutionError> {
        self.semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| infrastructure("acquire build effect guard", error.to_string()))
    }
}

pub struct BuildExecutionResult {
    pub layout: ValidatedOciLayout,
    pub verified_commit: ployz_core::build::VerifiedGitCommit,
    pub toolchain: BuildToolchainEvidence,
    pub log_summary: BuildLogSummary,
}

pub(super) fn check_cancelled(
    cancelled: &watch::Receiver<bool>,
) -> Result<(), BuildExecutionError> {
    if *cancelled.borrow() {
        Err(BuildExecutionError::cancelled())
    } else {
        Ok(())
    }
}

pub(super) fn adapter_failure(message: impl Into<String>) -> BuildExecutionError {
    platform_failure(BuildPlatformFailure::AdapterFailed {
        message: failure(message),
    })
}

pub(super) fn platform_failure(failure: BuildPlatformFailure) -> BuildExecutionError {
    BuildExecutionError::Platform {
        failure,
        log_summary: BuildLogSummary::none(),
    }
}

pub(super) fn failure(message: impl Into<String>) -> FailureMessage {
    let message = message.into();
    FailureMessage::try_new(if message.is_empty() {
        "build execution failed".to_owned()
    } else {
        message
    })
    .expect("build execution failures are non-empty")
}

pub(super) fn infrastructure(
    action: &'static str,
    message: impl Into<String>,
) -> BuildExecutionError {
    BuildExecutionError::Infrastructure {
        action,
        message: message.into(),
        log_summary: BuildLogSummary::none(),
    }
}

fn validation_failure(error: OciLayoutError, log_summary: BuildLogSummary) -> BuildExecutionError {
    match error {
        OciLayoutError::Cancelled => BuildExecutionError::Cancelled { log_summary },
        OciLayoutError::TimedOut => BuildExecutionError::TimedOut { log_summary },
        error @ (OciLayoutError::UnsupportedLayoutVersion { .. }
        | OciLayoutError::InvalidSchemaVersion { .. }
        | OciLayoutError::PlatformSelection { .. }
        | OciLayoutError::UnexpectedMediaType { .. }
        | OciLayoutError::UnexpectedLayerMediaType { .. }
        | OciLayoutError::ConfigPlatformMismatch { .. }
        | OciLayoutError::InvalidDigestPath { .. }
        | OciLayoutError::InvalidComputedDigest { .. }
        | OciLayoutError::PathEscapesLayout { .. }
        | OciLayoutError::BlobNotFile { .. }
        | OciLayoutError::BlobSizeMismatch { .. }
        | OciLayoutError::BlobDigestMismatch { .. }
        | OciLayoutError::MetadataOutOfBounds { .. }
        | OciLayoutError::InvalidJson { .. }
        | OciLayoutError::Io { .. }) => BuildExecutionError::Platform {
            failure: BuildPlatformFailure::ImagePushFailed {
                message: failure(error.to_string()),
            },
            log_summary,
        },
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BuildExecutionError {
    #[error("build was cancelled")]
    Cancelled { log_summary: BuildLogSummary },
    #[error("build exceeded its deadline")]
    TimedOut { log_summary: BuildLogSummary },
    #[error("build platform failed: {failure:?}")]
    Platform {
        failure: BuildPlatformFailure,
        log_summary: BuildLogSummary,
    },
    #[error("failed to {action}: {message}")]
    Infrastructure {
        action: &'static str,
        message: String,
        log_summary: BuildLogSummary,
    },
}

impl BuildExecutionError {
    pub(super) fn cancelled() -> Self {
        Self::Cancelled {
            log_summary: BuildLogSummary::none(),
        }
    }

    pub(super) fn with_logs(self, logs: PublishedLogs) -> Self {
        let PublishedLogs {
            final_sequence,
            omitted_bytes,
        } = logs;
        match self {
            Self::Cancelled { .. } => Self::Cancelled {
                log_summary: BuildLogSummary::new(final_sequence, omitted_bytes),
            },
            Self::TimedOut { .. } => Self::TimedOut {
                log_summary: BuildLogSummary::new(final_sequence, omitted_bytes),
            },
            Self::Platform { failure, .. } => Self::Platform {
                failure,
                log_summary: BuildLogSummary::new(final_sequence, omitted_bytes),
            },
            Self::Infrastructure {
                action, message, ..
            } => Self::Infrastructure {
                action,
                message,
                log_summary: BuildLogSummary::new(final_sequence, omitted_bytes),
            },
        }
    }

    #[must_use]
    pub const fn log_summary(&self) -> BuildLogSummary {
        match self {
            Self::Cancelled { log_summary }
            | Self::TimedOut { log_summary }
            | Self::Platform { log_summary, .. }
            | Self::Infrastructure { log_summary, .. } => *log_summary,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cleanup_waits_until_blocking_validation_releases_effect_guard() {
        let guard = BuildEffectGuard::default();
        let validation_guard = guard.acquire().await.expect("validation guard");
        let cleanup_guard = guard.clone();
        let cleanup = tokio::spawn(async move {
            let _permit = cleanup_guard.acquire().await.expect("cleanup guard");
        });
        tokio::task::yield_now().await;
        assert!(!cleanup.is_finished());

        drop(validation_guard);
        cleanup.await.expect("cleanup waiter");
    }

    #[test]
    fn validation_interruptions_preserve_typed_outcome_and_log_summary() {
        let log_summary = BuildLogSummary::new(7, 11);
        assert!(matches!(
            validation_failure(OciLayoutError::Cancelled, log_summary),
            BuildExecutionError::Cancelled {
                log_summary: actual
            } if actual == log_summary
        ));
        assert!(matches!(
            validation_failure(OciLayoutError::TimedOut, log_summary),
            BuildExecutionError::TimedOut {
                log_summary: actual
            } if actual == log_summary
        ));
    }

    #[test]
    fn build_disk_policy_bounds_cache_and_protects_host_space() {
        let small = BuildDiskPolicy::for_capacity(ployz_core::machine::runtime::MachineDiskSpace {
            available_bytes: 0,
            total_bytes: GIB,
        });
        assert_eq!(small.cache_max_bytes, GIB / 5);
        assert_eq!(small.cache_reserved_bytes, GIB / 5);
        assert_eq!(small.cache_min_free_bytes, 5 * GIB);
        assert_eq!(small.required_free_bytes, 5 * GIB);

        let large = BuildDiskPolicy::for_capacity(ployz_core::machine::runtime::MachineDiskSpace {
            available_bytes: 0,
            total_bytes: 100 * GIB,
        });
        assert_eq!(large.cache_reserved_bytes, 512 * 1024 * 1024);
        assert_eq!(large.cache_max_bytes, 10 * GIB);
        assert_eq!(large.cache_min_free_bytes, 20 * GIB);
        assert_eq!(large.required_free_bytes, 5 * GIB);
    }

    #[test]
    fn buildkit_config_wires_native_gc_thresholds() {
        assert_eq!(
            render_buildkit_config(BuildDiskPolicy {
                cache_reserved_bytes: 1,
                cache_max_bytes: 2,
                cache_min_free_bytes: 3,
                required_free_bytes: 3,
            }),
            "[worker.oci]\ngc = true\nreservedSpace = 1\nmaxUsedSpace = 2\nminFreeSpace = 3\n"
        );
    }

    #[tokio::test]
    async fn prune_disk_evidence_can_start_with_an_absent_workspace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("builds");
        prepare_private_directory(&workspace)
            .await
            .expect("prepare workspace");
        assert!(build_disk_space(&workspace).is_ok());
    }
}
