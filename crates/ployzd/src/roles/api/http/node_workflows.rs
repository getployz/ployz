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
    DeployObservedContainer, DeployPrepareOutcome, DeployPrepareRequest, DeployRetireOutcome,
    DeployRetireRequest,
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
pub(super) const ROLLBACK_WAIT: Duration = Duration::from_secs(315);
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
            .unwrap_or(DeployPrepareOutcome::Failed {})
    }

    /// Starts or resumes the operation's stable retire instance and waits for
    /// its typed terminal result. Completed instances do no new host work.
    pub(super) async fn retire(&self, request: DeployRetireRequest) -> DeployRetireOutcome {
        if !request.rollback_services.is_empty() {
            return self.rollback(request).await;
        }
        self.run_retire(request).await
    }

    async fn run_retire(&self, request: DeployRetireRequest) -> DeployRetireOutcome {
        let instance = retire_instance(&request.namespace_name, &request.operation_id);
        self.run(&instance, RETIRE_WORKFLOW, &request, RETIRE_WAIT)
            .await
            .unwrap_or(DeployRetireOutcome::Failed {})
    }

    async fn rollback(&self, mut request: DeployRetireRequest) -> DeployRetireOutcome {
        if !request.containers.is_empty() || !request.restart_after_retire.is_empty() {
            return DeployRetireOutcome::Refused {};
        }
        let unique_services = request
            .rollback_services
            .iter()
            .collect::<std::collections::BTreeSet<_>>();
        if unique_services.len() != request.rollback_services.len() {
            return DeployRetireOutcome::Refused {};
        }

        let mut candidates = Vec::new();
        let mut incumbents = Vec::new();
        for service_name in &request.rollback_services {
            let outcome = match self
                .completed_prepare(&request.namespace_name, service_name, &request.operation_id)
                .await
            {
                Ok(Some(outcome)) => outcome,
                Ok(None) => continue,
                Err(()) => return DeployRetireOutcome::Failed {},
            };
            if merge_completed_prepare(
                &request,
                service_name,
                outcome,
                &mut candidates,
                &mut incumbents,
            )
            .is_err()
            {
                return DeployRetireOutcome::Refused {};
            }
        }
        request.containers = candidates;
        request.restart_after_retire = incumbents;
        request.rollback_services.clear();
        self.run_retire(request).await
    }

    async fn completed_prepare(
        &self,
        namespace: &CorrosionNamespaceName,
        service: &CorrosionServiceName,
        deploy: &DeployName,
    ) -> Result<Option<DeployPrepareOutcome>, ()> {
        let instance = prepare_instance(namespace, service, deploy);
        match self
            .client
            .get_orchestration_status(&instance)
            .await
            .map_err(|_| ())?
        {
            OrchestrationStatus::NotFound | OrchestrationStatus::Failed { .. } => Ok(None),
            OrchestrationStatus::Completed { .. } | OrchestrationStatus::Running { .. } => self
                .client
                .wait_for_orchestration_typed(&instance, PREPARE_WAIT)
                .await
                .map_err(|_| ())?
                .map(Some)
                .map_err(|_| ()),
        }
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

fn extend_unique(
    target: &mut Vec<DeployObservedContainer>,
    additions: impl IntoIterator<Item = DeployObservedContainer>,
) {
    for container in additions {
        if !target
            .iter()
            .any(|existing| existing.identity == container.identity)
        {
            target.push(container);
        }
    }
}

fn merge_completed_prepare(
    request: &DeployRetireRequest,
    service_name: &CorrosionServiceName,
    outcome: DeployPrepareOutcome,
    candidates: &mut Vec<DeployObservedContainer>,
    incumbents: &mut Vec<DeployObservedContainer>,
) -> Result<(), ()> {
    let DeployPrepareOutcome::Prepared {
        controller_machine_id,
        appointment_id,
        replicas,
        displaced_incumbents,
        ..
    } = outcome
    else {
        return Ok(());
    };
    if controller_machine_id != request.controller_machine_id
        || appointment_id != request.appointment_id
        || replicas.iter().any(|replica| {
            replica.identity.namespace_id != request.namespace_name
                || replica.identity.service_name != *service_name
                || replica.identity.operation_id != request.operation_id
        })
    {
        return Err(());
    }
    extend_unique(
        candidates,
        replicas.into_iter().map(|replica| DeployObservedContainer {
            identity: replica.identity,
            running: true,
            host_ports: Default::default(),
        }),
    );
    extend_unique(incumbents, displaced_incumbents);
    Ok(())
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

fn retire_instance(namespace: &CorrosionNamespaceName, deploy: &DeployName) -> String {
    format!("deploy-retire-{}/{}", namespace.as_str(), deploy.as_str())
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
        .unwrap_or(DeployPrepareOutcome::Failed {}))
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
        .unwrap_or(DeployRetireOutcome::Failed {}))
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

    use ployz_core::DeployPreparedReplica;
    use ployz_core::corrosion::{ControllerRevision, V2ManagedContainerIdentity};
    use ployz_core::deploy::{ImageReference, ReplicaSlot};
    use ployz_core::ids::MachineName;
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
            retire_instance(&production, &deploy),
            retire_instance(&staging, &deploy),
            "namespace-scoped deploy names must not collide during retire"
        );
    }

    #[test]
    fn rollback_wait_covers_a_lost_prepare_reply_and_retirement() {
        assert!(PREPARE_WAIT > RETIRE_WAIT);
        assert_eq!(ROLLBACK_WAIT, PREPARE_WAIT + RETIRE_WAIT);
    }

    #[test]
    fn stale_controller_rollback_is_derived_from_its_completed_prepare() {
        let namespace = CorrosionNamespaceName::try_new("production").expect("namespace");
        let service = CorrosionServiceName::try_new("api").expect("service");
        let deploy = DeployName::try_new("release-1").expect("deploy");
        let controller = MachineName::try_new("node-1").expect("controller");
        let appointment = ControllerRevision::INITIAL;
        let identity = V2ManagedContainerIdentity {
            namespace_id: namespace.clone(),
            service_name: service.clone(),
            operation_id: deploy.clone(),
            replica_slot: ReplicaSlot::Global,
        };
        let request = DeployRetireRequest {
            controller_machine_id: controller.clone(),
            appointment_id: appointment,
            operation_id: deploy,
            namespace_name: namespace,
            containers: Vec::new(),
            restart_after_retire: Vec::new(),
            rollback_services: vec![service.clone()],
        };
        let prepared = DeployPrepareOutcome::Prepared {
            controller_machine_id: controller,
            appointment_id: appointment,
            image: ImageReference::try_new(
                "nginx@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            )
            .expect("image"),
            replicas: vec![DeployPreparedReplica {
                identity: identity.clone(),
                ip: "10.210.20.2".parse().expect("ip"),
            }],
            displaced_incumbents: Vec::new(),
        };
        let mut candidates = Vec::new();
        let mut incumbents = Vec::new();

        merge_completed_prepare(
            &request,
            &service,
            prepared,
            &mut candidates,
            &mut incumbents,
        )
        .expect("matching completed prepare");

        let [candidate] = candidates.as_slice() else {
            panic!("one candidate expected")
        };
        assert_eq!(candidate.identity, identity);
        assert!(incumbents.is_empty());
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
