//! Fake enroll HTTP, Relay, and Machine RPC for membership join tests.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use ployz_core::{
    AdvertisedEndpoint, CloudPairingSet, ContainerChanged, ContainerCreated, ContainerDetails,
    ContainerId, ContainerKind, ContainerList, ContainerObservation, ContainerRuntimeObservation,
    ContractDescription, CreateDomainRecordsRequest, DESCRIBE_CONTRACT_CAPABILITY, Domain,
    DomainRecords, EnsureGlobalSlotRequest, HealthObservation, InitializeRequest, Initialized,
    JoinAccepted, JoinRequest, LocalMachinePhase, Machine, MachineDetails, MachineId,
    MachineImages, MachineList, MachineName, MachineObservation, MachineRpc, MachineToken,
    ManagementAddress, MembershipObservation, OpaquePayload, PROTOCOL_MAJOR, Registered,
    ReserveDomainRequest, ResetAccepted, RpcError, RpcErrorCode, RpcRequestBody, RpcResponse,
    VolumeInventory, WireGuardPublicKey,
};
use tokio::task::JoinHandle;
use tonic::{Request, Response, Status, Streaming};

#[path = "enroll_http.rs"]
mod enroll_http;
#[path = "../support/inspect_telemetry.rs"]
mod inspect_telemetry_fixture;
#[path = "relay.rs"]
mod relay;
#[path = "servers.rs"]
mod servers;

pub use enroll_http::{EnrollListen, EventLog};
use relay::hold_register;
pub use relay::{RelayListen, assert_not_held, wait_for_held};
pub use servers::{serve_ingress_probe, serve_local_machine, serve_machine};

pub const TOKEN: &str = "pmet_test";
pub const PAIRING: &str = "pairing-secret";
pub const CLUSTER_DOMAIN: &str = "abcd12.ployz.dev";

fn consume_transient_failure(remaining: &AtomicUsize) -> bool {
    remaining
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
            remaining.checked_sub(1)
        })
        .is_ok()
}

#[derive(Clone)]
pub struct JoinDaemon {
    inner: Arc<JoinInner>,
}

struct JoinInner {
    registration: Registered,
    daemon_version: Mutex<String>,
    joined: AtomicBool,
    join_request: Mutex<Option<JoinRequest>>,
    initialize_requests: Mutex<Vec<InitializeRequest>>,
    reserve_request: Mutex<Option<ReserveDomainRequest>>,
    reserve_attempts: AtomicUsize,
    transient_reserve_failures: AtomicUsize,
    domain_reserved: AtomicBool,
    domain_record_requests: Mutex<Vec<CreateDomainRecordsRequest>>,
    domain_record_attempts: AtomicUsize,
    transient_domain_record_failures: AtomicUsize,
    cloud_paired: AtomicBool,
    cloud_pairing_attempts: AtomicUsize,
    transient_cloud_pairing_failures: AtomicUsize,
    events: Mutex<EventLog>,
    resets: AtomicUsize,
    containers: Mutex<Vec<ContainerObservation>>,
    create_attempts: AtomicUsize,
    transient_create_failures: AtomicUsize,
    ensure_requests: Mutex<Vec<EnsureGlobalSlotRequest>>,
    ensure_attempts: AtomicUsize,
    transient_ensure_failures: AtomicUsize,
    target_inspect_attempts: AtomicUsize,
    transient_target_inspect_failures: AtomicUsize,
    fail_ensure: AtomicBool,
    fail_list_on: Mutex<Option<MachineId>>,
    assigned_membership: Mutex<MembershipObservation>,
    _register: Mutex<Option<JoinHandle<()>>>,
}

impl JoinDaemon {
    pub fn new(registration: Registered) -> Self {
        Self {
            inner: Arc::new(JoinInner {
                registration,
                daemon_version: Mutex::new(env!("CARGO_PKG_VERSION").into()),
                joined: AtomicBool::new(false),
                join_request: Mutex::new(None),
                initialize_requests: Mutex::new(Vec::new()),
                reserve_request: Mutex::new(None),
                reserve_attempts: AtomicUsize::new(0),
                transient_reserve_failures: AtomicUsize::new(0),
                domain_reserved: AtomicBool::new(false),
                domain_record_requests: Mutex::new(Vec::new()),
                domain_record_attempts: AtomicUsize::new(0),
                transient_domain_record_failures: AtomicUsize::new(0),
                cloud_paired: AtomicBool::new(false),
                cloud_pairing_attempts: AtomicUsize::new(0),
                transient_cloud_pairing_failures: AtomicUsize::new(0),
                events: Mutex::new(EventLog::default()),
                resets: AtomicUsize::new(0),
                containers: Mutex::new(Vec::new()),
                create_attempts: AtomicUsize::new(0),
                transient_create_failures: AtomicUsize::new(0),
                ensure_requests: Mutex::new(Vec::new()),
                ensure_attempts: AtomicUsize::new(0),
                transient_ensure_failures: AtomicUsize::new(0),
                target_inspect_attempts: AtomicUsize::new(0),
                transient_target_inspect_failures: AtomicUsize::new(0),
                fail_ensure: AtomicBool::new(false),
                fail_list_on: Mutex::new(None),
                assigned_membership: Mutex::new(MembershipObservation::Up),
                _register: Mutex::new(None),
            }),
        }
    }

    pub fn join_request(&self) -> JoinRequest {
        self.inner
            .join_request
            .lock()
            .unwrap()
            .clone()
            .expect("Join was called")
    }

    pub fn set_daemon_version(&self, version: &str) {
        *self.inner.daemon_version.lock().unwrap() = version.into();
    }

    pub fn initialize_request(&self) -> InitializeRequest {
        self.initialize_requests()
            .last()
            .cloned()
            .expect("Initialize was called")
    }

    pub fn initialize_requests(&self) -> Vec<InitializeRequest> {
        self.inner.initialize_requests.lock().unwrap().clone()
    }

    pub fn reset_count(&self) -> usize {
        self.inner.resets.load(Ordering::SeqCst)
    }

    pub fn reserve_request(&self) -> Option<ReserveDomainRequest> {
        self.inner.reserve_request.lock().unwrap().clone()
    }

    pub fn domain_record_requests(&self) -> Vec<CreateDomainRecordsRequest> {
        self.inner.domain_record_requests.lock().unwrap().clone()
    }

    pub fn founder_tail_attempts(&self) -> [usize; 4] {
        [
            self.inner.reserve_attempts.load(Ordering::SeqCst),
            self.inner.create_attempts.load(Ordering::SeqCst),
            self.inner.domain_record_attempts.load(Ordering::SeqCst),
            self.inner.cloud_pairing_attempts.load(Ordering::SeqCst),
        ]
    }

    pub fn with_containers(self, containers: Vec<ContainerObservation>) -> Self {
        *self.inner.containers.lock().unwrap() = containers;
        self
    }

    pub fn containers(&self) -> Vec<ContainerObservation> {
        self.inner.containers.lock().unwrap().clone()
    }

    pub fn with_reserved_domain(self) -> Self {
        self.inner.domain_reserved.store(true, Ordering::SeqCst);
        self
    }

    pub fn with_events(self, events: EventLog) -> Self {
        *self.inner.events.lock().unwrap() = events;
        self
    }

    pub fn transient_founder_tail_failures(self, failures: usize) -> Self {
        self.inner
            .transient_reserve_failures
            .store(failures, Ordering::SeqCst);
        self.inner
            .transient_create_failures
            .store(failures, Ordering::SeqCst);
        self.inner
            .transient_domain_record_failures
            .store(failures, Ordering::SeqCst);
        self.inner
            .transient_cloud_pairing_failures
            .store(failures, Ordering::SeqCst);
        self
    }

    fn record(&self, event: &'static str) {
        self.inner.events.lock().unwrap().record(event);
    }

    pub fn fail_ensure(self) -> Self {
        self.inner.fail_ensure.store(true, Ordering::SeqCst);
        self
    }

    pub fn transient_ensure_failures(self, failures: usize) -> Self {
        self.inner
            .transient_ensure_failures
            .store(failures, Ordering::SeqCst);
        self
    }

    /// Make this many targeted Inspect attempts transiently unavailable.
    pub fn transient_target_inspect_failures(self, failures: usize) -> Self {
        self.inner
            .transient_target_inspect_failures
            .store(failures, Ordering::SeqCst);
        self
    }

    pub fn fail_list_on(self, machine_id: MachineId) -> Self {
        *self.inner.fail_list_on.lock().unwrap() = Some(machine_id);
        self
    }

    /// Set the entry Machine's membership observation for the assigned Machine.
    pub fn with_membership(self, membership: MembershipObservation) -> Self {
        *self.inner.assigned_membership.lock().unwrap() = membership;
        self
    }

    pub fn ensure_requests(&self) -> Vec<EnsureGlobalSlotRequest> {
        self.inner.ensure_requests.lock().unwrap().clone()
    }

    pub fn ensure_attempts(&self) -> usize {
        self.inner.ensure_attempts.load(Ordering::SeqCst)
    }

    /// Count targeted Inspect attempts received by this fake daemon.
    pub fn target_inspect_attempts(&self) -> usize {
        self.inner.target_inspect_attempts.load(Ordering::SeqCst)
    }
}

#[expect(
    clippy::result_large_err,
    reason = "MachineRpc uses tonic::Status as the error type"
)]
fn rpc_ok(payload: impl Into<RpcResponse>) -> Result<Response<OpaquePayload>, Status> {
    Ok(Response::new(payload.into().encode().unwrap()))
}

#[expect(
    clippy::result_large_err,
    reason = "MachineRpc uses tonic::Status as the error type"
)]
fn unused<T>() -> Result<T, Status> {
    Err(Status::unimplemented("unused"))
}

#[tonic::async_trait]
impl MachineRpc for JoinDaemon {
    type ExecStream = tokio_stream::Empty<Result<OpaquePayload, Status>>;
    type ContainerLogsStream = tokio_stream::Empty<Result<OpaquePayload, Status>>;
    type MachineLogsStream = tokio_stream::Empty<Result<OpaquePayload, Status>>;
    type RuntimeWatchStream = tokio_stream::Empty<Result<OpaquePayload, Status>>;

    async fn describe_contract(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        rpc_ok(ContractDescription {
            machine_id: self.inner.registration.assigned_machine.id,
            protocol_major: PROTOCOL_MAJOR,
            daemon_version: self.inner.daemon_version.lock().unwrap().clone(),
            capabilities: [
                ployz_core::CapabilityName::parse(DESCRIBE_CONTRACT_CAPABILITY)
                    .expect("catalogued capability names are valid"),
            ]
            .into(),
        })
    }

    async fn inspect(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        if request
            .metadata()
            .contains_key(ployz_core::ONE_TARGET_HEADER)
        {
            self.inner
                .target_inspect_attempts
                .fetch_add(1, Ordering::SeqCst);
            if consume_transient_failure(&self.inner.transient_target_inspect_failures) {
                return Err(Status::unavailable("target Machine is not ready"));
            }
        }
        let request = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let RpcRequestBody::Inspect(inspect) = request.body else {
            return Err(Status::invalid_argument("expected Inspect"));
        };
        let joined = self.inner.joined.load(Ordering::SeqCst);
        let telemetry = inspect_telemetry_fixture::observation(inspect.telemetry);
        let ingress_proxy_backend = self
            .inner
            .initialize_requests
            .lock()
            .unwrap()
            .last()
            .map(|request| request.ingress_proxy_backend);
        rpc_ok(MachineDetails {
            id: self.inner.registration.assigned_machine.id,
            phase: if joined {
                LocalMachinePhase::Participating
            } else {
                LocalMachinePhase::Uninitialized
            },
            machine: joined.then(|| self.inner.registration.assigned_machine.clone()),
            public_key: self.inner.registration.assigned_machine.public_key,
            advertised_endpoints: self
                .inner
                .registration
                .assigned_machine
                .advertised_endpoints
                .clone(),
            store_version: Default::default(),
            rtts: Vec::new(),
            cloud_paired: self.inner.cloud_paired.load(Ordering::SeqCst),
            telemetry,
            storage: None,
            ingress_proxy_backend,
        })
    }

    async fn machine_token(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        rpc_ok(MachineToken {
            public_key: self.inner.registration.assigned_machine.public_key,
            public_ip: None,
            advertised_endpoints: self
                .inner
                .registration
                .assigned_machine
                .advertised_endpoints
                .clone(),
            runtime: Default::default(),
            memory_total_bytes: None,
            disk_total_bytes: None,
            disk_available_bytes: None,
        })
    }

    async fn join(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let decoded = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let RpcRequestBody::Join(join) = decoded.body else {
            return Err(Status::invalid_argument("expected Join"));
        };
        if let Some(pairing) = join.cloud_pairing.clone() {
            hold_register(
                pairing.relay_url(),
                pairing.secret(),
                &join.registration.assigned_machine.id,
                &self.inner._register,
            )
            .await?;
        }
        *self.inner.join_request.lock().unwrap() = Some(join);
        self.inner.joined.store(true, Ordering::SeqCst);
        rpc_ok(JoinAccepted {})
    }

    async fn set_cloud_pairing(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.inner
            .cloud_pairing_attempts
            .fetch_add(1, Ordering::SeqCst);
        if consume_transient_failure(&self.inner.transient_cloud_pairing_failures) {
            return Err(Status::unavailable("transient Cloud Pairing failure"));
        }
        let decoded = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let RpcRequestBody::SetCloudPairing(set) = decoded.body else {
            return Err(Status::invalid_argument("expected SetCloudPairing"));
        };
        match set.cloud_pairing {
            Some(pairing) => {
                hold_register(
                    pairing.relay_url(),
                    pairing.secret(),
                    &self.inner.registration.assigned_machine.id,
                    &self.inner._register,
                )
                .await?;
                self.inner.cloud_paired.store(true, Ordering::SeqCst);
                self.record("set_cloud_pairing");
            }
            None => {
                if let Some(old) = self.inner._register.lock().unwrap().take() {
                    old.abort();
                }
                self.inner.cloud_paired.store(false, Ordering::SeqCst);
            }
        }
        rpc_ok(CloudPairingSet {})
    }

    async fn initialize(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let decoded = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let RpcRequestBody::Initialize(init) = decoded.body else {
            return Err(Status::invalid_argument("expected Initialize"));
        };
        let pairing = init.cloud_pairing.clone();
        let mut machine = self.inner.registration.assigned_machine.clone();
        machine.name = init.name.clone();
        self.inner.initialize_requests.lock().unwrap().push(init);
        self.record("initialize");
        self.inner.joined.store(true, Ordering::SeqCst);
        if let Some(pairing) = pairing
            && let Err(status) = hold_register(
                pairing.relay_url(),
                pairing.secret(),
                &machine.id,
                &self.inner._register,
            )
            .await
        {
            if status.code() == tonic::Code::Unauthenticated {
                return rpc_ok(RpcError {
                    code: RpcErrorCode::Unauthenticated,
                    message: status.message().to_owned(),
                    details: serde_json::Value::Null,
                });
            }
            return Err(status);
        }
        rpc_ok(Initialized { machine })
    }
    async fn register(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        rpc_ok(self.inner.registration.clone())
    }
    async fn list_machines(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let assigned = MachineObservation::new(
            self.inner.registration.assigned_machine.clone(),
            self.inner.assigned_membership.lock().unwrap().clone(),
        );
        let mut machines = vec![assigned];
        machines.extend(
            self.inner
                .registration
                .visible_peers
                .iter()
                .cloned()
                .map(up_machine),
        );
        rpc_ok(MachineList { machines })
    }
    async fn list_containers(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let target = request
            .metadata()
            .get(ployz_core::ONE_TARGET_HEADER)
            .and_then(|value| value.to_str().ok());
        if self
            .inner
            .fail_list_on
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|machine_id| target == Some(machine_id.as_str()))
        {
            return rpc_ok(RpcError {
                code: RpcErrorCode::Unavailable,
                message: "unreachable".into(),
                details: serde_json::Value::Null,
            });
        }
        rpc_ok(ContainerList {
            containers: self.inner.containers.lock().unwrap().clone(),
        })
    }
    async fn create_volume(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn inspect_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let decoded = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let RpcRequestBody::InspectContainer(inspect) = decoded.body else {
            return Err(Status::invalid_argument("expected InspectContainer"));
        };
        let container = self
            .inner
            .containers
            .lock()
            .unwrap()
            .iter()
            .find(|container| container.container_id == inspect.container_id)
            .cloned()
            .ok_or_else(|| Status::not_found("container not found"))?;
        rpc_ok(ContainerDetails { container })
    }
    async fn get_container_observations(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn list_volumes(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        rpc_ok(VolumeInventory::default())
    }
    async fn inspect_volume(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn create_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.inner.create_attempts.fetch_add(1, Ordering::SeqCst);
        if consume_transient_failure(&self.inner.transient_create_failures) {
            return Err(Status::unavailable("transient Ingress deployment failure"));
        }
        let decoded = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let RpcRequestBody::CreateContainer(create) = decoded.body else {
            return Err(Status::invalid_argument("expected CreateContainer"));
        };
        let n = self.inner.containers.lock().unwrap().len() + 1;
        let container_id = ContainerId::parse(format!("{n:064x}")).unwrap();
        let display_name = format!("{}-{n}", create.resolved_spec.name);
        self.inner
            .containers
            .lock()
            .unwrap()
            .push(ContainerObservation {
                container_id,
                display_name: display_name.clone(),
                created_at_unix_nanos: n as i64,
                machine_id: self.inner.registration.assigned_machine.id,
                project_name: create.project_name,
                service_id: create.resolved_spec.service_id,
                service_name: create.resolved_spec.name.clone(),
                kind: create.kind,
                runtime: ContainerRuntimeObservation::Created,
                effective_healthcheck: None,
                resolved_spec: create.resolved_spec,
                address: None,
                labels: Default::default(),
            });
        rpc_ok(ContainerCreated {
            container_id,
            display_name,
        })
    }
    async fn ensure_global_slot(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let decoded = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let RpcRequestBody::EnsureGlobalSlot(ensure) = decoded.body else {
            return Err(Status::invalid_argument("expected EnsureGlobalSlot"));
        };
        self.inner.ensure_attempts.fetch_add(1, Ordering::SeqCst);
        if consume_transient_failure(&self.inner.transient_ensure_failures) {
            return Err(Status::unavailable("transient ensure failure"));
        }
        if self.inner.fail_ensure.load(Ordering::SeqCst) {
            return rpc_ok(RpcError {
                code: RpcErrorCode::Unavailable,
                message: "ensure failed".into(),
                details: serde_json::Value::Null,
            });
        }
        self.inner
            .ensure_requests
            .lock()
            .unwrap()
            .push(ensure.clone());
        let n = self.inner.ensure_requests.lock().unwrap().len();
        let container_id = ContainerId::parse(format!("{n:064x}")).unwrap();
        let machine_id = self.inner.registration.assigned_machine.id;
        self.inner
            .containers
            .lock()
            .unwrap()
            .push(ContainerObservation {
                container_id,
                display_name: format!("{}-slot", ensure.resolved_spec.name),
                created_at_unix_nanos: n as i64,
                machine_id,
                project_name: ensure.project_name,
                service_id: ensure.resolved_spec.service_id,
                service_name: ensure.resolved_spec.name.clone(),
                kind: ContainerKind::ServiceContainer,
                runtime: ContainerRuntimeObservation::Running {
                    health: HealthObservation::NotConfigured,
                },
                effective_healthcheck: None,
                resolved_spec: ensure.resolved_spec,
                address: None,
                labels: Default::default(),
            });
        self.record("deploy_ingress");
        rpc_ok(ContainerCreated {
            container_id,
            display_name: format!("slot-{n}"),
        })
    }
    async fn remove_volume(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn start_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let decoded = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let RpcRequestBody::StartContainer(start) = decoded.body else {
            return Err(Status::invalid_argument("expected StartContainer"));
        };
        let mut containers = self.inner.containers.lock().unwrap();
        let container = containers
            .iter_mut()
            .find(|container| container.container_id == start.container_id)
            .ok_or_else(|| Status::not_found("container not found"))?;
        container.runtime = ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        };
        drop(containers);
        self.record("deploy_ingress");
        rpc_ok(ContainerChanged {
            container_id: start.container_id,
        })
    }
    async fn stop_container(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn remove_container(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn list_images(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        rpc_ok(MachineImages {
            containerd_store: false,
            images: Vec::new(),
        })
    }
    async fn ensure_image_ingest(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn pull_image_from_machine(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn get_ingress_proxy_config(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn reserve_domain(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.inner.reserve_attempts.fetch_add(1, Ordering::SeqCst);
        if consume_transient_failure(&self.inner.transient_reserve_failures) {
            return Err(Status::unavailable("transient domain reservation failure"));
        }
        let decoded = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let RpcRequestBody::ReserveDomain(reserve) = decoded.body else {
            return Err(Status::invalid_argument("expected ReserveDomain"));
        };
        *self.inner.reserve_request.lock().unwrap() = Some(reserve);
        self.inner.domain_reserved.store(true, Ordering::SeqCst);
        self.record("reserve_domain");
        rpc_ok(Domain {
            name: CLUSTER_DOMAIN.into(),
        })
    }
    async fn get_domain(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        if self.inner.domain_reserved.load(Ordering::SeqCst) {
            rpc_ok(Domain {
                name: CLUSTER_DOMAIN.into(),
            })
        } else {
            rpc_ok(RpcError {
                code: RpcErrorCode::NotFound,
                message: "no reserved domain".into(),
                details: serde_json::Value::Null,
            })
        }
    }
    async fn release_domain(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn create_domain_records(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.inner
            .domain_record_attempts
            .fetch_add(1, Ordering::SeqCst);
        let decoded = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let RpcRequestBody::CreateDomainRecords(create) = decoded.body else {
            return Err(Status::invalid_argument("expected CreateDomainRecords"));
        };
        if consume_transient_failure(&self.inner.transient_domain_record_failures) {
            return Err(Status::unavailable("transient DNS publication failure"));
        }
        self.inner
            .domain_record_requests
            .lock()
            .unwrap()
            .push(create.clone());
        self.record("publish_dns");
        rpc_ok(DomainRecords {
            records: create.records,
        })
    }
    async fn reset(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.inner.resets.fetch_add(1, Ordering::SeqCst);
        self.inner.joined.store(false, Ordering::SeqCst);
        if let Some(hold) = self.inner._register.lock().unwrap().take() {
            hold.abort();
        }
        rpc_ok(ResetAccepted {})
    }
    async fn update_machine(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn remove_local_machine(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn remove_machine(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn inspect_wireguard(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn exec(
        &self,
        _request: Request<Streaming<OpaquePayload>>,
    ) -> Result<Response<Self::ExecStream>, Status> {
        unused()
    }
    async fn container_logs(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<Self::ContainerLogsStream>, Status> {
        unused()
    }
    async fn machine_logs(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<Self::MachineLogsStream>, Status> {
        unused()
    }
    async fn runtime_watch(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<Self::RuntimeWatchStream>, Status> {
        unused()
    }
}

pub fn registration() -> Registered {
    Registered {
        assigned_machine: joiner_machine(),
        visible_peers: Vec::new(),
        target_versions: Default::default(),
    }
}

pub fn ingress_on(machine: &Machine) -> ContainerObservation {
    let spec: ployz_core::RequestedServiceSpec = serde_json::from_value(serde_json::json!({
        "name": "ingress",
        "mode": { "mode": "global" },
        "container": {
            "image": "caddy:2.10.0",
            "pull_policy": "missing",
            "command": ["caddy", "run", "-c", "/config/caddy/Caddyfile"],
            "environment": { "CADDY_ADMIN": "unix//run/ingress/caddy/admin.sock" }
        }
    }))
    .unwrap();
    let spec = spec.to_resolved(
        ployz_core::ServiceId::parse("c".repeat(32)).unwrap(),
        ployz_core::ResolvedUpdateConfig::default(),
    );
    ContainerObservation {
        container_id: ContainerId::parse("a".repeat(64)).unwrap(),
        display_name: "ingress-a".into(),
        created_at_unix_nanos: 1,
        machine_id: machine.id,
        project_name: ployz_core::ProjectName::system(),
        service_id: spec.service_id,
        service_name: spec.name.clone(),
        kind: ContainerKind::ServiceContainer,
        runtime: ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        },
        effective_healthcheck: None,
        resolved_spec: spec,
        address: None,
        labels: Default::default(),
    }
}

pub fn envoy_ingress_on(machine: &Machine) -> ContainerObservation {
    let spec = ployz_core::IngressProxyBackend::Envoy
        .requested_service_spec("envoy:test".into(), Vec::new(), None)
        .unwrap()
        .to_resolved(
            ployz_core::ServiceId::parse("e".repeat(32)).unwrap(),
            ployz_core::ResolvedUpdateConfig::default(),
        );
    ContainerObservation {
        container_id: ContainerId::parse("b".repeat(64)).unwrap(),
        display_name: "ingress-envoy".into(),
        created_at_unix_nanos: 2,
        machine_id: machine.id,
        project_name: ployz_core::ProjectName::system(),
        service_id: spec.service_id,
        service_name: spec.name.clone(),
        kind: ContainerKind::ServiceContainer,
        runtime: ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        },
        effective_healthcheck: None,
        resolved_spec: spec,
        address: None,
        labels: Default::default(),
    }
}

fn up_machine(machine: Machine) -> MachineObservation {
    MachineObservation::new(machine, MembershipObservation::Up)
}

fn joiner_machine() -> Machine {
    Machine {
        id: MachineId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
        name: MachineName::parse("joiner").unwrap(),
        subnet: "10.210.1.0/24".parse().unwrap(),
        management_address: ManagementAddress("fd00::1".parse().unwrap()),
        public_key: WireGuardPublicKey([1; 32]),
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.2:51820".parse().unwrap())],
        runtime: Default::default(),
    }
}

pub fn founder_machine() -> Machine {
    Machine {
        id: MachineId::parse("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap(),
        name: MachineName::parse("founder").unwrap(),
        subnet: "10.210.0.0/24".parse().unwrap(),
        management_address: ManagementAddress("fd00::2".parse().unwrap()),
        public_key: WireGuardPublicKey([2; 32]),
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
        runtime: Default::default(),
    }
}
