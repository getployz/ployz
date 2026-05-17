use std::collections::BTreeMap;
use std::fmt::{self, Display, Formatter};

use crate::{FactKey, FactKeyPattern, RequestTarget, Subject, SubjectPattern};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IslandId(String);

impl IslandId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn default_for_mvp() -> Self {
        Self::new("default")
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for IslandId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

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
    island: IslandId,
    principal: PrincipalId,
}

impl BusSession {
    pub(crate) fn new(island: IslandId, principal: PrincipalId) -> Self {
        Self { island, principal }
    }

    #[must_use]
    pub fn island(&self) -> &IslandId {
        &self.island
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
    fact_write_allow: Vec<FactKeyPattern>,
    fact_write_deny: Vec<FactKeyPattern>,
    fact_read_allow: Vec<FactKeyPattern>,
    fact_read_deny: Vec<FactKeyPattern>,
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
            fact_write_allow: vec![FactKeyPattern::parse("/>").expect("valid allow-all pattern")],
            fact_write_deny: Vec::new(),
            fact_read_allow: vec![FactKeyPattern::parse("/>").expect("valid allow-all pattern")],
            fact_read_deny: Vec::new(),
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
    pub fn with_fact_write(mut self, pattern: FactKeyPattern) -> Self {
        self.fact_write_allow.push(pattern);
        self
    }

    #[must_use]
    pub fn with_fact_write_deny(mut self, pattern: FactKeyPattern) -> Self {
        self.fact_write_deny.push(pattern);
        self
    }

    #[must_use]
    pub fn with_fact_read(mut self, pattern: FactKeyPattern) -> Self {
        self.fact_read_allow.push(pattern);
        self
    }

    #[must_use]
    pub fn with_fact_read_deny(mut self, pattern: FactKeyPattern) -> Self {
        self.fact_read_deny.push(pattern);
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

    #[must_use]
    pub(crate) fn can_write_fact(&self, key: &FactKey) -> bool {
        pattern_allows_key(&self.fact_write_allow, &self.fact_write_deny, key)
    }

    #[must_use]
    pub(crate) fn can_read_fact(&self, key: &FactKey) -> bool {
        pattern_allows_key(&self.fact_read_allow, &self.fact_read_deny, key)
    }
}

fn pattern_allows_key(allow: &[FactKeyPattern], deny: &[FactKeyPattern], key: &FactKey) -> bool {
    allow.iter().any(|pattern| pattern.matches(key))
        && !deny.iter().any(|pattern| pattern.matches(key))
}

#[derive(Debug, Clone, Default)]
pub(crate) struct GrantBook {
    grants: BTreeMap<IslandId, BTreeMap<PrincipalId, Grant>>,
}

impl GrantBook {
    pub(crate) fn set(&mut self, island: IslandId, principal: PrincipalId, grant: Grant) {
        self.grants
            .entry(island)
            .or_default()
            .insert(principal, grant);
    }

    pub(crate) fn revoke(&mut self, island: &IslandId, principal: &PrincipalId) -> bool {
        let Some(island_grants) = self.grants.get_mut(island) else {
            return false;
        };
        island_grants.remove(principal).is_some()
    }

    pub(crate) fn can_publish(
        &self,
        island: &IslandId,
        principal: &PrincipalId,
        subject: &Subject,
    ) -> bool {
        self.grant(island, principal)
            .is_some_and(|grant| grant.can_publish(subject))
    }

    pub(crate) fn can_publish_target(
        &self,
        island: &IslandId,
        principal: &PrincipalId,
        target: &RequestTarget,
    ) -> bool {
        self.grant(island, principal)
            .is_some_and(|grant| grant.can_publish_target(target))
    }

    pub(crate) fn can_subscribe(
        &self,
        island: &IslandId,
        principal: &PrincipalId,
        pattern: &SubjectPattern,
    ) -> bool {
        self.grant(island, principal)
            .is_some_and(|grant| grant.can_subscribe(pattern))
    }

    pub(crate) fn can_queue_subscribe(
        &self,
        island: &IslandId,
        principal: &PrincipalId,
        pattern: &SubjectPattern,
        queue: &QueueName,
    ) -> bool {
        self.grant(island, principal)
            .is_some_and(|grant| grant.can_queue_subscribe(pattern, queue))
    }

    pub(crate) fn can_respond(&self, island: &IslandId, principal: &PrincipalId) -> bool {
        self.grant(island, principal)
            .is_some_and(Grant::can_respond)
    }

    pub(crate) fn can_drain(&self, island: &IslandId, principal: &PrincipalId) -> bool {
        self.grant(island, principal).is_some_and(Grant::can_drain)
    }

    pub(crate) fn can_write_fact(
        &self,
        island: &IslandId,
        principal: &PrincipalId,
        key: &FactKey,
    ) -> bool {
        self.grant(island, principal)
            .is_some_and(|grant| grant.can_write_fact(key))
    }

    fn grant(&self, island: &IslandId, principal: &PrincipalId) -> Option<&Grant> {
        self.grants
            .get(island)
            .and_then(|island_grants| island_grants.get(principal))
    }

    pub(crate) fn can_read_fact(
        &self,
        island: &IslandId,
        principal: &PrincipalId,
        key: &FactKey,
    ) -> bool {
        self.grant(island, principal)
            .is_some_and(|grant| grant.can_read_fact(key))
    }
}
