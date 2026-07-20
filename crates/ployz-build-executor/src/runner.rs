use super::ValidatedOciLayout;
use super::lifecycle::{
    BUILDER_LABEL, BUILDER_OWNER_LABEL_KEY, BuildLogContext, CACHE_VOLUME, DOCKER_COMMAND_TIMEOUT,
    PinnedImageKind, await_buildkit, builder_name, create_builder, prune_buildkit_cache,
    pull_exact_image, remove_builder, require_success, restore_oci_layout_ownership, run_bounded,
    run_buildctl, run_prepare,
};
use super::logs::{BuildLogProgress, PublishedLogs};
use super::oci::{OciLayoutError, OciValidationControl, validate_oci_layout};
use super::plan::{BuildAdapterToolchain, lower_build_adapter, toolchain_for_platform};
use super::source::{LocalSnapshotError, prepare_build_source, stage_local_snapshot};
use super::workspace::{
    clean_failed_workspace, prepare_private_directory, prepare_workspace, remove_workspace_tree,
    verify_helper,
};
use ployz_core::build::BuildLogSummary;
use ployz_core::build::{BuildAdapter, BuildContextPath, BuildSource};
use ployz_core::ids::OperationId;
use ployz_core::image::OciPlatform;
use ployz_core::operation::{
    BuildCachePruneEvidence, BuildPlatformFailure, BuildToolchainEvidence, FailureMessage,
};
use rustix::fs::statvfs;
use sha2::{Digest, Sha256};
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
    workspace_root: PathBuf,
    effect_guard: BuildEffectGuard,
    docker_hub_registry_mirror: Option<DockerHubRegistryMirror>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DockerHubRegistryMirror(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Docker Hub registry mirror must be a lowercase DNS host with an optional port")]
pub struct DockerHubRegistryMirrorError;

impl DockerHubRegistryMirror {
    pub fn try_new(value: impl Into<String>) -> Result<Self, DockerHubRegistryMirrorError> {
        let value = value.into();
        let (host, port) = match value.split_once(':') {
            Some((host, port)) if !port.contains(':') => (host, Some(port)),
            Some(_) => return Err(DockerHubRegistryMirrorError),
            None => (value.as_str(), None),
        };
        if host.is_empty()
            || host.len() > 253
            || !host.split('.').all(valid_dns_label)
            || port.is_some_and(|port| {
                port.is_empty()
                    || port.starts_with('0')
                    || port.len() > 5
                    || !port.bytes().all(|byte| byte.is_ascii_digit())
                    || port.parse::<u16>().map_or(true, |port| port == 0)
            })
        {
            return Err(DockerHubRegistryMirrorError);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn valid_dns_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 63
        && label
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && label
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && label
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
}

impl DockerBuildExecutor {
    #[must_use]
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            effect_guard: BuildEffectGuard::default(),
            docker_hub_registry_mirror: None,
        }
    }

    pub async fn prepare_local_snapshot(
        &self,
        source_root: PathBuf,
        subdir: Option<BuildContextPath>,
    ) -> Result<BuildSource, LocalSnapshotError> {
        let _effect_guard =
            self.effect_guard
                .acquire()
                .await
                .map_err(|error| LocalSnapshotError::Io {
                    action: "acquire build effect guard",
                    message: error.to_string(),
                })?;
        stage_local_snapshot(source_root, self.workspace_root.join("prepared"), subdir).await
    }

    #[must_use]
    pub fn with_docker_hub_registry_mirror(mut self, mirror: DockerHubRegistryMirror) -> Self {
        self.docker_hub_registry_mirror = Some(mirror);
        self
    }

    pub async fn recover_orphans(&self) -> Result<(), BuildExecutionError> {
        let filters = orphan_builder_filters(&workspace_owner_digest(&self.workspace_root));
        let output = run_bounded(
            "docker",
            [
                "ps",
                "-aq",
                "--filter",
                filters[0].as_str(),
                "--filter",
                filters[1].as_str(),
            ],
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

    pub async fn prune_cache(&self) -> Result<BuildCachePruneEvidence, BuildExecutionError> {
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
            log_destination,
        } = request;
        let workspace = self.workspace(operation_id, platform);
        check_cancelled(cancelled)?;
        let toolchain = toolchain_for_platform(platform, adapter).map_err(|error| {
            platform_failure(BuildPlatformFailure::AdapterFailed {
                message: failure(error.to_string()),
            })
        })?;
        let buildkit_config = self.ensure_host_disk_capacity(&workspace).await?;
        let prepared = prepare_build_source(source, &self.workspace_root, &workspace)
            .await
            .map_err(|error| {
                platform_failure(BuildPlatformFailure::SourceFetchFailed {
                    message: failure(error.to_string()),
                })
            })?;
        verify_helper(&toolchain.adapter).await?;
        let plan = lower_build_adapter(&prepared, adapter, platform, &workspace, &toolchain)
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
            &workspace_owner_digest(&self.workspace_root),
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
                BuildLogContext {
                    destination: log_destination,
                    operation_id,
                    platform,
                    progress: log_progress,
                },
            )
            .await
        }
        .await;
        let ownership = restore_oci_layout_ownership(&builder, &workspace).await;
        let cleanup = remove_builder(&builder).await;
        let logs = finish_builder_run(build, ownership, cleanup)?;
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
            verified_source: prepared.verified().clone(),
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
        write_buildkit_config(&config, policy, self.docker_hub_registry_mirror.as_ref()).await?;
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

fn workspace_owner_digest(workspace_root: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"ployz.build_workspace_owner.v1");
    hasher.update(workspace_root.as_os_str().as_encoded_bytes());
    format!("{:x}", hasher.finalize())
}

fn orphan_builder_filters(owner: &str) -> [String; 2] {
    [
        format!("label={BUILDER_LABEL}"),
        format!("label={BUILDER_OWNER_LABEL_KEY}={owner}"),
    ]
}

fn finish_builder_run(
    build: Result<PublishedLogs, BuildExecutionError>,
    ownership: Result<(), BuildExecutionError>,
    cleanup: Result<(), BuildExecutionError>,
) -> Result<PublishedLogs, BuildExecutionError> {
    match build {
        Ok(logs) => ownership.and(cleanup).map(|()| logs),
        Err(error) => Err(error),
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
    let mut existing = path;
    while !existing.exists() {
        let Some(parent) = existing.parent() else {
            existing = Path::new("/");
            break;
        };
        existing = parent;
    }
    let stat = statvfs(existing)
        .map_err(|error| infrastructure("read build host disk capacity", error.to_string()))?;
    Ok(ployz_core::machine::runtime::MachineDiskSpace {
        available_bytes: bytes_from_blocks(stat.f_bavail, stat.f_frsize),
        total_bytes: bytes_from_blocks(stat.f_blocks, stat.f_frsize),
    })
}

fn bytes_from_blocks(blocks: u64, block_size: u64) -> u64 {
    u64::try_from(u128::from(blocks).saturating_mul(u128::from(block_size))).unwrap_or(u64::MAX)
}

async fn write_buildkit_config(
    path: &Path,
    policy: BuildDiskPolicy,
    docker_hub_registry_mirror: Option<&DockerHubRegistryMirror>,
) -> Result<(), BuildExecutionError> {
    tokio::fs::write(
        path,
        render_buildkit_config(policy, docker_hub_registry_mirror),
    )
    .await
    .map_err(|error| infrastructure("write BuildKit configuration", error.to_string()))
}

fn render_buildkit_config(
    policy: BuildDiskPolicy,
    docker_hub_registry_mirror: Option<&DockerHubRegistryMirror>,
) -> String {
    let mut config = format!(
        "[worker.oci]\ngc = true\nreservedSpace = {}\nmaxUsedSpace = {}\nminFreeSpace = {}\n",
        policy.cache_reserved_bytes, policy.cache_max_bytes, policy.cache_min_free_bytes
    );
    if let Some(mirror) = docker_hub_registry_mirror {
        config.push_str(&format!(
            "\n[registry.\"docker.io\"]\nmirrors = [\"{}\"]\n",
            mirror.as_str()
        ));
    }
    config
}

#[derive(Clone, Copy)]
pub struct BuildExecutionRequest<'a> {
    operation_id: &'a OperationId,
    source: &'a BuildSource,
    adapter: &'a BuildAdapter,
    platform: &'a OciPlatform,
    log_destination: &'a super::logs::BuildLogDestination,
}

impl<'a> BuildExecutionRequest<'a> {
    #[must_use]
    pub const fn new(
        operation_id: &'a OperationId,
        source: &'a BuildSource,
        adapter: &'a BuildAdapter,
        platform: &'a OciPlatform,
        log_destination: &'a super::logs::BuildLogDestination,
    ) -> Self {
        Self {
            operation_id,
            source,
            adapter,
            platform,
            log_destination,
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
    pub verified_source: ployz_core::build::VerifiedBuildSource,
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
    fn orphan_recovery_filters_are_workspace_owner_scoped() {
        let first = workspace_owner_digest(Path::new("/tmp/ployz-owner-a"));
        let second = workspace_owner_digest(Path::new("/tmp/ployz-owner-b"));
        let first_filters = orphan_builder_filters(&first);
        let second_filters = orphan_builder_filters(&second);

        assert_ne!(first, second);
        assert_eq!(first_filters[0], "label=com.getployz.build=true");
        assert_eq!(second_filters[0], "label=com.getployz.build=true");
        assert_eq!(
            first_filters[1],
            format!("label=com.getployz.build.owner={first}")
        );
        assert_eq!(
            second_filters[1],
            format!("label=com.getployz.build.owner={second}")
        );
        assert!(!first_filters.contains(&second_filters[1]));
        assert!(!second_filters.contains(&first_filters[1]));
    }

    #[test]
    fn builder_cleanup_preserves_build_then_ownership_then_removal_precedence() {
        fn error(action: &'static str) -> BuildExecutionError {
            infrastructure(action, action)
        }

        let Err(build_error) = finish_builder_run(
            Err(error("build")),
            Err(error("ownership")),
            Err(error("removal")),
        ) else {
            panic!("build failure remains primary");
        };
        assert!(matches!(
            build_error,
            BuildExecutionError::Infrastructure {
                action: "build",
                ..
            }
        ));

        let Err(ownership_error) = finish_builder_run(
            Ok(PublishedLogs {
                final_sequence: 1,
                omitted_bytes: 0,
            }),
            Err(error("ownership")),
            Err(error("removal")),
        ) else {
            panic!("ownership failure blocks success");
        };
        assert!(matches!(
            ownership_error,
            BuildExecutionError::Infrastructure {
                action: "ownership",
                ..
            }
        ));

        let Err(removal_error) = finish_builder_run(
            Ok(PublishedLogs {
                final_sequence: 1,
                omitted_bytes: 0,
            }),
            Ok(()),
            Err(error("removal")),
        ) else {
            panic!("builder removal remains mandatory");
        };
        assert!(matches!(
            removal_error,
            BuildExecutionError::Infrastructure {
                action: "removal",
                ..
            }
        ));
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
            render_buildkit_config(
                BuildDiskPolicy {
                    cache_reserved_bytes: 1,
                    cache_max_bytes: 2,
                    cache_min_free_bytes: 3,
                    required_free_bytes: 3,
                },
                None,
            ),
            "[worker.oci]\ngc = true\nreservedSpace = 1\nmaxUsedSpace = 2\nminFreeSpace = 3\n"
        );
    }

    #[test]
    fn docker_hub_registry_mirror_is_a_canonical_bare_authority() {
        for line in include_str!("../../../config/docker-hub-mirror-validation-vectors.txt").lines()
        {
            let (expected, value) = line
                .split_once(' ')
                .expect("validation vector has a result");
            let value = expanded_mirror_validation_vector(value);
            match expected {
                "valid" => assert!(
                    DockerHubRegistryMirror::try_new(&value).is_ok(),
                    "rejected valid mirror {value:?}"
                ),
                "invalid" => assert!(
                    DockerHubRegistryMirror::try_new(&value).is_err(),
                    "accepted invalid mirror {value:?}"
                ),
                other => panic!("unknown mirror-validation result {other:?}"),
            }
        }
        for invalid in [
            "",
            "user@mirror.gcr.io",
            "mirror.gcr.io/path",
            "mirror.gcr.io?query",
            "mirror.gcr.io\"]\n[worker.oci]",
        ] {
            assert!(
                DockerHubRegistryMirror::try_new(invalid).is_err(),
                "accepted invalid mirror {invalid:?}"
            );
        }
    }

    fn expanded_mirror_validation_vector(value: &str) -> String {
        match value {
            "<oversized-host>" => {
                let label = "a".repeat(63);
                format!("{label}.{label}.{label}.{label}")
            }
            "<oversized-port>" => "cache.example:999999999999999999999".to_owned(),
            value => value.to_owned(),
        }
    }

    #[test]
    fn buildkit_config_routes_docker_hub_through_the_validated_mirror() {
        let mirror = DockerHubRegistryMirror::try_new("mirror.gcr.io").expect("valid mirror");
        assert_eq!(
            render_buildkit_config(
                BuildDiskPolicy {
                    cache_reserved_bytes: 1,
                    cache_max_bytes: 2,
                    cache_min_free_bytes: 3,
                    required_free_bytes: 3,
                },
                Some(&mirror),
            ),
            "[worker.oci]\ngc = true\nreservedSpace = 1\nmaxUsedSpace = 2\nminFreeSpace = 3\n\n[registry.\"docker.io\"]\nmirrors = [\"mirror.gcr.io\"]\n"
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
