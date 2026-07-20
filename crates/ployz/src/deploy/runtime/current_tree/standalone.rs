use std::path::Path;

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
use crate::build::embedded_executor;
use crate::deploy::command::CurrentTreeDeployCommand;
use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_error::PloyzctlExecutionError;
use crate::execution_support::{
    PloyzctlExecutionOutput, api_error, current_unix_seconds, generate_client_build_id,
    generate_client_deploy_id, operation_api_client, watch_operation_until_terminal,
};
use crate::operation::runtime::replay_request;

use super::{OBSERVE_TIMEOUT, current_tree_error, write_private};

pub(super) async fn execute(
    command: CurrentTreeDeployCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    if command.organization.is_some() {
        return Err(current_tree_error(
            "--organization applies only when PLOYZ_CLOUD_URL is configured",
        ));
    }
    let config = crate::execution_support::with_cluster_context_from_disk(config.clone())?;
    let environment = command.environment.as_deref().unwrap_or("default");
    let namespace_id = NamespaceId::try_new(environment).map_err(current_tree_error)?;
    let history =
        super::super::history::stream(&config, namespace_id.clone()).map_err(current_tree_error)?;
    let entries = history.load().map_err(current_tree_error)?;
    let template = entries
        .last()
        .ok_or_else(|| current_tree_error("selected service has no successful deploy history"))?;
    let template_request = template
        .request
        .clone()
        .try_into_rollback_request()
        .map_err(|error| {
            current_tree_error(format!(
                "current-tree deploy cannot restore redacted environment values: {error}"
            ))
        })?;
    let service_id = select_standalone_service(&template_request, command.service.as_deref())?;
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
        &template_request,
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
    let executor = crate::build::external_runtime::run_controlled(
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
    );
    let build_result = embedded_executor::run_once(
        executor,
        ready_rx,
        shutdown_tx,
        || run_standalone_build(&api, &config, source, adapter, platform, pool_id),
        tokio::signal::ctrl_c(),
    )
    .await;
    let revoke_result = revoke_executor(&api, &config, public_key).await;
    let receipt = match (build_result, revoke_result) {
        (Ok(receipt), Ok(())) => receipt,
        (Err(error), Ok(())) => return Err(error),
        (_, Err(error)) => return Err(error),
    };

    let mut target = template_request;
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
    super::super::follow::execute_deploy(deploy, &config).await
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

pub(super) fn select_standalone_service(
    request: &ployz_core::deploy::DeployRequest,
    selector: Option<&str>,
) -> Result<ServiceId, PloyzctlExecutionError> {
    let services = request
        .services
        .iter()
        .filter(|service| selector.is_none_or(|value| value == service.service_id.as_str()))
        .map(|service| service.service_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    match services.len() {
        0 => Err(current_tree_error(
            "no matching service exists in deploy history",
        )),
        1 => Ok(services.into_iter().next().expect("one service")),
        count => Err(current_tree_error(format!(
            "standalone service selection is ambiguous ({count} matches); pass --service"
        ))),
    }
}

pub(super) fn validate_standalone_template(
    request: &ployz_core::deploy::DeployRequest,
    live_services: impl IntoIterator<Item = ServingTargetEntry>,
    automatic_hostname_configuration: &AutomaticHostnameConfiguration,
    live_routes: &[RouteBindingState],
) -> Result<(), PloyzctlExecutionError> {
    let mut expected_services = request
        .services
        .iter()
        .map(|service| {
            let mut volume_names = service
                .runtime
                .volume_mounts
                .iter()
                .map(|mount| mount.volume_name.clone())
                .collect::<Vec<_>>();
            volume_names.sort();
            (
                service.service_id.clone(),
                service.image.clone(),
                service.mode,
                volume_names,
            )
        })
        .collect::<Vec<_>>();
    expected_services.sort_by(|left, right| left.0.cmp(&right.0));
    let mut actual_services = live_services
        .into_iter()
        .filter(|service| service.namespace_id == request.namespace_id)
        .map(|service| {
            let mut volume_names = service.volume_names;
            volume_names.sort();
            (
                service.service_id,
                service.image,
                service.mode,
                volume_names,
            )
        })
        .collect::<Vec<_>>();
    actual_services.sort_by(|left, right| left.0.cmp(&right.0));
    if expected_services != actual_services {
        return Err(current_tree_error(
            "newest successful deploy no longer matches the active namespace service set",
        ));
    }

    let target = DeployPlanningTarget::try_from_deploy(request).map_err(current_tree_error)?;
    let automatic_suffix = match automatic_hostname_configuration {
        AutomaticHostnameConfiguration::Disabled => None,
        AutomaticHostnameConfiguration::Ployz => Some(
            ployz_core::operation::RouteHostname::try_new(
                ployz_core::certificate::MANAGED_LEASE_DOMAIN_SUFFIX,
            )
            .map_err(current_tree_error)?,
        ),
        AutomaticHostnameConfiguration::Custom { suffix } => Some(suffix.as_hostname().clone()),
    };
    let additions = validate_deploy_route_bindings(&target, automatic_suffix.as_ref(), &[])
        .map_err(current_tree_error)?;
    let mut prospective_id = 0_u64;
    let expected_routes = commit_deploy_route_bindings(additions, &[], |_| {
        prospective_id += 1;
        RouteBindingId::try_new(format!("prospective_{prospective_id}"))
            .expect("generated route binding id is valid")
    });
    let mut expected_routes = expected_routes
        .into_iter()
        .map(|route| route_shape(&route))
        .collect::<Vec<_>>();
    expected_routes.sort();
    let mut actual_routes = live_routes
        .iter()
        .filter(|route| route.namespace_id == request.namespace_id)
        .map(route_shape)
        .collect::<Vec<_>>();
    actual_routes.sort();
    if expected_routes != actual_routes {
        return Err(current_tree_error(
            "newest successful deploy no longer matches active namespace route bindings",
        ));
    }
    Ok(())
}

fn route_shape(
    route: &RouteBindingState,
) -> (
    ployz_core::operation::RouteTarget,
    ployz_core::operation::RoutePort,
    ServiceId,
    u8,
) {
    (
        route.target.clone(),
        route.endpoint_port,
        route.service_id.clone(),
        match route.origin {
            ployz_core::ingress::RouteBindingOrigin::Declared => 0,
            ployz_core::ingress::RouteBindingOrigin::Automatic => 1,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::deploy::{
        ContainerRuntimeSpec, DeployRequest, DeployServiceSpec, ImageReference, ImageSource,
        ReplicaCount, ServiceMode,
    };
    use ployz_core::ids::NamespaceRevisionEntryId;
    use ployz_core::ingress::RouteBindingOrigin;
    use ployz_core::operation::{RouteHostname, RoutePort, RouteTarget};

    fn request(service_ids: &[&str]) -> DeployRequest {
        DeployRequest {
            namespace_id: NamespaceId::try_new("production").expect("namespace"),
            origin: None,
            volumes: std::collections::BTreeMap::new(),
            services: service_ids
                .iter()
                .map(|service_id| DeployServiceSpec {
                    service_id: ServiceId::try_new(*service_id).expect("service"),
                    image: ImageReference::try_new(format!("ghcr.io/acme/{service_id}:current"))
                        .expect("image"),
                    image_source: ImageSource::Registry,
                    mode: ServiceMode::Replicated {
                        replicas: ReplicaCount::try_new(1).expect("replicas"),
                    },
                    keep: None,
                    runtime: ContainerRuntimeSpec::image_defaults(),
                    pre_start: None,
                    depends_on: Vec::new(),
                    routes: Vec::new(),
                })
                .collect(),
        }
    }

    fn live_service(request: &DeployRequest, index: usize) -> ServingTargetEntry {
        let service = request.services.get(index).expect("service index");
        ServingTargetEntry {
            namespace_id: request.namespace_id.clone(),
            service_id: service.service_id.clone(),
            namespace_revision_entry_id: NamespaceRevisionEntryId::try_new("entry_test")
                .expect("entry id"),
            image: service.image.clone(),
            mode: service.mode,
            volume_names: Vec::new(),
        }
    }

    #[test]
    fn exact_active_projection_accepts_the_newest_template() {
        let request = request(&["api", "worker"]);
        let live = [live_service(&request, 0), live_service(&request, 1)];

        validate_standalone_template(
            &request,
            live,
            &AutomaticHostnameConfiguration::Disabled,
            &[],
        )
        .expect("exact projection");
    }

    #[test]
    fn changed_image_rejects_stale_history() {
        let request = request(&["api"]);
        let mut live = live_service(&request, 0);
        live.image = ImageReference::try_new("ghcr.io/acme/api:changed").expect("changed image");

        let error = validate_standalone_template(
            &request,
            [live],
            &AutomaticHostnameConfiguration::Disabled,
            &[],
        )
        .expect_err("stale entry rejected");
        assert!(error.to_string().contains("active namespace service set"));
    }

    #[test]
    fn extra_active_service_rejects_stale_history() {
        let target = request(&["api"]);
        let newer = request(&["api", "worker"]);
        let live = [live_service(&newer, 0), live_service(&newer, 1)];

        let error = validate_standalone_template(
            &target,
            live,
            &AutomaticHostnameConfiguration::Disabled,
            &[],
        )
        .expect_err("extra service rejected");
        assert!(error.to_string().contains("active namespace service set"));
    }

    #[test]
    fn extra_active_route_rejects_stale_history() {
        let target = request(&["api"]);
        let live_service = live_service(&target, 0);
        let live_route = RouteBindingState {
            id: RouteBindingId::try_new("route_old").expect("route id"),
            namespace_id: target.namespace_id.clone(),
            target: RouteTarget::new(RouteHostname::try_new("old.example.com").expect("hostname")),
            endpoint_port: RoutePort::try_new(8080).expect("port"),
            service_id: ServiceId::try_new("api").expect("service"),
            origin: RouteBindingOrigin::Declared,
        };

        let error = validate_standalone_template(
            &target,
            [live_service],
            &AutomaticHostnameConfiguration::Disabled,
            &[live_route],
        )
        .expect_err("extra route rejected");
        assert!(
            error
                .to_string()
                .contains("active namespace route bindings")
        );
    }
}
