use std::path::PathBuf;
use std::time::Duration;

use ployz_build_executor::DockerBuildExecutor;
use ployz_core::build::{BuildContextPath, BuildSource, LocalSnapshotDigest};
use ployz_core::image::OciPlatform;
use ployz_core::nats_config::MintedNatsUser;

use crate::build::command::{BuildExecutorAdmission, BuildExecutorCommand, BuildExecutorRunMode};
use crate::build::embedded_executor;
use crate::cloud_current_tree::{
    ApprovalState, CloudCurrentTreeClient, CloudDeploymentStatus, FrozenBuild, ObserveState,
    select_context,
};
use crate::deploy::command::CurrentTreeDeployCommand;
use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_error::PloyzctlExecutionError;
use crate::execution_support::PloyzctlExecutionOutput;

use super::{OBSERVE_TIMEOUT, current_tree_error, write_private};

const APPROVAL_TIMEOUT: Duration = Duration::from_secs(10 * 60);

struct ApprovedCloudInputs {
    command: CurrentTreeDeployCommand,
    runtime_config: PloyzctlRuntimeConfig,
    source_root: PathBuf,
    workspace_root: PathBuf,
    platform: OciPlatform,
    digest: LocalSnapshotDigest,
    subdir: Option<BuildContextPath>,
    client: CloudCurrentTreeClient,
    session_secret: String,
    poll_after_seconds: u64,
}

pub(super) async fn execute(
    command: CurrentTreeDeployCommand,
    config: &PloyzctlRuntimeConfig,
    cloud_url: &str,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let cwd = std::env::current_dir().map_err(current_tree_error)?;
    let workspace_root = cwd.join(".ployz").join("build-executor");
    let executor = DockerBuildExecutor::new(workspace_root.clone());
    let source = executor
        .prepare_local_snapshot(cwd.clone(), None)
        .await
        .map_err(current_tree_error)?;
    let BuildSource::LocalSnapshot { digest, subdir } = source else {
        unreachable!("local snapshot preparation returns local evidence");
    };
    let platform = ployz_build_executor::native_oci_platform().map_err(current_tree_error)?;
    let client = CloudCurrentTreeClient::new(cloud_url).map_err(current_tree_error)?;
    let begun = client.begin().await.map_err(current_tree_error)?;
    eprintln!(
        "Open {} and approve code {}",
        begun.browser_url, begun.user_code
    );
    let result = execute_approved(ApprovedCloudInputs {
        command,
        runtime_config: config.clone(),
        source_root: cwd,
        workspace_root,
        platform,
        digest,
        subdir,
        client: client.clone(),
        session_secret: begun.session_secret.clone(),
        poll_after_seconds: begun.poll_after_seconds,
    })
    .await;
    let cancel_result = client.cancel(&begun.session_secret).await;
    match (result, cancel_result) {
        (Ok(output), Ok(())) => Ok(output),
        (Err(error), Ok(())) => Err(error),
        (_, Err(error)) => Err(current_tree_error(error)),
    }
}

async fn execute_approved(
    inputs: ApprovedCloudInputs,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let ApprovedCloudInputs {
        command,
        runtime_config,
        source_root,
        workspace_root,
        platform,
        digest,
        subdir,
        client,
        session_secret,
        poll_after_seconds,
    } = inputs;
    await_approval(&client, &session_secret, poll_after_seconds).await?;
    let contexts = client
        .contexts(&session_secret)
        .await
        .map_err(current_tree_error)?;
    let selected = select_context(
        &contexts,
        command.organization.as_deref(),
        command.environment.as_deref(),
        command.service.as_deref(),
    )
    .map_err(current_tree_error)?
    .clone();
    client
        .select(&session_secret, &selected)
        .await
        .map_err(current_tree_error)?;
    let frozen = client
        .freeze(&session_secret, &digest, &platform)
        .await
        .map_err(current_tree_error)?;
    let minted = MintedNatsUser::generate().map_err(current_tree_error)?;

    // Activation is the admission boundary. No local executor or dispatch exists before it.
    let activated = client
        .activate(&session_secret, &frozen, &digest, &minted.public)
        .await
        .map_err(current_tree_error)?;
    if activated.platform != platform {
        return Err(current_tree_error(
            "Cloud activated a different executor platform",
        ));
    }

    DockerBuildExecutor::new(workspace_root.clone())
        .recover_orphans()
        .await
        .map_err(current_tree_error)?;
    restage_local_snapshot(
        &DockerBuildExecutor::new(workspace_root.clone()),
        source_root,
        &digest,
        subdir.as_ref(),
    )
    .await
    .map_err(current_tree_error)?;

    let material = tempfile::tempdir().map_err(current_tree_error)?;
    let ca_path = material.path().join("nats-ca.pem");
    let seed_path = material.path().join("executor.nk");
    write_private(&ca_path, activated.trusted_nats.ca_pem.as_str(), 0o600)?;
    write_private(&seed_path, minted.seed.secret(), 0o600)?;
    let executor_config = PloyzctlRuntimeConfig {
        nats_url: Some(activated.runtime_nats_url.as_str().to_owned()),
        nats_ca_file: Some(ca_path),
        nats_seed_file: Some(seed_path),
        nats_connect_timeout: runtime_config.nats_connect_timeout,
        ..PloyzctlRuntimeConfig::default()
    };
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let executor = crate::build::external_runtime::run_controlled(
        BuildExecutorCommand {
            pool_id: activated.pool_id,
            executor_id: activated.executor_id,
            workspace_root: Some(workspace_root),
            admission: controlled_admission(&frozen),
            mode: BuildExecutorRunMode::Once {
                wait_timeout: OBSERVE_TIMEOUT,
            },
        },
        executor_config,
        crate::build::external_runtime::WorkspaceStartup::Prepared,
        activated.expires_at,
        Some(ready_tx),
        Some(shutdown_rx),
    );
    let deployment_id = frozen.deployment_id.clone();
    let operation_client = client.clone();
    let operation_session = session_secret.clone();
    let operation_digest = digest.clone();
    let operation_public_key = minted.public.clone();
    embedded_executor::run_once(
        executor,
        ready_rx,
        shutdown_tx,
        move || async move {
            operation_client
                .dispatch(
                    &operation_session,
                    &frozen,
                    &operation_digest,
                    &operation_public_key,
                )
                .await
                .map_err(current_tree_error)?;
            await_terminal(&operation_client, &operation_session).await
        },
        tokio::signal::ctrl_c(),
    )
    .await?;
    Ok(PloyzctlExecutionOutput::stdout(format!(
        "built current working tree for {}/{}/{} and completed deployment {}\n",
        selected.organization.slug,
        selected.environment.namespace,
        selected.service.slug,
        deployment_id,
    )))
}

fn controlled_admission(frozen: &FrozenBuild) -> BuildExecutorAdmission {
    BuildExecutorAdmission::ExactOperation(frozen.operation_id.clone())
}

async fn restage_local_snapshot(
    executor: &DockerBuildExecutor,
    source_root: PathBuf,
    expected_digest: &LocalSnapshotDigest,
    expected_subdir: Option<&BuildContextPath>,
) -> Result<(), RestageLocalSnapshotError> {
    let source = executor
        .prepare_local_snapshot(source_root, expected_subdir.cloned())
        .await
        .map_err(RestageLocalSnapshotError::Capture)?;
    let BuildSource::LocalSnapshot {
        digest: actual_digest,
        subdir: actual_subdir,
    } = source
    else {
        return Err(RestageLocalSnapshotError::SourceShapeChanged);
    };
    if actual_digest != *expected_digest || actual_subdir.as_ref() != expected_subdir {
        return Err(RestageLocalSnapshotError::SourceMutated {
            expected_digest: expected_digest.clone(),
            actual_digest,
            expected_subdir: expected_subdir.cloned(),
            actual_subdir,
        });
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum RestageLocalSnapshotError {
    #[error("failed to recapture the approved current working tree: {0}")]
    Capture(ployz_build_executor::LocalSnapshotError),
    #[error("recaptured current working tree was not a local snapshot")]
    SourceShapeChanged,
    #[error(
        "current working tree changed after Cloud approval (expected {expected_digest} at {expected_subdir:?}, captured {actual_digest} at {actual_subdir:?})"
    )]
    SourceMutated {
        expected_digest: LocalSnapshotDigest,
        actual_digest: LocalSnapshotDigest,
        expected_subdir: Option<BuildContextPath>,
        actual_subdir: Option<BuildContextPath>,
    },
}

async fn await_approval(
    client: &CloudCurrentTreeClient,
    session_secret: &str,
    poll_after_seconds: u64,
) -> Result<(), PloyzctlExecutionError> {
    let started = tokio::time::Instant::now();
    let interval = Duration::from_secs(poll_after_seconds.clamp(1, 10));
    loop {
        let state = client
            .poll(session_secret)
            .await
            .map_err(current_tree_error)?;
        match state {
            ApprovalState::Approved | ApprovalState::ContextSelected => return Ok(()),
            ApprovalState::PendingApproval => {}
            ApprovalState::Expired | ApprovalState::Cancelled | ApprovalState::Terminal => {
                return Err(current_tree_error("Cloud approval session is terminal"));
            }
        }
        if started.elapsed() >= APPROVAL_TIMEOUT {
            return Err(current_tree_error("Cloud approval timed out"));
        }
        tokio::time::sleep(interval).await;
    }
}

async fn await_terminal(
    client: &CloudCurrentTreeClient,
    session_secret: &str,
) -> Result<(), PloyzctlExecutionError> {
    let started = tokio::time::Instant::now();
    loop {
        let state = client
            .observe(session_secret)
            .await
            .map_err(current_tree_error)?;
        if let ObserveState::Terminal { deployment, .. } = state {
            return match deployment.status {
                CloudDeploymentStatus::Applied => Ok(()),
                CloudDeploymentStatus::Failed => {
                    Err(current_tree_error("Cloud deployment finished as failed"))
                }
                CloudDeploymentStatus::Cancelled => {
                    Err(current_tree_error("Cloud deployment finished as cancelled"))
                }
                CloudDeploymentStatus::Queued
                | CloudDeploymentStatus::Planning
                | CloudDeploymentStatus::Building
                | CloudDeploymentStatus::Deploying => Err(current_tree_error(
                    "Cloud terminal observation retained a nonterminal deployment status",
                )),
            };
        }
        if started.elapsed() >= OBSERVE_TIMEOUT {
            return Err(current_tree_error("Cloud deployment observation timed out"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ployz_core::ids::OperationId;

    use super::*;

    #[test]
    fn controlled_admission_uses_the_frozen_operation_not_the_build_record() {
        let frozen = FrozenBuild {
            assignment_id: "assignment-distinct".to_owned(),
            build_record_id: "build-record-distinct".to_owned(),
            operation_id: OperationId::try_new("operation-distinct").expect("operation"),
            deployment_id: "deployment-distinct".to_owned(),
        };

        assert_eq!(
            controlled_admission(&frozen),
            BuildExecutorAdmission::ExactOperation(frozen.operation_id)
        );
    }

    #[tokio::test]
    async fn unchanged_source_is_restaged_for_the_prepared_executor() {
        let source = tempfile::tempdir().expect("source");
        fs::write(source.path().join("Dockerfile"), "FROM scratch\n").expect("source file");
        let workspace = tempfile::tempdir().expect("workspace");
        let executor = DockerBuildExecutor::new(workspace.path().to_path_buf());
        let BuildSource::LocalSnapshot { digest, subdir } = executor
            .prepare_local_snapshot(source.path().to_path_buf(), None)
            .await
            .expect("initial snapshot")
        else {
            panic!("local snapshot")
        };
        fs::remove_dir_all(workspace.path()).expect("recovery-equivalent cleanup");

        restage_local_snapshot(
            &executor,
            source.path().to_path_buf(),
            &digest,
            subdir.as_ref(),
        )
        .await
        .expect("restage unchanged source");

        let staged = workspace
            .path()
            .join("prepared")
            .join(digest.as_str().strip_prefix("sha256:").expect("digest"));
        let consumed = workspace.path().join("operation").join("source");
        fs::create_dir_all(consumed.parent().expect("operation directory"))
            .expect("operation directory");
        fs::rename(staged, &consumed).expect("prepared executor can consume snapshot");
        assert_eq!(
            fs::read_to_string(consumed.join("Dockerfile")).expect("restaged source"),
            "FROM scratch\n"
        );
    }

    #[tokio::test]
    async fn changed_source_is_rejected_before_execution() {
        let source = tempfile::tempdir().expect("source");
        let source_file = source.path().join("Dockerfile");
        fs::write(&source_file, "FROM scratch\n").expect("source file");
        let workspace = tempfile::tempdir().expect("workspace");
        let executor = DockerBuildExecutor::new(workspace.path().to_path_buf());
        let BuildSource::LocalSnapshot { digest, subdir } = executor
            .prepare_local_snapshot(source.path().to_path_buf(), None)
            .await
            .expect("initial snapshot")
        else {
            panic!("local snapshot")
        };
        fs::remove_dir_all(workspace.path()).expect("recovery-equivalent cleanup");
        fs::write(&source_file, "FROM busybox\n").expect("mutated source file");

        let error = restage_local_snapshot(
            &executor,
            source.path().to_path_buf(),
            &digest,
            subdir.as_ref(),
        )
        .await
        .expect_err("changed source must be rejected");

        assert!(matches!(
            error,
            RestageLocalSnapshotError::SourceMutated {
                expected_digest,
                actual_digest,
                ..
            } if expected_digest == digest && actual_digest != digest
        ));
    }
}
