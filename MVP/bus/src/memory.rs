use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{
    Receiver as DeliveryReceiver, SendTimeoutError, Sender as DeliverySender, TrySendError,
};

use crate::bridge::BridgeRuleSet;
use crate::facts::InMemoryFactSet;
use crate::grants::GrantBook;
use crate::message::{MessageId, ReplyInbox, ReplyPermit};
use crate::{
    BridgeFailure, BridgeOrigin, BridgeRuleId, BridgeRuleViolation, BridgeState, BusError,
    BusMessage, BusSession, Fact, FactContentHash, FactKey, FactWriteOutcome, Grant, IslandId,
    Payload, PrincipalId, QueueName, RequestManyPolicy, RequestTarget, ResponseEnvelope,
    ResponseMessage, Result, ServiceImport, StreamImport, Subject, SubjectPattern,
};

pub type HandlerOutcome = Result<()>;

pub(crate) type Handler = Arc<dyn Fn(RequestContext) -> HandlerOutcome + Send + Sync + 'static>;
pub(crate) const DEFAULT_PUBLISH_TIMEOUT: Duration = Duration::from_secs(30);

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

#[derive(Debug, Default)]
struct Inflight {
    count: Mutex<usize>,
    idle: Condvar,
}

impl Inflight {
    fn add(&self, count: usize) {
        let mut in_flight = self.count.lock().expect("in-flight mutex poisoned");
        *in_flight += count;
    }

    fn complete(&self) {
        let mut in_flight = self.count.lock().expect("in-flight mutex poisoned");
        *in_flight = in_flight.saturating_sub(1);
        if *in_flight == 0 {
            self.idle.notify_all();
        }
    }

    fn complete_many(&self, count: usize) {
        let mut in_flight = self.count.lock().expect("in-flight mutex poisoned");
        *in_flight = in_flight.saturating_sub(count);
        if *in_flight == 0 {
            self.idle.notify_all();
        }
    }

    fn guard(self: &Arc<Self>) -> InflightGuard {
        InflightGuard {
            inflight: Arc::clone(self),
        }
    }

    fn wait_for_idle(&self, deadline: Duration) -> Result<()> {
        let started = Instant::now();
        let mut in_flight = self.count.lock().expect("in-flight mutex poisoned");
        while *in_flight > 0 {
            let Some(remaining) = deadline.checked_sub(started.elapsed()) else {
                return Err(BusError::Timeout {
                    subject: String::from("drain"),
                });
            };
            let (guard, wait_result) = self
                .idle
                .wait_timeout(in_flight, remaining)
                .expect("in-flight condvar wait poisoned");
            in_flight = guard;
            if wait_result.timed_out() && *in_flight > 0 {
                return Err(BusError::Timeout {
                    subject: String::from("drain"),
                });
            }
        }
        Ok(())
    }
}

struct InflightGuard {
    inflight: Arc<Inflight>,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.inflight.complete();
    }
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
    inflight: Arc<Inflight>,
    runtime: Arc<DeliveryRuntime>,
}

impl MemoryBus {
    fn new() -> Self {
        Self::with_config(BusRuntimeConfig::default_for_mvp())
    }

    fn with_config(config: BusRuntimeConfig) -> Self {
        let inflight = Arc::new(Inflight::default());
        Self {
            inner: Arc::new(Mutex::new(Inner::new())),
            runtime: Arc::new(DeliveryRuntime::new(config, Arc::clone(&inflight))),
            inflight,
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
            self.inflight.add(dispatch.len());
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
        self.inflight.wait_for_idle(deadline)
    }

    pub fn write_fact(
        &self,
        session: &BusSession,
        key: FactKey,
        content_hash: FactContentHash,
    ) -> Result<FactWriteOutcome> {
        let mut inner = self.inner.lock().expect("memory bus mutex poisoned");
        if !inner
            .grants
            .can_write_fact(session.island(), session.principal(), &key)
        {
            return Err(BusError::UnauthorizedFactWrite {
                island: session.island().clone(),
                principal: session.principal().clone(),
                key: Box::new(key),
            });
        }
        inner.facts.write(
            session.island().clone(),
            session.principal().clone(),
            key,
            content_hash,
        )
    }

    pub fn read_fact(&self, session: &BusSession, key: &FactKey) -> Result<Option<Fact>> {
        let inner = self.inner.lock().expect("memory bus mutex poisoned");
        if !inner
            .grants
            .can_read_fact(session.island(), session.principal(), key)
        {
            return Err(BusError::UnauthorizedFactRead {
                island: session.island().clone(),
                principal: session.principal().clone(),
                key: Box::new(key.clone()),
            });
        }
        Ok(inner.facts.read(session.island(), key))
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
                self.inflight.add(imported.len());
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
                self.inflight.add(dispatch.len());
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
            BridgeState::RemoteUnavailable => {
                return Err(BusError::BridgeUnavailable {
                    rule_id: import.id().clone(),
                    local_island: import.local_island().clone(),
                    remote_island: import.remote_island().clone(),
                    subject: Box::new(import.local_subject().clone()),
                    failure: BridgeFailure::RemoteUnavailable,
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

#[derive(Clone)]
struct ReplySpec {
    inbox: ReplyInbox,
    expires_at: Instant,
    tx: mpsc::Sender<ResponseEnvelope>,
}

struct Delivery {
    subscriber_id: u64,
    principal: PrincipalId,
    handler: Handler,
    message: BusMessage,
    reply: Option<ReplySpec>,
}

impl Delivery {
    fn new(
        subscriber_id: u64,
        principal: PrincipalId,
        handler: Handler,
        message: BusMessage,
        reply: Option<ReplySpec>,
    ) -> Self {
        Self {
            subscriber_id,
            principal,
            handler,
            message,
            reply,
        }
    }

    fn invoke(self) -> Result<()> {
        let error_tx = self.reply.as_ref().map(|reply| reply.tx.clone());
        let handler = Arc::clone(&self.handler);
        let context = self.into_context();
        match handler(context) {
            Ok(()) => Ok(()),
            Err(error) => {
                if let Some(tx) = error_tx {
                    let _ = tx.send(ResponseEnvelope::HandlerError(error.clone()));
                }
                Err(error)
            }
        }
    }

    fn into_context(self) -> RequestContext {
        let permit = self.reply.as_ref().map(|reply| {
            ReplyPermit::new(
                reply.inbox.clone(),
                self.message.id(),
                self.message.island().clone(),
                self.principal.clone(),
                reply.expires_at,
                reply.tx.clone(),
            )
        });
        let mut message = self.message;
        if let Some(reply) = &self.reply {
            message.set_reply_to(reply.inbox.clone());
        }
        let _subscriber_id = self.subscriber_id;
        RequestContext::new(message, permit)
    }
}

struct DeliveryRuntime {
    config: BusRuntimeConfig,
    sender: DeliverySender<DeliveryJob>,
    inflight: Arc<Inflight>,
    metrics: Arc<DeliveryRuntimeMetrics>,
}

impl DeliveryRuntime {
    fn new(config: BusRuntimeConfig, inflight: Arc<Inflight>) -> Self {
        let (sender, receiver) = crossbeam_channel::bounded(config.delivery_queue_capacity());
        let metrics = Arc::new(DeliveryRuntimeMetrics::default());
        for worker_index in 0..config.delivery_workers() {
            spawn_delivery_worker(
                worker_index,
                receiver.clone(),
                Arc::clone(&metrics),
                Arc::clone(&inflight),
            );
        }
        Self {
            config,
            sender,
            inflight,
            metrics,
        }
    }

    fn spawn_until(
        &self,
        deliveries: Vec<Delivery>,
        deadline: Instant,
        timeout_subject: String,
    ) -> Result<()> {
        self.enqueue(deliveries, None, deadline, timeout_subject)
    }

    fn run_and_wait_until(
        &self,
        deliveries: Vec<Delivery>,
        deadline: Instant,
        timeout_subject: String,
    ) -> Result<()> {
        if deliveries.is_empty() {
            return Ok(());
        }
        let expected = deliveries.len();
        let (result_tx, result_rx) = mpsc::channel();
        self.enqueue(
            deliveries,
            Some(result_tx),
            deadline,
            timeout_subject.clone(),
        )?;

        let mut first_error = None;
        for _ in 0..expected {
            match result_rx.recv_timeout(remaining_until(deadline, timeout_subject.clone())?) {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(BusError::Timeout {
                        subject: timeout_subject,
                    });
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(BusError::DeliveryRuntimeStopped);
                }
            }
        }

        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn snapshot(&self) -> BusRuntimeSnapshot {
        BusRuntimeSnapshot {
            delivery_workers: self.config.delivery_workers(),
            delivery_queue_capacity: self.config.delivery_queue_capacity(),
            max_active_deliveries: self.metrics.max_active_deliveries(),
            max_queued_deliveries: self.metrics.max_queued_deliveries(),
            enqueue_full_count: self.metrics.enqueue_full_count(),
            enqueue_blocked_ns: self.metrics.enqueue_blocked_ns(),
        }
    }

    fn enqueue(
        &self,
        deliveries: Vec<Delivery>,
        result_tx: Option<mpsc::Sender<Result<()>>>,
        deadline: Instant,
        timeout_subject: String,
    ) -> Result<()> {
        let total = deliveries.len();
        for (queued, delivery) in deliveries.into_iter().enumerate() {
            let queue_len = self.sender.len();
            self.metrics.record_queue_depth(queue_len);
            let job = DeliveryJob {
                delivery,
                result_tx: result_tx.clone(),
            };
            let result = match self.sender.try_send(job) {
                Ok(()) => Ok(()),
                Err(TrySendError::Full(job)) => {
                    self.metrics
                        .record_queue_depth(self.config.delivery_queue_capacity());
                    let send_started = Instant::now();
                    let remaining = match remaining_until(deadline, timeout_subject.clone()) {
                        Ok(remaining) => remaining,
                        Err(error) => {
                            self.inflight.complete_many(total - queued);
                            return Err(error);
                        }
                    };
                    let result =
                        self.sender
                            .send_timeout(job, remaining)
                            .map_err(|error| match error {
                                SendTimeoutError::Timeout(_) => BusError::Timeout {
                                    subject: timeout_subject.clone(),
                                },
                                SendTimeoutError::Disconnected(_) => {
                                    BusError::DeliveryRuntimeStopped
                                }
                            });
                    self.metrics.record_enqueue_block(send_started.elapsed());
                    result
                }
                Err(TrySendError::Disconnected(_)) => Err(BusError::DeliveryRuntimeStopped),
            };
            if let Err(error) = result {
                self.inflight.complete_many(total - queued);
                return Err(error);
            }
            self.metrics.record_queue_depth(self.sender.len());
        }
        Ok(())
    }
}

struct DeliveryJob {
    delivery: Delivery,
    result_tx: Option<mpsc::Sender<Result<()>>>,
}

#[derive(Debug, Default)]
struct DeliveryRuntimeMetrics {
    active_deliveries: AtomicUsize,
    max_active_deliveries: AtomicUsize,
    max_queued_deliveries: AtomicUsize,
    enqueue_full_count: AtomicUsize,
    enqueue_blocked_ns: AtomicU64,
}

impl DeliveryRuntimeMetrics {
    fn start_delivery(&self) -> ActiveDeliveryGuard<'_> {
        let active = self.active_deliveries.fetch_add(1, Ordering::SeqCst) + 1;
        self.record_max_active(active);
        ActiveDeliveryGuard { metrics: self }
    }

    fn record_max_active(&self, active: usize) {
        self.max_active_deliveries
            .fetch_max(active, Ordering::SeqCst);
    }

    fn finish_delivery(&self) {
        self.active_deliveries.fetch_sub(1, Ordering::SeqCst);
    }

    fn record_queue_depth(&self, queued: usize) {
        self.max_queued_deliveries
            .fetch_max(queued, Ordering::SeqCst);
    }

    fn record_enqueue_block(&self, duration: Duration) {
        self.enqueue_full_count.fetch_add(1, Ordering::SeqCst);
        let blocked_ns = duration_to_ns(duration);
        self.enqueue_blocked_ns
            .fetch_add(blocked_ns, Ordering::SeqCst);
    }

    fn max_active_deliveries(&self) -> usize {
        self.max_active_deliveries.load(Ordering::SeqCst)
    }

    fn max_queued_deliveries(&self) -> usize {
        self.max_queued_deliveries.load(Ordering::SeqCst)
    }

    fn enqueue_full_count(&self) -> usize {
        self.enqueue_full_count.load(Ordering::SeqCst)
    }

    fn enqueue_blocked_ns(&self) -> u64 {
        self.enqueue_blocked_ns.load(Ordering::SeqCst)
    }
}

struct ActiveDeliveryGuard<'a> {
    metrics: &'a DeliveryRuntimeMetrics,
}

impl Drop for ActiveDeliveryGuard<'_> {
    fn drop(&mut self) {
        self.metrics.finish_delivery();
    }
}

fn spawn_delivery_worker(
    worker_index: usize,
    receiver: DeliveryReceiver<DeliveryJob>,
    metrics: Arc<DeliveryRuntimeMetrics>,
    inflight: Arc<Inflight>,
) {
    thread::Builder::new()
        .name(format!("mvp-bus-delivery-{worker_index}"))
        .spawn(move || {
            while let Ok(job) = receiver.recv() {
                let _inflight_guard = inflight.guard();
                let _active_delivery = metrics.start_delivery();
                let result = job.delivery.invoke();
                if let Some(result_tx) = job.result_tx {
                    let _ = result_tx.send(result);
                }
            }
        })
        .expect("delivery worker starts");
}

fn duration_to_ns(duration: Duration) -> u64 {
    let nanos = duration.as_nanos();
    if nanos > u128::from(u64::MAX) {
        return u64::MAX;
    }
    nanos as u64
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::MemoryBus;
    use crate::{
        BridgeEndpoint, BridgeRuleId, BridgeRuleViolation, BridgeState, BusError, BusRuntimeConfig,
        BusSession, FactContentHash, FactKey, FactKeyPattern, FactWriteOutcome, Grant,
        HandlerFailure, IslandId, Payload, PrincipalId, RequestManyPolicy, RequestTarget,
        ServiceImport, StreamImport, Subject, SubjectPattern, SubjectTransform,
    };

    fn island(name: &str) -> IslandId {
        IslandId::new(name)
    }

    fn principal(name: &str) -> PrincipalId {
        PrincipalId::new(name)
    }

    fn subject(value: &str) -> Subject {
        Subject::parse(value).expect("subject parses")
    }

    fn pattern(value: &str) -> SubjectPattern {
        SubjectPattern::parse(value).expect("pattern parses")
    }

    fn fact_key(value: &str) -> FactKey {
        FactKey::parse(value).expect("fact key parses")
    }

    fn fact_pattern(value: &str) -> FactKeyPattern {
        FactKeyPattern::parse(value).expect("fact key pattern parses")
    }

    fn hash(value: &str) -> FactContentHash {
        FactContentHash::new(value)
    }

    fn rule_id(value: &str) -> BridgeRuleId {
        BridgeRuleId::new(value)
    }

    fn bus_with_admin() -> (MemoryBus, BusSession) {
        let (bus, authority) = MemoryBus::new_with_authority();
        let admin = authority.grant(principal("admin"), Grant::allow_all());
        (bus, admin)
    }

    #[test]
    fn publish_fans_out_to_matching_subscribers() {
        let (bus, admin) = bus_with_admin();
        let received = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            let received = Arc::clone(&received);
            bus.subscribe(&admin, pattern("node.*.status"), move |_| {
                received.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .expect("subscribe");
        }

        bus.publish(&admin, subject("node.alpha.status"), b"up".to_vec())
            .expect("publish");

        assert_eq!(received.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn fanout_reuses_payload_storage_across_subscribers() {
        let (bus, admin) = bus_with_admin();
        let payload_ptrs = Arc::new(Mutex::new(Vec::new()));
        for _ in 0..2 {
            let payload_ptrs = Arc::clone(&payload_ptrs);
            bus.subscribe(&admin, pattern("node.*.status"), move |ctx| {
                payload_ptrs
                    .lock()
                    .expect("payload ptr lock")
                    .push(ctx.message.payload().as_bytes().as_ptr() as usize);
                Ok(())
            })
            .expect("subscribe");
        }

        bus.publish(
            &admin,
            subject("node.alpha.status"),
            Payload::from(vec![1, 2, 3, 4]),
        )
        .expect("publish");

        let payload_ptrs = payload_ptrs.lock().expect("payload ptr lock");
        assert_eq!(payload_ptrs.len(), 2);
        assert_eq!(payload_ptrs[0], payload_ptrs[1]);
    }

    #[test]
    fn delivery_runtime_bounds_handler_concurrency() {
        let (bus, authority) =
            MemoryBus::new_with_authority_and_config(BusRuntimeConfig::with_delivery_workers(2));
        let admin = authority.grant(principal("admin"), Grant::allow_all());
        let active = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));

        for _ in 0..8 {
            let active = Arc::clone(&active);
            let completed = Arc::clone(&completed);
            let max_active = Arc::clone(&max_active);
            bus.subscribe(&admin, pattern("node.*.status"), move |_| {
                let active_now = active.fetch_add(1, Ordering::SeqCst) + 1;
                record_max(&max_active, active_now);
                std::thread::sleep(Duration::from_millis(10));
                active.fetch_sub(1, Ordering::SeqCst);
                completed.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .expect("subscribe");
        }

        bus.publish(&admin, subject("node.alpha.status"), b"up".to_vec())
            .expect("publish");

        let snapshot = bus.runtime_snapshot();
        assert_eq!(completed.load(Ordering::SeqCst), 8);
        assert_eq!(snapshot.delivery_workers, 2);
        assert!(max_active.load(Ordering::SeqCst) <= 2);
        assert!(snapshot.max_active_deliveries <= 2);
    }

    #[test]
    fn publish_until_times_out_when_delivery_queue_stays_full_and_records_pressure() {
        let config = BusRuntimeConfig::with_delivery_workers(1).with_delivery_queue_capacity(1);
        let (bus, authority) = MemoryBus::new_with_authority_and_config(config);
        let admin = authority.grant(principal("admin"), Grant::allow_all());

        for _ in 0..3 {
            bus.subscribe(&admin, pattern("gateway.changed"), move |_| {
                std::thread::sleep(Duration::from_millis(100));
                Ok(())
            })
            .expect("subscribe slow gateway handler");
        }

        let error = bus
            .publish_until(
                &admin,
                subject("gateway.changed"),
                Vec::new(),
                super::deadline_after(Duration::from_millis(5)),
            )
            .unwrap_err();
        let snapshot = bus.runtime_snapshot();

        assert!(matches!(error, BusError::Timeout { .. }));
        assert!(snapshot.max_queued_deliveries >= 1);
        assert!(snapshot.enqueue_full_count > 0);
        assert!(snapshot.enqueue_blocked_ns > 0);
    }

    #[test]
    fn queue_publish_delivers_to_one_group_member() {
        let (bus, admin) = bus_with_admin();
        let received = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            let received = Arc::clone(&received);
            bus.queue_subscribe(&admin, pattern("deploy.submit"), "schedulers", move |_| {
                received.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .expect("queue subscribe");
        }

        bus.publish(&admin, subject("deploy.submit"), b"deploy".to_vec())
            .expect("publish");

        assert_eq!(received.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn queue_publish_delivers_once_per_queue_name_for_overlapping_patterns() {
        let (bus, admin) = bus_with_admin();
        let received = Arc::new(AtomicUsize::new(0));
        for pattern in ["deploy.*", "deploy.submit"] {
            let received = Arc::clone(&received);
            bus.queue_subscribe(&admin, self::pattern(pattern), "schedulers", move |_| {
                received.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .expect("queue subscribe");
        }

        bus.publish(&admin, subject("deploy.submit"), b"deploy".to_vec())
            .expect("publish");

        assert_eq!(received.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn request_receives_reply() {
        let (bus, admin) = bus_with_admin();
        bus.subscribe(&admin, pattern("node.alpha.inspect"), |ctx| {
            ctx.reply(b"ok".to_vec())
        })
        .expect("subscribe");

        let response = bus
            .request(
                &admin,
                subject("node.alpha.inspect"),
                Vec::new(),
                Duration::from_secs(1),
            )
            .expect("request");

        assert_eq!(response.payload().as_bytes(), b"ok".as_slice());
    }

    #[test]
    fn request_reports_no_responders() {
        let (bus, admin) = bus_with_admin();

        let error = bus
            .request(
                &admin,
                subject("node.alpha.inspect"),
                Vec::new(),
                Duration::from_millis(10),
            )
            .unwrap_err();

        assert_eq!(
            error,
            BusError::NoResponders {
                target: Box::new(RequestTarget::Subject(subject("node.alpha.inspect"))),
            }
        );
    }

    #[test]
    fn request_reports_timeout() {
        let (bus, admin) = bus_with_admin();
        bus.subscribe(&admin, pattern("node.alpha.inspect"), |_ctx| {
            std::thread::sleep(Duration::from_millis(50));
            Ok(())
        })
        .expect("subscribe");

        let error = bus
            .request(
                &admin,
                subject("node.alpha.inspect"),
                Vec::new(),
                Duration::from_millis(5),
            )
            .unwrap_err();

        assert_eq!(
            error,
            BusError::Timeout {
                subject: String::from("node.alpha.inspect")
            }
        );
    }

    #[test]
    fn request_reports_handler_failure() {
        let (bus, admin) = bus_with_admin();
        bus.subscribe(&admin, pattern("node.alpha.inspect"), |_ctx| {
            Err(BusError::HandlerFailed {
                subject: String::from("node.alpha.inspect"),
                failure: HandlerFailure::Application,
            })
        })
        .expect("subscribe");

        let error = bus
            .request(
                &admin,
                subject("node.alpha.inspect"),
                Vec::new(),
                Duration::from_secs(1),
            )
            .unwrap_err();

        assert_eq!(
            error,
            BusError::HandlerFailed {
                subject: String::from("node.alpha.inspect"),
                failure: HandlerFailure::Application,
            }
        );
    }

    #[test]
    fn request_many_can_target_pattern() {
        let (bus, admin) = bus_with_admin();
        for node in ["alpha", "beta"] {
            bus.subscribe(
                &admin,
                pattern(&format!("node.{node}.capacity")),
                move |ctx| ctx.reply(ctx.message.subject().as_str().as_bytes().to_vec()),
            )
            .expect("subscribe");
        }

        let mut replies = bus
            .request_many(
                &admin,
                RequestTarget::Pattern(pattern("node.>")),
                subject("node.broadcast.capacity"),
                Vec::new(),
                RequestManyPolicy::new(8, Duration::from_secs(1)),
            )
            .expect("request many");
        replies.sort_by(|left, right| left.payload().cmp(right.payload()));

        assert_eq!(replies.len(), 2);
        assert_eq!(
            replies[0].payload().as_bytes(),
            b"node.broadcast.capacity".as_slice()
        );
        assert_eq!(
            replies[1].payload().as_bytes(),
            b"node.broadcast.capacity".as_slice()
        );
    }

    #[test]
    fn request_many_can_target_broad_subscriber_pattern() {
        let (bus, admin) = bus_with_admin();
        bus.subscribe(&admin, pattern("node.>"), |ctx| {
            ctx.reply(ctx.message.subject().as_str().as_bytes().to_vec())
        })
        .expect("subscribe");

        let replies = bus
            .request_many(
                &admin,
                RequestTarget::Pattern(pattern("node.*.capacity")),
                subject("node.broadcast.capacity"),
                Vec::new(),
                RequestManyPolicy::new(8, Duration::from_secs(1)),
            )
            .expect("request many");

        assert_eq!(replies.len(), 1);
        assert_eq!(
            replies[0].payload().as_bytes(),
            b"node.broadcast.capacity".as_slice()
        );
    }

    #[test]
    fn request_many_excludes_queue_groups() {
        let (bus, admin) = bus_with_admin();
        bus.queue_subscribe(
            &admin,
            pattern("node.alpha.capacity"),
            "capacity-workers",
            |ctx| ctx.reply(b"queue".to_vec()),
        )
        .expect("queue subscribe");
        bus.subscribe(&admin, pattern("node.beta.capacity"), |ctx| {
            ctx.reply(b"normal".to_vec())
        })
        .expect("subscribe");

        let replies = bus
            .request_many(
                &admin,
                RequestTarget::Pattern(pattern("node.*.capacity")),
                subject("node.broadcast.capacity"),
                Vec::new(),
                RequestManyPolicy::new(8, Duration::from_secs(1)),
            )
            .expect("request many");

        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].payload().as_bytes(), b"normal".as_slice());
    }

    #[test]
    fn request_many_authorizes_target_pattern() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let limited = authority.grant(
            principal("limited"),
            Grant::empty().with_publish(pattern("node.broadcast.capacity")),
        );

        let error = bus
            .request_many(
                &limited,
                RequestTarget::Pattern(pattern("node.*.capacity")),
                subject("node.broadcast.capacity"),
                Vec::new(),
                RequestManyPolicy::new(8, Duration::from_secs(1)),
            )
            .unwrap_err();

        assert!(matches!(error, BusError::UnauthorizedRequestTarget { .. }));
    }

    #[test]
    fn request_many_authorizes_operation_subject() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let limited = authority.grant(
            principal("limited"),
            Grant::empty().with_publish(pattern("node.*.capacity")),
        );

        let error = bus
            .request_many(
                &limited,
                RequestTarget::Pattern(pattern("node.*.capacity")),
                subject("deploy.submit"),
                Vec::new(),
                RequestManyPolicy::new(8, Duration::from_secs(1)),
            )
            .unwrap_err();

        assert!(matches!(error, BusError::UnauthorizedPublish { .. }));
    }

    #[test]
    fn request_many_reports_incomplete_responses() {
        let (bus, admin) = bus_with_admin();
        bus.subscribe(&admin, pattern("node.alpha.capacity"), |ctx| {
            ctx.reply(b"alpha".to_vec())
        })
        .expect("subscribe alpha");
        bus.subscribe(&admin, pattern("node.beta.capacity"), |_ctx| {
            std::thread::sleep(Duration::from_millis(50));
            Ok(())
        })
        .expect("subscribe beta");

        let error = bus
            .request_many(
                &admin,
                RequestTarget::Pattern(pattern("node.*.capacity")),
                subject("node.broadcast.capacity"),
                Vec::new(),
                RequestManyPolicy::new(8, Duration::from_millis(5)),
            )
            .unwrap_err();

        assert_eq!(
            error,
            BusError::IncompleteResponses {
                target: Box::new(RequestTarget::Pattern(pattern("node.*.capacity"))),
                expected: 2,
                received: 1,
            }
        );
    }

    #[test]
    fn request_many_zero_max_returns_without_dispatch() {
        let (bus, admin) = bus_with_admin();
        let called = Arc::new(AtomicUsize::new(0));
        let called_for_handler = Arc::clone(&called);
        bus.subscribe(&admin, pattern("node.alpha.capacity"), move |ctx| {
            called_for_handler.fetch_add(1, Ordering::SeqCst);
            ctx.reply(b"capacity".to_vec())
        })
        .expect("subscribe");

        let replies = bus
            .request_many(
                &admin,
                RequestTarget::Pattern(pattern("node.*.capacity")),
                subject("node.broadcast.capacity"),
                Vec::new(),
                RequestManyPolicy::new(0, Duration::from_secs(1)),
            )
            .expect("request many");

        assert!(replies.is_empty());
        assert_eq!(called.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn request_many_zero_max_still_requires_authority() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let requester = authority.grant(principal("requester"), Grant::empty());

        let error = bus
            .request_many(
                &requester,
                RequestTarget::Pattern(pattern("node.*.capacity")),
                subject("node.broadcast.capacity"),
                Vec::new(),
                RequestManyPolicy::new(0, Duration::from_secs(1)),
            )
            .unwrap_err();

        assert!(matches!(error, BusError::UnauthorizedRequestTarget { .. }));
    }

    #[test]
    fn request_many_zero_max_still_respects_draining() {
        let (bus, admin) = bus_with_admin();
        bus.drain(&admin, Duration::from_secs(1)).expect("drain");

        let error = bus
            .request_many(
                &admin,
                RequestTarget::Pattern(pattern("node.*.capacity")),
                subject("node.broadcast.capacity"),
                Vec::new(),
                RequestManyPolicy::new(0, Duration::from_secs(1)),
            )
            .unwrap_err();

        assert_eq!(error, BusError::Draining);
    }

    #[test]
    fn unauthorized_publish_fails_before_handler_runs() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let subscriber = authority.grant(principal("subscriber"), Grant::allow_all());
        let publisher = authority.grant(principal("publisher"), Grant::empty());
        let received = Arc::new(AtomicUsize::new(0));
        let received_for_handler = Arc::clone(&received);
        bus.subscribe(&subscriber, pattern("node.alpha.status"), move |_| {
            received_for_handler.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .expect("subscribe");

        let error = bus
            .publish(&publisher, subject("node.alpha.status"), b"up".to_vec())
            .unwrap_err();

        assert!(matches!(error, BusError::UnauthorizedPublish { .. }));
        assert_eq!(received.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn unauthorized_request_fails_before_handler_runs() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let responder = authority.grant(principal("responder"), Grant::allow_all());
        let requester = authority.grant(principal("requester"), Grant::empty());
        let received = Arc::new(AtomicUsize::new(0));
        let received_for_handler = Arc::clone(&received);
        bus.subscribe(&responder, pattern("node.alpha.inspect"), move |ctx| {
            received_for_handler.fetch_add(1, Ordering::SeqCst);
            ctx.reply(b"ok".to_vec())
        })
        .expect("subscribe");

        let error = bus
            .request(
                &requester,
                subject("node.alpha.inspect"),
                Vec::new(),
                Duration::from_millis(20),
            )
            .unwrap_err();

        assert!(matches!(error, BusError::UnauthorizedRequestTarget { .. }));
        assert_eq!(received.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn same_subject_publish_stays_inside_session_island() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let laptop = authority.grant_in(island("laptop"), principal("admin"), Grant::allow_all());
        let prod = authority.grant_in(island("prod"), principal("admin"), Grant::allow_all());
        let laptop_deliveries = Arc::new(AtomicUsize::new(0));
        let prod_deliveries = Arc::new(AtomicUsize::new(0));
        let laptop_deliveries_for_handler = Arc::clone(&laptop_deliveries);
        let prod_deliveries_for_handler = Arc::clone(&prod_deliveries);
        bus.subscribe(&laptop, pattern("deploy.submit"), move |ctx| {
            assert_eq!(ctx.message.island().as_str(), "laptop");
            laptop_deliveries_for_handler.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .expect("subscribe laptop");
        bus.subscribe(&prod, pattern("deploy.submit"), move |ctx| {
            assert_eq!(ctx.message.island().as_str(), "prod");
            prod_deliveries_for_handler.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .expect("subscribe prod");

        bus.publish(&laptop, subject("deploy.submit"), b"from-laptop".to_vec())
            .expect("publish laptop");

        assert_eq!(laptop_deliveries.load(Ordering::SeqCst), 1);
        assert_eq!(prod_deliveries.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn request_does_not_use_cross_island_responders() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let laptop = authority.grant_in(island("laptop"), principal("admin"), Grant::allow_all());
        let prod = authority.grant_in(island("prod"), principal("admin"), Grant::allow_all());
        let prod_handler_calls = Arc::new(AtomicUsize::new(0));
        let prod_handler_calls_for_handler = Arc::clone(&prod_handler_calls);
        bus.subscribe(&prod, pattern("deploy.submit"), move |ctx| {
            prod_handler_calls_for_handler.fetch_add(1, Ordering::SeqCst);
            ctx.reply(b"prod".to_vec())
        })
        .expect("subscribe prod responder");

        let error = bus
            .request(
                &laptop,
                subject("deploy.submit"),
                Vec::new(),
                Duration::from_millis(20),
            )
            .unwrap_err();

        assert!(matches!(error, BusError::NoResponders { .. }));
        assert_eq!(prod_handler_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn queue_groups_are_island_scoped_for_publish_and_request() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let island_a = island("island-a");
        let admin_a = authority.grant_in(island_a.clone(), principal("admin"), Grant::allow_all());
        let admin_b =
            authority.grant_in(island("island-b"), principal("admin"), Grant::allow_all());
        let island_a_calls = Arc::new(AtomicUsize::new(0));
        let island_b_calls = Arc::new(AtomicUsize::new(0));

        let island_a_calls_for_handler = Arc::clone(&island_a_calls);
        bus.queue_subscribe(&admin_a, pattern("image.pull"), "workers", move |ctx| {
            island_a_calls_for_handler.fetch_add(1, Ordering::SeqCst);
            let _ = ctx.reply(b"island-a".to_vec());
            Ok(())
        })
        .expect("island-a queue subscriber");
        let island_b_calls_for_handler = Arc::clone(&island_b_calls);
        bus.queue_subscribe(&admin_b, pattern("image.pull"), "workers", move |ctx| {
            island_b_calls_for_handler.fetch_add(1, Ordering::SeqCst);
            let _ = ctx.reply(b"island-b".to_vec());
            Ok(())
        })
        .expect("island-b queue subscriber");

        bus.publish(&admin_a, subject("image.pull"), b"pull".to_vec())
            .expect("publish in island-a");
        assert_eq!(island_a_calls.load(Ordering::SeqCst), 1);
        assert_eq!(island_b_calls.load(Ordering::SeqCst), 0);

        let response = bus
            .request(
                &admin_a,
                subject("image.pull"),
                b"pull".to_vec(),
                Duration::from_secs(1),
            )
            .expect("request in island-a");
        assert_eq!(response.island(), &island_a);
        assert_eq!(response.payload().as_bytes(), b"island-a".as_slice());
        assert_eq!(island_a_calls.load(Ordering::SeqCst), 2);
        assert_eq!(island_b_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn revoking_grant_blocks_future_publish_before_dispatch() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let admin = authority.grant(principal("admin"), Grant::allow_all());
        let publisher = authority.grant(principal("publisher"), Grant::allow_all());
        let received = Arc::new(AtomicUsize::new(0));
        let received_for_handler = Arc::clone(&received);
        bus.subscribe(&admin, pattern("gateway.changed"), move |_| {
            received_for_handler.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .expect("subscribe");
        assert!(authority.revoke(&publisher));

        let error = bus
            .publish(&publisher, subject("gateway.changed"), Vec::new())
            .unwrap_err();

        assert!(matches!(error, BusError::UnauthorizedPublish { .. }));
        assert_eq!(received.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn authorization_error_carries_island_and_principal_context() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let laptop = authority.grant_in(island("laptop"), principal("writer"), Grant::empty());

        let error = bus
            .publish(&laptop, subject("gateway.changed"), Vec::new())
            .unwrap_err();

        assert!(matches!(
            error,
            BusError::UnauthorizedPublish {
                island,
                principal,
                ..
            } if island.as_str() == "laptop" && principal.as_str() == "writer"
        ));
    }

    #[test]
    fn allowed_fact_write_inserts_fact_in_session_island() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let writer = authority.grant_in(
            island("prod"),
            principal("deployer"),
            Grant::empty()
                .with_fact_write(fact_pattern("/facts/deploy/>"))
                .with_fact_read(fact_pattern("/facts/deploy/>")),
        );

        let outcome = bus
            .write_fact(&writer, fact_key("/facts/deploy/d1/plan"), hash("b3:plan"))
            .expect("write fact");
        let fact = bus
            .read_fact(&writer, &fact_key("/facts/deploy/d1/plan"))
            .expect("read fact")
            .expect("fact exists");

        assert!(matches!(outcome, FactWriteOutcome::Inserted(_)));
        assert_eq!(fact.island().as_str(), "prod");
        assert_eq!(fact.author().as_str(), "deployer");
        assert_eq!(fact.content_hash().as_str(), "b3:plan");
    }

    #[test]
    fn denied_fact_write_fails_before_mutation() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let reader = authority.grant_in(island("prod"), principal("reader"), Grant::allow_all());
        let writer = authority.grant_in(
            island("prod"),
            principal("deployer"),
            Grant::empty()
                .with_fact_write(fact_pattern("/facts/deploy/>"))
                .with_fact_write_deny(fact_pattern("/facts/deploy/*/secret")),
        );

        let error = bus
            .write_fact(
                &writer,
                fact_key("/facts/deploy/d1/secret"),
                hash("b3:secret"),
            )
            .unwrap_err();

        assert!(matches!(error, BusError::UnauthorizedFactWrite { .. }));
        assert!(
            bus.read_fact(&reader, &fact_key("/facts/deploy/d1/secret"))
                .expect("read secret")
                .is_none()
        );
    }

    #[test]
    fn fact_reads_are_island_scoped_through_bus() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let laptop = authority.grant_in(island("laptop"), principal("admin"), Grant::allow_all());
        let prod = authority.grant_in(island("prod"), principal("admin"), Grant::allow_all());
        bus.write_fact(&prod, fact_key("/facts/deploy/d1/plan"), hash("b3:prod"))
            .expect("write prod fact");
        bus.write_fact(
            &laptop,
            fact_key("/facts/deploy/d1/plan"),
            hash("b3:laptop"),
        )
        .expect("write laptop fact");

        let prod_fact = bus
            .read_fact(&prod, &fact_key("/facts/deploy/d1/plan"))
            .expect("read prod")
            .expect("prod fact exists");
        let laptop_fact = bus
            .read_fact(&laptop, &fact_key("/facts/deploy/d1/plan"))
            .expect("read laptop")
            .expect("laptop fact exists");

        assert_eq!(prod_fact.content_hash().as_str(), "b3:prod");
        assert_eq!(laptop_fact.content_hash().as_str(), "b3:laptop");
    }

    #[test]
    fn fact_read_requires_active_read_grant() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let admin = authority.grant_in(island("prod"), principal("admin"), Grant::allow_all());
        let intruder = authority.grant_in(island("prod"), principal("intruder"), Grant::empty());
        bus.write_fact(&admin, fact_key("/facts/deploy/d1/plan"), hash("b3:prod"))
            .expect("write prod fact");

        let error = bus
            .read_fact(&intruder, &fact_key("/facts/deploy/d1/plan"))
            .unwrap_err();

        assert!(matches!(error, BusError::UnauthorizedFactRead { .. }));
    }

    #[test]
    fn revoked_session_cannot_read_facts() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let admin = authority.grant_in(island("prod"), principal("admin"), Grant::allow_all());
        let reader = authority.grant_in(island("prod"), principal("reader"), Grant::allow_all());
        bus.write_fact(&admin, fact_key("/facts/deploy/d1/plan"), hash("b3:prod"))
            .expect("write prod fact");
        assert!(authority.revoke(&reader));

        let error = bus
            .read_fact(&reader, &fact_key("/facts/deploy/d1/plan"))
            .unwrap_err();

        assert!(matches!(error, BusError::UnauthorizedFactRead { .. }));
    }

    #[test]
    fn fact_write_conflict_is_structured() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let writer = authority.grant(principal("admin"), Grant::allow_all());
        bus.write_fact(&writer, fact_key("/facts/deploy/d1/plan"), hash("b3:first"))
            .expect("write fact");

        let repeated = bus
            .write_fact(&writer, fact_key("/facts/deploy/d1/plan"), hash("b3:first"))
            .expect("repeat fact");
        let conflict = bus
            .write_fact(
                &writer,
                fact_key("/facts/deploy/d1/plan"),
                hash("b3:second"),
            )
            .unwrap_err();

        assert!(matches!(repeated, FactWriteOutcome::AlreadyPresent(_)));
        assert!(matches!(conflict, BusError::FactConflict { .. }));
    }

    #[test]
    fn unauthorized_subscribe_fails_before_registration() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let subscriber = authority.grant(principal("subscriber"), Grant::empty());

        let error = bus
            .subscribe(&subscriber, pattern("node.alpha.status"), |_| Ok(()))
            .unwrap_err();

        assert!(matches!(error, BusError::UnauthorizedSubscribe { .. }));
    }

    #[test]
    fn unauthorized_queue_subscribe_fails_before_registration() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let subscriber = authority.grant(
            principal("subscriber"),
            Grant::empty().with_subscribe(pattern("deploy.submit")),
        );

        let error = bus
            .queue_subscribe(&subscriber, pattern("deploy.submit"), "schedulers", |_| {
                Ok(())
            })
            .unwrap_err();

        assert!(matches!(error, BusError::UnauthorizedQueue { .. }));
    }

    #[test]
    fn unauthorized_queue_name_fails_before_registration() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let subscriber = authority.grant(
            principal("subscriber"),
            Grant::empty()
                .with_subscribe(pattern("deploy.submit"))
                .with_queue(pattern("deploy.submit"), "schedulers"),
        );

        let error = bus
            .queue_subscribe(
                &subscriber,
                pattern("deploy.submit"),
                "privileged-schedulers",
                |_| Ok(()),
            )
            .unwrap_err();

        assert!(matches!(error, BusError::UnauthorizedQueue { .. }));
    }

    #[test]
    fn unauthorized_response_does_not_reach_requester() {
        let (bus, admin) = bus_with_admin();
        let response_error = Arc::new(Mutex::new(None));
        let response_error_for_handler = Arc::clone(&response_error);
        bus.subscribe(&admin, pattern("node.alpha.inspect"), move |ctx| {
            let intruder = principal("intruder");
            let error = ctx.reply_as(&intruder, b"bad".to_vec()).unwrap_err();
            *response_error_for_handler
                .lock()
                .expect("lock response error") = Some(error);
            Ok(())
        })
        .expect("subscribe");

        let request_error = bus
            .request(
                &admin,
                subject("node.alpha.inspect"),
                Vec::new(),
                Duration::from_millis(20),
            )
            .unwrap_err();

        assert!(matches!(request_error, BusError::Timeout { .. }));
        assert!(matches!(
            response_error.lock().expect("lock response error").as_ref(),
            Some(BusError::UnauthorizedResponse { .. })
        ));
    }

    #[test]
    fn reply_permit_is_one_use() {
        let (bus, admin) = bus_with_admin();
        let duplicate_error = Arc::new(Mutex::new(None));
        let duplicate_error_for_handler = Arc::clone(&duplicate_error);
        let (duplicate_recorded_tx, duplicate_recorded_rx) = mpsc::channel();
        bus.subscribe(&admin, pattern("node.alpha.inspect"), move |ctx| {
            ctx.reply(b"first".to_vec())?;
            let error = ctx.reply(b"second".to_vec()).unwrap_err();
            *duplicate_error_for_handler
                .lock()
                .expect("lock duplicate error") = Some(error);
            let _ = duplicate_recorded_tx.send(());
            Ok(())
        })
        .expect("subscribe");

        let response = bus
            .request(
                &admin,
                subject("node.alpha.inspect"),
                Vec::new(),
                Duration::from_secs(1),
            )
            .expect("request");

        assert_eq!(response.payload().as_bytes(), b"first".as_slice());
        duplicate_recorded_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("duplicate response recorded");
        assert!(matches!(
            duplicate_error
                .lock()
                .expect("lock duplicate error")
                .as_ref(),
            Some(BusError::DuplicateResponse { .. })
        ));
    }

    #[test]
    fn reply_permit_expires_with_request_deadline() {
        let (bus, admin) = bus_with_admin();
        let expired_error = Arc::new(Mutex::new(None));
        let expired_error_for_handler = Arc::clone(&expired_error);
        bus.subscribe(&admin, pattern("node.alpha.inspect"), move |ctx| {
            std::thread::sleep(Duration::from_millis(30));
            let error = ctx.reply(b"late".to_vec()).unwrap_err();
            *expired_error_for_handler
                .lock()
                .expect("lock expired error") = Some(error);
            Ok(())
        })
        .expect("subscribe");

        let request_error = bus
            .request(
                &admin,
                subject("node.alpha.inspect"),
                Vec::new(),
                Duration::from_millis(5),
            )
            .unwrap_err();
        std::thread::sleep(Duration::from_millis(40));

        assert!(matches!(request_error, BusError::Timeout { .. }));
        assert!(matches!(
            expired_error.lock().expect("lock expired error").as_ref(),
            Some(BusError::UnauthorizedResponse { .. })
        ));
    }

    #[test]
    fn reply_permit_deadline_includes_delivery_queue_wait() {
        let (bus, authority) =
            MemoryBus::new_with_authority_and_config(BusRuntimeConfig::with_delivery_workers(1));
        let admin = authority.grant(principal("admin"), Grant::allow_all());
        let (blocker_started_tx, blocker_started_rx) = mpsc::channel();
        bus.subscribe(&admin, pattern("node.blocker"), move |_| {
            let _ = blocker_started_tx.send(());
            std::thread::sleep(Duration::from_millis(40));
            Ok(())
        })
        .expect("subscribe blocker");

        let reply_error = Arc::new(Mutex::new(None));
        let reply_error_for_handler = Arc::clone(&reply_error);
        bus.subscribe(&admin, pattern("node.alpha.inspect"), move |ctx| {
            let error = ctx.reply(b"late".to_vec()).unwrap_err();
            *reply_error_for_handler.lock().expect("reply error lock") = Some(error);
            Ok(())
        })
        .expect("subscribe inspect");

        let bus_for_publish = bus.clone();
        let admin_for_publish = admin.clone();
        let publish = std::thread::spawn(move || {
            bus_for_publish.publish(&admin_for_publish, subject("node.blocker"), Vec::new())
        });
        blocker_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("blocker started");

        let request_error = bus
            .request(
                &admin,
                subject("node.alpha.inspect"),
                Vec::new(),
                Duration::from_millis(5),
            )
            .unwrap_err();
        publish
            .join()
            .expect("publish thread joins")
            .expect("publish");
        std::thread::sleep(Duration::from_millis(20));

        assert!(matches!(request_error, BusError::Timeout { .. }));
        assert!(matches!(
            reply_error.lock().expect("reply error lock").as_ref(),
            Some(BusError::UnauthorizedResponse { .. })
        ));
    }

    #[test]
    fn revoked_subscriber_is_skipped_at_dispatch() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let admin = authority.grant(principal("admin"), Grant::allow_all());
        let subscriber_id = principal("subscriber");
        let subscriber = authority.grant(subscriber_id.clone(), Grant::allow_all());
        let received = Arc::new(AtomicUsize::new(0));
        let received_for_handler = Arc::clone(&received);
        bus.subscribe(&subscriber, pattern("node.alpha.status"), move |_| {
            received_for_handler.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .expect("subscribe");
        authority.grant(subscriber_id, Grant::empty());

        bus.publish(&admin, subject("node.alpha.status"), Vec::new())
            .expect("publish");

        assert_eq!(received.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn unauthorized_drain_fails() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let limited = authority.grant(
            principal("limited"),
            Grant::empty().with_publish(pattern(">")),
        );

        let error = bus.drain(&limited, Duration::from_secs(1)).unwrap_err();

        assert!(matches!(error, BusError::UnauthorizedDrain { .. }));
    }

    #[test]
    fn drain_rejects_new_work() {
        let (bus, admin) = bus_with_admin();
        bus.drain(&admin, Duration::from_secs(1)).expect("drain");

        let error = bus
            .publish(&admin, subject("node.alpha.status"), Vec::new())
            .unwrap_err();

        assert_eq!(error, BusError::Draining);
    }

    #[test]
    fn drain_rejects_new_request() {
        let (bus, admin) = bus_with_admin();
        bus.subscribe(&admin, pattern("node.alpha.inspect"), |ctx| {
            ctx.reply(b"ok".to_vec())
        })
        .expect("subscribe");
        bus.drain(&admin, Duration::from_secs(1)).expect("drain");

        let error = bus
            .request(
                &admin,
                subject("node.alpha.inspect"),
                Vec::new(),
                Duration::from_millis(10),
            )
            .unwrap_err();

        assert_eq!(error, BusError::Draining);
    }

    #[test]
    fn drain_waits_for_inflight_work() {
        let (bus, admin) = bus_with_admin();
        let completed = Arc::new(AtomicUsize::new(0));
        let completed_for_handler = Arc::clone(&completed);
        let (started_tx, started_rx) = mpsc::channel();
        bus.subscribe(&admin, pattern("node.alpha.inspect"), move |ctx| {
            let _ = started_tx.send(());
            std::thread::sleep(Duration::from_millis(20));
            completed_for_handler.fetch_add(1, Ordering::SeqCst);
            ctx.reply(b"ok".to_vec())
        })
        .expect("subscribe");

        let bus_for_request = bus.clone();
        let admin_for_request = admin.clone();
        let request = std::thread::spawn(move || {
            bus_for_request.request(
                &admin_for_request,
                subject("node.alpha.inspect"),
                Vec::new(),
                Duration::from_secs(1),
            )
        });
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("handler started");

        bus.drain(&admin, Duration::from_secs(1)).expect("drain");

        let response = request
            .join()
            .expect("request thread joins")
            .expect("request");
        assert_eq!(response.payload().as_bytes(), b"ok".as_slice());
        assert_eq!(completed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn drain_waits_for_queued_delivery_work() {
        let (bus, authority) =
            MemoryBus::new_with_authority_and_config(BusRuntimeConfig::with_delivery_workers(1));
        let admin = authority.grant(principal("admin"), Grant::allow_all());
        let completed = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        for index in 0..3 {
            let completed_for_handler = Arc::clone(&completed);
            let started_tx = started_tx.clone();
            bus.subscribe(&admin, pattern("node.*.capacity"), move |ctx| {
                if index == 0 {
                    let _ = started_tx.send(());
                }
                std::thread::sleep(Duration::from_millis(10));
                completed_for_handler.fetch_add(1, Ordering::SeqCst);
                ctx.reply(b"ok".to_vec())
            })
            .expect("subscribe");
        }
        drop(started_tx);

        let bus_for_request = bus.clone();
        let admin_for_request = admin.clone();
        let request = std::thread::spawn(move || {
            bus_for_request.request_many(
                &admin_for_request,
                RequestTarget::Pattern(pattern("node.*.capacity")),
                subject("node.broadcast.capacity"),
                Vec::new(),
                RequestManyPolicy::new(8, Duration::from_secs(1)),
            )
        });
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("first queued handler started");

        bus.drain(&admin, Duration::from_secs(1)).expect("drain");

        let replies = request
            .join()
            .expect("request thread joins")
            .expect("request many");
        assert_eq!(replies.len(), 3);
        assert_eq!(completed.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn drain_times_out_waiting_for_inflight_work() {
        let (bus, admin) = bus_with_admin();
        let (started_tx, started_rx) = mpsc::channel();
        bus.subscribe(&admin, pattern("node.alpha.inspect"), move |_ctx| {
            let _ = started_tx.send(());
            std::thread::sleep(Duration::from_millis(50));
            Ok(())
        })
        .expect("subscribe");

        let bus_for_request = bus.clone();
        let admin_for_request = admin.clone();
        let request = std::thread::spawn(move || {
            bus_for_request.request(
                &admin_for_request,
                subject("node.alpha.inspect"),
                Vec::new(),
                Duration::from_millis(100),
            )
        });
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("handler started");

        let error = bus.drain(&admin, Duration::from_millis(1)).unwrap_err();

        assert_eq!(
            error,
            BusError::Timeout {
                subject: String::from("drain"),
            }
        );
        assert!(request.join().expect("request thread joins").is_err());
    }

    #[test]
    fn service_import_routes_request_to_remote_island() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let laptop = island("laptop");
        let prod = island("prod");
        let laptop_user = authority.grant_in(
            laptop.clone(),
            principal("user"),
            Grant::empty().with_publish(pattern("gpu.deploy.submit")),
        );
        let prod_bridge = authority.grant_in(
            prod.clone(),
            principal("bridge"),
            Grant::empty().with_publish(pattern("deploy.submit")),
        );
        let prod_scheduler = authority.grant_in(
            prod.clone(),
            principal("scheduler"),
            Grant::empty()
                .with_queue(pattern("deploy.submit"), "schedulers")
                .with_response(),
        );
        authority
            .add_service_import(ServiceImport::new(
                rule_id("gpu-deploy"),
                BridgeEndpoint::new(laptop.clone(), subject("gpu.deploy.submit")),
                BridgeEndpoint::new(prod.clone(), subject("deploy.submit")),
                prod_bridge.principal().clone(),
            ))
            .expect("add service import");

        let observed = Arc::new(Mutex::new(None));
        let observed_for_handler = Arc::clone(&observed);
        bus.queue_subscribe(
            &prod_scheduler,
            pattern("deploy.submit"),
            "schedulers",
            move |ctx| {
                *observed_for_handler.lock().expect("lock observed") = Some((
                    ctx.message.island().clone(),
                    ctx.message.principal().clone(),
                    ctx.message.subject().clone(),
                    ctx.message.bridge_origin().cloned(),
                ));
                ctx.reply(b"accepted".to_vec())
            },
        )
        .expect("prod scheduler subscribes");

        let response = bus
            .request(
                &laptop_user,
                subject("gpu.deploy.submit"),
                b"manifest".to_vec(),
                Duration::from_secs(1),
            )
            .expect("imported request");

        assert_eq!(response.payload().as_bytes(), b"accepted".as_slice());
        assert_eq!(response.island(), &prod);
        assert_eq!(response.responder().as_str(), "scheduler");
        let observed = observed.lock().expect("lock observed").clone();
        assert!(matches!(
            observed,
            Some((observed_island, observed_principal, observed_subject, Some(origin)))
                if observed_island == prod
                    && observed_principal.as_str() == "bridge"
                    && observed_subject.as_str() == "deploy.submit"
                    && origin.rule_id().as_str() == "gpu-deploy"
                    && origin.source_island() == &laptop
                    && origin.source_principal().as_str() == "user"
                    && origin.original_subject().as_str() == "gpu.deploy.submit"
        ));
    }

    #[test]
    fn service_import_request_uses_one_remote_responder() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let laptop = island("laptop");
        let prod = island("prod");
        let laptop_user = authority.grant_in(
            laptop.clone(),
            principal("user"),
            Grant::empty().with_publish(pattern("gpu.deploy.submit")),
        );
        let prod_bridge = authority.grant_in(
            prod.clone(),
            principal("bridge"),
            Grant::empty().with_publish(pattern("deploy.submit")),
        );
        authority
            .add_service_import(ServiceImport::new(
                rule_id("gpu-deploy"),
                BridgeEndpoint::new(laptop, subject("gpu.deploy.submit")),
                BridgeEndpoint::new(prod.clone(), subject("deploy.submit")),
                prod_bridge.principal().clone(),
            ))
            .expect("add service import");

        let calls = Arc::new(AtomicUsize::new(0));
        for responder in ["scheduler-a", "scheduler-b"] {
            let session = authority.grant_in(
                prod.clone(),
                principal(responder),
                Grant::empty()
                    .with_subscribe(pattern("deploy.submit"))
                    .with_response(),
            );
            let calls = Arc::clone(&calls);
            bus.subscribe(&session, pattern("deploy.submit"), move |ctx| {
                calls.fetch_add(1, Ordering::SeqCst);
                ctx.reply(format!("accepted:{responder}").into_bytes())
            })
            .expect("prod scheduler subscribes");
        }

        let response = bus
            .request(
                &laptop_user,
                subject("gpu.deploy.submit"),
                b"manifest".to_vec(),
                Duration::from_secs(1),
            )
            .expect("imported request");

        assert_eq!(response.island(), &prod);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn exact_request_many_can_use_one_imported_service_responder() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let laptop = island("laptop");
        let prod = island("prod");
        let laptop_user = authority.grant_in(
            laptop.clone(),
            principal("user"),
            Grant::empty().with_publish(pattern("gpu.deploy.submit")),
        );
        let prod_bridge = authority.grant_in(
            prod.clone(),
            principal("bridge"),
            Grant::empty().with_publish(pattern("deploy.submit")),
        );
        authority
            .add_service_import(ServiceImport::new(
                rule_id("gpu-deploy"),
                BridgeEndpoint::new(laptop, subject("gpu.deploy.submit")),
                BridgeEndpoint::new(prod.clone(), subject("deploy.submit")),
                prod_bridge.principal().clone(),
            ))
            .expect("add service import");

        let calls = Arc::new(AtomicUsize::new(0));
        let prod_scheduler = authority.grant_in(
            prod.clone(),
            principal("scheduler"),
            Grant::empty()
                .with_queue(pattern("deploy.submit"), "schedulers")
                .with_response(),
        );
        let calls_for_handler = Arc::clone(&calls);
        bus.queue_subscribe(
            &prod_scheduler,
            pattern("deploy.submit"),
            "schedulers",
            move |ctx| {
                calls_for_handler.fetch_add(1, Ordering::SeqCst);
                ctx.reply(b"accepted".to_vec())
            },
        )
        .expect("prod scheduler subscribes");

        let replies = bus
            .request_many(
                &laptop_user,
                RequestTarget::Subject(subject("gpu.deploy.submit")),
                subject("gpu.deploy.submit"),
                b"manifest".to_vec(),
                RequestManyPolicy::new(1, Duration::from_secs(1)),
            )
            .expect("imported request_many");
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].island(), &prod);
        assert_eq!(replies[0].payload().as_bytes(), b"accepted".as_slice());
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let unsupported = bus
            .request_many(
                &laptop_user,
                RequestTarget::Subject(subject("gpu.deploy.submit")),
                subject("gpu.deploy.submit"),
                b"manifest".to_vec(),
                RequestManyPolicy::new(2, Duration::from_secs(1)),
            )
            .unwrap_err();
        assert!(matches!(
            unsupported,
            BusError::BridgeRequestManyUnsupported { requested: 2, .. }
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn service_import_preserves_remote_response_scope() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let laptop = island("laptop");
        let prod = island("prod");
        let laptop_user = authority.grant_in(
            laptop.clone(),
            principal("user"),
            Grant::empty().with_publish(pattern("gpu.deploy.submit")),
        );
        let prod_bridge = authority.grant_in(
            prod.clone(),
            principal("bridge"),
            Grant::empty().with_publish(pattern("deploy.submit")),
        );
        let prod_scheduler = authority.grant_in(
            prod,
            principal("scheduler"),
            Grant::empty()
                .with_subscribe(pattern("deploy.submit"))
                .with_response(),
        );
        authority
            .add_service_import(ServiceImport::new(
                rule_id("gpu-deploy"),
                BridgeEndpoint::new(laptop, subject("gpu.deploy.submit")),
                BridgeEndpoint::new(island("prod"), subject("deploy.submit")),
                prod_bridge.principal().clone(),
            ))
            .expect("add service import");

        let duplicate_error = Arc::new(Mutex::new(None));
        let wrong_principal_error = Arc::new(Mutex::new(None));
        let (handler_done_tx, handler_done_rx) = mpsc::channel();
        let duplicate_error_for_handler = Arc::clone(&duplicate_error);
        let wrong_principal_error_for_handler = Arc::clone(&wrong_principal_error);
        bus.subscribe(&prod_scheduler, pattern("deploy.submit"), move |ctx| {
            let wrong = ctx
                .reply_as(&principal("bridge"), b"wrong".to_vec())
                .unwrap_err();
            *wrong_principal_error_for_handler
                .lock()
                .expect("lock wrong principal error") = Some(wrong);
            ctx.reply(b"ok".to_vec())?;
            let duplicate = ctx.reply(b"second".to_vec()).unwrap_err();
            *duplicate_error_for_handler
                .lock()
                .expect("lock duplicate error") = Some(duplicate);
            let _ = handler_done_tx.send(());
            Ok(())
        })
        .expect("prod scheduler subscribes");

        let response = bus
            .request(
                &laptop_user,
                subject("gpu.deploy.submit"),
                Vec::new(),
                Duration::from_secs(1),
            )
            .expect("imported request");

        assert_eq!(response.payload().as_bytes(), b"ok".as_slice());
        handler_done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("handler completed");
        assert!(matches!(
            wrong_principal_error
                .lock()
                .expect("lock wrong principal error")
                .as_ref(),
            Some(BusError::UnauthorizedResponse { .. })
        ));
        assert!(matches!(
            duplicate_error
                .lock()
                .expect("lock duplicate error")
                .as_ref(),
            Some(BusError::DuplicateResponse { .. })
        ));
    }

    #[test]
    fn disabled_service_import_fails_before_remote_dispatch() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let laptop = island("laptop");
        let prod = island("prod");
        let laptop_user = authority.grant_in(
            laptop.clone(),
            principal("user"),
            Grant::empty().with_publish(pattern("gpu.deploy.submit")),
        );
        let prod_bridge = authority.grant_in(
            prod.clone(),
            principal("bridge"),
            Grant::empty().with_publish(pattern("deploy.submit")),
        );
        let rule = rule_id("gpu-deploy");
        authority
            .add_service_import(ServiceImport::new(
                rule.clone(),
                BridgeEndpoint::new(laptop.clone(), subject("gpu.deploy.submit")),
                BridgeEndpoint::new(prod.clone(), subject("deploy.submit")),
                prod_bridge.principal().clone(),
            ))
            .expect("add service import");
        authority
            .set_service_import_state(&rule, BridgeState::Disabled)
            .expect("disable import");
        let remote_calls = Arc::new(AtomicUsize::new(0));
        let remote_calls_for_handler = Arc::clone(&remote_calls);
        let prod_scheduler = authority.grant_in(
            prod,
            principal("scheduler"),
            Grant::empty()
                .with_subscribe(pattern("deploy.submit"))
                .with_response(),
        );
        bus.subscribe(&prod_scheduler, pattern("deploy.submit"), move |ctx| {
            remote_calls_for_handler.fetch_add(1, Ordering::SeqCst);
            ctx.reply(b"should-not-run".to_vec())
        })
        .expect("prod scheduler subscribes");

        let error = bus
            .request(
                &laptop_user,
                subject("gpu.deploy.submit"),
                Vec::new(),
                Duration::from_secs(1),
            )
            .unwrap_err();

        assert!(matches!(
            error,
            BusError::BridgeUnavailable {
                failure: crate::BridgeFailure::Disabled,
                ..
            }
        ));
        assert_eq!(remote_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn remote_unavailable_service_import_fails_before_remote_dispatch() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let laptop = island("laptop");
        let prod = island("prod");
        let laptop_user = authority.grant_in(
            laptop.clone(),
            principal("user"),
            Grant::empty().with_publish(pattern("gpu.deploy.submit")),
        );
        let prod_bridge = authority.grant_in(
            prod.clone(),
            principal("bridge"),
            Grant::empty().with_publish(pattern("deploy.submit")),
        );
        let rule = rule_id("gpu-deploy");
        authority
            .add_service_import(ServiceImport::new(
                rule.clone(),
                BridgeEndpoint::new(laptop.clone(), subject("gpu.deploy.submit")),
                BridgeEndpoint::new(prod.clone(), subject("deploy.submit")),
                prod_bridge.principal().clone(),
            ))
            .expect("add service import");
        authority
            .set_service_import_state(&rule, BridgeState::RemoteUnavailable)
            .expect("mark import unavailable");
        let remote_calls = Arc::new(AtomicUsize::new(0));
        let remote_calls_for_handler = Arc::clone(&remote_calls);
        let prod_scheduler = authority.grant_in(
            prod,
            principal("scheduler"),
            Grant::empty()
                .with_subscribe(pattern("deploy.submit"))
                .with_response(),
        );
        bus.subscribe(&prod_scheduler, pattern("deploy.submit"), move |ctx| {
            remote_calls_for_handler.fetch_add(1, Ordering::SeqCst);
            ctx.reply(b"should-not-run".to_vec())
        })
        .expect("prod scheduler subscribes");

        let error = bus
            .request(
                &laptop_user,
                subject("gpu.deploy.submit"),
                Vec::new(),
                Duration::from_secs(1),
            )
            .unwrap_err();

        assert!(matches!(
            error,
            BusError::BridgeUnavailable {
                failure: crate::BridgeFailure::RemoteUnavailable,
                ..
            }
        ));
        assert_eq!(remote_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn service_import_requires_local_and_remote_publish_grants() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let laptop = island("laptop");
        let prod = island("prod");
        let laptop_user = authority.grant_in(laptop.clone(), principal("user"), Grant::empty());
        let prod_bridge = authority.grant_in(prod.clone(), principal("bridge"), Grant::empty());
        authority
            .add_service_import(ServiceImport::new(
                rule_id("gpu-deploy"),
                BridgeEndpoint::new(laptop.clone(), subject("gpu.deploy.submit")),
                BridgeEndpoint::new(prod.clone(), subject("deploy.submit")),
                prod_bridge.principal().clone(),
            ))
            .expect("add service import");

        let local_error = bus
            .request(
                &laptop_user,
                subject("gpu.deploy.submit"),
                Vec::new(),
                Duration::from_secs(1),
            )
            .unwrap_err();
        assert!(matches!(
            local_error,
            BusError::UnauthorizedPublish { island, principal, subject }
                if island == laptop && principal.as_str() == "user"
                    && subject.as_str() == "gpu.deploy.submit"
        ));

        let laptop_user = authority.grant_in(
            laptop,
            principal("user"),
            Grant::empty().with_publish(pattern("gpu.deploy.submit")),
        );
        let remote_error = bus
            .request(
                &laptop_user,
                subject("gpu.deploy.submit"),
                Vec::new(),
                Duration::from_secs(1),
            )
            .unwrap_err();
        assert!(matches!(
            remote_error,
            BusError::UnauthorizedPublish { island, principal, subject }
                if island == prod && principal.as_str() == "bridge"
                    && subject.as_str() == "deploy.submit"
        ));
    }

    #[test]
    fn service_import_conflicts_with_local_subscriber_in_both_orders() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let laptop = island("laptop");
        let prod = island("prod");
        let laptop_admin =
            authority.grant_in(laptop.clone(), principal("admin"), Grant::allow_all());
        let prod_bridge = authority.grant_in(
            prod.clone(),
            principal("bridge"),
            Grant::empty().with_publish(pattern("deploy.submit")),
        );
        bus.subscribe(&laptop_admin, pattern("gpu.deploy.submit"), |_| Ok(()))
            .expect("local subscriber registers");

        let add_error = authority
            .add_service_import(ServiceImport::new(
                rule_id("gpu-deploy"),
                BridgeEndpoint::new(laptop.clone(), subject("gpu.deploy.submit")),
                BridgeEndpoint::new(prod.clone(), subject("deploy.submit")),
                prod_bridge.principal().clone(),
            ))
            .unwrap_err();
        assert!(matches!(
            add_error,
            BusError::BridgeRuleInvalid {
                violation: BridgeRuleViolation::LocalResponderConflict { .. }
            }
        ));

        let (bus, authority) = MemoryBus::new_with_authority();
        let laptop_admin =
            authority.grant_in(laptop.clone(), principal("admin"), Grant::allow_all());
        let prod_bridge = authority.grant_in(
            prod.clone(),
            principal("bridge"),
            Grant::empty().with_publish(pattern("deploy.submit")),
        );
        authority
            .add_service_import(ServiceImport::new(
                rule_id("gpu-deploy"),
                BridgeEndpoint::new(laptop.clone(), subject("gpu.deploy.submit")),
                BridgeEndpoint::new(prod, subject("deploy.submit")),
                prod_bridge.principal().clone(),
            ))
            .expect("service import registers");

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_handler = Arc::clone(&calls);
        let subscribe_error = bus
            .subscribe(&laptop_admin, pattern("gpu.deploy.submit"), move |_| {
                calls_for_handler.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .unwrap_err();
        assert!(matches!(
            subscribe_error,
            BusError::BridgeRuleInvalid {
                violation: BridgeRuleViolation::LocalResponderConflict { .. }
            }
        ));
        bus.publish(
            &laptop_admin,
            subject("gpu.deploy.submit"),
            b"local".to_vec(),
        )
        .expect("publish does not register failed subscriber");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn service_import_conflicts_with_local_queue_subscriber_in_both_orders() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let laptop = island("laptop");
        let prod = island("prod");
        let laptop_admin =
            authority.grant_in(laptop.clone(), principal("admin"), Grant::allow_all());
        let prod_bridge = authority.grant_in(
            prod.clone(),
            principal("bridge"),
            Grant::empty().with_publish(pattern("deploy.submit")),
        );
        bus.queue_subscribe(
            &laptop_admin,
            pattern("gpu.deploy.submit"),
            "schedulers",
            |_| Ok(()),
        )
        .expect("local queue subscriber registers");

        let add_error = authority
            .add_service_import(ServiceImport::new(
                rule_id("gpu-deploy"),
                BridgeEndpoint::new(laptop.clone(), subject("gpu.deploy.submit")),
                BridgeEndpoint::new(prod.clone(), subject("deploy.submit")),
                prod_bridge.principal().clone(),
            ))
            .unwrap_err();
        assert!(matches!(
            add_error,
            BusError::BridgeRuleInvalid {
                violation: BridgeRuleViolation::LocalResponderConflict { .. }
            }
        ));

        let (bus, authority) = MemoryBus::new_with_authority();
        let laptop_admin =
            authority.grant_in(laptop.clone(), principal("admin"), Grant::allow_all());
        let prod_bridge = authority.grant_in(
            prod.clone(),
            principal("bridge"),
            Grant::empty().with_publish(pattern("deploy.submit")),
        );
        authority
            .add_service_import(ServiceImport::new(
                rule_id("gpu-deploy"),
                BridgeEndpoint::new(laptop.clone(), subject("gpu.deploy.submit")),
                BridgeEndpoint::new(prod, subject("deploy.submit")),
                prod_bridge.principal().clone(),
            ))
            .expect("service import registers");

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_handler = Arc::clone(&calls);
        let queue_error = bus
            .queue_subscribe(
                &laptop_admin,
                pattern("gpu.deploy.submit"),
                "schedulers",
                move |_| {
                    calls_for_handler.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            )
            .unwrap_err();
        assert!(matches!(
            queue_error,
            BusError::BridgeRuleInvalid {
                violation: BridgeRuleViolation::LocalResponderConflict { .. }
            }
        ));
        bus.publish(
            &laptop_admin,
            subject("gpu.deploy.submit"),
            b"local".to_vec(),
        )
        .expect("publish does not register failed queue subscriber");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn stream_import_delivers_with_bridge_origin() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let laptop = island("laptop");
        let prod = island("prod");
        let prod_admin = authority.grant_in(
            prod.clone(),
            principal("prod-admin"),
            Grant::empty().with_publish(pattern("deploy.*.status")),
        );
        let bridge = authority.grant_in(
            prod.clone(),
            principal("bridge"),
            Grant::empty().with_bridge_export(pattern("deploy.*.status")),
        );
        authority.grant_in(
            laptop.clone(),
            principal("bridge"),
            Grant::empty().with_publish(pattern("prod.deploy.*.status")),
        );
        let laptop_reader = authority.grant_in(
            laptop.clone(),
            principal("reader"),
            Grant::empty().with_subscribe(pattern("prod.deploy.*.status")),
        );
        authority
            .add_stream_import(StreamImport::new(
                rule_id("prod-status"),
                prod.clone(),
                laptop.clone(),
                bridge.principal().clone(),
                SubjectTransform::new(pattern("deploy.*.status"), pattern("prod.deploy.*.status"))
                    .expect("transform validates"),
            ))
            .expect("add stream import");

        let observed = Arc::new(Mutex::new(None));
        let observed_for_handler = Arc::clone(&observed);
        bus.subscribe(
            &laptop_reader,
            pattern("prod.deploy.*.status"),
            move |ctx| {
                *observed_for_handler.lock().expect("lock observed") = Some((
                    ctx.message.island().clone(),
                    ctx.message.principal().clone(),
                    ctx.message.subject().clone(),
                    ctx.message.bridge_origin().cloned(),
                ));
                Ok(())
            },
        )
        .expect("laptop reader subscribes");

        bus.publish(
            &prod_admin,
            subject("deploy.d1.status"),
            b"running".to_vec(),
        )
        .expect("prod publishes status");

        let observed = observed
            .lock()
            .expect("lock observed")
            .clone()
            .expect("observed imported status");
        assert_eq!(observed.0, laptop);
        assert_eq!(observed.1.as_str(), "bridge");
        assert_eq!(observed.2.as_str(), "prod.deploy.d1.status");
        let origin = observed.3.expect("bridge origin exists");
        assert_eq!(origin.rule_id().as_str(), "prod-status");
        assert_eq!(origin.source_island(), &prod);
        assert_eq!(origin.source_principal().as_str(), "prod-admin");
        assert_eq!(origin.original_subject().as_str(), "deploy.d1.status");
    }

    #[test]
    fn stream_import_requires_remote_export_and_local_publish_grants() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let laptop = island("laptop");
        let prod = island("prod");
        let prod_admin = authority.grant_in(
            prod.clone(),
            principal("prod-admin"),
            Grant::empty().with_publish(pattern("deploy.*.status")),
        );
        let bridge = authority.grant_in(prod.clone(), principal("bridge"), Grant::empty());
        let laptop_reader = authority.grant_in(
            laptop.clone(),
            principal("reader"),
            Grant::empty().with_subscribe(pattern("prod.deploy.*.status")),
        );
        authority
            .add_stream_import(StreamImport::new(
                rule_id("prod-status"),
                prod.clone(),
                laptop.clone(),
                bridge.principal().clone(),
                SubjectTransform::new(pattern("deploy.*.status"), pattern("prod.deploy.*.status"))
                    .expect("transform validates"),
            ))
            .expect("add stream import");

        let deliveries = Arc::new(AtomicUsize::new(0));
        let deliveries_for_handler = Arc::clone(&deliveries);
        bus.subscribe(&laptop_reader, pattern("prod.deploy.*.status"), move |_| {
            deliveries_for_handler.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .expect("laptop reader subscribes");

        bus.publish(
            &prod_admin,
            subject("deploy.d1.status"),
            b"running".to_vec(),
        )
        .expect("prod publish still succeeds without export grant");
        assert_eq!(deliveries.load(Ordering::SeqCst), 0);

        authority.grant_in(
            prod.clone(),
            principal("bridge"),
            Grant::empty().with_bridge_export(pattern("deploy.*.status")),
        );
        bus.publish(
            &prod_admin,
            subject("deploy.d1.status"),
            b"running".to_vec(),
        )
        .expect("prod publish still succeeds without local bridge publish");
        assert_eq!(deliveries.load(Ordering::SeqCst), 0);

        authority.grant_in(
            laptop,
            principal("bridge"),
            Grant::empty().with_publish(pattern("prod.deploy.*.status")),
        );
        bus.publish(
            &prod_admin,
            subject("deploy.d1.status"),
            b"running".to_vec(),
        )
        .expect("prod publish succeeds with both bridge grants");
        assert_eq!(deliveries.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn disabled_stream_import_skips_remote_delivery_but_keeps_local_publish() {
        let (bus, authority) = MemoryBus::new_with_authority();
        let laptop = island("laptop");
        let prod = island("prod");
        let prod_admin = authority.grant_in(
            prod.clone(),
            principal("prod-admin"),
            Grant::empty()
                .with_publish(pattern("deploy.*.status"))
                .with_subscribe(pattern("deploy.*.status")),
        );
        let bridge = authority.grant_in(
            prod.clone(),
            principal("bridge"),
            Grant::empty().with_bridge_export(pattern("deploy.*.status")),
        );
        authority.grant_in(
            laptop.clone(),
            principal("bridge"),
            Grant::empty().with_publish(pattern("prod.deploy.*.status")),
        );
        let laptop_reader = authority.grant_in(
            laptop.clone(),
            principal("reader"),
            Grant::empty().with_subscribe(pattern("prod.deploy.*.status")),
        );
        let rule = rule_id("prod-status");
        authority
            .add_stream_import(StreamImport::new(
                rule.clone(),
                prod.clone(),
                laptop,
                bridge.principal().clone(),
                SubjectTransform::new(pattern("deploy.*.status"), pattern("prod.deploy.*.status"))
                    .expect("transform validates"),
            ))
            .expect("add stream import");
        authority
            .set_stream_import_state(&rule, BridgeState::Disabled)
            .expect("disable stream import");

        let prod_deliveries = Arc::new(AtomicUsize::new(0));
        let laptop_deliveries = Arc::new(AtomicUsize::new(0));
        let prod_deliveries_for_handler = Arc::clone(&prod_deliveries);
        bus.subscribe(&prod_admin, pattern("deploy.*.status"), move |_| {
            prod_deliveries_for_handler.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .expect("prod subscribes locally");
        let laptop_deliveries_for_handler = Arc::clone(&laptop_deliveries);
        bus.subscribe(&laptop_reader, pattern("prod.deploy.*.status"), move |_| {
            laptop_deliveries_for_handler.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .expect("laptop subscribes");

        bus.publish(
            &prod_admin,
            subject("deploy.d1.status"),
            b"running".to_vec(),
        )
        .expect("prod publish succeeds");

        assert_eq!(prod_deliveries.load(Ordering::SeqCst), 1);
        assert_eq!(laptop_deliveries.load(Ordering::SeqCst), 0);
    }

    fn record_max(max_active: &AtomicUsize, active_now: usize) {
        max_active.fetch_max(active_now, Ordering::SeqCst);
    }
}
