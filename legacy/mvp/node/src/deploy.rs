use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use mvp_bus::{BusActorHandle, BusSession, FactKeyPattern, Grant, PrincipalId, local::LocalBus};
use mvp_deploy::{
    DeployCoordinator, DeployEpochReservationFact, DeployError, DeployId, DeployManifest,
    DeployStatusFact, DeployStatusPhase, DeployTimeouts, DnsCommitId, GatewayCommitId,
    InstanceCapacityRequirement, InstanceId, InstancePlan, PandaDeployFactWriter, PhaseId,
    PhasePlan, PhasePolicy, PhaseReversibility, ProjectionCatchUp, RevisionId, RouteCommitId,
    ServingCommitId, ServingCommitPlan, decode_deploy_epoch_reservation_fact,
    deploy_epoch_reservation_fact_key, deploy_epoch_reservation_fact_payload,
    deploy_status_fact_key, deploy_status_fact_payload, read_deploy_statuses,
};
use mvp_identity::NodeId;
use mvp_p2panda_facts::{
    PandaFactStore, PandaFactWriteOutcome, PandaSqliteOpenConfig, PandaTrustedAuthorKey,
    SharedPandaFactStore,
};
use mvp_projection::{
    BackendEndpoint, CandidateStatus, DnsRecordFact, GatewayProjection, ProjectionActorHandle,
    ProjectionFactPayload, RouteId, ServiceName, SqliteProjectionStore,
};
use mvp_routing::PandaServingFactWriter;
use mvp_runtime::{ProcessRuntime, RuntimeBackend};

use crate::error::{NodeError, NodeResult};
use crate::load_node;
use crate::networking::apply_host_networking_snapshot;
use crate::node_agent::{node_agent_grant, register_node_agent_services};
use crate::node_agent_rpc::register_remote_node_agent_bridges;
use crate::state::LoadedNodeState;

const DEPLOY_TIMEOUT: Duration = Duration::from_secs(15);
const PROJECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductDeployOptions {
    pub state_dir: std::path::PathBuf,
    pub deploy_id: DeployId,
    pub target_node: NodeId,
    pub service: ServiceName,
    pub revision: RevisionId,
    pub hostname: String,
}

impl ProductDeployOptions {
    #[must_use]
    pub fn new(state_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            deploy_id: DeployId::new("deploy-main"),
            target_node: NodeId::new("node-a"),
            service: ServiceName::new("web"),
            revision: RevisionId::new("rev-1"),
            hostname: "web.example.test".to_string(),
        }
    }

    #[must_use]
    pub fn with_deploy_id(mut self, deploy_id: impl Into<String>) -> Self {
        self.deploy_id = DeployId::new(deploy_id);
        self
    }

    #[must_use]
    pub fn with_target_node(mut self, node_id: impl Into<String>) -> Self {
        self.target_node = NodeId::new(node_id);
        self
    }

    #[must_use]
    pub fn with_service(mut self, service: impl Into<String>) -> Self {
        self.service = ServiceName::new(service);
        self
    }

    #[must_use]
    pub fn with_revision(mut self, revision: impl Into<String>) -> Self {
        self.revision = RevisionId::new(revision);
        self
    }

    #[must_use]
    pub fn with_hostname(mut self, hostname: impl Into<String>) -> Self {
        self.hostname = hostname.into();
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductDeployReport {
    pub deploy_id: DeployId,
    pub active_backends: Vec<BackendEndpoint>,
    pub old_backends_to_drain: Vec<BackendEndpoint>,
    pub visible_nodes: usize,
    pub host_network_backends: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductDeployStatusReport {
    pub deploy_id: DeployId,
    pub statuses: Vec<DeployStatusFact>,
}

pub async fn deploy_product_service(
    options: ProductDeployOptions,
) -> NodeResult<ProductDeployReport> {
    deploy_product_service_with_process(options, None).await
}

pub async fn deploy_product_service_with_process(
    options: ProductDeployOptions,
    process: Option<ProcessRuntime>,
) -> NodeResult<ProductDeployReport> {
    let backend = process.map(|process| Arc::new(process) as Arc<dyn RuntimeBackend>);
    deploy_product_service_with_runtime(options, backend).await
}

pub async fn deploy_product_service_with_runtime(
    options: ProductDeployOptions,
    backend: Option<Arc<dyn RuntimeBackend>>,
) -> NodeResult<ProductDeployReport> {
    let state = load_node(&options.state_dir)?;
    let (bus, authority, raw_bus) = mvp_bus::local::actor_with_authority();
    let operator = authority.grant_in(state.island(), state.principal(), Grant::allow_all());
    let node_agent_session = authority.grant_in(
        state.island(),
        PrincipalId::new(format!("node-agent:{}", state.node_id_str())),
        node_agent_grant(state.node_id_str())?,
    );
    let (_node_agent, _node_agent_report) = match backend {
        Some(backend) => {
            crate::register_node_agent_services_with_runtime(
                &bus,
                &node_agent_session,
                &state,
                Some(backend),
            )
            .await?
        }
        None => register_node_agent_services(&bus, &node_agent_session, &state).await?,
    };

    ensure_local_daemon_not_running(&state)?;
    let remote_bridges = register_remote_node_agent_bridges(&bus, &operator, &state).await?;
    let (facts, fact_session) = match remote_bridges {
        Some(bridges) => (bridges.store, bridges.fact_session),
        None => (
            open_local_fact_store(&state, Arc::new(raw_bus)).await?,
            operator.clone(),
        ),
    };
    deploy_product_service_with_context(options, bus, operator, facts, fact_session, &state).await
}

pub(crate) async fn deploy_product_service_with_context(
    options: ProductDeployOptions,
    bus: BusActorHandle,
    operator: BusSession,
    facts: SharedPandaFactStore,
    fact_session: BusSession,
    state: &LoadedNodeState,
) -> NodeResult<ProductDeployReport> {
    let status_writer = ProductDeployStatusWriter::new(
        facts.clone(),
        fact_session.clone(),
        Arc::new(state.author()?),
        options.deploy_id.clone(),
    );
    status_writer
        .write(1, DeployStatusPhase::Planned, None, None)
        .await?;
    let result = deploy_product_service_with_context_inner(
        options,
        bus,
        operator,
        facts,
        fact_session,
        state,
        status_writer.clone(),
    )
    .await;
    if let Err(error) = &result {
        let _ = status_writer
            .write(
                900,
                DeployStatusPhase::Failed,
                None,
                Some(error.to_string()),
            )
            .await;
    }
    result
}

async fn deploy_product_service_with_context_inner(
    options: ProductDeployOptions,
    bus: BusActorHandle,
    operator: BusSession,
    facts: SharedPandaFactStore,
    fact_session: BusSession,
    state: &LoadedNodeState,
    status_writer: ProductDeployStatusWriter,
) -> NodeResult<ProductDeployReport> {
    let projection = projection_actor(state, facts.clone(), fact_session.clone());
    let existing = projection
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|source| NodeError::Projection { source })?;
    let route_id = RouteId::new(options.service.as_str());
    let old_backends_to_drain = current_backends_for_route(&existing.state.gateway, &route_id);
    let serving_epoch = next_serving_epoch(&facts, state, &fact_session).await?;
    let manifest = manifest_from_options(&options, old_backends_to_drain, serving_epoch);
    let author = Arc::new(state.author()?);
    reserve_serving_epoch(&facts, &fact_session, &author, &manifest).await?;
    let coordinator = DeployCoordinator::with_fact_writers(
        bus,
        operator.clone(),
        PandaDeployFactWriter::new(facts.clone(), fact_session.clone(), Arc::clone(&author)),
        PandaServingFactWriter::new(facts.clone(), fact_session, author),
        DeployTimeouts {
            capacity: DEPLOY_TIMEOUT,
            participant: DEPLOY_TIMEOUT,
        },
    );

    let pending = coordinator
        .execute_until_serving_commit(manifest.clone())
        .await
        .map_err(|source| NodeError::Deploy { source })?;
    let committed_manifest = pending.manifest().clone();
    status_writer
        .write(
            40,
            DeployStatusPhase::CleanupPending,
            Some(committed_manifest.serving_commit.epoch),
            None,
        )
        .await?;
    let projected = projection
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|source| NodeError::Projection { source })?;
    let gateway = projected
        .state
        .gateway
        .as_ref()
        .ok_or(NodeError::MissingGatewayProjection)?;
    let networking = apply_host_networking_snapshot(state, manifest.serving_commit.epoch, gateway)?;
    let catch_up = ProjectionCatchUp::from_report(&committed_manifest.serving_commit, &projected)
        .map_err(|source| NodeError::Routing { source })?;
    let result = coordinator
        .finish_cleanup(
            pending
                .after_projection(catch_up)
                .map_err(|source| NodeError::Deploy { source })?,
        )
        .await
        .map_err(|source| NodeError::Deploy { source })?;
    status_writer
        .write(
            50,
            DeployStatusPhase::CleanupDone,
            Some(committed_manifest.serving_commit.epoch),
            None,
        )
        .await?;
    let active_backends = current_backends_for_route(
        &projected.state.gateway,
        &committed_manifest.serving_commit.route_id,
    );

    Ok(ProductDeployReport {
        deploy_id: manifest.deploy_id,
        active_backends,
        old_backends_to_drain: committed_manifest.serving_commit.old_backends_to_drain,
        visible_nodes: result.visible_nodes.len(),
        host_network_backends: networking.applied.backend_count,
    })
}

async fn reserve_serving_epoch(
    facts: &SharedPandaFactStore,
    session: &BusSession,
    author: &Arc<mvp_p2panda_facts::PandaFactAuthor>,
    manifest: &DeployManifest,
) -> NodeResult<()> {
    let reservation = DeployEpochReservationFact::new(manifest);
    let key = deploy_epoch_reservation_fact_key(&reservation)
        .map_err(|source| NodeError::Deploy { source })?;
    let payload = deploy_epoch_reservation_fact_payload(&reservation)
        .map_err(|source| NodeError::Deploy { source })?;
    match facts
        .write_fact_payload(session, author.as_ref(), key.clone(), payload)
        .await
        .map_err(|source| NodeError::FactStore { source })?
    {
        PandaFactWriteOutcome::Inserted(_) | PandaFactWriteOutcome::AlreadyPresent(_) => Ok(()),
        PandaFactWriteOutcome::Conflict(metadata) => Err(NodeError::Deploy {
            source: DeployError::DeployFactConflict {
                key,
                principal: metadata.author.clone(),
                content_hash: metadata.content_hash.clone(),
            },
        }),
    }
}

pub fn read_product_deploy_status(
    state_dir: impl AsRef<std::path::Path>,
    deploy_id: impl Into<String>,
) -> NodeResult<ProductDeployStatusReport> {
    let state = load_node(state_dir)?;
    let (raw_bus, authority) = LocalBus::new_with_authority();
    let session = authority.grant_in(state.island(), state.principal(), Grant::allow_all());
    let state_for_store = state.clone();
    let facts = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(|source| NodeError::Runtime { source })?;
        runtime.block_on(open_local_fact_store(&state_for_store, Arc::new(raw_bus)))
    })
    .join()
    .map_err(|_| NodeError::NodeAgentRpc {
        message: "deploy status reader thread panicked".to_string(),
    })??;
    let deploy_id = DeployId::new(deploy_id);
    let statuses = read_deploy_statuses(&facts, &state.island(), &session, &deploy_id)
        .map_err(|source| NodeError::Deploy { source })?;
    Ok(ProductDeployStatusReport {
        deploy_id,
        statuses,
    })
}

#[derive(Clone)]
struct ProductDeployStatusWriter {
    facts: SharedPandaFactStore,
    session: BusSession,
    author: Arc<mvp_p2panda_facts::PandaFactAuthor>,
    deploy_id: DeployId,
}

impl ProductDeployStatusWriter {
    fn new(
        facts: SharedPandaFactStore,
        session: BusSession,
        author: Arc<mvp_p2panda_facts::PandaFactAuthor>,
        deploy_id: DeployId,
    ) -> Self {
        Self {
            facts,
            session,
            author,
            deploy_id,
        }
    }

    async fn write(
        &self,
        sequence: u64,
        phase: DeployStatusPhase,
        serving_epoch: Option<u64>,
        message: Option<String>,
    ) -> NodeResult<()> {
        let mut fact = DeployStatusFact::new(self.deploy_id.clone(), sequence, phase);
        if let Some(serving_epoch) = serving_epoch {
            fact = fact.with_serving_epoch(serving_epoch);
        }
        if let Some(message) = message {
            fact = fact.with_message(message);
        }
        let key = deploy_status_fact_key(&fact).map_err(|source| NodeError::Deploy { source })?;
        let payload =
            deploy_status_fact_payload(&fact).map_err(|source| NodeError::Deploy { source })?;
        self.facts
            .write_fact_payload(&self.session, self.author.as_ref(), key, payload)
            .await
            .map_err(|source| NodeError::FactStore { source })?;
        Ok(())
    }
}

async fn open_local_fact_store(
    state: &LoadedNodeState,
    raw_bus: Arc<LocalBus>,
) -> NodeResult<SharedPandaFactStore> {
    let author = state.author()?;
    let mut store_config =
        PandaSqliteOpenConfig::new(state.paths().fact_store.clone(), vec![state.island()])
            .with_trusted_author_key(PandaTrustedAuthorKey::new(
                state.island(),
                state.principal(),
                author.author_key(),
            ));
    let trusted_authors = state.trusted_fact_authors()?;
    for trusted in &trusted_authors {
        store_config = store_config.with_trusted_author_key(PandaTrustedAuthorKey::new(
            state.island(),
            trusted.principal().clone(),
            trusted.author_key(),
        ));
    }
    let store = PandaFactStore::open_sqlite(raw_bus, store_config)
        .await
        .map_err(|source| NodeError::FactStore { source })?;
    let shared = SharedPandaFactStore::new(store);
    shared
        .trust_author_key(&state.island(), state.principal(), author.author_key())
        .await
        .map_err(|source| NodeError::FactStore { source })?;
    for trusted in &trusted_authors {
        shared
            .trust_author_key(
                &state.island(),
                trusted.principal().clone(),
                trusted.author_key(),
            )
            .await
            .map_err(|source| NodeError::FactStore { source })?;
    }
    Ok(shared)
}

fn projection_actor(
    state: &LoadedNodeState,
    facts: SharedPandaFactStore,
    session: mvp_bus::BusSession,
) -> ProjectionActorHandle {
    ProjectionActorHandle::spawn(
        Arc::new(facts),
        state.island(),
        session,
        FactKeyPattern::parse("/facts/>").expect("valid product projection pattern"),
        SqliteProjectionStore::new(state.paths().projection_db.clone()),
        state.paths().gateway_snapshot.clone(),
        state.paths().dns_snapshot.clone(),
    )
}

fn ensure_local_daemon_not_running(state: &LoadedNodeState) -> NodeResult<()> {
    match UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, state.p2panda_port())) {
        Ok(_socket) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            Err(NodeError::NodeAgentRpc {
                message: format!(
                    "local daemon appears to own p2panda port {}; stop it before standalone deploy",
                    state.p2panda_port()
                ),
            })
        }
        Err(source) => Err(NodeError::DaemonControlSocket {
            path: state.paths().state_dir.clone(),
            operation: "check local p2panda bind availability",
            source,
        }),
    }
}

fn manifest_from_options(
    options: &ProductDeployOptions,
    old_backends_to_drain: Vec<BackendEndpoint>,
    serving_epoch: u64,
) -> DeployManifest {
    let instance_id = InstanceId::new(format!(
        "{}-{}-{}",
        options.deploy_id,
        options.service.as_str(),
        options.revision.as_str()
    ));
    let serving_commit = ServingCommitPlan {
        serving_commit_id: ServingCommitId::new(format!("{}-serving", options.deploy_id)),
        route_commit_id: RouteCommitId::new(format!("{}-route", options.deploy_id)),
        gateway_commit_id: GatewayCommitId::new(format!("{}-gateway", options.deploy_id)),
        dns_commit_id: DnsCommitId::new(format!("{}-dns", options.deploy_id)),
        route_id: RouteId::new(options.service.as_str()),
        hostnames: vec![options.hostname.clone()],
        active_backends: vec![BackendEndpoint {
            node_id: options.target_node.clone(),
            address: instance_id.to_string(),
        }],
        old_backends_to_drain,
        dns_records: vec![DnsRecordFact {
            name: options.hostname.clone(),
            record_type: "A".to_string(),
            value: "127.0.0.1".to_string(),
            ttl_seconds: 30,
        }],
        epoch: serving_epoch,
    };
    DeployManifest::new(
        options.deploy_id.clone(),
        vec![PhasePlan::new(
            PhaseId::new(1),
            vec![InstancePlan::new(
                instance_id,
                options.target_node.clone(),
                options.service.clone(),
                options.revision.clone(),
                InstanceCapacityRequirement::General,
            )],
            PhasePolicy::serving(PhaseReversibility::Reversible),
        )],
        serving_commit,
    )
}

async fn next_serving_epoch(
    facts: &SharedPandaFactStore,
    state: &LoadedNodeState,
    session: &mvp_bus::BusSession,
) -> NodeResult<u64> {
    let pattern = FactKeyPattern::parse("/facts/serving/>").expect("valid serving fact pattern");
    let serving_candidates = facts
        .list_fact_candidates(&state.island(), &pattern, session)
        .await
        .map_err(|source| NodeError::FactSource { source })?;
    let serving_payloads = facts
        .read_fact_payloads(&state.island(), &serving_candidates, session)
        .await
        .map_err(|source| NodeError::FactSource { source })?;
    let mut max_epoch = 0;
    for candidate in serving_candidates {
        if candidate.status() != CandidateStatus::Verified {
            continue;
        }
        let Some(payload) = serving_payloads.get(candidate.content_hash()) else {
            continue;
        };
        let Ok(ProjectionFactPayload::ServingCommit(fact)) =
            ProjectionFactPayload::from_fact_bytes(payload.as_bytes())
        else {
            continue;
        };
        max_epoch = max_epoch.max(fact.epoch);
    }
    let reservation_pattern =
        FactKeyPattern::parse("/facts/deploy/epoch/>").expect("valid deploy epoch pattern");
    let reservation_candidates = facts
        .list_fact_candidates(&state.island(), &reservation_pattern, session)
        .await
        .map_err(|source| NodeError::FactSource { source })?;
    let reservation_payloads = facts
        .read_fact_payloads(&state.island(), &reservation_candidates, session)
        .await
        .map_err(|source| NodeError::FactSource { source })?;
    for candidate in reservation_candidates {
        if candidate.status() != CandidateStatus::Verified {
            continue;
        }
        let Some(payload) = reservation_payloads.get(candidate.content_hash()) else {
            continue;
        };
        let Ok(fact) = decode_deploy_epoch_reservation_fact(candidate.key(), payload) else {
            continue;
        };
        max_epoch = max_epoch.max(fact.serving_epoch);
    }
    Ok(max_epoch
        .checked_add(1)
        .expect("serving epoch overflow would break deploy ordering"))
}

fn current_backends_for_route(
    gateway: &Option<GatewayProjection>,
    route_id: &RouteId,
) -> Vec<BackendEndpoint> {
    let Some(gateway) = gateway else {
        return Vec::new();
    };
    gateway
        .routes
        .iter()
        .filter(|route| &route.route_id == route_id)
        .flat_map(|route| route.backends.iter().cloned())
        .collect()
}
