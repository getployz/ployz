use super::*;

#[derive(Clone)]
pub(super) enum BuildEffects {
    Docker(Box<DockerBuildEffects>),
    #[cfg(test)]
    Test(Arc<tests::TestBuildEffects>),
}

#[derive(Clone)]
pub(super) struct DockerBuildEffects {
    pub(super) executor: DockerBuildExecutor,
    pub(super) log_client: NatsClient,
    pub(super) image_state: Option<AvailableImageService>,
}

impl BuildEffects {
    pub(super) async fn prune_cache(
        &self,
        cancelled: watch::Receiver<bool>,
        progress: mpsc::UnboundedSender<BuildCachePruneRunningPhase>,
    ) -> Result<ployz_core::operation::BuildCachePruneEvidence, BuildExecutionError> {
        match self {
            Self::Docker(effects) => effects.executor.prune_cache(cancelled, progress).await,
            #[cfg(test)]
            Self::Test(effects) => effects.prune_cache(cancelled, progress).await,
        }
    }

    pub(super) async fn execute_and_ingest(
        &self,
        machine_id: MachineId,
        request: MachineBuildStartRpcRequest,
        acceptance: BuildExecutorAcceptance,
        cancel_rx: watch::Receiver<bool>,
        log_progress: BuildLogProgress,
    ) -> Result<MachineBuildOutput, BuildExecutionError> {
        match self {
            Self::Docker(effects) => {
                let operation_id = request.operation_id;
                let platform = request.platform;
                let route = machine_build_log_route(&machine_id, &operation_id);
                let log_destination = BuildLogDestination::new(
                    effects.log_client.clone(),
                    route.subject,
                    route.assignment,
                );
                let result: BuildExecutionResult = effects
                    .executor
                    .execute(
                        BuildExecutionRequest::new(
                            &operation_id,
                            &request.source,
                            &request.adapter,
                            &platform,
                            &log_destination,
                        ),
                        cancel_rx,
                        log_progress,
                        BuildExecutionSupervision::ProgressSupervised,
                    )
                    .await?;
                let log_summary = result.log_summary;
                let Some(images) = &effects.image_state else {
                    return Err(BuildExecutionError::Platform {
                        failure: BuildPlatformFailure::MachineUnavailable {
                            message: failure_message(
                                "machine image content service is unavailable",
                            ),
                        },
                        log_summary,
                    });
                };
                let lease_expires_at =
                    images
                        .ingest_build_layout(&result.layout)
                        .await
                        .map_err(|message| BuildExecutionError::Platform {
                            failure: BuildPlatformFailure::ImagePushFailed {
                                message: failure_message(message),
                            },
                            log_summary,
                        })?;
                let availability_expires_at =
                    ployz_core::deploy::ImageAvailabilityExpiresAt::from_content_lease_expiry(
                        lease_expires_at,
                    )
                    .map_err(|error| BuildExecutionError::Platform {
                        failure: BuildPlatformFailure::ImagePushFailed {
                            message: failure_message(error.to_string()),
                        },
                        log_summary,
                    })?;
                Ok(MachineBuildOutput {
                    acceptance,
                    image: PlatformImage {
                        seed: machine_id,
                        manifest_digest: result.layout.manifest_digest().clone(),
                        image_id: result.layout.image_id().clone(),
                        availability_expires_at,
                    },
                    verified_source: result.verified_source,
                    toolchain: result.toolchain,
                    log_summary,
                })
            }
            #[cfg(test)]
            Self::Test(effects) => {
                effects
                    .execute_and_ingest(&request, log_progress, cancel_rx)
                    .await
            }
        }
    }

    pub(super) async fn force_cleanup(
        &self,
        operation_id: &OperationId,
        platform: &OciPlatform,
    ) -> MachineBuildCleanupOutcome {
        let cleanup = async {
            match self {
                Self::Docker(effects) => {
                    effects.executor.force_cleanup(operation_id, platform).await
                }
                #[cfg(test)]
                Self::Test(effects) => effects.force_cleanup().await,
            }
        };
        match tokio::time::timeout(BUILD_FORCE_CLEANUP_TIMEOUT, cleanup).await {
            Ok(Ok(())) => MachineBuildCleanupOutcome::Confirmed,
            Ok(Err(_)) | Err(_) => MachineBuildCleanupOutcome::Unconfirmed,
        }
    }
}
