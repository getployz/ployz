use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::ops::Bound;

use crate::{IslandId, Payload, PrincipalId, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactKeyParseError {
    Empty,
    MissingLeadingSlash,
    EmptySegment,
    WildcardInKey,
    NonTerminalManyWildcard,
}

impl Display for FactKeyParseError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("fact key is empty"),
            Self::MissingLeadingSlash => f.write_str("fact key must start with '/'"),
            Self::EmptySegment => f.write_str("fact key contains an empty segment"),
            Self::WildcardInKey => f.write_str("concrete fact key may not contain wildcards"),
            Self::NonTerminalManyWildcard => f.write_str("multi-segment wildcard must be terminal"),
        }
    }
}

impl Error for FactKeyParseError {}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactKey {
    raw: String,
    segments: Vec<String>,
}

impl FactKey {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let segments = parse_fact_segments(&value, FactPathKind::Key)?;
        Ok(Self {
            raw: value,
            segments,
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    pub fn segments(&self) -> impl DoubleEndedIterator<Item = &str> + ExactSizeIterator {
        self.segments.iter().map(String::as_str)
    }
}

impl Display for FactKey {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactKeyPattern {
    raw: String,
    segments: Vec<FactKeyPatternSegment>,
}

impl FactKeyPattern {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let segments = parse_fact_segments(&value, FactPathKind::Pattern)?
            .into_iter()
            .map(|segment| match segment.as_str() {
                "*" => FactKeyPatternSegment::One,
                ">" => FactKeyPatternSegment::Many,
                _ => FactKeyPatternSegment::Literal(segment),
            })
            .collect();
        Ok(Self {
            raw: value,
            segments,
        })
    }

    #[must_use]
    pub fn matches(&self, key: &FactKey) -> bool {
        matches_fact_segments(&self.segments, &key.segments)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    fn scan_prefix(&self) -> Option<String> {
        let literal_count = self
            .segments
            .iter()
            .take_while(|segment| matches!(segment, FactKeyPatternSegment::Literal(_)))
            .count();
        if literal_count == 0 {
            return None;
        }

        let mut prefix = String::new();
        for segment in self.segments.iter().take(literal_count) {
            let FactKeyPatternSegment::Literal(value) = segment else {
                unreachable!("literal_count only covers literal segments");
            };
            prefix.push('/');
            prefix.push_str(value);
        }
        if literal_count < self.segments.len() {
            prefix.push('/');
        }
        Some(prefix)
    }
}

impl Display for FactKeyPattern {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FactKeyPatternSegment {
    Literal(String),
    One,
    Many,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactContentHash(String);

impl FactContentHash {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn for_payload(payload: &impl AsRef<[u8]>) -> Self {
        Self(format!("b3:{}", blake3::hash(payload.as_ref()).to_hex()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for FactContentHash {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub type FactPayload = Payload;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    island: IslandId,
    key: FactKey,
    author: PrincipalId,
    content_hash: FactContentHash,
}

impl Fact {
    fn new(
        island: IslandId,
        key: FactKey,
        author: PrincipalId,
        content_hash: FactContentHash,
    ) -> Self {
        Self {
            island,
            key,
            author,
            content_hash,
        }
    }

    #[must_use]
    pub fn island(&self) -> &IslandId {
        &self.island
    }

    #[must_use]
    pub fn key(&self) -> &FactKey {
        &self.key
    }

    #[must_use]
    pub fn author(&self) -> &PrincipalId {
        &self.author
    }

    #[must_use]
    pub fn content_hash(&self) -> &FactContentHash {
        &self.content_hash
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactWriteOutcome {
    Inserted(Fact),
    AlreadyPresent(Fact),
    Conflict(Fact),
}

#[derive(Debug, Default)]
pub(crate) struct InMemoryFactSet {
    facts: BTreeMap<FactIdentity, BTreeMap<FactContentHash, Fact>>,
    payloads: BTreeMap<FactPayloadIdentity, FactPayload>,
}

impl InMemoryFactSet {
    pub(crate) fn write(
        &mut self,
        island: IslandId,
        author: PrincipalId,
        key: FactKey,
        content_hash: FactContentHash,
    ) -> Result<FactWriteOutcome> {
        self.write_inner(island, author, key, content_hash, None)
    }

    pub(crate) fn write_payload(
        &mut self,
        island: IslandId,
        author: PrincipalId,
        key: FactKey,
        payload: FactPayload,
    ) -> Result<FactWriteOutcome> {
        let content_hash = FactContentHash::for_payload(&payload);
        self.write_inner(island, author, key, content_hash, Some(payload))
    }

    fn write_inner(
        &mut self,
        island: IslandId,
        author: PrincipalId,
        key: FactKey,
        content_hash: FactContentHash,
        payload: Option<FactPayload>,
    ) -> Result<FactWriteOutcome> {
        let identity = FactIdentity::new(&island, &key);
        if let Some(existing_by_hash) = self.facts.get_mut(&identity) {
            if let Some(existing) = existing_by_hash.get(&content_hash) {
                if let Some(payload) = payload {
                    self.payloads
                        .insert(FactPayloadIdentity::new(identity, content_hash), payload);
                }
                return Ok(FactWriteOutcome::AlreadyPresent(existing.clone()));
            }
            if existing_by_hash.len() >= 2 {
                let existing = existing_by_hash
                    .values()
                    .next()
                    .expect("existing_by_hash has at least one fact");
                return Ok(FactWriteOutcome::Conflict(existing.clone()));
            }
            let fact = Fact::new(island, key, author, content_hash.clone());
            existing_by_hash.insert(content_hash.clone(), fact.clone());
            if let Some(payload) = payload {
                self.payloads
                    .insert(FactPayloadIdentity::new(identity, content_hash), payload);
            }
            return Ok(FactWriteOutcome::Conflict(fact));
        }

        let fact = Fact::new(island, key, author, content_hash.clone());
        self.facts.insert(
            identity.clone(),
            BTreeMap::from([(content_hash.clone(), fact.clone())]),
        );
        if let Some(payload) = payload {
            self.payloads
                .insert(FactPayloadIdentity::new(identity, content_hash), payload);
        }
        Ok(FactWriteOutcome::Inserted(fact))
    }

    pub(crate) fn read(&self, island: &IslandId, key: &FactKey) -> Option<Fact> {
        let facts = self.facts.get(&FactIdentity::new(island, key))?;
        match facts.values().collect::<Vec<_>>().as_slice() {
            [fact] => Some((*fact).clone()),
            _ => None,
        }
    }

    pub(crate) fn read_exact(
        &self,
        island: &IslandId,
        key: &FactKey,
        content_hash: &FactContentHash,
    ) -> Option<Fact> {
        self.facts
            .get(&FactIdentity::new(island, key))?
            .get(content_hash)
            .cloned()
    }

    pub(crate) fn list_filtered(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
        mut include: impl FnMut(&Fact) -> bool,
    ) -> Vec<Fact> {
        self.facts
            .range(FactIdentity::scan_bounds(island, pattern))
            .filter(|(identity, _facts)| {
                identity.island == *island && pattern.matches(&identity.key)
            })
            .flat_map(|(_identity, facts)| facts.values())
            .filter(|fact| include(fact))
            .cloned()
            .collect()
    }

    pub(crate) fn payload(
        &self,
        island: &IslandId,
        key: &FactKey,
        content_hash: &FactContentHash,
    ) -> Option<FactPayload> {
        self.payloads
            .get(&FactPayloadIdentity::new(
                FactIdentity::new(island, key),
                content_hash.clone(),
            ))
            .cloned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct FactIdentity {
    island: IslandId,
    key: FactKey,
}

impl FactIdentity {
    fn new(island: &IslandId, key: &FactKey) -> Self {
        Self {
            island: island.clone(),
            key: key.clone(),
        }
    }

    fn scan_bounds(
        island: &IslandId,
        pattern: &FactKeyPattern,
    ) -> (Bound<FactIdentity>, Bound<FactIdentity>) {
        let start = pattern.scan_prefix().unwrap_or_default();
        let mut end = start.clone();
        end.push(char::MAX);
        (
            Bound::Included(Self {
                island: island.clone(),
                key: FactKey {
                    raw: start,
                    segments: Vec::new(),
                },
            }),
            Bound::Excluded(Self {
                island: island.clone(),
                key: FactKey {
                    raw: end,
                    segments: Vec::new(),
                },
            }),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct FactPayloadIdentity {
    fact: FactIdentity,
    content_hash: FactContentHash,
}

impl FactPayloadIdentity {
    fn new(fact: FactIdentity, content_hash: FactContentHash) -> Self {
        Self { fact, content_hash }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FactPathKind {
    Key,
    Pattern,
}

fn parse_fact_segments(
    value: &str,
    kind: FactPathKind,
) -> std::result::Result<Vec<String>, FactKeyParseError> {
    if value.is_empty() {
        return Err(FactKeyParseError::Empty);
    }
    let Some(stripped) = value.strip_prefix('/') else {
        return Err(FactKeyParseError::MissingLeadingSlash);
    };
    let segments = stripped.split('/').map(str::to_string).collect::<Vec<_>>();
    if segments.is_empty() || segments.iter().any(|segment| segment.is_empty()) {
        return Err(FactKeyParseError::EmptySegment);
    }
    for (index, segment) in segments.iter().enumerate() {
        match (kind, segment.as_str()) {
            (FactPathKind::Key, "*" | ">") => return Err(FactKeyParseError::WildcardInKey),
            (FactPathKind::Pattern, ">") if index + 1 != segments.len() => {
                return Err(FactKeyParseError::NonTerminalManyWildcard);
            }
            (FactPathKind::Pattern, "*" | ">") => {}
            (FactPathKind::Pattern, _) => {}
            (FactPathKind::Key, _) => {}
        }
    }
    Ok(segments)
}

fn matches_fact_segments(pattern: &[FactKeyPatternSegment], key: &[String]) -> bool {
    match (pattern, key) {
        ([FactKeyPatternSegment::Many], key_segments) => !key_segments.is_empty(),
        ([], []) => true,
        ([], [_head, ..]) => false,
        ([FactKeyPatternSegment::One, pattern_tail @ ..], [_key_head, key_tail @ ..]) => {
            matches_fact_segments(pattern_tail, key_tail)
        }
        ([FactKeyPatternSegment::One, ..], []) => false,
        (
            [FactKeyPatternSegment::Literal(literal), pattern_tail @ ..],
            [key_head, key_tail @ ..],
        ) if literal == key_head => matches_fact_segments(pattern_tail, key_tail),
        ([FactKeyPatternSegment::Literal(_), ..], []) => false,
        ([FactKeyPatternSegment::Literal(_), ..], [_key_head, ..]) => false,
        ([FactKeyPatternSegment::Many, ..], _key_segments) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FactContentHash, FactKey, FactKeyPattern, FactPayload, FactWriteOutcome, InMemoryFactSet,
    };
    use crate::{IslandId, PrincipalId};

    fn island(name: &str) -> IslandId {
        IslandId::new(name)
    }

    fn principal(name: &str) -> PrincipalId {
        PrincipalId::new(name)
    }

    fn key(value: &str) -> FactKey {
        FactKey::parse(value).expect("fact key parses")
    }

    fn pattern(value: &str) -> FactKeyPattern {
        FactKeyPattern::parse(value).expect("fact key pattern parses")
    }

    fn hash(value: &str) -> FactContentHash {
        FactContentHash::new(value)
    }

    fn payload(value: &str) -> FactPayload {
        value.to_string().into()
    }

    #[test]
    fn fact_pattern_matches_slash_keys() {
        assert!(pattern("/facts/deploy/>").matches(&key("/facts/deploy/d1/plan")));
        assert!(pattern("/facts/node/*/joined/*").matches(&key("/facts/node/n1/joined/e1")));
        assert!(!pattern("/facts/node/*/joined/*").matches(&key("/facts/node/n1/tombstoned/e1")));
    }

    #[test]
    fn fact_keys_reject_extra_leading_slashes() {
        assert!(FactKey::parse("//facts/deploy/d1/plan").is_err());
    }

    #[test]
    fn fact_writes_are_immutable_idempotent_and_allow_conflict_candidates() {
        let mut facts = InMemoryFactSet::default();
        let inserted = facts
            .write(
                island("prod"),
                principal("admin"),
                key("/facts/a"),
                hash("h1"),
            )
            .expect("insert fact");
        let repeated = facts
            .write(
                island("prod"),
                principal("admin"),
                key("/facts/a"),
                hash("h1"),
            )
            .expect("repeat fact");
        let conflict = facts
            .write(
                island("prod"),
                principal("admin"),
                key("/facts/a"),
                hash("h2"),
            )
            .expect("write conflicting candidate");

        assert!(matches!(inserted, FactWriteOutcome::Inserted(_)));
        assert!(matches!(repeated, FactWriteOutcome::AlreadyPresent(_)));
        assert!(matches!(conflict, FactWriteOutcome::Conflict(_)));
        assert_eq!(
            facts
                .list_filtered(&island("prod"), &pattern("/facts/>"), |_| true)
                .len(),
            2
        );
        assert!(facts.read(&island("prod"), &key("/facts/a")).is_none());

        let third = facts
            .write(
                island("prod"),
                principal("admin"),
                key("/facts/a"),
                hash("h3"),
            )
            .expect("third conflicting write is bounded");
        assert!(matches!(third, FactWriteOutcome::Conflict(_)));
        assert_eq!(
            facts
                .list_filtered(&island("prod"), &pattern("/facts/>"), |_| true)
                .len(),
            2
        );
    }

    #[test]
    fn fact_reads_are_island_scoped() {
        let mut facts = InMemoryFactSet::default();
        facts
            .write(
                island("prod"),
                principal("admin"),
                key("/facts/a"),
                hash("h1"),
            )
            .expect("insert fact");

        assert!(facts.read(&island("prod"), &key("/facts/a")).is_some());
        assert!(facts.read(&island("laptop"), &key("/facts/a")).is_none());
    }

    #[test]
    fn payload_writes_derive_hash_and_store_body() {
        let mut facts = InMemoryFactSet::default();
        let payload = payload("route-commit");
        let expected_hash = FactContentHash::for_payload(&payload);
        let inserted = facts
            .write_payload(
                island("prod"),
                principal("admin"),
                key("/facts/routes/r1"),
                payload.clone(),
            )
            .expect("insert payload fact");

        let FactWriteOutcome::Inserted(fact) = inserted else {
            panic!("expected inserted fact");
        };
        assert_eq!(fact.content_hash(), &expected_hash);
        assert_eq!(
            facts.payload(&island("prod"), &key("/facts/routes/r1"), &expected_hash),
            Some(payload)
        );
    }

    #[test]
    fn payload_writes_store_conflicting_bodies_by_hash() {
        let mut facts = InMemoryFactSet::default();
        let key = key("/facts/routes/r1");
        let first_payload = payload("route-commit");
        let first_hash = FactContentHash::for_payload(&first_payload);
        let changed_payload = payload("changed");
        let changed_hash = FactContentHash::for_payload(&changed_payload);
        facts
            .write_payload(
                island("prod"),
                principal("admin"),
                key.clone(),
                first_payload.clone(),
            )
            .expect("insert payload fact");
        let repeated = facts
            .write_payload(
                island("prod"),
                principal("admin"),
                key.clone(),
                payload("route-commit"),
            )
            .expect("repeat payload fact");
        let conflict = facts
            .write_payload(
                island("prod"),
                principal("admin"),
                key.clone(),
                changed_payload.clone(),
            )
            .expect("write conflicting payload fact");

        assert!(matches!(repeated, FactWriteOutcome::AlreadyPresent(_)));
        assert!(matches!(conflict, FactWriteOutcome::Conflict(_)));
        assert_eq!(
            facts.payload(&island("prod"), &key, &first_hash),
            Some(first_payload)
        );
        assert_eq!(
            facts.payload(&island("prod"), &key, &changed_hash),
            Some(changed_payload)
        );
        let dropped_payload = payload("dropped");
        let dropped_hash = FactContentHash::for_payload(&dropped_payload);
        let third = facts
            .write_payload(
                island("prod"),
                principal("admin"),
                key.clone(),
                dropped_payload,
            )
            .expect("third conflicting payload write is bounded");
        assert!(matches!(third, FactWriteOutcome::Conflict(_)));
        assert_eq!(facts.payload(&island("prod"), &key, &dropped_hash), None);
    }

    #[test]
    fn fact_list_is_deterministic_and_island_scoped() {
        let mut facts = InMemoryFactSet::default();
        facts
            .write(
                island("prod"),
                principal("admin"),
                key("/facts/service/web/node-2"),
                hash("h2"),
            )
            .expect("insert fact");
        facts
            .write(
                island("laptop"),
                principal("admin"),
                key("/facts/service/web/node-1"),
                hash("h-laptop"),
            )
            .expect("insert laptop fact");
        facts
            .write(
                island("prod"),
                principal("admin"),
                key("/facts/service/web/node-1"),
                hash("h1"),
            )
            .expect("insert fact");

        let listed = facts.list_filtered(&island("prod"), &pattern("/facts/service/>"), |_| true);
        let keys = listed
            .iter()
            .map(|fact| fact.key().as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            keys,
            vec!["/facts/service/web/node-1", "/facts/service/web/node-2"]
        );
    }
}
