use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::bridge::BridgeRuleSet;
use crate::facts::InMemoryFactSet;
use crate::grants::GrantBook;
use crate::message::{MessageId, ReplyInbox, ReplyPermit};
use crate::{
    BridgeFailure, BridgeOrigin, BridgeRuleId, BridgeRuleViolation, BridgeState, BusError,
    BusMessage, BusSession, Fact, FactAccess, FactAuthorizer, FactContentHash, FactKey,
    FactKeyPattern, FactPayload, FactWriteOutcome, Grant, IslandId, Payload, PrincipalId,
    QueueName, RequestManyPolicy, RequestTarget, ResponseEnvelope, ResponseMessage, Result,
    ServiceImport, StreamImport, Subject, SubjectPattern,
};

pub type HandlerOutcome = Result<()>;

pub(crate) type Handler = Arc<dyn Fn(RequestContext) -> HandlerOutcome + Send + Sync + 'static>;
pub(crate) const DEFAULT_PUBLISH_TIMEOUT: Duration = Duration::from_secs(30);

mod delivery_runtime;
mod fact_store;

use delivery_runtime::{Delivery, DeliveryRuntime, ReplySpec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusRuntimeConfig {
    delivery_workers: NonZeroUsize,
    delivery_queue_capacity: NonZeroUsize,
}

impl BusRuntimeConfig {
    #[must_use]
    pub fn default_for_mvp() -> Self {
        Self::with_delivery_workers(64)
    }

    #[must_use]
    pub fn with_delivery_workers(delivery_workers: usize) -> Self {
        let delivery_workers =
            NonZeroUsize::new(delivery_workers).expect("delivery workers must be at least one");
        let delivery_queue_capacity =
            NonZeroUsize::new(delivery_workers.get() * 4096).expect("queue capacity is non-zero");
        Self {
            delivery_workers,
            delivery_queue_capacity,
        }
    }

    #[must_use]
    pub fn with_delivery_queue_capacity(self, delivery_queue_capacity: usize) -> Self {
        let delivery_queue_capacity = NonZeroUsize::new(delivery_queue_capacity)
            .expect("delivery queue capacity must be at least one");
        Self {
            delivery_queue_capacity,
            ..self
        }
    }

    #[must_use]
    pub fn delivery_workers(self) -> usize {
        self.delivery_workers.get()
    }

    #[must_use]
    pub fn delivery_queue_capacity(self) -> usize {
        self.delivery_queue_capacity.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusRuntimeSnapshot {
    pub delivery_workers: usize,
    pub delivery_queue_capacity: usize,
    pub max_active_deliveries: usize,
    pub max_queued_deliveries: usize,
    pub enqueue_full_count: usize,
    pub enqueue_blocked_ns: u64,
}

pub(crate) fn deadline_after(timeout: Duration) -> Instant {
    Instant::now() + timeout
}

pub(crate) fn remaining_until(deadline: Instant, subject: impl Into<String>) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| BusError::Timeout {
            subject: subject.into(),
        })
}

struct RequestDispatchSpec {
    target: RequestTarget,
    subject: Subject,
    payload: Payload,
    deadline: Instant,
    mode: RequestDispatchMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestDispatchMode {
    Broadcast,
    One,
    Many { max: usize },
}

struct AuthorizedRequestDispatchSpec {
    island: IslandId,
    principal: PrincipalId,
    target: RequestTarget,
    subject: Subject,
    payload: Payload,
    deadline: Instant,
    mode: RequestDispatchMode,
    bridge_origin: Option<BridgeOrigin>,
    tx: mpsc::Sender<ResponseEnvelope>,
}

impl RequestDispatchMode {
    fn max_deliveries(self) -> Option<usize> {
        match self {
            Self::Broadcast => None,
            Self::One => Some(1),
            Self::Many { max } => Some(max),
        }
    }

    fn includes_queue_groups(self) -> bool {
        match self {
            Self::Broadcast | Self::One => true,
            Self::Many { .. } => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct QueueGroupKey {
    island: IslandId,
    queue: QueueName,
}

impl QueueGroupKey {
    fn new(island: &IslandId, queue: &QueueName) -> Self {
        Self {
            island: island.clone(),
            queue: queue.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RequestManyDeadlinePolicy {
    pub max: usize,
    pub deadline: Instant,
}

struct StreamBridgeTarget {
    rule_id: BridgeRuleId,
    source_island: IslandId,
    local_island: IslandId,
    bridge_principal: PrincipalId,
    mapped_subject: Subject,
    state: BridgeState,
}

#[derive(Debug, Clone)]
pub struct RequestContext {
    pub message: BusMessage,
    reply_permit: Option<ReplyPermit>,
}

impl RequestContext {
    #[must_use]
    pub(crate) fn new(message: BusMessage, reply_permit: Option<ReplyPermit>) -> Self {
        Self {
            message,
            reply_permit,
        }
    }

    pub fn reply(&self, payload: impl Into<Payload>) -> Result<()> {
        let Some(reply_permit) = &self.reply_permit else {
            return Err(BusError::NoReplyPermit {
                island: self.message.island().clone(),
                subject: Box::new(self.message.subject().clone()),
            });
        };
        reply_permit.respond(payload)
    }

    pub fn reply_as(&self, principal: &PrincipalId, payload: impl Into<Payload>) -> Result<()> {
        let Some(reply_permit) = &self.reply_permit else {
            return Err(BusError::NoReplyPermit {
                island: self.message.island().clone(),
                subject: Box::new(self.message.subject().clone()),
            });
        };
        reply_permit.respond_as(principal, payload)
    }
}

#[derive(Clone)]
pub struct MemoryBus {
    inner: Arc<Mutex<Inner>>,
    runtime: Arc<DeliveryRuntime>,
}

impl MemoryBus {
    fn new() -> Self {
        Self::with_config(BusRuntimeConfig::default_for_mvp())
    }

    fn with_config(config: BusRuntimeConfig) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::new())),
            runtime: Arc::new(DeliveryRuntime::new(config)),
        }
    }

    #[must_use]
    pub fn new_with_authority() -> (Self, BusAuthority) {
        let bus = Self::new();
        (bus.clone(), BusAuthority { bus: bus.clone() })
    }

    #[must_use]
    pub fn new_with_authority_and_config(config: BusRuntimeConfig) -> (Self, BusAuthority) {
        let bus = Self::with_config(config);
        (bus.clone(), BusAuthority { bus: bus.clone() })
    }

    #[must_use]
    pub fn runtime_snapshot(&self) -> BusRuntimeSnapshot {
        self.runtime.snapshot()
    }

    fn set_grant(&self, island: IslandId, principal: PrincipalId, grant: Grant) -> BusSession {
        self.inner
            .lock()
            .expect("memory bus mutex poisoned")
            .grants
            .set(island.clone(), principal.clone(), grant);
        BusSession::new(island, principal)
    }

    fn revoke_grant(&self, island: &IslandId, principal: &PrincipalId) -> bool {
        self.inner
            .lock()
            .expect("memory bus mutex poisoned")
            .grants
            .revoke(island, principal)
    }

    fn add_service_import(&self, import: ServiceImport) -> Result<()> {
        let mut inner = self.inner.lock().expect("memory bus mutex poisoned");
        if inner.has_local_responder(import.local_island(), import.local_subject()) {
            return Err(BusError::BridgeRuleInvalid {
                violation: BridgeRuleViolation::LocalResponderConflict {
                    local_island: import.local_island().clone(),
                    local_subject: import.local_subject().clone(),
                },
            });
        }
        inner
            .bridges
            .add_service_import(import)
            .map_err(|violation| BusError::BridgeRuleInvalid { violation })
    }

    fn add_stream_import(&self, import: StreamImport) -> Result<()> {
        self.inner
            .lock()
            .expect("memory bus mutex poisoned")
            .bridges
            .add_stream_import(import)
            .map_err(|violation| BusError::BridgeRuleInvalid { violation })
    }

    fn set_service_import_state(&self, id: &BridgeRuleId, state: BridgeState) -> Result<()> {
        self.inner
            .lock()
            .expect("memory bus mutex poisoned")
            .bridges
            .set_service_import_state(id, state)
            .map_err(|violation| BusError::BridgeRuleInvalid { violation })
    }

    fn set_stream_import_state(&self, id: &BridgeRuleId, state: BridgeState) -> Result<()> {
        self.inner
            .lock()
            .expect("memory bus mutex poisoned")
            .bridges
            .set_stream_import_state(id, state)
            .map_err(|violation| BusError::BridgeRuleInvalid { violation })
    }

    pub fn subscribe<F>(
        &self,
        session: &BusSession,
        pattern: SubjectPattern,
        handler: F,
    ) -> Result<u64>
    where
        F: Fn(RequestContext) -> HandlerOutcome + Send + Sync + 'static,
    {
        self.subscribe_handler(session, pattern, Arc::new(handler))
    }

    pub(crate) fn subscribe_handler(
        &self,
        session: &BusSession,
        pattern: SubjectPattern,
        handler: Handler,
    ) -> Result<u64> {
        let mut inner = self.inner.lock().expect("memory bus mutex poisoned");
        inner.ensure_not_draining()?;
        let island = session.island().clone();
        let principal = session.principal().clone();
        if !inner.grants.can_subscribe(&island, &principal, &pattern) {
            return Err(BusError::UnauthorizedSubscribe {
                island,
                principal,
                pattern: Box::new(pattern),
            });
        }
        if let Some(local_subject) = inner.service_import_subject_matching(&island, &pattern) {
            return Err(BusError::BridgeRuleInvalid {
                violation: BridgeRuleViolation::LocalResponderConflict {
                    local_island: island,
                    local_subject,
                },
            });
        }
        let id = inner.next_id();
        let subscriber_index = inner.subscribers.len();
        inner
            .subscriber_indexes_by_island
            .entry(island.clone())
            .or_default()
            .push(subscriber_index);
        inner.subscribers.push(Subscriber {
            id,
            island,
            principal,
            pattern,
            handler,
        });
        Ok(id)
    }

    pub fn queue_subscribe<F>(
        &self,
        session: &BusSession,
        pattern: SubjectPattern,
        queue: impl Into<QueueName>,
        handler: F,
    ) -> Result<u64>
    where
        F: Fn(RequestContext) -> HandlerOutcome + Send + Sync + 'static,
    {
        self.queue_subscribe_handler(session, pattern, queue.into(), Arc::new(handler))
    }

    pub(crate) fn queue_subscribe_handler(
        &self,
        session: &BusSession,
        pattern: SubjectPattern,
        queue: QueueName,
        handler: Handler,
    ) -> Result<u64> {
        let mut inner = self.inner.lock().expect("memory bus mutex poisoned");
        inner.ensure_not_draining()?;
        let island = session.island().clone();
        let principal = session.principal().clone();
        if !inner
            .grants
            .can_queue_subscribe(&island, &principal, &pattern, &queue)
        {
            return Err(BusError::UnauthorizedQueue {
                island,
                principal,
                pattern: Box::new(pattern),
                queue,
            });
        }
        if let Some(local_subject) = inner.service_import_subject_matching(&island, &pattern) {
            return Err(BusError::BridgeRuleInvalid {
                violation: BridgeRuleViolation::LocalResponderConflict {
                    local_island: island,
                    local_subject,
                },
            });
        }
        let id = inner.next_id();
        let subscriber_index = inner.queue_subscribers.len();
        inner
            .queue_subscriber_indexes_by_island
            .entry(island.clone())
            .or_default()
            .push(subscriber_index);
        inner.queue_subscribers.push(QueueSubscriber {
            id,
            island,
            principal,
            pattern,
            queue,
            handler,
        });
        Ok(id)
    }

    pub fn publish(
        &self,
        session: &BusSession,
        subject: Subject,
        payload: impl Into<Payload>,
    ) -> Result<()> {
        self.publish_until(
            session,
            subject,
            payload,
            deadline_after(DEFAULT_PUBLISH_TIMEOUT),
        )
    }

    pub(crate) fn publish_until(
        &self,
        session: &BusSession,
        subject: Subject,
        payload: impl Into<Payload>,
        deadline: Instant,
    ) -> Result<()> {
        let payload = payload.into();
        let subject_display = subject.to_string();
        let dispatch = {
            let mut inner = self.inner.lock().expect("memory bus mutex poisoned");
            inner.ensure_not_draining()?;
            let island = session.island().clone();
            let principal = session.principal().clone();
            if !inner.grants.can_publish(&island, &principal, &subject) {
                return Err(BusError::UnauthorizedPublish {
                    island,
                    principal,
                    subject: Box::new(subject),
                });
            }
            let message = BusMessage::new(
                MessageId::new(inner.next_id()),
                island,
                subject.clone(),
                principal,
                payload,
            );
            let mut dispatch = inner.dispatch_for_subject(
                &subject,
                message.clone(),
                None,
                RequestDispatchMode::Broadcast,
            );
            let stream_dispatch = inner.stream_bridge_deliveries_for(&message);
            dispatch.extend(stream_dispatch);
            dispatch
        };

        self.runtime
            .run_and_wait_until(dispatch, deadline, subject_display)
    }

    pub fn request(
        &self,
        session: &BusSession,
        subject: Subject,
        payload: impl Into<Payload>,
        timeout: Duration,
    ) -> Result<ResponseMessage> {
        self.request_until(session, subject, payload, deadline_after(timeout))
    }

    pub(crate) fn request_until(
        &self,
        session: &BusSession,
        subject: Subject,
        payload: impl Into<Payload>,
        deadline: Instant,
    ) -> Result<ResponseMessage> {
        let subject_display = subject.to_string();
        let (rx, dispatch) = self.prepare_request(
            session,
            RequestDispatchSpec {
                target: RequestTarget::Subject(subject.clone()),
                subject: subject.clone(),
                payload: payload.into(),
                deadline,
                mode: RequestDispatchMode::One,
            },
        )?;
        self.runtime
            .spawn_until(dispatch, deadline, subject_display.clone())?;
        match rx.recv_timeout(remaining_until(deadline, subject_display.clone())?) {
            Ok(ResponseEnvelope::Reply(response)) => Ok(response),
            Ok(ResponseEnvelope::HandlerError(error)) => Err(error),
            Err(_) => Err(BusError::Timeout {
                subject: subject_display,
            }),
        }
    }

    pub fn request_many(
        &self,
        session: &BusSession,
        target: RequestTarget,
        subject: Subject,
        payload: impl Into<Payload>,
        policy: RequestManyPolicy,
    ) -> Result<Vec<ResponseMessage>> {
        self.request_many_until(
            session,
            target,
            subject,
            payload,
            RequestManyDeadlinePolicy {
                max: policy.max,
                deadline: deadline_after(policy.deadline),
            },
        )
    }

    pub(crate) fn request_many_until(
        &self,
        session: &BusSession,
        target: RequestTarget,
        subject: Subject,
        payload: impl Into<Payload>,
        policy: RequestManyDeadlinePolicy,
    ) -> Result<Vec<ResponseMessage>> {
        if policy.max == 0 {
            self.inner
                .lock()
                .expect("memory bus mutex poisoned")
                .authorize_request(session, &target, &subject)?;
            return Ok(Vec::new());
        }
        let target_display = target.display();
        let target_for_error = target.clone();
        let (rx, dispatch) = self.prepare_request(
            session,
            RequestDispatchSpec {
                target,
                subject,
                payload: payload.into(),
                deadline: policy.deadline,
                mode: RequestDispatchMode::Many { max: policy.max },
            },
        )?;
        let expected = dispatch.len();
        self.runtime
            .spawn_until(dispatch, policy.deadline, target_display.clone())?;

        let mut replies = Vec::with_capacity(expected);
        while replies.len() < expected {
            let now = Instant::now();
            if now >= policy.deadline {
                break;
            }
            match rx.recv_timeout(policy.deadline.saturating_duration_since(now)) {
                Ok(ResponseEnvelope::Reply(response)) => replies.push(response),
                Ok(ResponseEnvelope::HandlerError(error)) => return Err(error),
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        if replies.len() < expected {
            return Err(BusError::IncompleteResponses {
                target: Box::new(target_for_error),
                expected,
                received: replies.len(),
            });
        }
        Ok(replies)
    }

    pub fn drain(&self, session: &BusSession, deadline: Duration) -> Result<()> {
        {
            let mut inner = self.inner.lock().expect("memory bus mutex poisoned");
            if !inner
                .grants
                .can_drain(session.island(), session.principal())
            {
                return Err(BusError::UnauthorizedDrain {
                    island: session.island().clone(),
                    principal: session.principal().clone(),
                });
            }
            inner.draining = true;
        }
        self.runtime.wait_for_idle(deadline)
    }

    fn prepare_request(
        &self,
        session: &BusSession,
        spec: RequestDispatchSpec,
    ) -> Result<(mpsc::Receiver<ResponseEnvelope>, Vec<Delivery>)> {
        let (tx, rx) = mpsc::channel();
        let dispatch = {
            let mut inner = self.inner.lock().expect("memory bus mutex poisoned");
            inner.ensure_not_draining()?;
            if let RequestTarget::Subject(local_subject) = &spec.target
                && let Some(import) = inner
                    .bridges
                    .service_import_for(session.island(), local_subject)
                    .cloned()
            {
                let imported = inner.prepare_service_import_request(session, import, spec, tx)?;
                imported
            } else {
                inner.authorize_request(session, &spec.target, &spec.subject)?;
                let dispatch =
                    inner.prepare_authorized_request_dispatch(AuthorizedRequestDispatchSpec {
                        island: session.island().clone(),
                        principal: session.principal().clone(),
                        target: spec.target,
                        subject: spec.subject,
                        payload: spec.payload,
                        deadline: spec.deadline,
                        mode: spec.mode,
                        bridge_origin: None,
                        tx,
                    })?;
                dispatch
            }
        };

        Ok((rx, dispatch))
    }
}

#[derive(Clone)]
pub struct BusAuthority {
    bus: MemoryBus,
}

impl BusAuthority {
    pub fn grant(&self, principal: PrincipalId, grant: Grant) -> BusSession {
        self.grant_in(IslandId::default_for_mvp(), principal, grant)
    }

    pub fn grant_in(&self, island: IslandId, principal: PrincipalId, grant: Grant) -> BusSession {
        self.bus.set_grant(island, principal, grant)
    }

    pub fn revoke(&self, session: &BusSession) -> bool {
        self.revoke_in(session.island(), session.principal())
    }

    pub fn revoke_in(&self, island: &IslandId, principal: &PrincipalId) -> bool {
        self.bus.revoke_grant(island, principal)
    }

    pub fn add_service_import(&self, import: ServiceImport) -> Result<()> {
        self.bus.add_service_import(import)
    }

    pub fn add_stream_import(&self, import: StreamImport) -> Result<()> {
        self.bus.add_stream_import(import)
    }

    pub fn set_service_import_state(&self, id: &BridgeRuleId, state: BridgeState) -> Result<()> {
        self.bus.set_service_import_state(id, state)
    }

    pub fn set_stream_import_state(&self, id: &BridgeRuleId, state: BridgeState) -> Result<()> {
        self.bus.set_stream_import_state(id, state)
    }
}

struct Inner {
    subscribers: Vec<Subscriber>,
    subscriber_indexes_by_island: BTreeMap<IslandId, Vec<usize>>,
    queue_subscribers: Vec<QueueSubscriber>,
    queue_subscriber_indexes_by_island: BTreeMap<IslandId, Vec<usize>>,
    queue_cursor: BTreeMap<QueueGroupKey, usize>,
    grants: GrantBook,
    facts: InMemoryFactSet,
    bridges: BridgeRuleSet,
    next_id: u64,
    draining: bool,
}

impl Inner {
    fn new() -> Self {
        Self {
            subscribers: Vec::new(),
            subscriber_indexes_by_island: BTreeMap::new(),
            queue_subscribers: Vec::new(),
            queue_subscriber_indexes_by_island: BTreeMap::new(),
            queue_cursor: BTreeMap::new(),
            grants: GrantBook::default(),
            facts: InMemoryFactSet::default(),
            bridges: BridgeRuleSet::default(),
            next_id: 1,
            draining: false,
        }
    }

    fn ensure_not_draining(&self) -> Result<()> {
        if self.draining {
            return Err(BusError::Draining);
        }
        Ok(())
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn has_local_responder(&self, island: &IslandId, subject: &Subject) -> bool {
        self.subscriber_indexes_by_island
            .get(island)
            .into_iter()
            .flat_map(|indexes| indexes.iter())
            .filter_map(|index| self.subscribers.get(*index))
            .any(|subscriber| subscriber.pattern.matches(subject))
            || self
                .queue_subscriber_indexes_by_island
                .get(island)
                .into_iter()
                .flat_map(|indexes| indexes.iter())
                .filter_map(|index| self.queue_subscribers.get(*index))
                .any(|subscriber| subscriber.pattern.matches(subject))
    }

    fn service_import_subject_matching(
        &self,
        island: &IslandId,
        pattern: &SubjectPattern,
    ) -> Option<Subject> {
        self.bridges
            .service_imports()
            .iter()
            .find(|import| {
                import.local_island() == island && pattern.matches(import.local_subject())
            })
            .map(|import| import.local_subject().clone())
    }

    fn stream_bridge_deliveries_for(&mut self, message: &BusMessage) -> Vec<Delivery> {
        if message.bridge_origin().is_some() {
            return Vec::new();
        }

        let imports = self
            .bridges
            .matching_stream_imports(message.island(), message.subject())
            .map(|import| StreamBridgeTarget {
                rule_id: import.id().clone(),
                source_island: import.source_island().clone(),
                local_island: import.local_island().clone(),
                bridge_principal: import.bridge_principal().clone(),
                mapped_subject: import.transform().apply_after_match(message.subject()),
                state: import.state(),
            })
            .collect::<Vec<_>>();
        let mut dispatch = Vec::new();
        for import in imports {
            if import.state != BridgeState::Enabled {
                continue;
            }
            if !self.grants.can_bridge_export(
                &import.source_island,
                &import.bridge_principal,
                message.subject(),
            ) {
                continue;
            }
            if !self.grants.can_publish(
                &import.local_island,
                &import.bridge_principal,
                &import.mapped_subject,
            ) {
                continue;
            }
            let mut bridged_message = BusMessage::new(
                MessageId::new(self.next_id()),
                import.local_island.clone(),
                import.mapped_subject.clone(),
                import.bridge_principal.clone(),
                message.payload().clone(),
            );
            bridged_message.set_bridge_origin(BridgeOrigin::new(
                import.rule_id,
                message.island().clone(),
                message.principal().clone(),
                message.subject().clone(),
            ));
            dispatch.extend(self.dispatch_for_subject(
                &import.mapped_subject,
                bridged_message,
                None,
                RequestDispatchMode::Broadcast,
            ));
        }
        dispatch
    }

    fn authorize_request(
        &self,
        session: &BusSession,
        target: &RequestTarget,
        subject: &Subject,
    ) -> Result<()> {
        self.ensure_not_draining()?;
        if !self
            .grants
            .can_publish_target(session.island(), session.principal(), target)
        {
            return Err(BusError::UnauthorizedRequestTarget {
                island: session.island().clone(),
                principal: session.principal().clone(),
                target: Box::new(target.clone()),
            });
        }
        if !self
            .grants
            .can_publish(session.island(), session.principal(), subject)
        {
            return Err(BusError::UnauthorizedPublish {
                island: session.island().clone(),
                principal: session.principal().clone(),
                subject: Box::new(subject.clone()),
            });
        }
        Ok(())
    }

    fn prepare_service_import_request(
        &mut self,
        session: &BusSession,
        import: ServiceImport,
        spec: RequestDispatchSpec,
        tx: mpsc::Sender<ResponseEnvelope>,
    ) -> Result<Vec<Delivery>> {
        if !self.grants.can_publish(
            session.island(),
            session.principal(),
            import.local_subject(),
        ) {
            return Err(BusError::UnauthorizedPublish {
                island: session.island().clone(),
                principal: session.principal().clone(),
                subject: Box::new(import.local_subject().clone()),
            });
        }
        match import.state() {
            BridgeState::Enabled => {}
            BridgeState::Disabled => {
                return Err(BusError::BridgeUnavailable {
                    rule_id: import.id().clone(),
                    local_island: import.local_island().clone(),
                    remote_island: import.remote_island().clone(),
                    subject: Box::new(import.local_subject().clone()),
                    failure: BridgeFailure::Disabled,
                });
            }
        }
        if !self.grants.can_publish(
            import.remote_island(),
            import.bridge_principal(),
            import.remote_subject(),
        ) {
            return Err(BusError::UnauthorizedPublish {
                island: import.remote_island().clone(),
                principal: import.bridge_principal().clone(),
                subject: Box::new(import.remote_subject().clone()),
            });
        }

        let remote_mode = match spec.mode {
            RequestDispatchMode::One => RequestDispatchMode::One,
            RequestDispatchMode::Many { max: 1 } => RequestDispatchMode::One,
            RequestDispatchMode::Many { max } => {
                return Err(BusError::BridgeRequestManyUnsupported {
                    rule_id: import.id().clone(),
                    local_island: import.local_island().clone(),
                    remote_island: import.remote_island().clone(),
                    subject: Box::new(import.local_subject().clone()),
                    requested: max,
                });
            }
            RequestDispatchMode::Broadcast => unreachable!("service imports are request-only"),
        };

        self.prepare_authorized_request_dispatch(AuthorizedRequestDispatchSpec {
            island: import.remote_island().clone(),
            principal: import.bridge_principal().clone(),
            target: RequestTarget::Subject(import.remote_subject().clone()),
            subject: import.remote_subject().clone(),
            payload: spec.payload,
            deadline: spec.deadline,
            mode: remote_mode,
            bridge_origin: Some(BridgeOrigin::new(
                import.id().clone(),
                session.island().clone(),
                session.principal().clone(),
                import.local_subject().clone(),
            )),
            tx,
        })
    }

    fn prepare_authorized_request_dispatch(
        &mut self,
        spec: AuthorizedRequestDispatchSpec,
    ) -> Result<Vec<Delivery>> {
        let AuthorizedRequestDispatchSpec {
            island,
            principal,
            target,
            subject,
            payload,
            deadline,
            mode,
            bridge_origin,
            tx,
        } = spec;
        let id = MessageId::new(self.next_id());
        let inbox = ReplyInbox::new(Subject::parse(format!("_INBOX.{}", id.value()))?);
        let mut message = BusMessage::new(id, island, subject, principal, payload);
        message.set_reply_to(inbox.clone());
        if let Some(bridge_origin) = bridge_origin {
            message.set_bridge_origin(bridge_origin);
        }
        let reply = ReplySpec {
            inbox,
            expires_at: deadline,
            tx,
        };
        let dispatch = self.dispatch_for_target(&target, message, Some(reply), mode);
        if dispatch.is_empty() {
            return Err(BusError::NoResponders {
                target: Box::new(target),
            });
        }
        Ok(dispatch)
    }

    fn dispatch_for_subject(
        &mut self,
        subject: &Subject,
        message: BusMessage,
        reply: Option<ReplySpec>,
        mode: RequestDispatchMode,
    ) -> Vec<Delivery> {
        let target = RequestTarget::Subject(subject.clone());
        self.dispatch_for_target(&target, message, reply, mode)
    }

    fn dispatch_for_target(
        &mut self,
        target: &RequestTarget,
        message: BusMessage,
        reply: Option<ReplySpec>,
        mode: RequestDispatchMode,
    ) -> Vec<Delivery> {
        let mut dispatch = Vec::new();
        let max_deliveries = mode.max_deliveries();

        for subscriber in self
            .subscriber_indexes_by_island
            .get(message.island())
            .into_iter()
            .flat_map(|indexes| indexes.iter())
            .filter_map(|index| self.subscribers.get(*index))
        {
            if target_matches_subject_pattern(target, &subscriber.pattern)
                && self.grants.can_subscribe(
                    &subscriber.island,
                    &subscriber.principal,
                    &subscriber.pattern,
                )
                && reply.as_ref().is_none_or(|_| {
                    self.grants
                        .can_respond(&subscriber.island, &subscriber.principal)
                })
            {
                dispatch.push(subscriber.delivery(message.clone(), reply.clone()));
                if max_deliveries.is_some_and(|max| dispatch.len() >= max) {
                    return dispatch;
                }
            }
        }

        if !mode.includes_queue_groups() {
            return dispatch;
        }

        let mut groups = BTreeMap::<QueueGroupKey, Vec<usize>>::new();
        for subscriber_index in self
            .queue_subscriber_indexes_by_island
            .get(message.island())
            .into_iter()
            .flat_map(|indexes| indexes.iter().copied())
        {
            let subscriber = &self.queue_subscribers[subscriber_index];
            if target_matches_subject_pattern(target, &subscriber.pattern)
                && self.grants.can_queue_subscribe(
                    &subscriber.island,
                    &subscriber.principal,
                    &subscriber.pattern,
                    &subscriber.queue,
                )
                && reply.as_ref().is_none_or(|_| {
                    self.grants
                        .can_respond(&subscriber.island, &subscriber.principal)
                })
            {
                groups
                    .entry(QueueGroupKey::new(&subscriber.island, &subscriber.queue))
                    .or_default()
                    .push(subscriber_index);
            }
        }
        for (group_key, subscriber_indices) in groups {
            let cursor = self.queue_cursor.entry(group_key).or_insert(0);
            let selected_index = subscriber_indices[*cursor % subscriber_indices.len()];
            *cursor += 1;
            let selected = &self.queue_subscribers[selected_index];
            dispatch.push(selected.delivery(message.clone(), reply.clone()));
            if max_deliveries.is_some_and(|max| dispatch.len() >= max) {
                return dispatch;
            }
        }

        dispatch
    }
}

fn target_matches_subject_pattern(target: &RequestTarget, pattern: &SubjectPattern) -> bool {
    match target {
        RequestTarget::Subject(subject) => pattern.matches(subject),
        RequestTarget::Pattern(target_pattern) => target_pattern.overlaps(pattern),
    }
}

#[derive(Clone)]
struct Subscriber {
    id: u64,
    island: IslandId,
    principal: PrincipalId,
    pattern: SubjectPattern,
    handler: Handler,
}

impl Subscriber {
    fn delivery(&self, message: BusMessage, reply: Option<ReplySpec>) -> Delivery {
        Delivery::new(
            self.id,
            self.principal.clone(),
            Arc::clone(&self.handler),
            message,
            reply,
        )
    }
}

#[derive(Clone)]
struct QueueSubscriber {
    id: u64,
    island: IslandId,
    principal: PrincipalId,
    pattern: SubjectPattern,
    queue: QueueName,
    handler: Handler,
}

impl QueueSubscriber {
    fn delivery(&self, message: BusMessage, reply: Option<ReplySpec>) -> Delivery {
        Delivery::new(
            self.id,
            self.principal.clone(),
            Arc::clone(&self.handler),
            message,
            reply,
        )
    }
}

#[cfg(test)]
mod tests;
