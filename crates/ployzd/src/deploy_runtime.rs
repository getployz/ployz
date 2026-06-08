//! Owned deploy execution launched by the control service.

use crate::controllers::{AcceptedDeployOperation, OperationControllers};
use crate::deploy_launcher::{
    DeployLaunchError, DeployLaunchPorts, DeployLaunchStores, run_deploy_operation,
};
use crate::deploy_worker::{
    DeployContainer, DeployExecutionNodeScope, DeployExecutionOutcome, DeployHealthCheckError,
    DeployHealthChecker,
};
use crate::node_rpc::{NatsNodeContainerRuntime, NatsNodeWireGuardEbpfPreparer};
use ployz_core::node::ContainerRuntimeState;
use ployz_core::ops::{FailureMessage, OperatorHint};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::observations::{AsyncNatsObservationStore, ObservationStoreError};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinHandle;

const DEPLOY_HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub struct OwnedDeployLauncher {
    client: async_nats::Client,
    core_state: AsyncNatsCoreStateStore,
    observations: AsyncNatsObservationStore,
    controllers: OperationControllers,
    node_scope: DeployExecutionNodeScope,
    step_timeout: Duration,
    task_registry: DeployTaskRegistry,
}

impl OwnedDeployLauncher {
    #[must_use]
    pub fn new(
        client: async_nats::Client,
        core_state: AsyncNatsCoreStateStore,
        observations: AsyncNatsObservationStore,
        controllers: OperationControllers,
        node_scope: DeployExecutionNodeScope,
        step_timeout: Duration,
        task_registry: DeployTaskRegistry,
    ) -> Self {
        Self {
            client,
            core_state,
            observations,
            controllers,
            node_scope,
            step_timeout,
            task_registry,
        }
    }

    pub fn launch(&self, accepted: AcceptedDeployOperation) {
        let launcher = self.clone();
        self.task_registry.spawn(async move {
            let _outcome = launcher.run(accepted).await;
        });
    }

    pub async fn run(
        self,
        accepted: AcceptedDeployOperation,
    ) -> Result<DeployExecutionOutcome, DeployLaunchError> {
        let mut wireguard_ebpf = NatsNodeWireGuardEbpfPreparer::new(self.client.clone())
            .with_request_timeout(self.step_timeout);
        let mut node_runtime = NatsNodeContainerRuntime::new(self.client.clone())
            .with_request_timeout(self.step_timeout);
        let mut health_checker =
            ObservationHealthChecker::new(self.observations.clone(), DEPLOY_HEALTH_POLL_INTERVAL);

        run_deploy_operation(
            accepted,
            self.node_scope,
            DeployLaunchStores {
                core_state: self.core_state,
                observations: self.observations,
                controllers: self.controllers,
            },
            DeployLaunchPorts {
                wireguard_ebpf: &mut wireguard_ebpf,
                node_runtime: &mut node_runtime,
                health_checker: &mut health_checker,
            },
            self.step_timeout,
        )
        .await
    }
}

#[derive(Debug, Clone, Default)]
pub struct DeployTaskRegistry {
    handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl DeployTaskRegistry {
    pub fn spawn(&self, future: impl std::future::Future<Output = ()> + Send + 'static) {
        let mut handles = self
            .handles
            .lock()
            .expect("deploy task registry lock is not poisoned");
        handles.retain(|handle| !handle.is_finished());
        handles.push(tokio::spawn(future));
    }

    pub fn abort_all(&self) {
        let mut handles = self
            .handles
            .lock()
            .expect("deploy task registry lock is not poisoned");
        for handle in handles.drain(..) {
            handle.abort();
        }
    }
}

pub struct ObservationHealthChecker {
    observations: AsyncNatsObservationStore,
    poll_interval: Duration,
}

impl ObservationHealthChecker {
    #[must_use]
    pub fn new(observations: AsyncNatsObservationStore, poll_interval: Duration) -> Self {
        Self {
            observations,
            poll_interval,
        }
    }
}

impl DeployHealthChecker for ObservationHealthChecker {
    async fn wait_healthy(
        &mut self,
        containers: &[DeployContainer],
    ) -> Result<(), DeployHealthCheckError> {
        loop {
            let mut all_running = true;
            for container in containers {
                match self
                    .observations
                    .container(&container.node_id, &container.container_id)
                    .await
                {
                    Ok(Some(observation))
                        if observation.state == ContainerRuntimeState::Running => {}
                    Ok(Some(_)) => return Err(unhealthy_container(container, "container exited")),
                    Ok(None) => all_running = false,
                    Err(error) => {
                        return Err(unhealthy_container(container, health_read_error(error)));
                    }
                }
            }

            if all_running {
                return Ok(());
            }

            tokio::time::sleep(self.poll_interval).await;
        }
    }
}

fn unhealthy_container(
    container: &DeployContainer,
    message: impl Into<String>,
) -> DeployHealthCheckError {
    let message = FailureMessage::try_new(message).expect("health failure message is non-empty");
    let log_hint = OperatorHint::try_new(format!("ployz logs {}", container.container_id.as_str()))
        .expect("generated log hint is non-empty");
    DeployHealthCheckError::Unhealthy {
        node_id: container.node_id.clone(),
        container_id: container.container_id.clone(),
        message,
        log_hint,
    }
}

fn health_read_error(error: ObservationStoreError) -> String {
    format!("container observation could not be read: {error}")
}
