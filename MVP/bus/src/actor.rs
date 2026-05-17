use std::sync::Arc;
use std::time::{Duration, Instant};

use kameo::Actor;
use kameo::actor::{ActorRef, Spawn};
use kameo::error::SendError;
use kameo::mailbox;
use kameo::message::{Context, Message};
use kameo::reply::DelegatedReply;

use crate::memory::{DEFAULT_PUBLISH_TIMEOUT, MemoryBus};
use crate::memory::{Handler, RequestManyDeadlinePolicy, deadline_after, remaining_until};
use crate::{
    ActorFailure, BusAuthority, BusError, BusRuntimeSnapshot, BusSession, Fact, FactContentHash,
    FactKey, FactWriteOutcome, HandlerOutcome, Payload, QueueName, RequestContext,
    RequestManyPolicy, RequestTarget, ResponseMessage, Result, Subject, SubjectPattern,
};

const BUS_ACTOR_MAILBOX_CAPACITY: usize = 64;
const BUS_ACTOR_MAILBOX_TIMEOUT: Duration = Duration::from_secs(5);
const BUS_ACTOR_REPLY_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Actor)]
pub(crate) struct BusActor {
    bus: MemoryBus,
}

impl BusActor {
    #[must_use]
    pub(crate) fn new(bus: MemoryBus) -> Self {
        Self { bus }
    }
}

#[derive(Clone)]
pub struct BusActorHandle {
    actor: ActorRef<BusActor>,
}

impl BusActorHandle {
    #[must_use]
    pub(crate) fn spawn(bus: MemoryBus) -> Self {
        Self {
            actor: BusActor::spawn_with_mailbox(
                BusActor::new(bus),
                mailbox::bounded(BUS_ACTOR_MAILBOX_CAPACITY),
            ),
        }
    }

    #[must_use]
    pub fn new_with_authority() -> (Self, BusAuthority) {
        let (bus, authority) = MemoryBus::new_with_authority();
        (Self::spawn(bus), authority)
    }

    #[must_use]
    pub fn new_with_authority_and_config(config: crate::BusRuntimeConfig) -> (Self, BusAuthority) {
        let (bus, authority) = MemoryBus::new_with_authority_and_config(config);
        (Self::spawn(bus), authority)
    }

    pub async fn subscribe<F>(
        &self,
        session: &BusSession,
        pattern: SubjectPattern,
        handler: F,
    ) -> Result<u64>
    where
        F: Fn(RequestContext) -> HandlerOutcome + Send + Sync + 'static,
    {
        self.ask(Subscribe {
            session: session.clone(),
            pattern,
            handler: Arc::new(handler),
        })
        .await
    }

    pub async fn queue_subscribe<F>(
        &self,
        session: &BusSession,
        pattern: SubjectPattern,
        queue: impl Into<QueueName>,
        handler: F,
    ) -> Result<u64>
    where
        F: Fn(RequestContext) -> HandlerOutcome + Send + Sync + 'static,
    {
        self.ask(QueueSubscribe {
            session: session.clone(),
            pattern,
            queue: queue.into(),
            handler: Arc::new(handler),
        })
        .await
    }

    pub async fn publish(
        &self,
        session: &BusSession,
        subject: Subject,
        payload: impl Into<Payload>,
    ) -> Result<()> {
        self.publish_with_timeout(session, subject, payload, DEFAULT_PUBLISH_TIMEOUT)
            .await
    }

    pub async fn publish_with_timeout(
        &self,
        session: &BusSession,
        subject: Subject,
        payload: impl Into<Payload>,
        timeout: Duration,
    ) -> Result<()> {
        let deadline = deadline_after(timeout);
        let timeout_subject = subject.to_string();
        self.ask_until(
            Publish {
                session: session.clone(),
                subject,
                payload: payload.into(),
                deadline,
            },
            deadline,
            timeout_subject,
        )
        .await
    }

    pub async fn request(
        &self,
        session: &BusSession,
        subject: Subject,
        payload: impl Into<Payload>,
        timeout: Duration,
    ) -> Result<ResponseMessage> {
        let deadline = deadline_after(timeout);
        let timeout_subject = subject.to_string();
        self.ask_until(
            RequestOne {
                session: session.clone(),
                subject,
                payload: payload.into(),
                deadline,
            },
            deadline,
            timeout_subject,
        )
        .await
    }

    pub async fn request_many(
        &self,
        session: &BusSession,
        target: RequestTarget,
        subject: Subject,
        payload: impl Into<Payload>,
        policy: RequestManyPolicy,
    ) -> Result<Vec<ResponseMessage>> {
        let payload = payload.into();
        if policy.max == 0 {
            return self
                .ask(RequestManyZero {
                    session: session.clone(),
                    target,
                    subject,
                    payload,
                })
                .await;
        }
        let deadline = deadline_after(policy.deadline);
        let timeout_subject = target.display();
        self.ask_until(
            RequestMany {
                session: session.clone(),
                target,
                subject,
                payload,
                policy: RequestManyDeadlinePolicy {
                    max: policy.max,
                    deadline,
                },
            },
            deadline,
            timeout_subject,
        )
        .await
    }

    pub async fn drain(&self, session: &BusSession, deadline: Duration) -> Result<()> {
        let deadline = deadline_after(deadline);
        self.ask_until(
            Drain {
                session: session.clone(),
                deadline,
            },
            deadline,
            String::from("drain"),
        )
        .await
    }

    pub async fn runtime_snapshot(&self) -> Result<BusRuntimeSnapshot> {
        self.ask(RuntimeSnapshot).await
    }

    pub async fn write_fact(
        &self,
        session: &BusSession,
        key: FactKey,
        content_hash: FactContentHash,
    ) -> Result<FactWriteOutcome> {
        self.ask(WriteFact {
            session: session.clone(),
            key,
            content_hash,
        })
        .await
    }

    pub async fn read_fact(&self, session: &BusSession, key: FactKey) -> Result<Option<Fact>> {
        self.ask(ReadFact {
            session: session.clone(),
            key,
        })
        .await
    }

    async fn ask<M, R>(&self, message: M) -> Result<R>
    where
        BusActor: Message<M, Reply = Result<R>>,
        M: Send + 'static,
        R: Send + 'static,
    {
        self.actor
            .ask(message)
            .mailbox_timeout(BUS_ACTOR_MAILBOX_TIMEOUT)
            .reply_timeout(BUS_ACTOR_REPLY_TIMEOUT)
            .await
            .map_err(map_actor_send_error)
    }

    async fn ask_until<M, R>(
        &self,
        message: M,
        deadline: Instant,
        timeout_subject: String,
    ) -> Result<R>
    where
        BusActor: Message<M, Reply = DelegatedReply<Result<R>>>,
        M: Send + 'static,
        R: Send + 'static,
    {
        let mailbox_timeout = remaining_until(deadline, timeout_subject.clone())?;
        let pending = self
            .actor
            .ask(message)
            .mailbox_timeout(mailbox_timeout)
            .enqueue()
            .await
            .map_err(|error| map_actor_mailbox_send_error(error, timeout_subject.clone()))?;
        let reply_timeout = remaining_until(deadline, timeout_subject.clone())?;
        match tokio::time::timeout(reply_timeout, pending).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(error)) => Err(map_actor_deadline_send_error(
                error,
                timeout_subject.clone(),
            )),
            Err(_) => Err(BusError::Timeout {
                subject: timeout_subject,
            }),
        }
    }
}

fn map_actor_deadline_send_error<M>(error: SendError<M, BusError>, subject: String) -> BusError {
    match error {
        SendError::Timeout(_) | SendError::MailboxFull(_) => BusError::Timeout { subject },
        other => map_actor_send_error(other),
    }
}

fn map_actor_mailbox_send_error(error: SendError, subject: String) -> BusError {
    match error {
        SendError::Timeout(_) | SendError::MailboxFull(_) => BusError::Timeout { subject },
        SendError::ActorNotRunning(_) => BusError::ActorUnavailable {
            actor: String::from("bus"),
            failure: ActorFailure::NotRunning,
        },
        SendError::ActorStopped => BusError::ActorUnavailable {
            actor: String::from("bus"),
            failure: ActorFailure::Stopped,
        },
        SendError::HandlerError(_) => BusError::ActorUnavailable {
            actor: String::from("bus"),
            failure: ActorFailure::MailboxSendFailed,
        },
    }
}

fn map_actor_send_error<M>(error: SendError<M, BusError>) -> BusError {
    match error {
        SendError::HandlerError(error) => error,
        SendError::ActorNotRunning(_) => BusError::ActorUnavailable {
            actor: String::from("bus"),
            failure: ActorFailure::NotRunning,
        },
        SendError::ActorStopped => BusError::ActorUnavailable {
            actor: String::from("bus"),
            failure: ActorFailure::Stopped,
        },
        SendError::MailboxFull(_) => BusError::ActorUnavailable {
            actor: String::from("bus"),
            failure: ActorFailure::MailboxFull,
        },
        SendError::Timeout(_) => BusError::ActorUnavailable {
            actor: String::from("bus"),
            failure: ActorFailure::AskTimeout,
        },
    }
}

struct Subscribe {
    session: BusSession,
    pattern: SubjectPattern,
    handler: Handler,
}

impl Message<Subscribe> for BusActor {
    type Reply = Result<u64>;

    async fn handle(
        &mut self,
        message: Subscribe,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.bus
            .subscribe_handler(&message.session, message.pattern, message.handler)
    }
}

struct QueueSubscribe {
    session: BusSession,
    pattern: SubjectPattern,
    queue: QueueName,
    handler: Handler,
}

impl Message<QueueSubscribe> for BusActor {
    type Reply = Result<u64>;

    async fn handle(
        &mut self,
        message: QueueSubscribe,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.bus.queue_subscribe_handler(
            &message.session,
            message.pattern,
            message.queue,
            message.handler,
        )
    }
}

struct Publish {
    session: BusSession,
    subject: Subject,
    payload: Payload,
    deadline: Instant,
}

impl Message<Publish> for BusActor {
    type Reply = DelegatedReply<Result<()>>;

    async fn handle(
        &mut self,
        message: Publish,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let bus = self.bus.clone();
        ctx.spawn(run_blocking(move || {
            bus.publish_until(
                &message.session,
                message.subject,
                message.payload,
                message.deadline,
            )
        }))
    }
}

struct RequestOne {
    session: BusSession,
    subject: Subject,
    payload: Payload,
    deadline: Instant,
}

impl Message<RequestOne> for BusActor {
    type Reply = DelegatedReply<Result<ResponseMessage>>;

    async fn handle(
        &mut self,
        message: RequestOne,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let bus = self.bus.clone();
        ctx.spawn(run_blocking(move || {
            bus.request_until(
                &message.session,
                message.subject,
                message.payload,
                message.deadline,
            )
        }))
    }
}

struct RequestMany {
    session: BusSession,
    target: RequestTarget,
    subject: Subject,
    payload: Payload,
    policy: RequestManyDeadlinePolicy,
}

struct RequestManyZero {
    session: BusSession,
    target: RequestTarget,
    subject: Subject,
    payload: Payload,
}

impl Message<RequestManyZero> for BusActor {
    type Reply = Result<Vec<ResponseMessage>>;

    async fn handle(
        &mut self,
        message: RequestManyZero,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.bus.request_many_until(
            &message.session,
            message.target,
            message.subject,
            message.payload,
            RequestManyDeadlinePolicy {
                max: 0,
                deadline: Instant::now(),
            },
        )
    }
}

impl Message<RequestMany> for BusActor {
    type Reply = DelegatedReply<Result<Vec<ResponseMessage>>>;

    async fn handle(
        &mut self,
        message: RequestMany,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let bus = self.bus.clone();
        ctx.spawn(run_blocking(move || {
            bus.request_many_until(
                &message.session,
                message.target,
                message.subject,
                message.payload,
                message.policy,
            )
        }))
    }
}

struct Drain {
    session: BusSession,
    deadline: Instant,
}

impl Message<Drain> for BusActor {
    type Reply = DelegatedReply<Result<()>>;

    async fn handle(
        &mut self,
        message: Drain,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let bus = self.bus.clone();
        ctx.spawn(run_blocking(move || {
            let remaining = remaining_until(message.deadline, "drain")?;
            bus.drain(&message.session, remaining)
        }))
    }
}

struct RuntimeSnapshot;

impl Message<RuntimeSnapshot> for BusActor {
    type Reply = Result<BusRuntimeSnapshot>;

    async fn handle(
        &mut self,
        _message: RuntimeSnapshot,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        Ok(self.bus.runtime_snapshot())
    }
}

struct WriteFact {
    session: BusSession,
    key: FactKey,
    content_hash: FactContentHash,
}

impl Message<WriteFact> for BusActor {
    type Reply = Result<FactWriteOutcome>;

    async fn handle(
        &mut self,
        message: WriteFact,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.bus
            .write_fact(&message.session, message.key, message.content_hash)
    }
}

struct ReadFact {
    session: BusSession,
    key: FactKey,
}

impl Message<ReadFact> for BusActor {
    type Reply = Result<Option<Fact>>;

    async fn handle(
        &mut self,
        message: ReadFact,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.bus.read_fact(&message.session, &message.key)
    }
}

async fn run_blocking<R>(operation: impl FnOnce() -> Result<R> + Send + 'static) -> Result<R>
where
    R: Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_error| BusError::ActorUnavailable {
            actor: String::from("bus-blocking-task"),
            failure: ActorFailure::BlockingTaskJoinFailed,
        })?
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;
    use std::time::Instant;

    use crate::{
        BusActorHandle, BusError, BusRuntimeConfig, FactContentHash, FactKey, FactKeyPattern,
        FactWriteOutcome, Grant, PrincipalId, RequestManyPolicy, RequestTarget, Subject,
    };

    use super::SubjectPattern;

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

    #[tokio::test]
    async fn actor_facade_routes_publish_request_and_request_many() {
        let (bus, authority) = BusActorHandle::new_with_authority();
        let admin = authority.grant(principal("admin"), Grant::allow_all());
        let status_count = Arc::new(AtomicUsize::new(0));
        let status_count_for_handler = Arc::clone(&status_count);
        bus.subscribe(&admin, pattern("node.*.status"), move |_| {
            status_count_for_handler.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await
        .expect("subscribe status");

        bus.subscribe(&admin, pattern("node.alpha.inspect"), |ctx| {
            ctx.reply(b"ok".to_vec())
        })
        .await
        .expect("subscribe inspect");

        for node in ["alpha", "beta"] {
            bus.subscribe(
                &admin,
                pattern(&format!("node.{node}.capacity")),
                move |ctx| ctx.reply(ctx.message.subject().as_str().as_bytes().to_vec()),
            )
            .await
            .expect("subscribe capacity");
        }

        bus.publish(&admin, subject("node.alpha.status"), b"healthy".to_vec())
            .await
            .expect("publish");
        let response = bus
            .request(
                &admin,
                subject("node.alpha.inspect"),
                Vec::new(),
                Duration::from_secs(1),
            )
            .await
            .expect("request");
        let replies = bus
            .request_many(
                &admin,
                RequestTarget::Pattern(pattern("node.*.capacity")),
                subject("node.broadcast.capacity"),
                Vec::new(),
                RequestManyPolicy::new(8, Duration::from_secs(1)),
            )
            .await
            .expect("request many");

        assert_eq!(status_count.load(Ordering::SeqCst), 1);
        assert_eq!(response.payload().as_bytes(), b"ok".as_slice());
        assert_eq!(replies.len(), 2);
        assert_eq!(
            bus.runtime_snapshot()
                .await
                .expect("snapshot")
                .delivery_workers,
            64
        );
    }

    #[tokio::test]
    async fn actor_facade_exposes_drain() {
        let (bus, authority) = BusActorHandle::new_with_authority();
        let admin = authority.grant(principal("admin"), Grant::allow_all());

        bus.drain(&admin, Duration::from_secs(1))
            .await
            .expect("drain");
        let error = bus
            .publish(&admin, subject("node.alpha.status"), Vec::new())
            .await
            .unwrap_err();

        assert!(matches!(error, crate::BusError::Draining));
    }

    #[tokio::test]
    async fn actor_facade_exposes_authorized_fact_writes() {
        let (bus, authority) = BusActorHandle::new_with_authority();
        let writer = authority.grant(
            principal("writer"),
            Grant::empty().with_fact_write(fact_pattern("/facts/deploy/>")),
        );
        let reader = authority.grant(principal("reader"), Grant::allow_all());

        let outcome = bus
            .write_fact(&writer, fact_key("/facts/deploy/d1/plan"), hash("b3:plan"))
            .await
            .expect("write fact");
        let fact = bus
            .read_fact(&reader, fact_key("/facts/deploy/d1/plan"))
            .await
            .expect("read fact")
            .expect("fact exists");

        assert!(matches!(outcome, FactWriteOutcome::Inserted(_)));
        assert_eq!(fact.author().as_str(), "writer");
        assert_eq!(fact.content_hash().as_str(), "b3:plan");
    }

    #[tokio::test]
    async fn actor_facade_zero_max_request_many_checks_authority_before_deadline() {
        let (bus, authority) = BusActorHandle::new_with_authority();
        let requester = authority.grant(principal("requester"), Grant::empty());

        let error = bus
            .request_many(
                &requester,
                RequestTarget::Pattern(pattern("node.*.capacity")),
                subject("node.broadcast.capacity"),
                Vec::new(),
                RequestManyPolicy::new(0, Duration::ZERO),
            )
            .await
            .unwrap_err();

        assert!(matches!(error, BusError::UnauthorizedRequestTarget { .. }));
    }

    #[tokio::test]
    async fn actor_facade_publish_timeout_uses_single_deadline() {
        let (bus, authority) = BusActorHandle::new_with_authority_and_config(
            BusRuntimeConfig::with_delivery_workers(1).with_delivery_queue_capacity(1),
        );
        let admin = authority.grant(principal("admin"), Grant::allow_all());
        for _ in 0..3 {
            bus.subscribe(&admin, pattern("gateway.changed"), move |_| {
                std::thread::sleep(Duration::from_millis(100));
                Ok(())
            })
            .await
            .expect("subscribe slow gateway handler");
        }

        let error = bus
            .publish_with_timeout(
                &admin,
                subject("gateway.changed"),
                Vec::new(),
                Duration::from_millis(5),
            )
            .await
            .unwrap_err();
        let snapshot = bus.runtime_snapshot().await.expect("snapshot");

        assert!(matches!(error, BusError::Timeout { .. }));
        assert!(snapshot.max_queued_deliveries >= 1);
        assert!(snapshot.enqueue_full_count > 0);
        assert!(snapshot.enqueue_blocked_ns > 0);
    }

    #[tokio::test]
    async fn actor_facade_drain_waits_for_inflight_request() {
        let (bus, authority) = BusActorHandle::new_with_authority();
        let admin = authority.grant(principal("admin"), Grant::allow_all());
        let completed = Arc::new(AtomicUsize::new(0));
        let completed_for_handler = Arc::clone(&completed);
        let (started_tx, started_rx) = mpsc::channel();
        bus.subscribe(&admin, pattern("node.alpha.inspect"), move |ctx| {
            let _ = started_tx.send(());
            std::thread::sleep(Duration::from_millis(30));
            completed_for_handler.fetch_add(1, Ordering::SeqCst);
            ctx.reply(b"ok".to_vec())
        })
        .await
        .expect("subscribe inspect");

        let bus_for_request = bus.clone();
        let admin_for_request = admin.clone();
        let request = tokio::spawn(async move {
            bus_for_request
                .request(
                    &admin_for_request,
                    subject("node.alpha.inspect"),
                    Vec::new(),
                    Duration::from_secs(1),
                )
                .await
        });
        tokio::task::spawn_blocking(move || started_rx.recv_timeout(Duration::from_secs(1)))
            .await
            .expect("started wait joins")
            .expect("handler started");

        bus.drain(&admin, Duration::from_secs(1))
            .await
            .expect("drain");
        let response = request.await.expect("request task joins").expect("request");

        assert_eq!(response.payload().as_bytes(), b"ok".as_slice());
        assert_eq!(completed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn actor_facade_drain_waits_for_queued_delivery_work() {
        let (bus, authority) = BusActorHandle::new_with_authority_and_config(
            BusRuntimeConfig::with_delivery_workers(1),
        );
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
            .await
            .expect("subscribe capacity");
        }
        drop(started_tx);

        let bus_for_request = bus.clone();
        let admin_for_request = admin.clone();
        let request = tokio::spawn(async move {
            bus_for_request
                .request_many(
                    &admin_for_request,
                    RequestTarget::Pattern(pattern("node.*.capacity")),
                    subject("node.broadcast.capacity"),
                    Vec::new(),
                    RequestManyPolicy::new(8, Duration::from_secs(1)),
                )
                .await
        });
        tokio::task::spawn_blocking(move || started_rx.recv_timeout(Duration::from_secs(1)))
            .await
            .expect("started wait joins")
            .expect("first handler started");

        bus.drain(&admin, Duration::from_secs(1))
            .await
            .expect("drain");
        let replies = request
            .await
            .expect("request task joins")
            .expect("request many");

        assert_eq!(replies.len(), 3);
        assert_eq!(completed.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn actor_facade_drain_times_out_waiting_for_inflight_work() {
        let (bus, authority) = BusActorHandle::new_with_authority();
        let admin = authority.grant(principal("admin"), Grant::allow_all());
        let (started_tx, started_rx) = mpsc::channel();
        bus.subscribe(&admin, pattern("node.alpha.inspect"), move |ctx| {
            let _ = started_tx.send(());
            std::thread::sleep(Duration::from_millis(50));
            ctx.reply(b"ok".to_vec())
        })
        .await
        .expect("subscribe inspect");

        let bus_for_request = bus.clone();
        let admin_for_request = admin.clone();
        let request = tokio::spawn(async move {
            bus_for_request
                .request(
                    &admin_for_request,
                    subject("node.alpha.inspect"),
                    Vec::new(),
                    Duration::from_secs(1),
                )
                .await
        });
        tokio::task::spawn_blocking(move || started_rx.recv_timeout(Duration::from_secs(1)))
            .await
            .expect("started wait joins")
            .expect("handler started");

        let error = bus
            .drain(&admin, Duration::from_millis(1))
            .await
            .unwrap_err();

        assert_eq!(
            error,
            BusError::Timeout {
                subject: String::from("drain"),
            }
        );
        request.await.expect("request task joins").expect("request");
    }

    #[tokio::test]
    async fn actor_facade_does_not_block_mailbox_while_request_waits() {
        let (bus, authority) = BusActorHandle::new_with_authority();
        let admin = authority.grant(principal("admin"), Grant::allow_all());
        let (started_tx, started_rx) = mpsc::channel();
        bus.subscribe(&admin, pattern("node.alpha.inspect"), move |ctx| {
            let _ = started_tx.send(());
            std::thread::sleep(Duration::from_millis(100));
            ctx.reply(b"ok".to_vec())
        })
        .await
        .expect("subscribe inspect");

        let bus_for_request = bus.clone();
        let admin_for_request = admin.clone();
        let request = tokio::spawn(async move {
            bus_for_request
                .request(
                    &admin_for_request,
                    subject("node.alpha.inspect"),
                    Vec::new(),
                    Duration::from_secs(1),
                )
                .await
        });
        tokio::task::spawn_blocking(move || started_rx.recv_timeout(Duration::from_secs(1)))
            .await
            .expect("started wait joins")
            .expect("handler started");

        let snapshot_started = Instant::now();
        let snapshot = bus.runtime_snapshot().await.expect("snapshot");

        assert!(snapshot_started.elapsed() < Duration::from_millis(50));
        assert_eq!(snapshot.delivery_workers, 64);
        assert_eq!(
            request
                .await
                .expect("request task joins")
                .expect("request")
                .payload()
                .as_bytes(),
            b"ok".as_slice()
        );
    }
}
