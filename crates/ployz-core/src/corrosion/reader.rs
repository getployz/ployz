//! Tolerant interpretation and deterministic adjudication of Corrosion rows.

use std::collections::BTreeMap;

use serde_json::Value;

use super::{CorrosionDocument, NameClaim, NamedCorrosionDocument};
use crate::ids::{ClusterId, CorrosionUlid, CorrosionUlidError};

/// A row as returned by a Corrosion query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRow {
    pub key: String,
    pub document: String,
}

impl StoredRow {
    #[must_use]
    pub fn new(key: impl Into<String>, document: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            document: document.into(),
        }
    }
}

/// A stored row interpreted as one current typed document.
#[derive(Debug)]
pub struct AcceptedRow<Document> {
    pub source: StoredRow,
    pub value: Document,
}

/// Evidence explaining why a stored row did not enter a typed view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedRow {
    pub source: StoredRow,
    pub reason: RowSkipReason,
}

/// A reader-law disposition that keeps non-truth rows visible to diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowSkipReason {
    Empty,
    ForeignCluster { expected: String, found: String },
    NewerVersion { found: u64, supported: u32 },
    Malformed(MalformedDocument),
    InvalidRowId { error: CorrosionUlidError },
}

/// The precise malformed-document class observed during staged parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MalformedDocument {
    InvalidJson { message: String },
    NotObject,
    MissingClusterId,
    InvalidClusterId,
    MissingVersion,
    InvalidVersion,
    UnsupportedVersion { found: u64 },
    InvalidPayload { message: String },
}

/// The complete outcome of reading a set of rows from one table.
#[derive(Debug)]
pub struct ReadReport<Document> {
    pub accepted: Vec<AcceptedRow<Document>>,
    pub skipped: Vec<SkippedRow>,
}

/// Reads stored rows using the cross-cutting cluster and version fences.
#[must_use]
pub fn read_rows<Document>(
    expected_cluster: &ClusterId,
    rows: impl IntoIterator<Item = StoredRow>,
) -> ReadReport<Document>
where
    Document: CorrosionDocument,
{
    let mut accepted = Vec::new();
    let mut skipped = Vec::new();

    for source in rows {
        match read_one::<Document>(expected_cluster, source) {
            Ok(row) => accepted.push(row),
            Err(row) => skipped.push(row),
        }
    }

    ReadReport { accepted, skipped }
}

fn read_one<Document>(
    expected_cluster: &ClusterId,
    source: StoredRow,
) -> Result<AcceptedRow<Document>, SkippedRow>
where
    Document: CorrosionDocument,
{
    if source.document.trim().is_empty() {
        return Err(skipped(source, RowSkipReason::Empty));
    }

    let parsed = match serde_json::from_str::<Value>(&source.document) {
        Ok(parsed) => parsed,
        Err(error) => {
            return Err(skipped(
                source,
                RowSkipReason::Malformed(MalformedDocument::InvalidJson {
                    message: error.to_string(),
                }),
            ));
        }
    };
    let Value::Object(fields) = &parsed else {
        return Err(skipped(
            source,
            RowSkipReason::Malformed(MalformedDocument::NotObject),
        ));
    };
    if fields.is_empty() {
        return Err(skipped(source, RowSkipReason::Empty));
    }

    let Some(cluster_value) = fields.get("cluster_id") else {
        return Err(skipped(
            source,
            RowSkipReason::Malformed(MalformedDocument::MissingClusterId),
        ));
    };
    let Some(found_cluster) = cluster_value.as_str() else {
        return Err(skipped(
            source,
            RowSkipReason::Malformed(MalformedDocument::InvalidClusterId),
        ));
    };
    if found_cluster != expected_cluster.as_str() {
        return Err(skipped(
            source,
            RowSkipReason::ForeignCluster {
                expected: expected_cluster.as_str().to_owned(),
                found: found_cluster.to_owned(),
            },
        ));
    }

    let Some(version_value) = fields.get("v") else {
        return Err(skipped(
            source,
            RowSkipReason::Malformed(MalformedDocument::MissingVersion),
        ));
    };
    let Some(version) = version_value.as_u64() else {
        return Err(skipped(
            source,
            RowSkipReason::Malformed(MalformedDocument::InvalidVersion),
        ));
    };
    let supported = Document::SUPPORTED_VERSION.get();
    if version > u64::from(supported) {
        return Err(skipped(
            source,
            RowSkipReason::NewerVersion {
                found: version,
                supported,
            },
        ));
    }
    if version != u64::from(supported) {
        return Err(skipped(
            source,
            RowSkipReason::Malformed(MalformedDocument::UnsupportedVersion { found: version }),
        ));
    }

    let value = match serde_json::from_value::<Document>(parsed) {
        Ok(value) => value,
        Err(error) => {
            return Err(skipped(
                source,
                RowSkipReason::Malformed(MalformedDocument::InvalidPayload {
                    message: error.to_string(),
                }),
            ));
        }
    };
    Ok(AcceptedRow { source, value })
}

fn skipped(source: StoredRow, reason: RowSkipReason) -> SkippedRow {
    SkippedRow { source, reason }
}

/// A named document whose canonical row identifier survived adjudication.
#[derive(Debug)]
pub struct NamedAcceptedRow<Document> {
    pub id: CorrosionUlid,
    pub source: StoredRow,
    pub value: Document,
}

impl<Document> NamedAcceptedRow<Document> {
    fn evidence(&self) -> NamedRowEvidence {
        NamedRowEvidence {
            id: self.id.clone(),
            source: self.source.clone(),
        }
    }
}

/// Stored evidence for one valid named row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedRowEvidence {
    pub id: CorrosionUlid,
    pub source: StoredRow,
}

/// A valid duplicate hidden by the deterministic lowest-ULID reader law.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowConflict {
    pub claim: NameClaim,
    pub winner: NamedRowEvidence,
    pub loser: NamedRowEvidence,
}

/// The complete outcome of reading and adjudicating one named table.
#[derive(Debug)]
pub struct NamedReadReport<Document> {
    pub accepted: Vec<NamedAcceptedRow<Document>>,
    pub skipped: Vec<SkippedRow>,
    pub shadows: Vec<ShadowConflict>,
}

/// Reads a sealed named document table and resolves each claim by lowest ULID.
#[must_use]
pub fn read_named_rows<Document>(
    expected_cluster: &ClusterId,
    rows: impl IntoIterator<Item = StoredRow>,
) -> NamedReadReport<Document>
where
    Document: NamedCorrosionDocument,
{
    let ReadReport {
        accepted,
        mut skipped,
    } = read_rows::<Document>(expected_cluster, rows);
    let mut by_claim = BTreeMap::<NameClaim, Vec<NamedAcceptedRow<Document>>>::new();

    for AcceptedRow { source, value } in accepted {
        let id = match CorrosionUlid::try_new(source.key.clone()) {
            Ok(id) => id,
            Err(error) => {
                skipped.push(SkippedRow {
                    source,
                    reason: RowSkipReason::InvalidRowId { error },
                });
                continue;
            }
        };
        by_claim
            .entry(value.name_claim())
            .or_default()
            .push(NamedAcceptedRow { id, source, value });
    }

    let mut accepted = Vec::with_capacity(by_claim.len());
    let mut shadows = Vec::new();
    for (claim, mut candidates) in by_claim {
        candidates.sort_by(|left, right| left.id.cmp(&right.id));
        let mut candidates = candidates.into_iter();
        let Some(winner) = candidates.next() else {
            continue;
        };
        let winner_evidence = winner.evidence();
        for loser in candidates {
            shadows.push(ShadowConflict {
                claim: claim.clone(),
                winner: winner_evidence.clone(),
                loser: loser.evidence(),
            });
        }
        accepted.push(winner);
    }

    NamedReadReport {
        accepted,
        skipped,
        shadows,
    }
}
