use std::path::PathBuf;
use std::time::Duration;

use ployz_build_executor::DockerBuildExecutor;
use ployz_core::build::LocalSnapshotDigest;
use ployz_core::image::OciPlatform;
use ployz_core::nats_config::MintedNatsUser;

use crate::build::command::{BuildExecutorCommand, BuildExecutorRunMode};
use crate::build::embedded_executor;
use crate::cloud_current_tree::{
    ApprovalState, CloudCurrentTreeClient, CloudDeploymentStatus, ObserveState, select_context,
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
    workspace_root: PathBuf,
    platform: OciPlatform,
    digest: LocalSnapshotDigest,
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
        .prepare_local_snapshot(cwd, None)
        .await
        .map_err(current_tree_error)?;
    let ployz_core::build::BuildSource::LocalSnapshot { digest, subdir: _ } = source else {
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
        workspace_root,
        platform,
        digest,
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
        workspace_root,
        platform,
        digest,
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
            mode: BuildExecutorRunMode::Once {
                wait_timeout: OBSERVE_TIMEOUT,
            },
        },
        executor_config,
        crate::build::external_runtime::WorkspaceStartup::Prepared,
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
