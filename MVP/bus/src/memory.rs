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

use crate::grants::GrantBook;
use crate::message::{MessageId, ReplyInbox, ReplyPermit};
use crate::{
    BusError, BusMessage, BusSession, Grant, Payload, PrincipalId, QueueName, RequestManyPolicy,
    RequestTarget, ResponseEnvelope, ResponseMessage, Result, Subject, SubjectPattern,
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
    max_deliveries: Option<usize>,
    queue_dispatch: QueueDispatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueueDispatch {
    Include,
    Exclude,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RequestManyDeadlinePolicy {
    pub max: usize,
    pub deadline: Instant,
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
            return Err(BusError::ResponseClosed {
                inbox: String::from("<none>"),
            });
        };
        reply_permit.respond(payload)
    }

    pub fn reply_as(&self, principal: &PrincipalId, payload: impl Into<Payload>) -> Result<()> {
        let Some(reply_permit) = &self.reply_permit else {
            return Err(BusError::ResponseClosed {
                inbox: String::from("<none>"),
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

    fn set_grant(&self, principal: PrincipalId, grant: Grant) -> BusSession {
        self.inner
            .lock()
            .expect("memory bus mutex poisoned")
            .grants
            .set(principal.clone(), grant);
        BusSession::new(principal)
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
        let principal = session.principal().clone();
        if !inner.grants.can_subscribe(&principal, &pattern) {
            return Err(BusError::UnauthorizedSubscribe { principal, pattern });
        }
        let id = inner.next_id();
        inner.subscribers.push(Subscriber {
            id,
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
        let principal = session.principal().clone();
        if !inner
            .grants
            .can_queue_subscribe(&principal, &pattern, &queue)
        {
            return Err(BusError::UnauthorizedQueue {
                principal,
                pattern,
                queue,
            });
        }
        let id = inner.next_id();
        inner.queue_subscribers.push(QueueSubscriber {
            id,
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
            let principal = session.principal().clone();
            if !inner.grants.can_publish(&principal, &subject) {
                return Err(BusError::UnauthorizedPublish { principal, subject });
            }
            let message = BusMessage::new(
                MessageId::new(inner.next_id()),
                subject.clone(),
                principal,
                payload,
            );
            let dispatch =
                inner.dispatch_for_subject(&subject, message, None, QueueDispatch::Include);
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
                max_deliveries: None,
                queue_dispatch: QueueDispatch::Include,
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
            return Ok(Vec::new());
        }
        let target_display = target.display();
        let (rx, dispatch) = self.prepare_request(
            session,
            RequestDispatchSpec {
                target,
                subject,
                payload: payload.into(),
                deadline: policy.deadline,
                max_deliveries: Some(policy.max),
                queue_dispatch: QueueDispatch::Exclude,
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
                target: target_display,
                expected,
                received: replies.len(),
            });
        }
        Ok(replies)
    }

    pub fn drain(&self, session: &BusSession, deadline: Duration) -> Result<()> {
        {
            let mut inner = self.inner.lock().expect("memory bus mutex poisoned");
            if !inner.grants.can_drain(session.principal()) {
                return Err(BusError::UnauthorizedDrain {
                    principal: session.principal().clone(),
                });
            }
            inner.draining = true;
        }
        self.inflight.wait_for_idle(deadline)
    }

    fn prepare_request(
        &self,
        session: &BusSession,
        spec: RequestDispatchSpec,
    ) -> Result<(mpsc::Receiver<ResponseEnvelope>, Vec<Delivery>)> {
        let (tx, rx) = mpsc::channel();
        let target_display = spec.target.display();
        let dispatch = {
            let mut inner = self.inner.lock().expect("memory bus mutex poisoned");
            inner.ensure_not_draining()?;
            let principal = session.principal().clone();
            if !inner.grants.can_publish_target(&principal, &spec.target)
                || !inner.grants.can_publish(&principal, &spec.subject)
            {
                return Err(BusError::UnauthorizedPublish {
                    principal,
                    subject: spec.subject,
                });
            }

            let id = MessageId::new(inner.next_id());
            let inbox = ReplyInbox::new(Subject::parse(format!("_INBOX.{}", id.value()))?);
            let mut message = BusMessage::new(id, spec.subject.clone(), principal, spec.payload);
            message.set_reply_to(inbox.clone());
            let reply = ReplySpec {
                inbox,
                expires_at: spec.deadline,
                tx,
            };
            let dispatch = inner.dispatch_for_target(
                &spec.target,
                message,
                Some(reply),
                spec.max_deliveries,
                spec.queue_dispatch,
            );
            if dispatch.is_empty() {
                return Err(BusError::NoResponders {
                    subject: target_display,
                });
            }
            self.inflight.add(dispatch.len());
            dispatch
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
        self.bus.set_grant(principal, grant)
    }
}

struct Inner {
    subscribers: Vec<Subscriber>,
    queue_subscribers: Vec<QueueSubscriber>,
    queue_cursor: BTreeMap<QueueName, usize>,
    grants: GrantBook,
    next_id: u64,
    draining: bool,
}

impl Inner {
    fn new() -> Self {
        Self {
            subscribers: Vec::new(),
            queue_subscribers: Vec::new(),
            queue_cursor: BTreeMap::new(),
            grants: GrantBook::default(),
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

    fn dispatch_for_subject(
        &mut self,
        subject: &Subject,
        message: BusMessage,
        reply: Option<ReplySpec>,
        queue_dispatch: QueueDispatch,
    ) -> Vec<Delivery> {
        let target = RequestTarget::Subject(subject.clone());
        self.dispatch_for_target(&target, message, reply, None, queue_dispatch)
    }

    fn dispatch_for_target(
        &mut self,
        target: &RequestTarget,
        message: BusMessage,
        reply: Option<ReplySpec>,
        max_deliveries: Option<usize>,
        queue_dispatch: QueueDispatch,
    ) -> Vec<Delivery> {
        let mut dispatch = Vec::new();

        for subscriber in &self.subscribers {
            if target.matches_subject_pattern(&subscriber.pattern)
                && self
                    .grants
                    .can_subscribe(&subscriber.principal, &subscriber.pattern)
                && reply
                    .as_ref()
                    .is_none_or(|_| self.grants.can_respond(&subscriber.principal))
            {
                dispatch.push(subscriber.delivery(message.clone(), reply.clone()));
                if max_deliveries.is_some_and(|max| dispatch.len() >= max) {
                    return dispatch;
                }
            }
        }

        if queue_dispatch == QueueDispatch::Exclude {
            return dispatch;
        }

        let mut groups = BTreeMap::<QueueName, Vec<QueueSubscriber>>::new();
        for subscriber in &self.queue_subscribers {
            if target.matches_subject_pattern(&subscriber.pattern)
                && self.grants.can_queue_subscribe(
                    &subscriber.principal,
                    &subscriber.pattern,
                    &subscriber.queue,
                )
                && reply
                    .as_ref()
                    .is_none_or(|_| self.grants.can_respond(&subscriber.principal))
            {
                groups
                    .entry(subscriber.queue.clone())
                    .or_default()
                    .push(subscriber.clone());
            }
        }
        for (group_key, subscribers) in groups {
            let cursor = self.queue_cursor.entry(group_key).or_insert(0);
            let selected = subscribers[*cursor % subscribers.len()].clone();
            *cursor += 1;
            dispatch.push(selected.delivery(message.clone(), reply.clone()));
            if max_deliveries.is_some_and(|max| dispatch.len() >= max) {
                return dispatch;
            }
        }

        dispatch
    }
}

trait TargetMatch {
    fn matches_subject_pattern(&self, pattern: &SubjectPattern) -> bool;
}

impl TargetMatch for RequestTarget {
    fn matches_subject_pattern(&self, pattern: &SubjectPattern) -> bool {
        match self {
            RequestTarget::Subject(subject) => pattern.matches(subject),
            RequestTarget::Pattern(target_pattern) => target_pattern.overlaps(pattern),
        }
    }
}

#[derive(Clone)]
struct Subscriber {
    id: u64,
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
        BusError, BusRuntimeConfig, BusSession, Grant, Payload, PrincipalId, RequestManyPolicy,
        RequestTarget, Subject, SubjectPattern,
    };

    fn principal(name: &str) -> PrincipalId {
        PrincipalId::new(name)
    }

    fn subject(value: &str) -> Subject {
        Subject::parse(value).expect("subject parses")
    }

    fn pattern(value: &str) -> SubjectPattern {
        SubjectPattern::parse(value).expect("pattern parses")
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
                subject: String::from("node.alpha.inspect")
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
                reason: String::from("boom"),
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
                reason: String::from("boom"),
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

        assert!(matches!(error, BusError::UnauthorizedPublish { .. }));
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
                target: String::from("node.*.capacity"),
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

        assert!(matches!(error, BusError::UnauthorizedPublish { .. }));
        assert_eq!(received.load(Ordering::SeqCst), 0);
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
        bus.subscribe(&admin, pattern("node.alpha.inspect"), move |ctx| {
            ctx.reply(b"first".to_vec())?;
            let error = ctx.reply(b"second".to_vec()).unwrap_err();
            *duplicate_error_for_handler
                .lock()
                .expect("lock duplicate error") = Some(error);
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

    fn record_max(max_active: &AtomicUsize, active_now: usize) {
        max_active.fetch_max(active_now, Ordering::SeqCst);
    }
}
