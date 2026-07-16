use super::ValidatedOciLayout;
use super::lifecycle::{
    BUILDER_LABEL, CACHE_VOLUME, DOCKER_COMMAND_TIMEOUT, PinnedImageKind, await_buildkit,
    builder_name, create_builder, pull_exact_image, remove_builder, require_success, run_bounded,
    run_buildctl, run_prepare,
};
use super::logs::{BuildLogProgress, PublishedLogs};
use super::oci::validate_oci_layout;
use super::plan::{BuildAdapterToolchain, lower_build_adapter, toolchain_for_platform};
use super::source::checkout_git_source;
use super::workspace::{
    clean_failed_workspace, prepare_private_directory, prepare_workspace, remove_workspace_tree,
    verify_helper,
};
use crate::roles::machine::protocol::BuildLogSummary;
use ployz_core::build::{BuildAdapter, GitSource};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::image::OciPlatform;
use ployz_core::operation::{BuildPlatformFailure, BuildToolchainEvidence, FailureMessage};
use ployz_nats::service_runtime::NatsClient;
use std::path::PathBuf;
use tokio::sync::watch;

#[derive(Clone)]
pub struct DockerBuildExecutor {
    machine_id: MachineId,
    client: NatsClient,
    workspace_root: PathBuf,
}

impl DockerBuildExecutor {
    #[must_use]
    pub fn new(machine_id: MachineId, client: NatsClient, workspace_root: PathBuf) -> Self {
        Self {
            machine_id,
            client,
            workspace_root,
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
        let builder = builder_name(operation_id, platform);
        let builder_result = remove_builder(&builder).await;
        let workspace_result = remove_workspace_tree(&self.workspace(operation_id, platform)).await;
        builder_result.and(workspace_result)
    }

    pub async fn execute(
        &self,
        operation_id: &OperationId,
        source: &GitSource,
        adapter: &BuildAdapter,
        platform: &OciPlatform,
        mut cancelled: watch::Receiver<bool>,
        log_progress: BuildLogProgress,
    ) -> Result<BuildExecutionResult, BuildExecutionError> {
        let workspace = self.workspace(operation_id, platform);
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
                operation_id,
                source,
                adapter,
                platform,
                &mut cancelled,
                log_progress,
            )
            .await;
        clean_failed_workspace(&workspace, result).await
    }

    async fn execute_in_workspace(
        &self,
        operation_id: &OperationId,
        source: &GitSource,
        adapter: &BuildAdapter,
        platform: &OciPlatform,
        cancelled: &mut watch::Receiver<bool>,
        log_progress: BuildLogProgress,
    ) -> Result<BuildExecutionResult, BuildExecutionError> {
        let workspace = self.workspace(operation_id, platform);
        check_cancelled(cancelled)?;
        let checkout = checkout_git_source(source, &workspace)
            .await
            .map_err(|error| {
                platform_failure(BuildPlatformFailure::SourceFetchFailed {
                    message: failure(error.to_string()),
                })
            })?;
        let toolchain = toolchain_for_platform(platform, adapter).map_err(|error| {
            platform_failure(BuildPlatformFailure::AdapterFailed {
                message: failure(error.to_string()),
            })
        })?;
        verify_helper(&toolchain).await?;
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
        let layout = tokio::task::spawn_blocking(move || {
            validate_oci_layout(&layout_path, &platform_for_validation)
        })
        .await
        .map_err(|error| infrastructure("join OCI validation", error.to_string()))?
        .map_err(|error| {
            platform_failure(BuildPlatformFailure::ImagePushFailed {
                message: failure(error.to_string()),
            })
        })?;
        Ok(BuildExecutionResult {
            layout,
            verified_commit: checkout.commit().clone(),
            toolchain: toolchain.evidence(),
            log_summary: BuildLogSummary::new(logs.final_sequence, logs.omitted_bytes),
        })
    }

    fn workspace(&self, operation_id: &OperationId, platform: &OciPlatform) -> PathBuf {
        self.workspace_root
            .join(operation_id.as_str())
            .join(format!("{}-{}", platform.os(), platform.architecture()))
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

#[derive(Debug, thiserror::Error)]
pub enum BuildExecutionError {
    #[error("build was cancelled")]
    Cancelled { log_summary: BuildLogSummary },
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
            | Self::Platform { log_summary, .. }
            | Self::Infrastructure { log_summary, .. } => *log_summary,
        }
    }
}
