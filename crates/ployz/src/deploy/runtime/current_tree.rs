use std::path::{Path, PathBuf};
use std::time::Duration;

use ployz_build_executor::DockerBuildExecutor;
use ployz_core::build::{
    BuildAdapter, BuildCacheScope, BuildContextPath, BuildPlatforms, BuildTarget,
};
use ployz_core::deploy::{
    DeployPlanningTarget, ImageReference, ImageSource, commit_deploy_route_bindings,
    validate_deploy_route_bindings,
};
use ployz_core::ids::{
    BuildExecutorId, BuildPoolId, NamespaceId, OperationId, RouteBindingId, ServiceId,
};
use ployz_core::ingress::AutomaticHostnameConfiguration;
use ployz_core::intent::{RouteBindingState, ServingTargetEntry};
use ployz_core::nats_config::{
    BuildExecutorCredentialExpiresAt, CredentialGrant, CredentialName, CredentialRole,
    MintedNatsUser,
};
use ployz_core::operation::{BuildOperationState, OperationOutcome, OperationStatus};
use ployz_sdk_types::{
    BuildSubmitRequest, CredentialAddRequest, CredentialRemoveRequest, OpsStatusRequest,
    RuntimeSnapshotRequest, ServiceListRequest,
};

use crate::build::command::{BuildExecutorCommand, BuildExecutorRunMode};
use crate::cloud_current_tree::{
    ApprovalState, CloudCurrentTreeClient, CloudDeploymentStatus, ObserveState, select_context,
};
use crate::deploy::command::CurrentTreeDeployCommand;
use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_error::PloyzctlExecutionError;
use crate::execution_support::PloyzctlExecutionOutput;
use crate::execution_support::{
    api_error, current_unix_seconds, generate_client_build_id, generate_client_deploy_id,
    operation_api_client, watch_operation_until_terminal,
};
use crate::operation::runtime::replay_request;

use super::DeployExecutionError;

mod standalone;
use standalone::{select_standalone_service, validate_standalone_template};

const CLOUD_URL_ENV: &str = "PLOYZ_CLOUD_URL";
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const OBSERVE_TIMEOUT: Duration = Duration::from_secs(40 * 60);
const EXECUTOR_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) async fn execute(
    command: CurrentTreeDeployCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let cloud_url = std::env::var(CLOUD_URL_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty());
    let Some(cloud_url) = cloud_url else {
        return execute_standalone(command, config).await;
    };
    execute_cloud(command, config, &cloud_url).await
}

async fn execute_cloud(
    command: CurrentTreeDeployCommand,
    config: &PloyzctlRuntimeConfig,
    cloud_url: &str,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let cwd = std::env::current_dir().map_err(current_tree_error)?;
    let workspace_root = cwd.join(".ployz").join("build-executor");
    let executor = DockerBuildExecutor::new(workspace_root.clone());
    executor
        .recover_orphans()
        .await
        .map_err(current_tree_error)?;
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
    let result = execute_approved_cloud(
        command,
        config,
        workspace_root,
        platform,
        digest,
        client.clone(),
        &begun.session_secret,
        begun.poll_after_seconds,
    )
    .await;
    let cancel_result = client.cancel(&begun.session_secret).await;
    match (result, cancel_result) {
        (Ok(output), Ok(())) => Ok(output),
        (Err(error), Ok(())) => Err(error),
        (_, Err(error)) => Err(current_tree_error(error)),
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_approved_cloud(
    command: CurrentTreeDeployCommand,
    config: &PloyzctlRuntimeConfig,
    workspace_root: PathBuf,
    platform: ployz_core::image::OciPlatform,
    digest: ployz_core::build::LocalSnapshotDigest,
    client: CloudCurrentTreeClient,
    session_secret: &str,
    poll_after_seconds: u64,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    await_approval(&client, session_secret, poll_after_seconds).await?;
    let contexts = client
        .contexts(session_secret)
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
        .select(session_secret, &selected)
        .await
        .map_err(current_tree_error)?;
    let frozen = client
        .freeze(session_secret, &digest, &platform)
        .await
        .map_err(current_tree_error)?;
    let minted = MintedNatsUser::generate().map_err(current_tree_error)?;
    let activated = client
        .activate(session_secret, &frozen, &digest, &minted.public)
        .await
        .map_err(current_tree_error)?;
    if activated.platform != platform {
        return Err(current_tree_error(
            "Cloud activated a different executor platform",
        ));
    }
    let material = tempfile::tempdir().map_err(current_tree_error)?;
    let ca_path = material.path().join("nats-ca.pem");
    let seed_path = material.path().join("executor.nk");
    write_private(&ca_path, activated.trusted_nats.ca_pem.as_str(), 0o600)?;
    write_private(&seed_path, minted.seed.secret(), 0o600)?;
    let executor_config = PloyzctlRuntimeConfig {
        nats_url: Some(activated.runtime_nats_url.as_str().to_owned()),
        nats_ca_file: Some(ca_path),
        nats_seed_file: Some(seed_path),
        nats_connect_timeout: config.nats_connect_timeout,
        ..PloyzctlRuntimeConfig::default()
    };
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let mut executor_task = tokio::spawn(crate::build::external_runtime::run_controlled(
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
    ));
    let (ready, executor_joined) = tokio::select! {
        ready = ready_rx => (ready.map_err(current_tree_error), false),
        executor = &mut executor_task => {
            (match executor.map_err(current_tree_error) {
                Ok(Ok(_)) => Err(current_tree_error("build executor exited before becoming ready")),
                Ok(Err(error)) | Err(error) => Err(error),
            }, true)
        }
        signal = tokio::signal::ctrl_c() => {
            (signal
                .map_err(current_tree_error)
                .and_then(|()| Err(current_tree_error("current-tree deploy cancelled"))), false)
        }
    };
    if let Err(error) = ready {
        let shutdown = if executor_joined {
            Ok(())
        } else {
            shutdown_executor(shutdown_tx, executor_task).await
        };
        return prefer_shutdown_result(Err(error), shutdown);
    }
    let dispatch = client
        .dispatch(session_secret, &frozen, &digest, &minted.public)
        .await
        .map_err(current_tree_error);
    if let Err(error) = dispatch {
        let shutdown = shutdown_executor(shutdown_tx, executor_task).await;
        return prefer_shutdown_result(Err(error), shutdown);
    }
    let observed = await_cloud_terminal(&client, session_secret);
    tokio::pin!(observed);
    let (result, executor_joined): (Result<(), PloyzctlExecutionError>, bool) = tokio::select! {
        terminal = &mut observed => (terminal, false),
        executor = &mut executor_task => {
            (match executor.map_err(current_tree_error) {
                Ok(Ok(_)) => await_cloud_terminal(&client, session_secret).await,
                Ok(Err(error)) => Err(error),
                Err(error) => Err(error),
            }, true)
        }
        signal = tokio::signal::ctrl_c() => {
            (match signal {
                Ok(()) => Err(current_tree_error("current-tree deploy cancelled")),
                Err(error) => Err(current_tree_error(error)),
            }, false)
        }
    };
    let shutdown_result = if executor_joined {
        Ok(())
    } else {
        shutdown_executor(shutdown_tx, executor_task).await
    };
    prefer_shutdown_result(result, shutdown_result)?;
    Ok(PloyzctlExecutionOutput::stdout(format!(
        "built current working tree for {}/{}/{} and completed deployment {}\n",
        selected.organization.slug,
        selected.environment.namespace,
        selected.service.slug,
        frozen.deployment_id,
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

async fn await_cloud_terminal(
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

async fn execute_standalone(
    command: CurrentTreeDeployCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    if command.organization.is_some() {
        return Err(current_tree_error(
            "--organization applies only when PLOYZ_CLOUD_URL is configured",
        ));
    }
    let config = crate::execution_support::with_cluster_context_from_disk(config.clone())?;
    let namespace_id = NamespaceId::try_new(command.environment.as_deref().unwrap_or("default"))
        .map_err(current_tree_error)?;
    let history =
        super::history::stream(&config, namespace_id.clone()).map_err(current_tree_error)?;
    let entries = history.load().map_err(current_tree_error)?;
    let template = entries
        .last()
        .ok_or_else(|| current_tree_error("selected service has no successful deploy history"))?;
    let service_id = select_standalone_service(&template.request, command.service.as_deref())?;
    let api = operation_api_client(&config).await?;
    let live_services = api
        .service_list(&ServiceListRequest {})
        .await
        .map_err(api_error)?;
    let runtime = api
        .runtime_snapshot(&RuntimeSnapshotRequest {})
        .await
        .map_err(api_error)?;
    validate_standalone_template(
        &template.request,
        live_services
            .services
            .iter()
            .map(|service| service.active.clone()),
        &runtime.snapshot.automatic_hostname_configuration,
        &runtime.snapshot.routes,
    )?;
    let cwd = std::env::current_dir().map_err(current_tree_error)?;
    let workspace_root = cwd.join(".ployz").join("build-executor");
    let executor = DockerBuildExecutor::new(workspace_root.clone());
    executor
        .recover_orphans()
        .await
        .map_err(current_tree_error)?;
    let source = executor
        .prepare_local_snapshot(cwd.clone(), None)
        .await
        .map_err(current_tree_error)?;
    let platform = ployz_build_executor::native_oci_platform().map_err(current_tree_error)?;
    let adapter = standalone_adapter(&cwd, &service_id)?;
    let pool_id =
        BuildPoolId::try_new(format!("local_{}", nuid::next())).map_err(current_tree_error)?;
    let executor_id =
        BuildExecutorId::try_new(format!("local_{}", nuid::next())).map_err(current_tree_error)?;
    let minted = MintedNatsUser::generate().map_err(current_tree_error)?;
    let grant_id = generated_operation_id("credential_add")?;
    let expires_at =
        BuildExecutorCredentialExpiresAt::try_new(current_unix_seconds().saturating_add(45 * 60))
            .map_err(current_tree_error)?;
    let grant = api
        .credential_add(&CredentialAddRequest {
            operation_id: grant_id,
            grant: CredentialGrant {
                public_key: minted.public.clone(),
                name: CredentialName::try_new("Current working tree executor")
                    .map_err(current_tree_error)?,
                role: CredentialRole::BuildExecutor {
                    pool_id: pool_id.clone(),
                    executor_id: executor_id.clone(),
                    expires_at,
                },
            },
        })
        .await
        .map_err(api_error)?;
    require_operation_success(&api, grant.operation_id, &config).await?;

    let public_key = minted.public.clone();
    let build_result = async {
        let material = tempfile::tempdir().map_err(current_tree_error)?;
        let seed_path = material.path().join("executor.nk");
        write_private(&seed_path, minted.seed.secret(), 0o600)?;
        let executor_config = PloyzctlRuntimeConfig {
            nats_url: config.nats_url.clone(),
            nats_ca_file: config.nats_ca_file.clone(),
            nats_seed_file: Some(seed_path),
            nats_connect_timeout: config.nats_connect_timeout,
            ..PloyzctlRuntimeConfig::default()
        };
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let mut executor_task = tokio::spawn(crate::build::external_runtime::run_controlled(
            BuildExecutorCommand {
                pool_id: pool_id.clone(),
                executor_id,
                workspace_root: Some(workspace_root),
                mode: BuildExecutorRunMode::Once {
                    wait_timeout: OBSERVE_TIMEOUT,
                },
            },
            executor_config,
            crate::build::external_runtime::WorkspaceStartup::Prepared,
            Some(ready_tx),
            Some(shutdown_rx),
        ));
        let (ready, executor_joined) = tokio::select! {
            ready = ready_rx => (ready.map_err(current_tree_error), false),
            executor = &mut executor_task => {
                (match executor.map_err(current_tree_error) {
                    Ok(Ok(_)) => Err(current_tree_error("build executor exited before becoming ready")),
                    Ok(Err(error)) | Err(error) => Err(error),
                }, true)
            }
            signal = tokio::signal::ctrl_c() => {
                (signal
                    .map_err(current_tree_error)
                    .and_then(|()| Err(current_tree_error("current-tree deploy cancelled"))), false)
            }
        };
        if let Err(error) = ready {
            if !executor_joined {
                let _ = shutdown_executor(shutdown_tx, executor_task).await;
            }
            return Err(error);
        }
        let (build, executor_joined) = tokio::select! {
            result = run_standalone_build(
                &api,
                &config,
                source,
                adapter,
                platform,
                pool_id,
            ) => (result, false),
            executor = &mut executor_task => {
                (match executor.map_err(current_tree_error) {
                    Ok(Ok(_)) => Err(current_tree_error(
                        "build executor exited before the build became terminal",
                    )),
                    Ok(Err(error)) | Err(error) => Err(error),
                }, true)
            }
            signal = tokio::signal::ctrl_c() => {
                (signal
                    .map_err(current_tree_error)
                    .and_then(|()| Err(current_tree_error("current-tree deploy cancelled"))), false)
            }
        };
        let shutdown = if executor_joined {
            Ok(())
        } else {
            shutdown_executor(shutdown_tx, executor_task).await
        };
        prefer_shutdown_result(build, shutdown)
    }
    .await;
    let revoke_result = revoke_executor(&api, &config, public_key).await;
    let receipt = match (build_result, revoke_result) {
        (Ok(receipt), Ok(())) => receipt,
        (Err(error), Ok(())) => return Err(error),
        (_, Err(error)) => return Err(error),
    };

    let mut target = template.request.clone();
    let service = target
        .services
        .iter_mut()
        .find(|service| service.service_id == service_id)
        .ok_or_else(|| current_tree_error("deploy history lost selected service"))?;
    service.image = ImageReference::try_new(format!("ployz.local/{}:current", service_id.as_str()))
        .map_err(current_tree_error)?
        .with_digest(receipt.index_digest())
        .map_err(current_tree_error)?;
    service.image_source = ImageSource::PushedToSeed(receipt);
    let deploy = crate::deploy::command::DeployCommand {
        idempotency_key: generate_client_deploy_id(&service_id)
            .map_err(current_tree_error)?
            .idempotency_key,
        target: crate::deploy::command::DeployCommandTarget::Ordinary(target),
        warnings: Vec::new(),
        detach: false,
        from_registry: false,
    };
    super::follow::execute_deploy(deploy, &config).await
}

async fn shutdown_executor(
    shutdown: tokio::sync::oneshot::Sender<()>,
    mut task: tokio::task::JoinHandle<Result<PloyzctlExecutionOutput, PloyzctlExecutionError>>,
) -> Result<(), PloyzctlExecutionError> {
    let _ = shutdown.send(());
    match tokio::time::timeout(EXECUTOR_SHUTDOWN_TIMEOUT, &mut task).await {
        Ok(joined) => {
            let _ = joined.map_err(current_tree_error)??;
            Ok(())
        }
        Err(_) => {
            task.abort();
            let _ = task.await;
            Err(current_tree_error("build executor shutdown timed out"))
        }
    }
}

fn prefer_shutdown_result<T>(
    operation: Result<T, PloyzctlExecutionError>,
    shutdown: Result<(), PloyzctlExecutionError>,
) -> Result<T, PloyzctlExecutionError> {
    match shutdown {
        Ok(()) => operation,
        Err(error) => Err(error),
    }
}

fn standalone_adapter(
    cwd: &Path,
    service_id: &ServiceId,
) -> Result<BuildAdapter, PloyzctlExecutionError> {
    if cwd.join("Dockerfile").is_file() {
        return Ok(BuildAdapter::Dockerfile {
            dockerfile: BuildContextPath::try_new("Dockerfile").map_err(current_tree_error)?,
            target: None,
        });
    }
    Ok(BuildAdapter::Railpack {
        cache_scope: BuildCacheScope::try_new(format!(
            "local-current-tree-{}",
            service_id.as_str()
        ))
        .map_err(current_tree_error)?,
    })
}

async fn run_standalone_build(
    api: &crate::api_client::OperationApiClient,
    config: &PloyzctlRuntimeConfig,
    source: ployz_core::build::BuildSource,
    adapter: BuildAdapter,
    platform: ployz_core::image::OciPlatform,
    pool_id: BuildPoolId,
) -> Result<ployz_core::deploy::PushedImageReceipt, PloyzctlExecutionError> {
    let generated = generate_client_build_id().map_err(current_tree_error)?;
    let accepted = api
        .build_submit(&BuildSubmitRequest {
            operation_id: generated.operation_id.clone(),
            target: BuildTarget::External { pool_id },
            source,
            adapter,
            platforms: BuildPlatforms::try_new([platform]).map_err(current_tree_error)?,
        })
        .await
        .map_err(api_error)?;
    require_operation_success(api, accepted.operation_id.clone(), config).await?;
    let snapshot = api
        .ops_status(&OpsStatusRequest {
            operation_id: accepted.operation_id,
        })
        .await
        .map_err(api_error)?;
    let OperationStatus::Build { status } = snapshot.status else {
        return Err(current_tree_error(
            "build status returned the wrong operation kind",
        ));
    };
    let BuildOperationState::Completed { receipt } = status.state() else {
        return Err(current_tree_error(
            "build finished without a signed receipt",
        ));
    };
    Ok(receipt.clone())
}

async fn revoke_executor(
    api: &crate::api_client::OperationApiClient,
    config: &PloyzctlRuntimeConfig,
    public_key: ployz_core::nats_config::NatsUserPublicKey,
) -> Result<(), PloyzctlExecutionError> {
    let accepted = api
        .credential_remove(&CredentialRemoveRequest {
            operation_id: generated_operation_id("credential_remove")?,
            public_key,
        })
        .await
        .map_err(api_error)?;
    require_operation_success(api, accepted.operation_id, config).await
}

async fn require_operation_success(
    api: &crate::api_client::OperationApiClient,
    operation_id: OperationId,
    config: &PloyzctlRuntimeConfig,
) -> Result<(), PloyzctlExecutionError> {
    let (_, outcome) = watch_operation_until_terminal(
        api,
        replay_request(operation_id.clone()),
        config.ops_watch_timeout(),
        config.ops_watch_poll_interval(),
    )
    .await?;
    match outcome {
        OperationOutcome::Succeeded => Ok(()),
        OperationOutcome::Failed | OperationOutcome::Cancelled => Err(current_tree_error(format!(
            "operation {} did not succeed",
            operation_id.as_str()
        ))),
    }
}

fn generated_operation_id(prefix: &str) -> Result<OperationId, PloyzctlExecutionError> {
    OperationId::try_new(format!("op_{prefix}_{}", nuid::next())).map_err(current_tree_error)
}

fn write_private(path: &Path, contents: &str, mode: u32) -> Result<(), PloyzctlExecutionError> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    use std::io::Write;
    let mut file = options.open(path).map_err(current_tree_error)?;
    file.write_all(contents.as_bytes())
        .map_err(current_tree_error)
}

fn current_tree_error(message: impl std::fmt::Display) -> PloyzctlExecutionError {
    DeployExecutionError::CurrentTree {
        message: message.to_string(),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_failure_takes_precedence_over_operation_result() {
        let operation: Result<(), _> = Err(current_tree_error("deployment failed"));
        let shutdown = Err(current_tree_error("executor shutdown failed"));

        let error = prefer_shutdown_result(operation, shutdown).expect_err("shutdown failure");
        assert!(error.to_string().contains("executor shutdown failed"));
    }

    #[test]
    fn successful_shutdown_preserves_the_operation_result() {
        let output = PloyzctlExecutionOutput::stdout("done".to_owned());
        let result = prefer_shutdown_result(Ok(output), Ok(())).expect("operation result");

        assert_eq!(result.stdout, "done");
    }
}
