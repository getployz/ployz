use std::sync::Arc;
use std::time::Duration;

use kameo::Actor;
use kameo::actor::{ActorRef, Spawn};
use kameo::error::SendError;
use kameo::message::{Context, Message};

use crate::memory::Handler;
use crate::{
    BusAuthority, BusError, BusRuntimeSnapshot, BusSession, HandlerOutcome, MemoryBus, Payload,
    QueueName, RequestContext, RequestManyPolicy, RequestTarget, ResponseMessage, Result, Subject,
    SubjectPattern,
};

#[derive(Actor)]
pub struct BusActor {
    bus: MemoryBus,
}

impl BusActor {
    #[must_use]
    pub fn new(bus: MemoryBus) -> Self {
        Self { bus }
    }
}

#[derive(Clone)]
pub struct BusActorHandle {
    actor: ActorRef<BusActor>,
}

impl BusActorHandle {
    #[must_use]
    pub fn spawn(bus: MemoryBus) -> Self {
        Self {
            actor: BusActor::spawn(BusActor::new(bus)),
        }
    }

    #[must_use]
    pub fn new_with_authority() -> (Self, BusAuthority) {
        let (bus, authority) = MemoryBus::new_with_authority();
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
        self.ask(Publish {
            session: session.clone(),
            subject,
            payload: payload.into(),
        })
        .await
    }

    pub async fn request(
        &self,
        session: &BusSession,
        subject: Subject,
        payload: impl Into<Payload>,
        timeout: Duration,
    ) -> Result<ResponseMessage> {
        self.ask(RequestOne {
            session: session.clone(),
            subject,
            payload: payload.into(),
            timeout,
        })
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
        self.ask(RequestMany {
            session: session.clone(),
            target,
            subject,
            payload: payload.into(),
            policy,
        })
        .await
    }

    pub async fn drain(&self, session: &BusSession, deadline: Duration) -> Result<()> {
        self.ask(Drain {
            session: session.clone(),
            deadline,
        })
        .await
    }

    pub async fn runtime_snapshot(&self) -> Result<BusRuntimeSnapshot> {
        self.ask(RuntimeSnapshot).await
    }

    async fn ask<M, R>(&self, message: M) -> Result<R>
    where
        BusActor: Message<M, Reply = Result<R>>,
        M: Send + 'static,
        R: Send + 'static,
    {
        self.actor.ask(message).await.map_err(map_actor_send_error)
    }
}

fn map_actor_send_error<M>(error: SendError<M, BusError>) -> BusError {
    match error {
        SendError::HandlerError(error) => error,
        SendError::ActorNotRunning(_) => BusError::ActorUnavailable {
            actor: String::from("bus"),
            reason: String::from("not running"),
        },
        SendError::ActorStopped => BusError::ActorUnavailable {
            actor: String::from("bus"),
            reason: String::from("stopped"),
        },
        SendError::MailboxFull(_) => BusError::ActorUnavailable {
            actor: String::from("bus"),
            reason: String::from("mailbox full"),
        },
        SendError::Timeout(_) => BusError::ActorUnavailable {
            actor: String::from("bus"),
            reason: String::from("ask timed out"),
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
}

impl Message<Publish> for BusActor {
    type Reply = Result<()>;

    async fn handle(
        &mut self,
        message: Publish,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.bus
            .publish(&message.session, message.subject, message.payload)
    }
}

struct RequestOne {
    session: BusSession,
    subject: Subject,
    payload: Payload,
    timeout: Duration,
}

impl Message<RequestOne> for BusActor {
    type Reply = Result<ResponseMessage>;

    async fn handle(
        &mut self,
        message: RequestOne,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.bus.request(
            &message.session,
            message.subject,
            message.payload,
            message.timeout,
        )
    }
}

struct RequestMany {
    session: BusSession,
    target: RequestTarget,
    subject: Subject,
    payload: Payload,
    policy: RequestManyPolicy,
}

impl Message<RequestMany> for BusActor {
    type Reply = Result<Vec<ResponseMessage>>;

    async fn handle(
        &mut self,
        message: RequestMany,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.bus.request_many(
            &message.session,
            message.target,
            message.subject,
            message.payload,
            message.policy,
        )
    }
}

struct Drain {
    session: BusSession,
    deadline: Duration,
}

impl Message<Drain> for BusActor {
    type Reply = Result<()>;

    async fn handle(
        &mut self,
        message: Drain,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.bus.drain(&message.session, message.deadline)
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use crate::{BusActorHandle, Grant, PrincipalId, RequestManyPolicy, RequestTarget, Subject};

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
                move |ctx| ctx.reply(ctx.message.subject.as_str().as_bytes().to_vec()),
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
        assert_eq!(response.payload, b"ok".to_vec());
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
}
