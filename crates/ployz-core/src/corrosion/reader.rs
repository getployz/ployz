//! Tolerant interpretation and deterministic adjudication of Corrosion rows.

use serde_json::Value;

use super::{
    ClusterDocument, CorrosionDocument, CorrosionTable, MeshProvider, OrdinaryCorrosionDocument,
    RosterCorrosionDocument,
};
use crate::ids::{
    ClusterName, CorrosionNamespaceName, DeployName, MachineName, PeerName, TokenName,
};
use crate::operation::RouteHostname;

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
    ForeignCluster {
        expected: String,
        found: String,
    },
    NewerVersion {
        found: u64,
        supported: u32,
    },
    MeshProviderMismatch {
        expected: MeshProvider,
        found: MeshProvider,
    },
    Malformed(MalformedDocument),
    InvalidRowKey {
        expected: String,
    },
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
    expected_cluster: &ClusterName,
    rows: impl IntoIterator<Item = StoredRow>,
) -> ReadReport<Document>
where
    Document: OrdinaryCorrosionDocument,
{
    read_document_rows(expected_cluster, rows)
}

fn read_document_rows<Document>(
    expected_cluster: &ClusterName,
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

/// Reads roster rows after the caller has accepted the cluster document that
/// fixes the cluster-wide mesh provider.
#[must_use]
pub fn read_roster_rows<Document>(
    cluster: &ClusterDocument,
    rows: impl IntoIterator<Item = StoredRow>,
) -> ReadReport<Document>
where
    Document: RosterCorrosionDocument,
{
    let ReadReport {
        accepted: candidates,
        skipped: mut skipped_rows,
    } = read_document_rows::<Document>(&cluster.cluster_id, rows);
    let mut accepted = Vec::new();

    for row in candidates {
        let found = row.value.mesh_provider();
        if found == cluster.provider {
            accepted.push(row);
        } else {
            skipped_rows.push(skipped(
                row.source,
                RowSkipReason::MeshProviderMismatch {
                    expected: cluster.provider,
                    found,
                },
            ));
        }
    }

    ReadReport {
        accepted,
        skipped: skipped_rows,
    }
}

fn read_one<Document>(
    expected_cluster: &ClusterName,
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

    let value = match Document::deserialize(&parsed) {
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

    match Document::TABLE {
        CorrosionTable::Cluster => validate_key(&source, expected_cluster.as_str())?,
        CorrosionTable::Machines => validate_document_key(&source, fields, "name")?,
        CorrosionTable::Peers => {
            let expected = fields
                .get("name")
                .and_then(Value::as_str)
                .and_then(|name| PeerName::try_new(name).ok())
                .map(|name| name.as_str().to_owned());
            validate_optional_key(&source, expected, "peer name")?;
        }
        CorrosionTable::Tokens => {
            let expected = TokenName::try_new(source.key.clone())
                .ok()
                .map(|name| name.as_str().to_owned());
            validate_optional_key(&source, expected, "token name")?;
        }
        CorrosionTable::Namespaces => validate_document_key(&source, fields, "name")?,
        CorrosionTable::RouteBindings => validate_document_key(&source, fields, "hostname")?,
        CorrosionTable::MachineEndpoints => {
            let expected = MachineName::try_new(source.key.clone())
                .ok()
                .map(|name| name.as_str().to_owned());
            validate_optional_key(&source, expected, "machine name")?;
        }
        CorrosionTable::MachineStatus | CorrosionTable::GatewayObservations => {
            validate_document_key(&source, fields, "machine_id")?;
        }
        CorrosionTable::Operations => {
            let expected = fields
                .get("namespace_id")
                .and_then(Value::as_str)
                .and_then(|value| CorrosionNamespaceName::try_new(value.to_owned()).ok())
                .zip(
                    fields
                        .get("deploy_name")
                        .and_then(Value::as_str)
                        .and_then(|value| DeployName::try_new(value.to_owned()).ok()),
                )
                .map(|(namespace, deploy)| super::operation::deploy_key(&namespace, &deploy));
            validate_optional_key(&source, expected, "namespace/deploy")?;
        }
        CorrosionTable::Controller => {
            let expected = value.cluster_id().as_str().to_owned();
            if source.key != expected {
                return Err(skipped(source, RowSkipReason::InvalidRowKey { expected }));
            }
        }
        CorrosionTable::CertHoldings => {
            let expected = fields
                .get("machine_id")
                .and_then(Value::as_str)
                .and_then(|value| MachineName::try_new(value.to_owned()).ok())
                .zip(
                    fields
                        .get("hostname")
                        .and_then(Value::as_str)
                        .and_then(|value| RouteHostname::try_new(value.to_owned()).ok()),
                )
                .map(|(machine_id, hostname)| format!("{machine_id}:{}", hostname.as_str()));
            let Some(expected) = expected else {
                return Err(skipped(
                    source,
                    RowSkipReason::Malformed(MalformedDocument::InvalidPayload {
                        message: "certificate holding identity fields are invalid".to_owned(),
                    }),
                ));
            };
            if source.key != expected {
                return Err(skipped(source, RowSkipReason::InvalidRowKey { expected }));
            }
        }
        CorrosionTable::AcmeHttp01 => {}
    }

    Ok(AcceptedRow { source, value })
}

fn skipped(source: StoredRow, reason: RowSkipReason) -> SkippedRow {
    SkippedRow { source, reason }
}

fn validate_key(source: &StoredRow, expected: &str) -> Result<(), SkippedRow> {
    if source.key == expected {
        Ok(())
    } else {
        Err(skipped(
            source.clone(),
            RowSkipReason::InvalidRowKey {
                expected: expected.to_owned(),
            },
        ))
    }
}

fn validate_document_key(
    source: &StoredRow,
    fields: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<(), SkippedRow> {
    let expected = fields.get(field).and_then(Value::as_str).map(str::to_owned);
    validate_optional_key(source, expected, field)
}

fn validate_optional_key(
    source: &StoredRow,
    expected: Option<String>,
    identity: &str,
) -> Result<(), SkippedRow> {
    let Some(expected) = expected else {
        return Err(skipped(
            source.clone(),
            RowSkipReason::Malformed(MalformedDocument::InvalidPayload {
                message: format!("{identity} is invalid"),
            }),
        ));
    };
    validate_key(source, &expected)
}

/// Reads a named table whose canonical name is its unique row key.
#[must_use]
pub fn read_named_rows<Document>(
    expected_cluster: &ClusterName,
    rows: impl IntoIterator<Item = StoredRow>,
) -> ReadReport<Document>
where
    Document: OrdinaryCorrosionDocument,
{
    read_rows::<Document>(expected_cluster, rows)
}

/// Reads a sealed named roster table and resolves each provider-valid claim by
/// canonical name.
#[must_use]
pub fn read_named_roster_rows<Document>(
    cluster: &ClusterDocument,
    rows: impl IntoIterator<Item = StoredRow>,
) -> ReadReport<Document>
where
    Document: RosterCorrosionDocument,
{
    read_roster_rows::<Document>(cluster, rows)
}
