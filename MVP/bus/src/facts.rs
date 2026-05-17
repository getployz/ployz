use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};

use crate::{BusError, IslandId, PrincipalId, Result};

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
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for FactContentHash {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

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
}

#[derive(Debug, Default)]
pub(crate) struct InMemoryFactSet {
    facts: BTreeMap<FactIdentity, Fact>,
}

impl InMemoryFactSet {
    pub(crate) fn write(
        &mut self,
        island: IslandId,
        author: PrincipalId,
        key: FactKey,
        content_hash: FactContentHash,
    ) -> Result<FactWriteOutcome> {
        let identity = FactIdentity::new(&island, &key);
        if let Some(existing) = self.facts.get(&identity) {
            if existing.content_hash == content_hash {
                return Ok(FactWriteOutcome::AlreadyPresent(existing.clone()));
            }
            return Err(BusError::FactConflict {
                island,
                key: Box::new(key),
                existing: existing.content_hash.clone(),
                attempted: content_hash,
            });
        }

        let fact = Fact::new(island, key, author, content_hash);
        self.facts.insert(identity, fact.clone());
        Ok(FactWriteOutcome::Inserted(fact))
    }

    pub(crate) fn read(&self, island: &IslandId, key: &FactKey) -> Option<Fact> {
        self.facts.get(&FactIdentity::new(island, key)).cloned()
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
    use super::{FactContentHash, FactKey, FactKeyPattern, FactWriteOutcome, InMemoryFactSet};
    use crate::{BusError, IslandId, PrincipalId};

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
    fn fact_writes_are_immutable_and_idempotent() {
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
            .unwrap_err();

        assert!(matches!(inserted, FactWriteOutcome::Inserted(_)));
        assert!(matches!(repeated, FactWriteOutcome::AlreadyPresent(_)));
        assert!(matches!(conflict, BusError::FactConflict { .. }));
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
}
