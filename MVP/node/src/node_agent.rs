use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use mvp_bus::{BusActorHandle, BusSession, HandlerFailure, RequestContext, SubjectPattern};
use mvp_deploy::{
    CapacityReply, CapacityRequest, CleanupDeployCandidatesRequest, DrainInstanceRequest,
    InstanceCommandReply, InstanceCommandRequest, InstanceId, InstanceStartOutcome,
    StopInstanceRequest, decode, encode,
};
use mvp_runtime::{ProcessInstanceSpec, ProcessRuntime, RuntimeState};

use crate::error::{NodeError, NodeResult};
use crate::state::LoadedNodeState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeAgentReport {
    pub node_id: String,
    pub registered_handlers: usize,
}

#[derive(Debug, Clone)]
pub struct NodeAgentServices {
    runtime: Arc<Mutex<NodeAgentRuntime>>,
}

#[derive(Debug)]
struct NodeAgentRuntime {
    node_id: mvp_identity::NodeId,
    process: Option<ProcessRuntime>,
    prepared: BTreeSet<InstanceId>,
    running: BTreeSet<InstanceId>,
    draining: BTreeSet<InstanceId>,
    stopped: BTreeSet<InstanceId>,
}

impl NodeAgentRuntime {
    fn new(node_id: mvp_identity::NodeId, process: Option<ProcessRuntime>) -> Self {
        Self {
            node_id,
            process,
            prepared: BTreeSet::new(),
            running: BTreeSet::new(),
            draining: BTreeSet::new(),
            stopped: BTreeSet::new(),
        }
    }

    fn load_process_instances(&mut self) -> NodeResult<()> {
        let Some(process) = &self.process else {
            return Ok(());
        };
        for instance in process
            .list()
            .map_err(|source| NodeError::RuntimeBackend { source })?
        {
            match instance.state {
                RuntimeState::Prepared => {
                    self.prepared.insert(instance.instance_id);
                }
                RuntimeState::Running => {
                    self.prepared.insert(instance.instance_id.clone());
                    self.running.insert(instance.instance_id);
                }
                RuntimeState::Draining => {
                    self.prepared.insert(instance.instance_id.clone());
                    self.running.insert(instance.instance_id.clone());
                    self.draining.insert(instance.instance_id);
                }
                RuntimeState::Stopped => {
                    self.stopped.insert(instance.instance_id);
                }
            }
        }
        Ok(())
    }
}

impl NodeAgentServices {
    #[must_use]
    pub fn prepared_instances(&self) -> Vec<InstanceId> {
        self.runtime
            .lock()
            .expect("node agent runtime mutex poisoned")
            .prepared
            .iter()
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn running_instances(&self) -> Vec<InstanceId> {
        self.runtime
            .lock()
            .expect("node agent runtime mutex poisoned")
            .running
            .iter()
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn draining_instances(&self) -> Vec<InstanceId> {
        self.runtime
            .lock()
            .expect("node agent runtime mutex poisoned")
            .draining
            .iter()
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn stopped_instances(&self) -> Vec<InstanceId> {
        self.runtime
            .lock()
            .expect("node agent runtime mutex poisoned")
            .stopped
            .iter()
            .cloned()
            .collect()
    }
}

pub async fn register_node_agent_services(
    bus: &BusActorHandle,
    session: &BusSession,
    state: &LoadedNodeState,
) -> NodeResult<(NodeAgentServices, NodeAgentReport)> {
    let program = std::env::current_exe().map_err(|source| NodeError::RuntimeBackend {
        source: mvp_runtime::RuntimeError::CurrentExe { source },
    })?;
    let process = ProcessRuntime::managed_http(state.paths().runtime_dir.clone(), program);
    register_node_agent_services_with_process(bus, session, state, Some(process)).await
}

pub async fn register_node_agent_services_with_process(
    bus: &BusActorHandle,
    session: &BusSession,
    state: &LoadedNodeState,
    process: Option<ProcessRuntime>,
) -> NodeResult<(NodeAgentServices, NodeAgentReport)> {
    let mut runtime_state = NodeAgentRuntime::new(state.node_id(), process);
    runtime_state.load_process_instances()?;
    let runtime = Arc::new(Mutex::new(runtime_state));
    let node_id = state.node_id_str();
    let mut registered_handlers = 0usize;

    let capacity_runtime = Arc::clone(&runtime);
    bus.subscribe(
        session,
        node_subject_pattern(node_id, "capacity")?,
        move |ctx| capacity_handler(ctx, &capacity_runtime),
    )
    .await
    .map_err(|source| NodeError::Bus { source })?;
    registered_handlers += 1;

    let prepare_runtime = Arc::clone(&runtime);
    bus.subscribe(
        session,
        node_rpc_subject_pattern(node_id, "prepare_instance")?,
        move |ctx| prepare_handler(ctx, &prepare_runtime),
    )
    .await
    .map_err(|source| NodeError::Bus { source })?;
    registered_handlers += 1;

    let start_runtime = Arc::clone(&runtime);
    bus.subscribe(
        session,
        node_rpc_subject_pattern(node_id, "start_instance")?,
        move |ctx| start_handler(ctx, &start_runtime),
    )
    .await
    .map_err(|source| NodeError::Bus { source })?;
    registered_handlers += 1;

    let drain_runtime = Arc::clone(&runtime);
    bus.subscribe(
        session,
        node_rpc_subject_pattern(node_id, "drain_instance")?,
        move |ctx| drain_handler(ctx, &drain_runtime),
    )
    .await
    .map_err(|source| NodeError::Bus { source })?;
    registered_handlers += 1;

    let stop_runtime = Arc::clone(&runtime);
    bus.subscribe(
        session,
        node_rpc_subject_pattern(node_id, "stop_instance")?,
        move |ctx| stop_handler(ctx, &stop_runtime),
    )
    .await
    .map_err(|source| NodeError::Bus { source })?;
    registered_handlers += 1;

    let cleanup_runtime = Arc::clone(&runtime);
    bus.subscribe(
        session,
        node_rpc_subject_pattern(node_id, "cleanup_deploy_candidates")?,
        move |ctx| cleanup_candidates_handler(ctx, &cleanup_runtime),
    )
    .await
    .map_err(|source| NodeError::Bus { source })?;
    registered_handlers += 1;

    Ok((
        NodeAgentServices {
            runtime: Arc::clone(&runtime),
        },
        NodeAgentReport {
            node_id: node_id.to_string(),
            registered_handlers,
        },
    ))
}

fn capacity_handler(
    ctx: RequestContext,
    runtime: &Arc<Mutex<NodeAgentRuntime>>,
) -> mvp_bus::HandlerOutcome {
    let _request: CapacityRequest =
        decode(ctx.message.payload(), "capacity request").map_err(|_| handler_failed(&ctx))?;
    let runtime = runtime.lock().map_err(|_| handler_failed(&ctx))?;
    ctx.reply(
        encode(
            &CapacityReply {
                node_id: runtime.node_id.clone(),
                memory_free_bytes: 1024 * 1024 * 1024,
                can_run_database: false,
            },
            "capacity reply",
        )
        .map_err(|_| handler_failed(&ctx))?,
    )
}

fn prepare_handler(
    ctx: RequestContext,
    runtime: &Arc<Mutex<NodeAgentRuntime>>,
) -> mvp_bus::HandlerOutcome {
    let request: InstanceCommandRequest = decode(ctx.message.payload(), "prepare instance request")
        .map_err(|_| handler_failed(&ctx))?;
    let mut runtime = runtime.lock().map_err(|_| handler_failed(&ctx))?;
    if let Some(process) = &runtime.process {
        process
            .prepare(&process_spec(&request))
            .map_err(|_| handler_failed(&ctx))?;
    }
    runtime.prepared.insert(request.instance_id);
    ctx.reply(b"prepared".to_vec())
}

fn start_handler(
    ctx: RequestContext,
    runtime: &Arc<Mutex<NodeAgentRuntime>>,
) -> mvp_bus::HandlerOutcome {
    let request: InstanceCommandRequest = decode(ctx.message.payload(), "start instance request")
        .map_err(|_| handler_failed(&ctx))?;
    let instance_id = request.instance_id;
    let mut runtime = runtime.lock().map_err(|_| handler_failed(&ctx))?;
    if let Some(process) = &runtime.process {
        process
            .start(&ProcessInstanceSpec::new(
                instance_id.clone(),
                request.service,
                request.revision,
            ))
            .map_err(|_| handler_failed(&ctx))?;
    }
    runtime.prepared.insert(instance_id.clone());
    runtime.running.insert(instance_id.clone());
    runtime.stopped.remove(&instance_id);
    ctx.reply(
        encode(
            &InstanceCommandReply {
                instance_id,
                outcome: InstanceStartOutcome::Ready,
            },
            "start instance reply",
        )
        .map_err(|_| handler_failed(&ctx))?,
    )
}

fn drain_handler(
    ctx: RequestContext,
    runtime: &Arc<Mutex<NodeAgentRuntime>>,
) -> mvp_bus::HandlerOutcome {
    let request: DrainInstanceRequest = decode(ctx.message.payload(), "drain instance request")
        .map_err(|_| handler_failed(&ctx))?;
    let mut runtime = runtime.lock().map_err(|_| handler_failed(&ctx))?;
    let instance_id = instance_id_from_backend(&request.cleanup_target);
    if let Some(process) = &runtime.process {
        process
            .drain(&instance_id)
            .map_err(|_| handler_failed(&ctx))?;
    }
    runtime.draining.insert(instance_id);
    ctx.reply(b"drained".to_vec())
}

fn stop_handler(
    ctx: RequestContext,
    runtime: &Arc<Mutex<NodeAgentRuntime>>,
) -> mvp_bus::HandlerOutcome {
    let request: StopInstanceRequest =
        decode(ctx.message.payload(), "stop instance request").map_err(|_| handler_failed(&ctx))?;
    let mut runtime = runtime.lock().map_err(|_| handler_failed(&ctx))?;
    let instance_id = instance_id_from_backend(&request.cleanup_target);
    if let Some(process) = &runtime.process {
        process
            .stop(&instance_id)
            .map_err(|_| handler_failed(&ctx))?;
    }
    runtime.running.remove(&instance_id);
    runtime.draining.remove(&instance_id);
    runtime.stopped.insert(instance_id);
    ctx.reply(b"stopped".to_vec())
}

fn cleanup_candidates_handler(
    ctx: RequestContext,
    runtime: &Arc<Mutex<NodeAgentRuntime>>,
) -> mvp_bus::HandlerOutcome {
    let request: CleanupDeployCandidatesRequest =
        decode(ctx.message.payload(), "cleanup candidates request")
            .map_err(|_| handler_failed(&ctx))?;
    let mut runtime = runtime.lock().map_err(|_| handler_failed(&ctx))?;
    for candidate in request.candidates {
        if let Some(process) = &runtime.process {
            process
                .stop(&candidate.instance_id)
                .map_err(|_| handler_failed(&ctx))?;
        }
        runtime.prepared.remove(&candidate.instance_id);
        runtime.running.remove(&candidate.instance_id);
        runtime.draining.remove(&candidate.instance_id);
        runtime.stopped.insert(candidate.instance_id);
    }
    ctx.reply(b"cleaned".to_vec())
}

fn process_spec(request: &InstanceCommandRequest) -> ProcessInstanceSpec {
    ProcessInstanceSpec::new(
        request.instance_id.clone(),
        request.service.clone(),
        request.revision.clone(),
    )
}

fn instance_id_from_backend(endpoint: &mvp_projection::BackendEndpoint) -> InstanceId {
    InstanceId::new(endpoint.address.clone())
}

fn node_subject_pattern(node_id: &str, suffix: &str) -> NodeResult<SubjectPattern> {
    SubjectPattern::parse(format!("node.{node_id}.{suffix}"))
        .map_err(|source| NodeError::BusSubject { source })
}

fn node_rpc_subject_pattern(node_id: &str, suffix: &str) -> NodeResult<SubjectPattern> {
    SubjectPattern::parse(format!("node.{node_id}.rpc.{suffix}"))
        .map_err(|source| NodeError::BusSubject { source })
}

fn handler_failed(ctx: &RequestContext) -> mvp_bus::BusError {
    mvp_bus::BusError::HandlerFailed {
        subject: ctx.message.subject().to_string(),
        failure: HandlerFailure::Application,
    }
}

pub fn node_agent_grant(node_id: &str) -> NodeResult<mvp_bus::Grant> {
    Ok(mvp_bus::Grant::empty()
        .with_subscribe(node_subject_pattern(node_id, "capacity")?)
        .with_subscribe(node_rpc_subject_pattern(node_id, "prepare_instance")?)
        .with_subscribe(node_rpc_subject_pattern(node_id, "start_instance")?)
        .with_subscribe(node_rpc_subject_pattern(node_id, "drain_instance")?)
        .with_subscribe(node_rpc_subject_pattern(node_id, "stop_instance")?)
        .with_subscribe(node_rpc_subject_pattern(
            node_id,
            "cleanup_deploy_candidates",
        )?)
        .with_response())
}

#[cfg(test)]
pub fn node_agent_request_grant() -> mvp_bus::Grant {
    mvp_bus::Grant::empty().with_publish(
        mvp_bus::SubjectPattern::parse("node.*.>").expect("valid node-agent request subject grant"),
    )
}

#[cfg(test)]
#[must_use]
pub fn product_bus_for_node_agent_tests() -> (
    BusActorHandle,
    mvp_bus::BusAuthority,
    mvp_bus::harness::InMemoryBus,
) {
    mvp_bus::harness::actor_with_authority()
}

#[cfg(test)]
async fn register_node_agent_services_in_memory(
    bus: &BusActorHandle,
    session: &BusSession,
    state: &LoadedNodeState,
) -> NodeResult<(NodeAgentServices, NodeAgentReport)> {
    register_node_agent_services_with_process(bus, session, state, None).await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use mvp_bus::{BusError, IslandId, PrincipalId, RequestManyPolicy, RequestTarget, Subject};
    use mvp_deploy::{
        CandidateCleanupState, CandidateCleanupTarget, CleanupDeployCandidatesRequest,
        DrainInstanceRequest, InstanceCommandReply, InstanceCommandRequest, InstanceId,
        InstanceStartOutcome, RevisionId, StopInstanceRequest, decode_capacity_reply, encode,
    };
    use mvp_projection::{BackendEndpoint, ServiceName};

    use crate::{InitOptions, init_node, load_node};

    use super::{
        node_agent_grant, node_agent_request_grant, product_bus_for_node_agent_tests,
        register_node_agent_services_in_memory,
    };

    #[tokio::test]
    async fn node_agent_services_respond_to_deploy_participant_requests() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state_dir = temp.path().join("node-a");
        let state = init_node(
            InitOptions::new(&state_dir)
                .with_island("prod")
                .with_node_id("node-a"),
        )
        .expect("init node");
        let (bus, authority, _raw) = product_bus_for_node_agent_tests();
        let agent = authority.grant_in(
            state.island(),
            PrincipalId::new("node-agent"),
            node_agent_grant(state.node_id_str()).expect("agent grant"),
        );
        let requester = authority.grant_in(
            IslandId::new("prod"),
            PrincipalId::new("operator"),
            node_agent_request_grant(),
        );

        let (services, report) = register_node_agent_services_in_memory(&bus, &agent, &state)
            .await
            .expect("register node agent");

        assert_eq!(report.node_id, "node-a");
        assert_eq!(report.registered_handlers, 6);

        let capacity_subject = Subject::parse("node.node-a.capacity").expect("capacity subject");
        let capacity_replies = bus
            .request_many(
                &requester,
                RequestTarget::Subject(capacity_subject.clone()),
                capacity_subject,
                encode(
                    &mvp_deploy::CapacityRequest {
                        deploy_id: mvp_deploy::DeployId::new("deploy-1"),
                    },
                    "capacity request",
                )
                .expect("capacity request"),
                RequestManyPolicy::new(1, Duration::from_secs(1)),
            )
            .await
            .expect("capacity replies");
        let capacity = decode_capacity_reply(capacity_replies[0].payload()).expect("capacity");
        assert_eq!(capacity.node_id.as_str(), "node-a");
        assert!(capacity.memory_free_bytes > 0);

        let instance = instance_request("instance-a");
        request_unit(
            &bus,
            &requester,
            "node.node-a.rpc.prepare_instance",
            &instance,
        )
        .await;
        let reply: InstanceCommandReply = request_json(
            &bus,
            &requester,
            "node.node-a.rpc.start_instance",
            &instance,
        )
        .await;
        assert_eq!(reply.instance_id, InstanceId::new("instance-a"));
        assert_eq!(reply.outcome, InstanceStartOutcome::Ready);
        assert_eq!(
            services.prepared_instances(),
            vec![InstanceId::new("instance-a")]
        );
        assert_eq!(
            services.running_instances(),
            vec![InstanceId::new("instance-a")]
        );

        let old_backend = BackendEndpoint {
            node_id: state.node_id(),
            address: "old-instance".to_string(),
        };
        request_unit(
            &bus,
            &requester,
            "node.node-a.rpc.drain_instance",
            &DrainInstanceRequest {
                deploy_id: mvp_deploy::DeployId::new("deploy-1"),
                cleanup_target: old_backend.clone(),
            },
        )
        .await;
        request_unit(
            &bus,
            &requester,
            "node.node-a.rpc.stop_instance",
            &StopInstanceRequest {
                deploy_id: mvp_deploy::DeployId::new("deploy-1"),
                cleanup_target: old_backend,
            },
        )
        .await;
        assert_eq!(
            services.stopped_instances(),
            vec![InstanceId::new("old-instance")]
        );
    }

    #[tokio::test]
    async fn node_agent_services_can_be_recreated_after_daemon_restart() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state_dir = temp.path().join("node-a");
        init_node(
            InitOptions::new(&state_dir)
                .with_island("prod")
                .with_node_id("node-a"),
        )
        .expect("init node");
        let state = load_node(&state_dir).expect("reload node");
        let (bus, authority, _raw) = product_bus_for_node_agent_tests();
        let agent = authority.grant_in(
            state.island(),
            PrincipalId::new("node-agent-restarted"),
            node_agent_grant(state.node_id_str()).expect("agent grant"),
        );

        let (_services, report) = register_node_agent_services_in_memory(&bus, &agent, &state)
            .await
            .expect("register restarted node agent");

        assert_eq!(report.registered_handlers, 6);
    }

    #[tokio::test]
    async fn node_agent_services_reject_unauthorized_requester() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state_dir = temp.path().join("node-a");
        let state = init_node(
            InitOptions::new(&state_dir)
                .with_island("prod")
                .with_node_id("node-a"),
        )
        .expect("init node");
        let (bus, authority, _raw) = product_bus_for_node_agent_tests();
        let agent = authority.grant_in(
            state.island(),
            PrincipalId::new("node-agent"),
            node_agent_grant(state.node_id_str()).expect("agent grant"),
        );
        let intruder = authority.grant_in(
            IslandId::new("prod"),
            PrincipalId::new("intruder"),
            mvp_bus::Grant::empty(),
        );
        register_node_agent_services_in_memory(&bus, &agent, &state)
            .await
            .expect("register node agent");

        let error = bus
            .request(
                &intruder,
                Subject::parse("node.node-a.rpc.prepare_instance").expect("subject"),
                encode(&instance_request("instance-a"), "prepare request").expect("request"),
                Duration::from_secs(1),
            )
            .await
            .expect_err("intruder should be denied");

        assert!(matches!(
            error,
            BusError::UnauthorizedPublish { .. } | BusError::UnauthorizedRequestTarget { .. }
        ));
    }

    #[tokio::test]
    async fn node_agent_cleanup_candidates_removes_local_candidate_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state_dir = temp.path().join("node-a");
        let state = init_node(
            InitOptions::new(&state_dir)
                .with_island("prod")
                .with_node_id("node-a"),
        )
        .expect("init node");
        let (bus, authority, _raw) = product_bus_for_node_agent_tests();
        let agent = authority.grant_in(
            state.island(),
            PrincipalId::new("node-agent"),
            node_agent_grant(state.node_id_str()).expect("agent grant"),
        );
        let requester = authority.grant_in(
            IslandId::new("prod"),
            PrincipalId::new("operator"),
            node_agent_request_grant(),
        );
        let (services, _report) = register_node_agent_services_in_memory(&bus, &agent, &state)
            .await
            .expect("register node agent");
        let instance = instance_request("instance-a");
        request_unit(
            &bus,
            &requester,
            "node.node-a.rpc.prepare_instance",
            &instance,
        )
        .await;
        request_unit(
            &bus,
            &requester,
            "node.node-a.rpc.start_instance",
            &instance,
        )
        .await;

        request_unit(
            &bus,
            &requester,
            "node.node-a.rpc.cleanup_deploy_candidates",
            &CleanupDeployCandidatesRequest {
                deploy_id: mvp_deploy::DeployId::new("deploy-1"),
                candidates: vec![CandidateCleanupTarget::new(
                    InstanceId::new("instance-a"),
                    ServiceName::new("web"),
                    RevisionId::new("rev-1"),
                    CandidateCleanupState::Started,
                )],
            },
        )
        .await;

        assert!(services.running_instances().is_empty());
        assert_eq!(
            services.stopped_instances(),
            vec![InstanceId::new("instance-a")]
        );
    }

    fn instance_request(instance_id: &str) -> InstanceCommandRequest {
        InstanceCommandRequest {
            deploy_id: mvp_deploy::DeployId::new("deploy-1"),
            instance_id: InstanceId::new(instance_id),
            service: ServiceName::new("web"),
            revision: RevisionId::new("rev-1"),
        }
    }

    async fn request_unit<T: serde::Serialize>(
        bus: &mvp_bus::BusActorHandle,
        session: &mvp_bus::BusSession,
        subject: &str,
        request: &T,
    ) {
        bus.request(
            session,
            Subject::parse(subject).expect("subject"),
            encode(request, "request").expect("request"),
            Duration::from_secs(1),
        )
        .await
        .expect("request");
    }

    async fn request_json<T: serde::Serialize, R: serde::de::DeserializeOwned>(
        bus: &mvp_bus::BusActorHandle,
        session: &mvp_bus::BusSession,
        subject: &str,
        request: &T,
    ) -> R {
        let response = bus
            .request(
                session,
                Subject::parse(subject).expect("subject"),
                encode(request, "request").expect("request"),
                Duration::from_secs(1),
            )
            .await
            .expect("request");
        mvp_deploy::decode(response.payload(), "reply").expect("reply")
    }
}
