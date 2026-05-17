use std::collections::BTreeMap;
use std::fmt::{self, Display, Formatter};

use crate::{RequestTarget, Subject, SubjectPattern};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrincipalId(String);

impl PrincipalId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QueueName(String);

impl QueueName {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for QueueName {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for QueueName {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl Display for QueueName {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Display for PrincipalId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusSession {
    principal: PrincipalId,
}

impl BusSession {
    pub(crate) fn new(principal: PrincipalId) -> Self {
        Self { principal }
    }

    #[must_use]
    pub fn principal(&self) -> &PrincipalId {
        &self.principal
    }
}

#[derive(Debug, Clone, Default)]
pub struct Grant {
    publish_allow: Vec<SubjectPattern>,
    subscribe_allow: Vec<SubjectPattern>,
    queue_allow: Vec<QueueGrant>,
    response_allow: bool,
    drain_allowed: bool,
}

#[derive(Debug, Clone)]
struct QueueGrant {
    subject: SubjectPattern,
    queue: QueueGrantQueue,
}

impl QueueGrant {
    fn named(subject: SubjectPattern, queue: QueueName) -> Self {
        Self {
            subject,
            queue: QueueGrantQueue::Named(queue),
        }
    }

    fn any(subject: SubjectPattern) -> Self {
        Self {
            subject,
            queue: QueueGrantQueue::Any,
        }
    }

    fn allows(&self, pattern: &SubjectPattern, queue: &QueueName) -> bool {
        self.subject.subsumes(pattern)
            && match &self.queue {
                QueueGrantQueue::Any => true,
                QueueGrantQueue::Named(allowed) => allowed == queue,
            }
    }
}

#[derive(Debug, Clone)]
enum QueueGrantQueue {
    Any,
    Named(QueueName),
}

impl Grant {
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn allow_all() -> Self {
        Self {
            publish_allow: vec![SubjectPattern::parse(">").expect("valid allow-all pattern")],
            subscribe_allow: vec![SubjectPattern::parse(">").expect("valid allow-all pattern")],
            queue_allow: vec![QueueGrant::any(
                SubjectPattern::parse(">").expect("valid allow-all pattern"),
            )],
            response_allow: true,
            drain_allowed: true,
        }
    }

    #[must_use]
    pub fn with_publish(mut self, pattern: SubjectPattern) -> Self {
        self.publish_allow.push(pattern);
        self
    }

    #[must_use]
    pub fn with_subscribe(mut self, pattern: SubjectPattern) -> Self {
        self.subscribe_allow.push(pattern);
        self
    }

    #[must_use]
    pub fn with_queue(mut self, pattern: SubjectPattern, queue: impl Into<QueueName>) -> Self {
        self.queue_allow
            .push(QueueGrant::named(pattern, queue.into()));
        self
    }

    #[must_use]
    pub fn with_response(mut self) -> Self {
        self.response_allow = true;
        self
    }

    #[must_use]
    pub fn with_drain(mut self) -> Self {
        self.drain_allowed = true;
        self
    }

    #[must_use]
    pub(crate) fn can_publish(&self, subject: &Subject) -> bool {
        self.publish_allow
            .iter()
            .any(|pattern| pattern.matches(subject))
    }

    #[must_use]
    pub(crate) fn can_publish_target(&self, target: &RequestTarget) -> bool {
        match target {
            RequestTarget::Subject(subject) => self.can_publish(subject),
            RequestTarget::Pattern(pattern) => self
                .publish_allow
                .iter()
                .any(|allowed| allowed.subsumes(pattern)),
        }
    }

    #[must_use]
    pub(crate) fn can_subscribe(&self, pattern: &SubjectPattern) -> bool {
        self.subscribe_allow
            .iter()
            .any(|allowed| allowed.subsumes(pattern))
    }

    #[must_use]
    pub(crate) fn can_queue_subscribe(&self, pattern: &SubjectPattern, queue: &QueueName) -> bool {
        self.queue_allow
            .iter()
            .any(|allowed| allowed.allows(pattern, queue))
    }

    #[must_use]
    pub(crate) fn can_respond(&self) -> bool {
        self.response_allow
    }

    #[must_use]
    pub(crate) fn can_drain(&self) -> bool {
        self.drain_allowed
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct GrantBook {
    grants: BTreeMap<PrincipalId, Grant>,
}

impl GrantBook {
    pub(crate) fn set(&mut self, principal: PrincipalId, grant: Grant) {
        self.grants.insert(principal, grant);
    }

    pub(crate) fn can_publish(&self, principal: &PrincipalId, subject: &Subject) -> bool {
        self.grants
            .get(principal)
            .is_some_and(|grant| grant.can_publish(subject))
    }

    pub(crate) fn can_publish_target(
        &self,
        principal: &PrincipalId,
        target: &RequestTarget,
    ) -> bool {
        self.grants
            .get(principal)
            .is_some_and(|grant| grant.can_publish_target(target))
    }

    pub(crate) fn can_subscribe(&self, principal: &PrincipalId, pattern: &SubjectPattern) -> bool {
        self.grants
            .get(principal)
            .is_some_and(|grant| grant.can_subscribe(pattern))
    }

    pub(crate) fn can_queue_subscribe(
        &self,
        principal: &PrincipalId,
        pattern: &SubjectPattern,
        queue: &QueueName,
    ) -> bool {
        self.grants
            .get(principal)
            .is_some_and(|grant| grant.can_queue_subscribe(pattern, queue))
    }

    pub(crate) fn can_respond(&self, principal: &PrincipalId) -> bool {
        self.grants.get(principal).is_some_and(Grant::can_respond)
    }

    pub(crate) fn can_drain(&self, principal: &PrincipalId) -> bool {
        self.grants.get(principal).is_some_and(Grant::can_drain)
    }
}
