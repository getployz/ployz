use std::collections::BTreeMap;
use std::fmt::{self, Display, Formatter};

use crate::{IslandId, PrincipalId, Subject, SubjectPattern};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BridgeRuleId(String);

impl BridgeRuleId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for BridgeRuleId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeState {
    Enabled,
    Disabled,
    RemoteUnavailable,
}

impl Display for BridgeState {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Enabled => f.write_str("enabled"),
            Self::Disabled => f.write_str("disabled"),
            Self::RemoteUnavailable => f.write_str("remote unavailable"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeOrigin {
    rule_id: BridgeRuleId,
    source_island: IslandId,
    source_principal: PrincipalId,
    original_subject: Subject,
}

impl BridgeOrigin {
    #[must_use]
    pub(crate) fn new(
        rule_id: BridgeRuleId,
        source_island: IslandId,
        source_principal: PrincipalId,
        original_subject: Subject,
    ) -> Self {
        Self {
            rule_id,
            source_island,
            source_principal,
            original_subject,
        }
    }

    #[must_use]
    pub fn rule_id(&self) -> &BridgeRuleId {
        &self.rule_id
    }

    #[must_use]
    pub fn source_island(&self) -> &IslandId {
        &self.source_island
    }

    #[must_use]
    pub fn source_principal(&self) -> &PrincipalId {
        &self.source_principal
    }

    #[must_use]
    pub fn original_subject(&self) -> &Subject {
        &self.original_subject
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeEndpoint {
    island: IslandId,
    subject: Subject,
}

impl BridgeEndpoint {
    #[must_use]
    pub fn new(island: IslandId, subject: Subject) -> Self {
        Self { island, subject }
    }

    #[must_use]
    pub fn island(&self) -> &IslandId {
        &self.island
    }

    #[must_use]
    pub fn subject(&self) -> &Subject {
        &self.subject
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceImport {
    id: BridgeRuleId,
    local_island: IslandId,
    local_subject: Subject,
    remote_island: IslandId,
    remote_subject: Subject,
    bridge_principal: PrincipalId,
    state: BridgeState,
}

impl ServiceImport {
    #[must_use]
    pub fn new(
        id: BridgeRuleId,
        local: BridgeEndpoint,
        remote: BridgeEndpoint,
        bridge_principal: PrincipalId,
    ) -> Self {
        Self {
            id,
            local_island: local.island,
            local_subject: local.subject,
            remote_island: remote.island,
            remote_subject: remote.subject,
            bridge_principal,
            state: BridgeState::Enabled,
        }
    }

    #[must_use]
    pub fn id(&self) -> &BridgeRuleId {
        &self.id
    }

    #[must_use]
    pub fn local_island(&self) -> &IslandId {
        &self.local_island
    }

    #[must_use]
    pub fn local_subject(&self) -> &Subject {
        &self.local_subject
    }

    #[must_use]
    pub fn remote_island(&self) -> &IslandId {
        &self.remote_island
    }

    #[must_use]
    pub fn remote_subject(&self) -> &Subject {
        &self.remote_subject
    }

    #[must_use]
    pub fn bridge_principal(&self) -> &PrincipalId {
        &self.bridge_principal
    }

    #[must_use]
    pub fn state(&self) -> BridgeState {
        self.state
    }

    pub(crate) fn set_state(&mut self, state: BridgeState) {
        self.state = state;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamImport {
    id: BridgeRuleId,
    source_island: IslandId,
    local_island: IslandId,
    bridge_principal: PrincipalId,
    transform: SubjectTransform,
    state: BridgeState,
}

impl StreamImport {
    #[must_use]
    pub fn new(
        id: BridgeRuleId,
        source_island: IslandId,
        local_island: IslandId,
        bridge_principal: PrincipalId,
        transform: SubjectTransform,
    ) -> Self {
        Self {
            id,
            source_island,
            local_island,
            bridge_principal,
            transform,
            state: BridgeState::Enabled,
        }
    }

    #[must_use]
    pub fn id(&self) -> &BridgeRuleId {
        &self.id
    }

    #[must_use]
    pub fn source_island(&self) -> &IslandId {
        &self.source_island
    }

    #[must_use]
    pub fn local_island(&self) -> &IslandId {
        &self.local_island
    }

    #[must_use]
    pub fn bridge_principal(&self) -> &PrincipalId {
        &self.bridge_principal
    }

    #[must_use]
    pub fn transform(&self) -> &SubjectTransform {
        &self.transform
    }

    #[must_use]
    pub fn state(&self) -> BridgeState {
        self.state
    }

    pub(crate) fn set_state(&mut self, state: BridgeState) {
        self.state = state;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubjectTransform {
    source: SubjectPattern,
    destination: SubjectPattern,
    source_capture_positions: Vec<usize>,
    destination_segments: Vec<TransformSegment>,
}

impl SubjectTransform {
    pub fn new(
        source: SubjectPattern,
        destination: SubjectPattern,
    ) -> std::result::Result<Self, BridgeRuleViolation> {
        let source_wildcards = single_token_wildcards(source.as_str())?;
        let destination_wildcards = single_token_wildcards(destination.as_str())?;
        if source_wildcards != destination_wildcards {
            return Err(BridgeRuleViolation::CaptureMismatch {
                source: source.to_string(),
                destination: destination.to_string(),
            });
        }
        let source_capture_positions = capture_positions(source.as_str())?;
        let destination_segments = destination_segments(destination.as_str())?;
        Ok(Self {
            source,
            destination,
            source_capture_positions,
            destination_segments,
        })
    }

    #[must_use]
    pub fn source(&self) -> &SubjectPattern {
        &self.source
    }

    #[must_use]
    pub fn destination(&self) -> &SubjectPattern {
        &self.destination
    }

    #[must_use]
    pub fn apply(&self, subject: &Subject) -> Option<Subject> {
        if !self.source.matches(subject) {
            return None;
        }
        Some(self.apply_after_match(subject))
    }

    #[must_use]
    pub(crate) fn apply_after_match(&self, subject: &Subject) -> Subject {
        let captures = captures_for(&self.source_capture_positions, subject);
        let mut output = Vec::new();
        for segment in &self.destination_segments {
            match segment {
                TransformSegment::Literal(value) => output.push(value.clone()),
                TransformSegment::Capture(index) => output.push(captures[*index].to_string()),
            }
        }
        Subject::parse(output.join(".")).expect("validated transform creates concrete subject")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TransformSegment {
    Literal(String),
    Capture(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeRuleSet {
    service_imports: Vec<ServiceImport>,
    service_imports_by_local: BTreeMap<(IslandId, Subject), usize>,
    stream_imports: Vec<StreamImport>,
    stream_imports_by_source: BTreeMap<IslandId, Vec<usize>>,
    stream_imports_by_source_and_local: BTreeMap<(IslandId, IslandId), Vec<usize>>,
    rule_ids: BTreeMap<BridgeRuleId, BridgeRuleIndex>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BridgeRuleIndex {
    Service(usize),
    Stream(usize),
}

impl BridgeRuleSet {
    #[must_use]
    pub(crate) fn empty() -> Self {
        Self {
            service_imports: Vec::new(),
            service_imports_by_local: BTreeMap::new(),
            stream_imports: Vec::new(),
            stream_imports_by_source: BTreeMap::new(),
            stream_imports_by_source_and_local: BTreeMap::new(),
            rule_ids: BTreeMap::new(),
        }
    }

    pub(crate) fn add_service_import(
        &mut self,
        import: ServiceImport,
    ) -> std::result::Result<(), BridgeRuleViolation> {
        validate_not_self_import(import.local_island(), import.remote_island())?;
        validate_unique_rule_id(&self.rule_ids, import.id())?;
        let local_key = (
            import.local_island().clone(),
            import.local_subject().clone(),
        );
        if self.service_imports_by_local.contains_key(&local_key) {
            return Err(BridgeRuleViolation::DuplicateServiceImport {
                local_island: import.local_island().clone(),
                local_subject: import.local_subject().clone(),
            });
        }
        let import_index = self.service_imports.len();
        self.rule_ids
            .insert(import.id().clone(), BridgeRuleIndex::Service(import_index));
        self.service_imports_by_local
            .insert(local_key, import_index);
        self.service_imports.push(import);
        Ok(())
    }

    pub(crate) fn add_stream_import(
        &mut self,
        import: StreamImport,
    ) -> std::result::Result<(), BridgeRuleViolation> {
        validate_not_self_import(import.local_island(), import.source_island())?;
        validate_unique_rule_id(&self.rule_ids, import.id())?;
        let stream_key = (
            import.source_island().clone(),
            import.local_island().clone(),
        );
        if let Some(existing_indexes) = self.stream_imports_by_source_and_local.get(&stream_key) {
            for existing_index in existing_indexes {
                let existing = &self.stream_imports[*existing_index];
                if existing
                    .transform()
                    .source()
                    .overlaps(import.transform().source())
                {
                    return Err(BridgeRuleViolation::AmbiguousStreamImport {
                        existing: existing.id().clone(),
                        attempted: import.id().clone(),
                    });
                }
            }
        }
        let source_island = import.source_island().clone();
        let import_index = self.stream_imports.len();
        self.rule_ids
            .insert(import.id().clone(), BridgeRuleIndex::Stream(import_index));
        self.stream_imports.push(import);
        self.stream_imports_by_source
            .entry(source_island)
            .or_default()
            .push(import_index);
        self.stream_imports_by_source_and_local
            .entry(stream_key)
            .or_default()
            .push(import_index);
        Ok(())
    }

    #[must_use]
    pub(crate) fn service_imports(&self) -> &[ServiceImport] {
        &self.service_imports
    }

    #[must_use]
    pub(crate) fn service_import_for(
        &self,
        local_island: &IslandId,
        local_subject: &Subject,
    ) -> Option<&ServiceImport> {
        let key = (local_island.clone(), local_subject.clone());
        self.service_imports_by_local
            .get(&key)
            .and_then(|index| self.service_imports.get(*index))
    }

    pub(crate) fn matching_stream_imports<'a>(
        &'a self,
        source_island: &IslandId,
        subject: &'a Subject,
    ) -> impl Iterator<Item = &'a StreamImport> + 'a {
        self.stream_imports_by_source
            .get(source_island)
            .into_iter()
            .flat_map(|indexes| indexes.iter())
            .filter_map(|index| self.stream_imports.get(*index))
            .filter(|import| import.transform().source().matches(subject))
    }

    pub(crate) fn set_service_import_state(
        &mut self,
        id: &BridgeRuleId,
        state: BridgeState,
    ) -> std::result::Result<(), BridgeRuleViolation> {
        let Some(BridgeRuleIndex::Service(index)) = self.rule_ids.get(id).copied() else {
            return Err(BridgeRuleViolation::RuleNotFound { id: id.clone() });
        };
        let import = &mut self.service_imports[index];
        import.set_state(state);
        Ok(())
    }

    pub(crate) fn set_stream_import_state(
        &mut self,
        id: &BridgeRuleId,
        state: BridgeState,
    ) -> std::result::Result<(), BridgeRuleViolation> {
        let Some(BridgeRuleIndex::Stream(index)) = self.rule_ids.get(id).copied() else {
            return Err(BridgeRuleViolation::RuleNotFound { id: id.clone() });
        };
        let import = &mut self.stream_imports[index];
        import.set_state(state);
        Ok(())
    }
}

impl Default for BridgeRuleSet {
    fn default() -> Self {
        Self::empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeRuleViolation {
    SelfImport {
        island: IslandId,
    },
    DuplicateServiceImport {
        local_island: IslandId,
        local_subject: Subject,
    },
    DuplicateRuleId {
        id: BridgeRuleId,
    },
    LocalResponderConflict {
        local_island: IslandId,
        local_subject: Subject,
    },
    ManyWildcardUnsupported {
        pattern: String,
    },
    CaptureMismatch {
        source: String,
        destination: String,
    },
    AmbiguousStreamImport {
        existing: BridgeRuleId,
        attempted: BridgeRuleId,
    },
    RuleNotFound {
        id: BridgeRuleId,
    },
}

impl Display for BridgeRuleViolation {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::SelfImport { island } => write!(f, "self-import is not valid for {island}"),
            Self::DuplicateServiceImport {
                local_island,
                local_subject,
            } => write!(
                f,
                "service import already exists for {local_subject} in {local_island}"
            ),
            Self::DuplicateRuleId { id } => write!(f, "bridge rule {id} already exists"),
            Self::LocalResponderConflict {
                local_island,
                local_subject,
            } => write!(
                f,
                "local responder conflicts with service import {local_subject} in {local_island}"
            ),
            Self::ManyWildcardUnsupported { pattern } => {
                write!(
                    f,
                    "bridge transform does not support > wildcard in {pattern}"
                )
            }
            Self::CaptureMismatch {
                source,
                destination,
            } => write!(
                f,
                "bridge transform capture mismatch from {source} to {destination}"
            ),
            Self::AmbiguousStreamImport {
                existing,
                attempted,
            } => write!(
                f,
                "stream import {attempted} overlaps existing stream import {existing}"
            ),
            Self::RuleNotFound { id } => write!(f, "bridge rule {id} was not found"),
        }
    }
}

fn validate_not_self_import(
    local: &IslandId,
    remote: &IslandId,
) -> std::result::Result<(), BridgeRuleViolation> {
    if local == remote {
        return Err(BridgeRuleViolation::SelfImport {
            island: local.clone(),
        });
    }
    Ok(())
}

fn validate_unique_rule_id(
    rule_ids: &BTreeMap<BridgeRuleId, BridgeRuleIndex>,
    id: &BridgeRuleId,
) -> std::result::Result<(), BridgeRuleViolation> {
    if rule_ids.contains_key(id) {
        return Err(BridgeRuleViolation::DuplicateRuleId { id: id.clone() });
    }
    Ok(())
}

fn single_token_wildcards(pattern: &str) -> std::result::Result<usize, BridgeRuleViolation> {
    let mut wildcards = 0usize;
    for token in pattern.split('.') {
        match token {
            "*" => wildcards += 1,
            ">" => {
                return Err(BridgeRuleViolation::ManyWildcardUnsupported {
                    pattern: pattern.to_string(),
                });
            }
            _ => {}
        }
    }
    Ok(wildcards)
}

fn capture_positions(pattern: &str) -> std::result::Result<Vec<usize>, BridgeRuleViolation> {
    let mut positions = Vec::new();
    for (index, token) in pattern.split('.').enumerate() {
        match token {
            "*" => positions.push(index),
            ">" => {
                return Err(BridgeRuleViolation::ManyWildcardUnsupported {
                    pattern: pattern.to_string(),
                });
            }
            _ => {}
        }
    }
    Ok(positions)
}

fn destination_segments(
    pattern: &str,
) -> std::result::Result<Vec<TransformSegment>, BridgeRuleViolation> {
    let mut capture_index = 0usize;
    let mut segments = Vec::new();
    for token in pattern.split('.') {
        match token {
            "*" => {
                segments.push(TransformSegment::Capture(capture_index));
                capture_index += 1;
            }
            ">" => {
                return Err(BridgeRuleViolation::ManyWildcardUnsupported {
                    pattern: pattern.to_string(),
                });
            }
            _ => segments.push(TransformSegment::Literal(token.to_string())),
        }
    }
    Ok(segments)
}

fn captures_for<'a>(positions: &[usize], subject: &'a Subject) -> Vec<&'a str> {
    positions
        .iter()
        .map(|index| subject.tokens()[*index].as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        BridgeEndpoint, BridgeRuleId, BridgeRuleSet, BridgeRuleViolation, ServiceImport,
        StreamImport, SubjectTransform,
    };
    use crate::{IslandId, PrincipalId, Subject, SubjectPattern};

    fn island(value: &str) -> IslandId {
        IslandId::new(value)
    }

    fn principal(value: &str) -> PrincipalId {
        PrincipalId::new(value)
    }

    fn subject(value: &str) -> Subject {
        Subject::parse(value).expect("subject parses")
    }

    fn endpoint(island: &str, subject: &str) -> BridgeEndpoint {
        BridgeEndpoint::new(self::island(island), self::subject(subject))
    }

    fn pattern(value: &str) -> SubjectPattern {
        SubjectPattern::parse(value).expect("pattern parses")
    }

    fn rule(value: &str) -> BridgeRuleId {
        BridgeRuleId::new(value)
    }

    #[test]
    fn exact_service_import_validates() {
        let mut rules = BridgeRuleSet::empty();
        rules
            .add_service_import(ServiceImport::new(
                rule("gpu-deploy"),
                endpoint("laptop", "gpu.deploy.submit"),
                endpoint("prod", "deploy.submit"),
                principal("bridge"),
            ))
            .expect("service import validates");
    }

    #[test]
    fn self_service_import_is_rejected() {
        let mut rules = BridgeRuleSet::empty();
        let error = rules
            .add_service_import(ServiceImport::new(
                rule("self"),
                endpoint("prod", "deploy.submit"),
                endpoint("prod", "deploy.submit"),
                principal("bridge"),
            ))
            .unwrap_err();

        assert_eq!(
            error,
            BridgeRuleViolation::SelfImport {
                island: island("prod")
            }
        );
    }

    #[test]
    fn duplicate_local_service_import_is_rejected() {
        let mut rules = BridgeRuleSet::empty();
        rules
            .add_service_import(ServiceImport::new(
                rule("first"),
                endpoint("laptop", "gpu.deploy.submit"),
                endpoint("prod", "deploy.submit"),
                principal("bridge"),
            ))
            .expect("first import validates");
        let error = rules
            .add_service_import(ServiceImport::new(
                rule("second"),
                endpoint("laptop", "gpu.deploy.submit"),
                endpoint("staging", "deploy.submit"),
                principal("bridge"),
            ))
            .unwrap_err();

        assert!(matches!(
            error,
            BridgeRuleViolation::DuplicateServiceImport { .. }
        ));
    }

    #[test]
    fn duplicate_rule_ids_are_rejected() {
        let mut rules = BridgeRuleSet::empty();
        rules
            .add_service_import(ServiceImport::new(
                rule("shared"),
                endpoint("laptop", "gpu.deploy.submit"),
                endpoint("prod", "deploy.submit"),
                principal("bridge"),
            ))
            .expect("first import validates");
        let error = rules
            .add_service_import(ServiceImport::new(
                rule("shared"),
                endpoint("laptop", "gpu.deploy.preview"),
                endpoint("prod", "deploy.preview"),
                principal("bridge"),
            ))
            .unwrap_err();

        assert_eq!(
            error,
            BridgeRuleViolation::DuplicateRuleId { id: rule("shared") }
        );
    }

    #[test]
    fn rule_ids_are_unique_across_service_and_stream_imports() {
        let mut rules = BridgeRuleSet::empty();
        rules
            .add_service_import(ServiceImport::new(
                rule("shared"),
                endpoint("laptop", "gpu.deploy.submit"),
                endpoint("prod", "deploy.submit"),
                principal("bridge"),
            ))
            .expect("service import validates");
        let transform =
            SubjectTransform::new(pattern("deploy.*.status"), pattern("prod.deploy.*.status"))
                .expect("transform validates");
        let error = rules
            .add_stream_import(StreamImport::new(
                rule("shared"),
                island("prod"),
                island("laptop"),
                principal("bridge"),
                transform,
            ))
            .unwrap_err();

        assert_eq!(
            error,
            BridgeRuleViolation::DuplicateRuleId { id: rule("shared") }
        );
    }

    #[test]
    fn stream_transform_preserves_capture() {
        let transform =
            SubjectTransform::new(pattern("deploy.*.status"), pattern("prod.deploy.*.status"))
                .expect("transform validates");

        let mapped = transform
            .apply(&subject("deploy.d1.status"))
            .expect("subject maps");

        assert_eq!(mapped.as_str(), "prod.deploy.d1.status");
    }

    #[test]
    fn transform_rejects_dropped_capture() {
        let error =
            SubjectTransform::new(pattern("deploy.*.status"), pattern("prod.deploy.status"))
                .unwrap_err();

        assert!(matches!(error, BridgeRuleViolation::CaptureMismatch { .. }));
    }

    #[test]
    fn transform_rejects_many_wildcard() {
        let error =
            SubjectTransform::new(pattern("deploy.>"), pattern("prod.deploy.>")).unwrap_err();

        assert!(matches!(
            error,
            BridgeRuleViolation::ManyWildcardUnsupported { .. }
        ));
    }

    #[test]
    fn overlapping_stream_imports_are_rejected() {
        let mut rules = BridgeRuleSet::empty();
        let first_transform =
            SubjectTransform::new(pattern("deploy.*.status"), pattern("prod.deploy.*.status"))
                .expect("first transform validates");
        rules
            .add_stream_import(StreamImport::new(
                rule("first"),
                island("prod"),
                island("laptop"),
                principal("bridge"),
                first_transform,
            ))
            .expect("first import validates");

        let second_transform =
            SubjectTransform::new(pattern("deploy.*.status"), pattern("prod.alt.*.status"))
                .expect("second transform validates");
        let error = rules
            .add_stream_import(StreamImport::new(
                rule("second"),
                island("prod"),
                island("laptop"),
                principal("bridge"),
                second_transform,
            ))
            .unwrap_err();

        assert!(matches!(
            error,
            BridgeRuleViolation::AmbiguousStreamImport { .. }
        ));
    }
}
