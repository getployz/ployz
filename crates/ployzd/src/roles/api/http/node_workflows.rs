//! Durable prepare and retire effects owned by this node.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use duroxide::providers::sqlite::SqliteProvider;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{ObservabilityConfig, Runtime, RuntimeOptions};
use duroxide::{
    Client, OrchestrationContext, OrchestrationRegistry, OrchestrationStatus, RetryPolicy,
};
use ployz_core::corrosion::{CorrosionNamespaceName, CorrosionServiceName};
use ployz_core::ids::DeployName;
use ployz_core::{
    DeployPrepareOutcome, DeployPrepareRequest, DeployRetireOutcome, DeployRetireRequest,
};
use tokio::sync::watch;

use super::deploy_effects::DeployHostEffects;

const PREPARE_WORKFLOW: &str = "ployz_node_prepare";
const RETIRE_WORKFLOW: &str = "ployz_node_retire";
const PREPARE_ACTIVITY: &str = "ployz_prepare";
const RETIRE_ACTIVITY: &str = "ployz_retire";
const DATABASE_FILE: &str = "workflows.sqlite3";
const PREPARE_WAIT: Duration = Duration::from_secs(245);
const RETIRE_WAIT: Duration = Duration::from_secs(70);
const PREPARE_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(240);
const RETIRE_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(60);
const SHUTDOWN_TIMEOUT_MILLIS: u64 = 1_000;

/// One protected Duroxide runtime shared by all local deploy workflows.
pub(super) struct NodeWorkflows {
    client: Client,
    runtime: Arc<Runtime>,
    shutdown: watch::Sender<bool>,
}

impl NodeWorkflows {
    pub(super) async fn open(
        node_state_dir: impl AsRef<Path>,
        effects: Arc<DeployHostEffects>,
    ) -> Result<Self, NodeWorkflowOpenError> {
        let state_dir = node_state_dir.as_ref();
        private_directory(state_dir).await?;
        let database_path = state_dir.join(DATABASE_FILE);
        let Some(database_path_text) = database_path.to_str() else {
            return Err(NodeWorkflowOpenError::NonUtf8Path);
        };
        tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&database_path)
            .await?;

        // Workflow inputs may contain registry credentials. The containing
        // directory is private node state, never operator evidence.
        let store = Arc::new(
            SqliteProvider::new(&format!("sqlite:{database_path_text}"), None)
                .await
                .map_err(|error| NodeWorkflowOpenError::Database(error.to_string()))?,
        );
        let (shutdown, shutdown_receiver) = watch::channel(false);
        let runtime = Runtime::start_with_options(
            store.clone(),
            activity_registry(effects, shutdown_receiver),
            orchestration_registry(),
            RuntimeOptions {
                // The activity worker is the node's only deploy-mutation lock.
                orchestration_concurrency: 1,
                worker_concurrency: 1,
                observability: ObservabilityConfig {
                    log_level: "info".to_owned(),
                    ..ObservabilityConfig::default()
                },
                ..RuntimeOptions::default()
            },
        )
        .await;

        Ok(Self {
            client: Client::new(store),
            runtime,
            shutdown,
        })
    }

    /// Starts or resumes the operation's stable prepare instance and waits for
    /// its typed terminal result. Completed instances do no new host work.
    pub(super) async fn prepare(&self, request: DeployPrepareRequest) -> DeployPrepareOutcome {
        let instance = prepare_instance(
            &request.namespace_name,
            &request.service_name,
            &request.operation_id,
        );
        self.run(&instance, PREPARE_WORKFLOW, &request, PREPARE_WAIT)
            .await
            .unwrap_or(DeployPrepareOutcome::Failed)
    }

    /// Starts or resumes the operation's stable retire instance and waits for
    /// its typed terminal result. Service-removal state and failed deploy
    /// cleanup are discardable so a later request can retry from Docker reality.
    pub(super) async fn retire(&self, request: DeployRetireRequest) -> DeployRetireOutcome {
        let instance = retire_instance(
            &request.namespace_name,
            request.removed_service.as_ref(),
            &request.operation_id,
        );
        let outcome = self
            .run(&instance, RETIRE_WORKFLOW, &request, RETIRE_WAIT)
            .await
            .unwrap_or(DeployRetireOutcome::Failed);
        if (request.removed_service.is_some() || outcome == DeployRetireOutcome::Failed)
            && let Err(error) = self.client.delete_instance(&instance, false).await
        {
            tracing::warn!(%instance, %error, "could not discard terminal retire workflow");
        }
        outcome
    }

    async fn run<Input, Output>(
        &self,
        instance: &str,
        workflow: &str,
        input: &Input,
        timeout: Duration,
    ) -> Option<Output>
    where
        Input: serde::Serialize,
        Output: serde::de::DeserializeOwned,
    {
        match self.client.get_orchestration_status(instance).await.ok()? {
            OrchestrationStatus::Failed { .. } => return None,
            OrchestrationStatus::Completed { .. } | OrchestrationStatus::Running { .. } => {}
            OrchestrationStatus::NotFound => {
                self.client
                    .start_orchestration_typed(instance, workflow, input)
                    .await
                    .ok()?;
            }
        }
        self.client
            .wait_for_orchestration_typed(instance, timeout)
            .await
            .ok()?
            .ok()
    }

    pub(super) async fn shutdown(&self) {
        self.shutdown.send_replace(true);
        self.runtime
            .clone()
            .shutdown(Some(SHUTDOWN_TIMEOUT_MILLIS))
            .await;
    }
}

fn prepare_instance(
    namespace: &CorrosionNamespaceName,
    service: &CorrosionServiceName,
    deploy: &DeployName,
) -> String {
    format!(
        "deploy-prepare-{}/{}/{}",
        namespace.as_str(),
        service.as_str(),
        deploy.as_str()
    )
}

fn retire_instance(
    namespace: &CorrosionNamespaceName,
    removed_service: Option<&CorrosionServiceName>,
    deploy: &DeployName,
) -> String {
    match removed_service {
        Some(service) => format!("service-remove/{}/{}", namespace.as_str(), service.as_str()),
        None => format!("deploy-retire/{}/{}", namespace.as_str(), deploy.as_str()),
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum NodeWorkflowOpenError {
    #[error("could not protect the node workflow directory: {0}")]
    Io(#[from] std::io::Error),
    #[error("node workflow path is not valid UTF-8")]
    NonUtf8Path,
    #[error("could not open node workflow database: {0}")]
    Database(String),
}

fn orchestration_registry() -> OrchestrationRegistry {
    OrchestrationRegistry::builder()
        .register_typed(PREPARE_WORKFLOW, prepare_workflow)
        .register_typed(RETIRE_WORKFLOW, retire_workflow)
        .build()
}

async fn prepare_workflow(
    context: OrchestrationContext,
    request: DeployPrepareRequest,
) -> Result<DeployPrepareOutcome, String> {
    Ok(context
        .schedule_activity_with_retry_typed(
            PREPARE_ACTIVITY,
            &request,
            activity_policy(PREPARE_ATTEMPT_TIMEOUT),
        )
        .await
        .unwrap_or(DeployPrepareOutcome::Failed))
}

async fn retire_workflow(
    context: OrchestrationContext,
    request: DeployRetireRequest,
) -> Result<DeployRetireOutcome, String> {
    Ok(context
        .schedule_activity_with_retry_typed(
            RETIRE_ACTIVITY,
            &request,
            activity_policy(RETIRE_ATTEMPT_TIMEOUT),
        )
        .await
        .unwrap_or(DeployRetireOutcome::Failed))
}

fn activity_policy(timeout: Duration) -> RetryPolicy {
    RetryPolicy::new(1).with_timeout(timeout)
}

fn activity_registry(
    effects: Arc<DeployHostEffects>,
    shutdown: watch::Receiver<bool>,
) -> ActivityRegistry {
    let prepare_effects = effects.clone();
    ActivityRegistry::builder()
        .register_typed(
            PREPARE_ACTIVITY,
            move |_context, request: DeployPrepareRequest| {
                let effects = prepare_effects.clone();
                let shutdown = shutdown.clone();
                async move { effects.prepare(request, shutdown).await }
            },
        )
        .register_typed(
            RETIRE_ACTIVITY,
            move |_context, request: DeployRetireRequest| {
                let effects = effects.clone();
                async move { effects.retire(request).await }
            },
        )
        .build()
}

async fn private_directory(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;

    tokio::fs::create_dir_all(path).await?;
    if !tokio::fs::symlink_metadata(path).await?.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a directory", path.display()),
        ));
    }
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use ployz_core::network::MachineEndpointSubnet;
    use tempfile::TempDir;

    use super::*;
    use crate::WireGuardMtuPolicy;
    use crate::roles::api::execution::docker::runner::DockerManagedContainerRunner;

    #[test]
    fn workflow_instances_follow_prepare_and_retire_collision_scopes() {
        let production = CorrosionNamespaceName::try_new("production").expect("namespace");
        let staging = CorrosionNamespaceName::try_new("staging").expect("namespace");
        let api = CorrosionServiceName::try_new("api").expect("service");
        let worker = CorrosionServiceName::try_new("worker").expect("service");
        let deploy = DeployName::try_new("release-1").expect("deploy");

        assert_ne!(
            prepare_instance(&production, &api, &deploy),
            prepare_instance(&production, &worker, &deploy),
            "services prepared on one machine must not share completed workflow state"
        );
        assert_ne!(
            prepare_instance(&production, &api, &deploy),
            prepare_instance(&staging, &api, &deploy),
            "namespace-scoped deploy names must not collide during prepare"
        );
        assert_ne!(
            retire_instance(&production, None, &deploy),
            retire_instance(&staging, None, &deploy),
            "namespace-scoped deploy names must not collide during retire"
        );
        assert_ne!(
            retire_instance(&production, None, &deploy),
            retire_instance(&production, Some(&api), &deploy),
            "deploy cleanup and service removal have disjoint named scopes"
        );
    }

    #[tokio::test]
    async fn workflow_directory_is_node_private() {
        let state = TempDir::new().expect("state directory");
        let directory = state.path().join("workflows");
        let subnet = MachineEndpointSubnet::try_new("10.210.12.0/24").expect("subnet");
        let runner = Arc::new(DockerManagedContainerRunner::lazy_local_defaults(
            subnet.as_string(),
            "br-test".to_owned(),
            "wg-test".to_owned(),
            WireGuardMtuPolicy::Fixed(1_420),
        ));
        let workflows = NodeWorkflows::open(&directory, Arc::new(DeployHostEffects::new(runner)))
            .await
            .expect("runtime opens");

        let mode = std::fs::metadata(&directory)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
        assert!(directory.join(DATABASE_FILE).exists());

        workflows.shutdown().await;
    }
}
