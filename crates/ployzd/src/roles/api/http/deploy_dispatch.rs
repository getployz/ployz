//! Machine-addressed execution for a deploy's Docker phases.
//!
//! The dispatch routes each verb to its target: the local machine goes
//! straight to the local runner (never self-HTTP), and a remote machine gets
//! a bounded `/deploy/execute` POST carrying the claim-scoped verb. Remote
//! `ClaimNotYetVisible` replies are replication lag and retry within one
//! bounded budget; authorization refusals are terminal for the phase.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use async_trait::async_trait;
use hyper::StatusCode;
use ployz_core::corrosion::{HostPortBindings, MachineTransport, V2ManagedContainerIdentity};
use ployz_core::deploy::ImageReference;
use ployz_core::ids::{ContainerId, MachineRowId, NamespaceRowId, OperationRowId, ServiceRowId};
use ployz_core::{
    DeployExecuteOutcome, DeployExecuteRequest, DeployRequest, DeployVerb, HealthGatePolicy,
    ServiceContainerObservation,
};
use tokio::sync::watch;

use super::deploy_runtime::DeployRuntime;
use super::promotion_store::ResolvedNamespace;
use crate::roles::api::runner::ExistingV2ManagedContainer;
use std::sync::Arc;

/// Per-POST budget for a non-pull verb: the responder's 60s effect budget
/// plus its 15s claim-visibility wait plus transport headroom.
pub(super) const REMOTE_EFFECT_TIMEOUT: Duration = Duration::from_secs(85);
/// Per-POST budget for the pull verb: the responder's 180s pull budget plus
/// its claim wait plus transport headroom.
pub(super) const REMOTE_PULL_TIMEOUT: Duration = Duration::from_secs(210);
/// How many `ClaimNotYetVisible` replies one verb tolerates before failing.
/// The responder already waits up to 15s per call, so four attempts cover
/// one minute of replication lag.
pub(super) const CLAIM_RETRY_ATTEMPTS: usize = 4;
/// Wall-clock ceiling on the `ClaimNotYetVisible` retry loop.
pub(super) const CLAIM_RETRY_WINDOW: Duration = Duration::from_secs(60);
const CLAIM_RETRY_PAUSE: Duration = Duration::from_secs(1);

/// Outer shutdown-select budget for one dispatched non-pull verb: the remote
/// POST budget plus the whole claim-retry window.
pub(super) const DISPATCH_EFFECT_BUDGET: Duration = Duration::from_secs(85 + 60 + 5);
/// Outer shutdown-select budget for one dispatched pull verb.
pub(super) const DISPATCH_PULL_BUDGET: Duration = Duration::from_secs(210 + 60 + 5);

/// Builds the bounded mesh HTTP client the driver uses for bids and verbs:
/// connect-limited, redirect-free, proxy-free. Per-request budgets are set
/// per verb.
pub(super) fn mesh_http_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
}

/// The mesh address a machine's API listens on, on the shared API port.
pub(super) fn machine_socket_addr(transport: &MachineTransport, api_port: u16) -> SocketAddr {
    let ip = match transport {
        MachineTransport::Wireguard { addr_v6, .. } => IpAddr::V6(*addr_v6),
        MachineTransport::Tailscale { ip, .. } => IpAddr::V4(*ip),
    };
    SocketAddr::new(ip, api_port)
}

/// One bounded `/deploy/execute` POST. The real implementation is a reqwest
/// client; tests fake the seam. Every execute reply — refusals included —
/// arrives as HTTP 200 with a typed body, so a non-200 status is a transport
/// or version failure, never an outcome.
#[async_trait]
pub(super) trait DeployVerbClient: Send + Sync {
    async fn execute(
        &self,
        target: SocketAddr,
        request: &DeployExecuteRequest,
        budget: Duration,
    ) -> Result<DeployExecuteOutcome, String>;
}

pub(super) struct MeshVerbClient {
    client: reqwest::Client,
}

impl MeshVerbClient {
    #[must_use]
    pub(super) fn new(client: reqwest::Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl DeployVerbClient for MeshVerbClient {
    async fn execute(
        &self,
        target: SocketAddr,
        request: &DeployExecuteRequest,
        budget: Duration,
    ) -> Result<DeployExecuteOutcome, String> {
        let url = format!(
            "http://{target}{}",
            ployz_core::V2Route::DeployExecute.path()
        );
        let response = self
            .client
            .post(url)
            .timeout(budget)
            .json(request)
            .send()
            .await
            .map_err(|error| format!("deploy verb transport failed: {error}"))?;
        let status = response.status();
        if status != StatusCode::OK.as_u16() {
            return Err(format!("deploy verb answered status {status}"));
        }
        response
            .json::<DeployExecuteOutcome>()
            .await
            .map_err(|error| format!("deploy verb reply was invalid: {error}"))
    }
}

/// The operation and service identity every dispatched verb binds, so the
/// responder can authorize it against the live claim on its own replica.
#[derive(Debug, Clone)]
pub(super) struct VerbScope {
    pub(super) operation_id: OperationRowId,
    pub(super) namespace_id: NamespaceRowId,
    pub(super) service_id: ServiceRowId,
}

/// One deploy's machine-addressed runtime: the claim scope every verb binds,
/// the target address book from the gather's roster, and the two execution
/// arms.
pub(super) struct TargetDispatch {
    local_machine: MachineRowId,
    local: Arc<dyn DeployRuntime>,
    local_effect_timeout: Duration,
    verbs: Arc<dyn DeployVerbClient>,
    addresses: BTreeMap<MachineRowId, SocketAddr>,
    scope: VerbScope,
}

impl TargetDispatch {
    #[must_use]
    pub(super) fn new(
        local_machine: MachineRowId,
        local: Arc<dyn DeployRuntime>,
        local_effect_timeout: Duration,
        verbs: Arc<dyn DeployVerbClient>,
        addresses: BTreeMap<MachineRowId, SocketAddr>,
        scope: VerbScope,
    ) -> Self {
        Self {
            local_machine,
            local,
            local_effect_timeout,
            verbs,
            addresses,
            scope,
        }
    }

    pub(super) fn is_local(&self, machine: &MachineRowId) -> bool {
        machine == &self.local_machine
    }

    pub(super) async fn service_containers(
        &self,
        machine: &MachineRowId,
    ) -> Result<Vec<ServiceContainerObservation>, String> {
        if self.is_local(machine) {
            let containers = self
                .bounded_local(
                    self.local
                        .namespace_docker_containers(&self.scope.namespace_id),
                )
                .await??;
            return Ok(containers.into_iter().map(observation_from).collect());
        }
        let outcome = self
            .execute_remote(machine, DeployVerb::ListServiceContainers)
            .await?;
        let DeployExecuteOutcome::ServiceContainers { containers } = outcome else {
            return Err(unexpected_reply("list", &outcome));
        };
        Ok(containers)
    }

    pub(super) async fn pull_image(
        &self,
        machine: &MachineRowId,
        image: &ImageReference,
        shutdown: watch::Receiver<bool>,
    ) -> Result<(), String> {
        if self.is_local(machine) {
            return self
                .bounded_local_with(
                    REMOTE_PULL_TIMEOUT,
                    self.local.pull_image(image, None, shutdown),
                )
                .await?;
        }
        // Registry credentials would forward inside the verb here; the deploy
        // request carries none until the registry facade lands.
        let outcome = self
            .execute_remote(
                machine,
                DeployVerb::PullImage {
                    image: image.clone(),
                    credential: None,
                },
            )
            .await?;
        let DeployExecuteOutcome::ImagePulled = outcome else {
            return Err(unexpected_reply("pull", &outcome));
        };
        Ok(())
    }

    pub(super) async fn create_container(
        &self,
        machine: &MachineRowId,
        request: &DeployRequest,
        image: &ImageReference,
        namespace: &ResolvedNamespace,
        identity: V2ManagedContainerIdentity,
        host_ports: &HostPortBindings,
    ) -> Result<ContainerId, String> {
        if self.is_local(machine) {
            return self
                .bounded_local(
                    self.local
                        .create_container(request, image, namespace, identity, host_ports),
                )
                .await?;
        }
        let outcome = self
            .execute_remote(
                machine,
                DeployVerb::CreateContainer {
                    image: image.clone(),
                    runtime: Box::new(request.runtime.clone()),
                    namespace_name: namespace.document.name.clone(),
                    host_ports: host_ports.clone(),
                },
            )
            .await?;
        let DeployExecuteOutcome::ContainerCreated { container_id } = outcome else {
            return Err(unexpected_reply("create", &outcome));
        };
        Ok(container_id)
    }

    /// Starts a created container, or restarts a stopped incumbent after a
    /// failed cutover gate. Both arms scope-check the container against the
    /// verb's service before starting it.
    pub(super) async fn start_container(
        &self,
        machine: &MachineRowId,
        container_id: &ContainerId,
    ) -> Result<(), String> {
        if self.is_local(machine) {
            return self
                .bounded_local(async {
                    let containers = self
                        .local
                        .namespace_docker_containers(&self.scope.namespace_id)
                        .await?;
                    if !containers.iter().any(|container| {
                        &container.container_id == container_id
                            && container.identity.service_id == self.scope.service_id
                    }) {
                        return Err("container to start was not found in Docker".to_owned());
                    }
                    self.local.start_container(container_id).await
                })
                .await?;
        }
        let outcome = self
            .execute_remote(
                machine,
                DeployVerb::StartContainer {
                    container_id: container_id.clone(),
                },
            )
            .await?;
        let DeployExecuteOutcome::ContainerStarted = outcome else {
            return Err(unexpected_reply("start", &outcome));
        };
        Ok(())
    }

    pub(super) async fn stop_container(
        &self,
        machine: &MachineRowId,
        container_id: &ContainerId,
        expected_identity: &V2ManagedContainerIdentity,
    ) -> Result<(), String> {
        if self.is_local(machine) {
            return self
                .bounded_local(self.local.stop_container(container_id, expected_identity))
                .await?
                .map(|_| ());
        }
        let outcome = self
            .execute_remote(
                machine,
                DeployVerb::StopContainer {
                    container_id: container_id.clone(),
                    expected: expected_identity.clone(),
                },
            )
            .await?;
        let DeployExecuteOutcome::ContainerStopped = outcome else {
            return Err(unexpected_reply("stop", &outcome));
        };
        Ok(())
    }

    pub(super) async fn remove_container(
        &self,
        machine: &MachineRowId,
        container_id: &ContainerId,
        expected_identity: &V2ManagedContainerIdentity,
    ) -> Result<(), String> {
        if self.is_local(machine) {
            return self
                .bounded_local(self.local.remove_container(container_id, expected_identity))
                .await?;
        }
        let outcome = self
            .execute_remote(
                machine,
                DeployVerb::RemoveContainer {
                    container_id: container_id.clone(),
                    expected: expected_identity.clone(),
                },
            )
            .await?;
        let DeployExecuteOutcome::ContainerRemoved = outcome else {
            return Err(unexpected_reply("remove", &outcome));
        };
        Ok(())
    }

    pub(super) async fn health_gate(
        &self,
        machine: &MachineRowId,
        container_id: &ContainerId,
        identity: &V2ManagedContainerIdentity,
        policy: HealthGatePolicy,
    ) -> Result<Ipv4Addr, String> {
        if self.is_local(machine) {
            return match policy {
                HealthGatePolicy::Enforce => {
                    self.bounded_local(self.local.health_gate(container_id, identity))
                        .await?
                }
                HealthGatePolicy::Skip => {
                    self.bounded_local(self.local.container_ip(container_id, identity))
                        .await?
                }
            };
        }
        let outcome = self
            .execute_remote(
                machine,
                DeployVerb::HealthGate {
                    container_id: container_id.clone(),
                    policy,
                },
            )
            .await?;
        let DeployExecuteOutcome::HealthGated { ip } = outcome else {
            return Err(unexpected_reply("health gate", &outcome));
        };
        Ok(ip)
    }

    /// Bounds a local runner effect with the driver's local effect budget, so
    /// a local target keeps the pre-placement timeout behavior.
    async fn bounded_local<T>(
        &self,
        effect: impl std::future::Future<Output = T> + Send,
    ) -> Result<T, String> {
        self.bounded_local_with(self.local_effect_timeout, effect)
            .await
    }

    async fn bounded_local_with<T>(
        &self,
        budget: Duration,
        effect: impl std::future::Future<Output = T> + Send,
    ) -> Result<T, String> {
        tokio::time::timeout(budget, effect)
            .await
            .map_err(|_| "local deploy effect timed out".to_owned())
    }

    async fn execute_remote(
        &self,
        machine: &MachineRowId,
        verb: DeployVerb,
    ) -> Result<DeployExecuteOutcome, String> {
        let Some(target) = self.addresses.get(machine) else {
            return Err(format!(
                "machine {machine} has no mesh address in the roster"
            ));
        };
        let budget = match &verb {
            DeployVerb::PullImage { .. } => REMOTE_PULL_TIMEOUT,
            DeployVerb::ListServiceContainers
            | DeployVerb::CreateContainer { .. }
            | DeployVerb::StartContainer { .. }
            | DeployVerb::StopContainer { .. }
            | DeployVerb::HealthGate { .. }
            | DeployVerb::RemoveContainer { .. } => REMOTE_EFFECT_TIMEOUT,
        };
        let request = DeployExecuteRequest {
            operation_id: self.scope.operation_id.clone(),
            namespace_id: self.scope.namespace_id.clone(),
            service_id: self.scope.service_id.clone(),
            verb,
        };
        let deadline = tokio::time::Instant::now() + CLAIM_RETRY_WINDOW;
        let mut attempts = 0;
        loop {
            attempts += 1;
            let outcome = self.verbs.execute(*target, &request, budget).await?;
            match outcome {
                DeployExecuteOutcome::ClaimNotYetVisible => {
                    if attempts >= CLAIM_RETRY_ATTEMPTS || tokio::time::Instant::now() >= deadline {
                        return Err(format!(
                            "deploy claim was not visible on machine {machine} after {attempts} attempts"
                        ));
                    }
                    tokio::time::sleep(CLAIM_RETRY_PAUSE).await;
                }
                DeployExecuteOutcome::CallerNotDriver { driver } => {
                    return Err(format!(
                        "machine {machine} refused the verb: this machine is not the operation's driver ({driver} is)"
                    ));
                }
                DeployExecuteOutcome::ClaimTerminal => {
                    return Err(format!(
                        "machine {machine} refused the verb: the operation claim is already terminal"
                    ));
                }
                DeployExecuteOutcome::ServiceMismatch => {
                    return Err(format!(
                        "machine {machine} refused the verb: it does not match the operation's service"
                    ));
                }
                DeployExecuteOutcome::ContainerIdentityMismatch { actual } => {
                    return Err(format!(
                        "machine {machine} refused the verb: the live container belongs to operation {}",
                        actual.operation_id
                    ));
                }
                DeployExecuteOutcome::Failed { diagnostic } => {
                    return Err(format!("machine {machine}: {diagnostic}"));
                }
                DeployExecuteOutcome::ServiceContainers { .. }
                | DeployExecuteOutcome::ImagePulled
                | DeployExecuteOutcome::ContainerCreated { .. }
                | DeployExecuteOutcome::ContainerStarted
                | DeployExecuteOutcome::ContainerStopped
                | DeployExecuteOutcome::HealthGated { .. }
                | DeployExecuteOutcome::ContainerRemoved => return Ok(outcome),
            }
        }
    }
}

fn observation_from(container: ExistingV2ManagedContainer) -> ServiceContainerObservation {
    ServiceContainerObservation {
        container_id: container.container_id,
        service_id: container.identity.service_id,
        deploy: container.identity.operation_id,
        named_volumes: container.named_volume_names,
    }
}

fn unexpected_reply(verb: &str, outcome: &DeployExecuteOutcome) -> String {
    format!("deploy {verb} verb answered a mismatched reply: {outcome:?}")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;
    use ployz_core::corrosion::V2ManagedContainerIdentity;
    use ployz_core::deploy::ImageReference;
    use ployz_core::ids::{
        ContainerId, MachineRowId, NamespaceRowId, OperationRowId, ServiceRowId,
    };
    use ployz_core::{DeployExecuteOutcome, DeployExecuteRequest};
    use tokio::sync::{Mutex, watch};

    use super::super::deploy_runtime::DeployRuntime;
    use super::{
        CLAIM_RETRY_ATTEMPTS, CLAIM_RETRY_WINDOW, DISPATCH_EFFECT_BUDGET, DISPATCH_PULL_BUDGET,
        DeployVerbClient, REMOTE_EFFECT_TIMEOUT, REMOTE_PULL_TIMEOUT, TargetDispatch, VerbScope,
    };
    use crate::roles::api::runner::{ExistingV2ManagedContainer, MachineContainerStopOutcome};

    struct IdleRuntime {
        starts: AtomicUsize,
    }

    #[async_trait]
    impl DeployRuntime for IdleRuntime {
        async fn bridge_ready(&self) -> bool {
            true
        }

        async fn resolve_image(&self, image: &ImageReference) -> Result<ImageReference, String> {
            Ok(image.clone())
        }

        async fn pull_image(
            &self,
            _image: &ImageReference,
            _credential: Option<&ployz_core::deploy::RegistryCredential>,
            _shutdown: watch::Receiver<bool>,
        ) -> Result<(), String> {
            Ok(())
        }

        async fn create_container_command(
            &self,
            _command: crate::roles::api::runner::CreateV2ManagedContainer,
        ) -> Result<ContainerId, String> {
            ContainerId::try_new("local-container").map_err(|error| error.to_string())
        }

        async fn start_container(&self, _container_id: &ContainerId) -> Result<(), String> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn health_gate(
            &self,
            _container_id: &ContainerId,
            _identity: &V2ManagedContainerIdentity,
        ) -> Result<Ipv4Addr, String> {
            Ok(Ipv4Addr::new(10, 210, 20, 2))
        }

        async fn container_ip(
            &self,
            _container_id: &ContainerId,
            _identity: &V2ManagedContainerIdentity,
        ) -> Result<Ipv4Addr, String> {
            Ok(Ipv4Addr::new(10, 210, 20, 2))
        }

        async fn managed_containers(&self) -> Result<Vec<ExistingV2ManagedContainer>, String> {
            Ok(vec![ExistingV2ManagedContainer {
                container_id: container(),
                identity: V2ManagedContainerIdentity {
                    namespace_id: NamespaceRowId::try_new("01J00000000000000000000013")
                        .expect("namespace"),
                    service_id: ServiceRowId::try_new("01J00000000000000000000014")
                        .expect("service"),
                    operation_id: OperationRowId::try_new("01J00000000000000000000011")
                        .expect("operation"),
                },
                state: crate::roles::api::runner::ExistingManagedContainerState::StartableStopped,
                health_status: None,
                resolved_image_identity: None,
                created_at_unix_seconds: None,
                named_volume_names: std::collections::BTreeSet::new(),
            }])
        }

        async fn held_volumes(
            &self,
            _namespace_id: &NamespaceRowId,
            _volumes: &std::collections::BTreeSet<ployz_core::deploy::VolumeName>,
        ) -> Result<std::collections::BTreeSet<ployz_core::deploy::VolumeName>, String> {
            Ok(std::collections::BTreeSet::new())
        }

        async fn stop_container(
            &self,
            _container_id: &ContainerId,
            _expected_identity: &V2ManagedContainerIdentity,
        ) -> Result<MachineContainerStopOutcome, String> {
            Ok(MachineContainerStopOutcome::StoppedRunning)
        }

        async fn remove_container(
            &self,
            _container_id: &ContainerId,
            _expected_identity: &V2ManagedContainerIdentity,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    struct ScriptedClient {
        replies: Mutex<Vec<DeployExecuteOutcome>>,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl DeployVerbClient for ScriptedClient {
        async fn execute(
            &self,
            _target: SocketAddr,
            _request: &DeployExecuteRequest,
            _budget: Duration,
        ) -> Result<DeployExecuteOutcome, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut replies = self.replies.lock().await;
            if replies.is_empty() {
                return Ok(DeployExecuteOutcome::ClaimNotYetVisible);
            }
            Ok(replies.remove(0))
        }
    }

    fn local_machine() -> MachineRowId {
        MachineRowId::try_new("01J00000000000000000000012").expect("machine")
    }

    fn remote_machine() -> MachineRowId {
        MachineRowId::try_new("01J00000000000000000000022").expect("machine")
    }

    fn dispatch(client: Arc<ScriptedClient>, runtime: Arc<IdleRuntime>) -> TargetDispatch {
        let mut addresses = BTreeMap::new();
        addresses.insert(
            remote_machine(),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 2)), 4480),
        );
        TargetDispatch::new(
            local_machine(),
            runtime,
            Duration::from_secs(60),
            client,
            addresses,
            VerbScope {
                operation_id: OperationRowId::try_new("01J00000000000000000000011")
                    .expect("operation"),
                namespace_id: NamespaceRowId::try_new("01J00000000000000000000013")
                    .expect("namespace"),
                service_id: ServiceRowId::try_new("01J00000000000000000000014").expect("service"),
            },
        )
    }

    fn scripted(replies: Vec<DeployExecuteOutcome>) -> Arc<ScriptedClient> {
        Arc::new(ScriptedClient {
            replies: Mutex::new(replies),
            calls: AtomicUsize::new(0),
        })
    }

    fn container() -> ContainerId {
        ContainerId::try_new("c0ffee").expect("container")
    }

    #[test]
    fn dispatch_budgets_cover_the_responder_and_the_claim_retry_window() {
        assert!(DISPATCH_EFFECT_BUDGET > REMOTE_EFFECT_TIMEOUT + CLAIM_RETRY_WINDOW);
        assert!(DISPATCH_PULL_BUDGET > REMOTE_PULL_TIMEOUT + CLAIM_RETRY_WINDOW);
        const {
            assert!(CLAIM_RETRY_ATTEMPTS >= 2);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn claim_not_yet_visible_retries_within_the_budget_then_succeeds() {
        let client = scripted(vec![
            DeployExecuteOutcome::ClaimNotYetVisible,
            DeployExecuteOutcome::ClaimNotYetVisible,
            DeployExecuteOutcome::ContainerStarted,
        ]);
        let runtime = Arc::new(IdleRuntime {
            starts: AtomicUsize::new(0),
        });
        let dispatch = dispatch(Arc::clone(&client), runtime);
        dispatch
            .start_container(&remote_machine(), &container())
            .await
            .expect("retried start succeeds");
        assert_eq!(client.calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn claim_not_yet_visible_exhausts_its_bounded_attempts() {
        let client = scripted(Vec::new());
        let runtime = Arc::new(IdleRuntime {
            starts: AtomicUsize::new(0),
        });
        let dispatch = dispatch(Arc::clone(&client), runtime);
        let error = dispatch
            .start_container(&remote_machine(), &container())
            .await
            .expect_err("exhausted retries fail");
        assert!(error.contains("not visible"));
        assert_eq!(client.calls.load(Ordering::SeqCst), CLAIM_RETRY_ATTEMPTS);
    }

    #[tokio::test]
    async fn authorization_refusals_are_terminal_and_never_retried() {
        for (refusal, needle) in [
            (
                DeployExecuteOutcome::CallerNotDriver {
                    driver: remote_machine(),
                },
                "not the operation's driver",
            ),
            (DeployExecuteOutcome::ClaimTerminal, "already terminal"),
            (
                DeployExecuteOutcome::ServiceMismatch,
                "does not match the operation's service",
            ),
        ] {
            let client = scripted(vec![refusal]);
            let runtime = Arc::new(IdleRuntime {
                starts: AtomicUsize::new(0),
            });
            let dispatch = dispatch(Arc::clone(&client), runtime);
            let error = dispatch
                .start_container(&remote_machine(), &container())
                .await
                .expect_err("refused verb fails");
            assert!(error.contains(needle), "{error}");
            assert_eq!(client.calls.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn local_targets_never_touch_the_mesh_client() {
        let client = scripted(Vec::new());
        let runtime = Arc::new(IdleRuntime {
            starts: AtomicUsize::new(0),
        });
        let dispatch = dispatch(Arc::clone(&client), Arc::clone(&runtime));
        dispatch
            .start_container(&local_machine(), &container())
            .await
            .expect("local start succeeds");
        assert_eq!(client.calls.load(Ordering::SeqCst), 0);
        assert_eq!(runtime.starts.load(Ordering::SeqCst), 1);
    }
}
