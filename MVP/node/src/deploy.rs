use std::sync::Arc;
use std::time::Duration;

use mvp_bus::{FactKeyPattern, Grant, PrincipalId};
use mvp_deploy::{
    DeployCoordinator, DeployId, DeployManifest, DeployTimeouts, DnsCommitId, GatewayCommitId,
    InstanceCapacityRequirement, InstanceId, InstancePlan, PhaseId, PhasePlan, PhasePolicy,
    PhaseReversibility, ProjectionCatchUp, RevisionId, RouteCommitId, ServingCommitId,
    ServingCommitPlan,
};
use mvp_deploy_p2panda::PandaDeployFactWriter;
use mvp_identity::NodeId;
use mvp_p2panda_facts::{
    PandaFactStore, PandaSqliteOpenConfig, PandaTrustedAuthorKey, SharedPandaFactStore,
};
use mvp_projection::{
    BackendEndpoint, CandidateStatus, DnsRecordFact, FactSource, GatewayProjection,
    ProjectionActorHandle, ProjectionFactPayload, RouteId, ServiceName, SqliteProjectionStore,
};
use mvp_routing_p2panda::PandaServingFactWriter;
use mvp_runtime::ProcessRuntime;

use crate::error::{NodeError, NodeResult};
use crate::load_node;
use crate::networking::apply_host_networking_snapshot;
use crate::node_agent::{node_agent_grant, register_node_agent_services};
use crate::state::LoadedNodeState;

const DEPLOY_TIMEOUT: Duration = Duration::from_secs(5);
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

pub async fn deploy_product_service(
    options: ProductDeployOptions,
) -> NodeResult<ProductDeployReport> {
    deploy_product_service_with_process(options, None).await
}

pub async fn deploy_product_service_with_process(
    options: ProductDeployOptions,
    process: Option<ProcessRuntime>,
) -> NodeResult<ProductDeployReport> {
    let state = load_node(&options.state_dir)?;
    let (bus, authority, raw_bus) = mvp_bus::harness::actor_with_authority();
    let operator = authority.grant_in(state.island(), state.principal(), Grant::allow_all());
    let node_agent_session = authority.grant_in(
        state.island(),
        PrincipalId::new(format!("node-agent:{}", state.node_id_str())),
        node_agent_grant(state.node_id_str())?,
    );
    let (_node_agent, _node_agent_report) = match process {
        Some(process) => {
            crate::register_node_agent_services_with_process(
                &bus,
                &node_agent_session,
                &state,
                Some(process),
            )
            .await?
        }
        None => register_node_agent_services(&bus, &node_agent_session, &state).await?,
    };

    let facts = open_local_fact_store(&state, Arc::new(raw_bus)).await?;
    let projection = projection_actor(&state, facts.clone(), operator.clone());
    let existing = projection
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|source| NodeError::Projection { source })?;
    let old_backends_to_drain = current_backends(&existing.state.gateway);
    let serving_epoch = next_serving_epoch(&facts, &state, &operator)?;
    let manifest = manifest_from_options(&options, old_backends_to_drain, serving_epoch);
    let author = Arc::new(state.author()?);
    let coordinator = DeployCoordinator::with_fact_writers(
        bus,
        operator.clone(),
        PandaDeployFactWriter::new(facts.clone(), operator.clone(), Arc::clone(&author)),
        PandaServingFactWriter::new(facts.clone(), operator.clone(), author),
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
    let projected = projection
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|source| NodeError::Projection { source })?;
    let gateway = projected
        .state
        .gateway
        .as_ref()
        .ok_or(NodeError::MissingGatewayProjection)?;
    let networking =
        apply_host_networking_snapshot(&state, manifest.serving_commit.epoch, gateway)?;
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
    let active_backends = current_backends(&projected.state.gateway);

    Ok(ProductDeployReport {
        deploy_id: manifest.deploy_id,
        active_backends,
        old_backends_to_drain: committed_manifest.serving_commit.old_backends_to_drain,
        visible_nodes: result.visible_nodes.len(),
        host_network_backends: networking.applied.backend_count,
    })
}

async fn open_local_fact_store(
    state: &LoadedNodeState,
    raw_bus: Arc<mvp_bus::harness::InMemoryBus>,
) -> NodeResult<SharedPandaFactStore> {
    let author = state.author()?;
    let store = PandaFactStore::open_sqlite(
        raw_bus,
        PandaSqliteOpenConfig::new(state.paths().fact_store.clone(), vec![state.island()])
            .with_trusted_author_key(PandaTrustedAuthorKey::new(
                state.island(),
                state.principal(),
                author.author_key(),
            )),
    )
    .await
    .map_err(|source| NodeError::FactStore { source })?;
    let shared = SharedPandaFactStore::new(store);
    shared
        .trust_author_key(&state.island(), state.principal(), author.author_key())
        .await
        .map_err(|source| NodeError::FactStore { source })?;
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

fn next_serving_epoch(
    facts: &SharedPandaFactStore,
    state: &LoadedNodeState,
    session: &mvp_bus::BusSession,
) -> NodeResult<u64> {
    let pattern = FactKeyPattern::parse("/facts/serving/>").expect("valid serving fact pattern");
    let candidates = facts
        .list_candidates(&state.island(), &pattern, session)
        .map_err(|source| NodeError::FactSource { source })?;
    let payloads = facts
        .read_payloads(&state.island(), &candidates, session)
        .map_err(|source| NodeError::FactSource { source })?;
    let mut max_epoch = 0;
    for candidate in candidates {
        if !matches!(
            candidate.status(),
            CandidateStatus::Verified | CandidateStatus::Conflict
        ) {
            continue;
        }
        let Some(payload) = payloads.get(candidate.content_hash()) else {
            continue;
        };
        let Ok(ProjectionFactPayload::ServingCommit(fact)) =
            ProjectionFactPayload::from_fact_bytes(payload.as_bytes())
        else {
            continue;
        };
        max_epoch = max_epoch.max(fact.epoch);
    }
    Ok(max_epoch
        .checked_add(1)
        .expect("serving epoch overflow would break deploy ordering"))
}

fn current_backends(gateway: &Option<GatewayProjection>) -> Vec<BackendEndpoint> {
    let Some(gateway) = gateway else {
        return Vec::new();
    };
    gateway
        .routes
        .iter()
        .flat_map(|route| route.backends.iter().cloned())
        .collect()
}
