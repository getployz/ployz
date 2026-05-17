use std::collections::BTreeMap;
use std::fmt::{self, Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use crate::{BusError, PrincipalId, Result, Subject, SubjectPattern};

pub type Headers = BTreeMap<String, String>;
pub type Payload = Vec<u8>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageId(u64);

impl MessageId {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }
}

impl Display for MessageId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "msg-{}", self.0)
    }
}

#[derive(Debug, Clone)]
pub struct BusMessage {
    pub id: MessageId,
    pub subject: Subject,
    pub headers: Headers,
    pub payload: Payload,
    pub principal: PrincipalId,
    pub reply_to: Option<ReplyInbox>,
}

impl BusMessage {
    #[must_use]
    pub fn new(id: MessageId, subject: Subject, principal: PrincipalId, payload: Payload) -> Self {
        Self {
            id,
            subject,
            headers: Headers::new(),
            payload,
            principal,
            reply_to: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyInbox {
    subject: Subject,
}

impl ReplyInbox {
    pub fn new(subject: Subject) -> Self {
        Self { subject }
    }

    #[must_use]
    pub fn subject(&self) -> &Subject {
        &self.subject
    }
}

#[derive(Debug, Clone)]
pub struct ResponseMessage {
    pub request_id: MessageId,
    pub responder: PrincipalId,
    pub payload: Payload,
}

#[derive(Debug, Clone)]
pub(crate) enum ResponseEnvelope {
    Reply(ResponseMessage),
    HandlerError(BusError),
}

#[derive(Debug, Clone)]
pub enum RequestTarget {
    Subject(Subject),
    Pattern(SubjectPattern),
}

impl RequestTarget {
    #[must_use]
    pub fn matches(&self, subject: &Subject) -> bool {
        match self {
            Self::Subject(target) => target == subject,
            Self::Pattern(pattern) => pattern.matches(subject),
        }
    }

    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Self::Subject(subject) => subject.to_string(),
            Self::Pattern(pattern) => pattern.to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestManyPolicy {
    pub max: usize,
    pub deadline: Duration,
}

impl RequestManyPolicy {
    #[must_use]
    pub fn new(max: usize, deadline: Duration) -> Self {
        Self { max, deadline }
    }
}

#[derive(Debug, Clone)]
pub struct ReplyPermit {
    inbox: ReplyInbox,
    request_id: MessageId,
    responder: PrincipalId,
    expires_at: Instant,
    used: Arc<AtomicBool>,
    tx: Sender<ResponseEnvelope>,
}

impl ReplyPermit {
    pub(crate) fn new(
        inbox: ReplyInbox,
        request_id: MessageId,
        responder: PrincipalId,
        expires_at: Instant,
        tx: Sender<ResponseEnvelope>,
    ) -> Self {
        Self {
            inbox,
            request_id,
            responder,
            expires_at,
            used: Arc::new(AtomicBool::new(false)),
            tx,
        }
    }

    pub fn respond(&self, payload: Payload) -> Result<()> {
        self.respond_as(&self.responder, payload)
    }

    pub fn respond_as(&self, principal: &PrincipalId, payload: Payload) -> Result<()> {
        if principal != &self.responder || Instant::now() > self.expires_at {
            return Err(BusError::UnauthorizedResponse {
                principal: principal.clone(),
                inbox: self.inbox.subject.clone(),
            });
        }
        if self.used.swap(true, Ordering::SeqCst) {
            return Err(BusError::DuplicateResponse {
                inbox: self.inbox.subject.to_string(),
            });
        }
        self.tx
            .send(ResponseEnvelope::Reply(ResponseMessage {
                request_id: self.request_id,
                responder: principal.clone(),
                payload,
            }))
            .map_err(|_| BusError::ResponseClosed {
                inbox: self.inbox.subject.to_string(),
            })
    }
}
