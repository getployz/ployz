use std::fmt::{self, Display, Formatter};
use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use bytes::Bytes;

use crate::{BridgeOrigin, BusError, IslandId, PrincipalId, Result, Subject, SubjectPattern};

#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Payload(Bytes);

impl Payload {
    #[must_use]
    pub fn from_static(bytes: &'static [u8]) -> Self {
        Self(Bytes::from_static(bytes))
    }

    #[must_use]
    pub fn copy_from_slice(bytes: &[u8]) -> Self {
        Self(Bytes::copy_from_slice(bytes))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_ref()
    }
}

impl AsRef<[u8]> for Payload {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl Deref for Payload {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.as_bytes()
    }
}

impl From<Vec<u8>> for Payload {
    fn from(value: Vec<u8>) -> Self {
        Self(Bytes::from(value))
    }
}

impl From<&'static [u8]> for Payload {
    fn from(value: &'static [u8]) -> Self {
        Self::from_static(value)
    }
}

impl From<String> for Payload {
    fn from(value: String) -> Self {
        value.into_bytes().into()
    }
}

impl PartialEq<Vec<u8>> for Payload {
    fn eq(&self, other: &Vec<u8>) -> bool {
        self.as_bytes() == other.as_slice()
    }
}

impl PartialEq<Payload> for Vec<u8> {
    fn eq(&self, other: &Payload) -> bool {
        self.as_slice() == other.as_bytes()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageId(u64);

impl MessageId {
    #[must_use]
    pub(crate) fn new(value: u64) -> Self {
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
    id: MessageId,
    island: IslandId,
    subject: Subject,
    payload: Payload,
    principal: PrincipalId,
    reply_to: Option<ReplyInbox>,
    bridge_origin: Option<BridgeOrigin>,
}

impl BusMessage {
    #[must_use]
    pub(crate) fn new(
        id: MessageId,
        island: IslandId,
        subject: Subject,
        principal: PrincipalId,
        payload: Payload,
    ) -> Self {
        Self {
            id,
            island,
            subject,
            payload,
            principal,
            reply_to: None,
            bridge_origin: None,
        }
    }

    #[must_use]
    pub fn subject(&self) -> &Subject {
        &self.subject
    }

    #[must_use]
    pub fn island(&self) -> &IslandId {
        &self.island
    }

    #[must_use]
    pub fn payload(&self) -> &Payload {
        &self.payload
    }

    #[must_use]
    pub fn principal(&self) -> &PrincipalId {
        &self.principal
    }

    #[must_use]
    pub fn bridge_origin(&self) -> Option<&BridgeOrigin> {
        self.bridge_origin.as_ref()
    }

    pub(crate) fn id(&self) -> MessageId {
        self.id
    }

    pub(crate) fn set_reply_to(&mut self, reply_to: ReplyInbox) {
        self.reply_to = Some(reply_to);
    }

    pub(crate) fn set_bridge_origin(&mut self, bridge_origin: BridgeOrigin) {
        self.bridge_origin = Some(bridge_origin);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyInbox {
    subject: Subject,
}

impl ReplyInbox {
    pub(crate) fn new(subject: Subject) -> Self {
        Self { subject }
    }
}

#[derive(Debug, Clone)]
pub struct ResponseMessage {
    request_id: MessageId,
    island: IslandId,
    responder: PrincipalId,
    payload: Payload,
}

impl ResponseMessage {
    #[must_use]
    pub fn request_id(&self) -> u64 {
        self.request_id.value()
    }

    #[must_use]
    pub fn island(&self) -> &IslandId {
        &self.island
    }

    #[must_use]
    pub fn responder(&self) -> &PrincipalId {
        &self.responder
    }

    #[must_use]
    pub fn payload(&self) -> &Payload {
        &self.payload
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ResponseEnvelope {
    Reply(ResponseMessage),
    HandlerError(BusError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    island: IslandId,
    responder: PrincipalId,
    expires_at: Instant,
    used: Arc<AtomicBool>,
    tx: Sender<ResponseEnvelope>,
}

impl ReplyPermit {
    pub(crate) fn new(
        inbox: ReplyInbox,
        request_id: MessageId,
        island: IslandId,
        responder: PrincipalId,
        expires_at: Instant,
        tx: Sender<ResponseEnvelope>,
    ) -> Self {
        Self {
            inbox,
            request_id,
            island,
            responder,
            expires_at,
            used: Arc::new(AtomicBool::new(false)),
            tx,
        }
    }

    pub fn respond(&self, payload: impl Into<Payload>) -> Result<()> {
        self.respond_as(&self.responder, payload)
    }

    pub fn respond_as(&self, principal: &PrincipalId, payload: impl Into<Payload>) -> Result<()> {
        if principal != &self.responder || Instant::now() >= self.expires_at {
            return Err(BusError::UnauthorizedResponse {
                island: self.island.clone(),
                principal: principal.clone(),
                inbox: Box::new(self.inbox.subject.clone()),
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
                island: self.island.clone(),
                responder: principal.clone(),
                payload: payload.into(),
            }))
            .map_err(|_| BusError::ResponseClosed {
                inbox: self.inbox.subject.to_string(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::Payload;

    #[test]
    fn payload_clone_shares_backing_storage() {
        let payload = Payload::from(vec![1, 2, 3, 4]);
        let clone = payload.clone();

        assert_eq!(payload.as_bytes(), clone.as_bytes());
        assert!(std::ptr::eq(
            payload.as_bytes().as_ptr(),
            clone.as_bytes().as_ptr()
        ));
    }
}
