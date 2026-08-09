//! Corrosion rows used by one preferred-controller deploy attempt.
//!
//! This adapter reads current rows and publishes each serving projection in one
//! Corrosion transaction. A failed or ambiguous reply is retried from reality.

use std::collections::BTreeMap;

use async_trait::async_trait;
use ployz_core::DeployRefusal;
use ployz_core::corrosion::{
    AutomaticHostnameMode, ClusterDocument, ControllerDocument, CorrosionDocumentVersion,
    CorrosionNamespaceName, CorrosionServiceName, CorrosionTable, IngressMode, MachineDocument,
    MachineStatusDocument, NamespaceDocument, OperationDocument, OperatorWriteProvenance,
    RouteBindingDocument, ServiceDocument, SqliteParameter, Statement, StoredRow,
    TransactionResult, read_named_roster_rows, read_named_rows, read_rows,
};
use ployz_core::ids::{ClusterName, MachineName};
use ployz_core::ingress::RouteBindingOrigin;
use ployz_core::operation::{RouteHostname, RoutePort};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::corrosion::{CorrosionClient, StoredRowLimit, collect_stored_rows};

use super::simple_deploy::{
    DeployCommand, DeployCommit, DeployMachineStatus, DeployReality, DeployRosterMachine,
    DeployStartError, DesiredRouteRow, ObservedServiceRow, SimpleDeployStore,
};

const MAX_SINGLETON_ROWS: usize = 2;
const MAX_DEPLOY_ROWS: usize = 10_000;

/// The one concrete cluster-row adapter for [`super::simple_deploy::SimpleDeploy`].
pub(super) struct CorrosionSimpleDeployStore {
    corrosion: CorrosionClient,
    cluster_id: ClusterName,
}

impl CorrosionSimpleDeployStore {
    #[must_use]
    pub(super) const fn new(corrosion: CorrosionClient, cluster_id: ClusterName) -> Self {
        Self {
            corrosion,
            cluster_id,
        }
    }

    async fn query(&self, statement: Statement, limit: usize) -> Result<Vec<StoredRow>, String> {
        let mut stream = self
            .corrosion
            .query(&statement)
            .await
            .map_err(|error| error.to_string())?;
        collect_stored_rows(&mut stream, StoredRowLimit::new(limit))
            .await
            .map_err(|error| error.to_string())
    }
}

#[async_trait]
impl SimpleDeployStore for CorrosionSimpleDeployStore {
    async fn controller(&self) -> Result<ControllerDocument, String> {
        let rows = self
            .query(
                select_by_id(CorrosionTable::Controller, self.cluster_id.as_str()),
                MAX_SINGLETON_ROWS,
            )
            .await?;
        decode_one::<ControllerDocument>(&self.cluster_id, CorrosionTable::Controller, rows)?
            .ok_or_else(|| "preferred controller row is missing".to_owned())
    }

    async fn observe(&self, command: &DeployCommand) -> Result<DeployReality, DeployStartError> {
        let matching_namespaces = self
            .query(
                namespace_named(&self.cluster_id, &command.request.namespace_name),
                MAX_DEPLOY_ROWS,
            )
            .await?;
        let namespace = decode_namespace(
            &self.cluster_id,
            &command.request.namespace_name,
            matching_namespaces,
        )?;

        let cluster = self.query(
            select_by_id(CorrosionTable::Cluster, self.cluster_id.as_str()),
            MAX_SINGLETON_ROWS,
        );
        let machines = self.query(cluster_rows(CorrosionTable::Machines), MAX_DEPLOY_ROWS);
        let statuses = self.query(machine_status_rows(&self.cluster_id), MAX_DEPLOY_ROWS);
        let routes = self.query(
            all_cluster_rows(CorrosionTable::RouteBindings, &self.cluster_id),
            MAX_DEPLOY_ROWS,
        );
        let services = self.query(
            namespace_rows(CorrosionTable::Services, &namespace.id),
            MAX_DEPLOY_ROWS,
        );
        let (cluster, machines, statuses, routes, services) =
            tokio::try_join!(cluster, machines, statuses, routes, services)
                .map_err(|error| error.to_string())?;

        let cluster = decode_cluster(&self.cluster_id, cluster)?;
        let services = decode_services(&self.cluster_id, &namespace.id, services)?;
        if command.request.services.iter().any(|requested| {
            !requested.runtime.volume_mounts.is_empty()
                && services
                    .iter()
                    .any(|service| service.document.name == requested.service_name)
        }) {
            return Err(DeployStartError::Refused(
                DeployRefusal::NamedVolumeRedeployUnsupported,
            ));
        }
        let (automatic_routes, routes_without_service) =
            desired_routes(&cluster, command, &namespace.id, routes)?;
        let roster = decode_roster(&cluster, machines, statuses)?;

        Ok(DeployReality {
            namespace_id: namespace.id,
            namespace: namespace.document,
            services,
            automatic_routes,
            routes_without_service,
            roster,
        })
    }

    async fn create_operation(&self, document: &OperationDocument) -> Result<bool, String> {
        let key = ployz_core::corrosion::deploy_key(&document.namespace_id, &document.deploy_name);
        let statement = insert_if_absent_document(CorrosionTable::Operations, &key, document)?;
        let response = self
            .corrosion
            .execute(&[statement])
            .await
            .map_err(|error| error.to_string())?;
        let [TransactionResult::Success(result)] = response.results.as_slice() else {
            return Err("operation insert returned an unexpected result".to_owned());
        };
        match result.rows_affected {
            0 => Ok(false),
            1 => Ok(true),
            rows => Err(format!("operation insert affected {rows} rows")),
        }
    }

    async fn write_operation(&self, document: &OperationDocument) -> Result<(), String> {
        let key = ployz_core::corrosion::deploy_key(&document.namespace_id, &document.deploy_name);
        let statement = upsert_document(CorrosionTable::Operations, &key, document)?;
        self.corrosion
            .execute(&[statement])
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    async fn commit(&self, commit: DeployCommit) -> Result<(), String> {
        let statements = commit_statements(&commit)?;
        self.corrosion
            .execute(&statements)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

fn decode_services(
    cluster_id: &ClusterName,
    namespace_id: &CorrosionNamespaceName,
    rows: Vec<StoredRow>,
) -> Result<Vec<ObservedServiceRow>, DeployStartError> {
    let report = read_rows::<ServiceDocument>(cluster_id, rows);
    if !report.skipped.is_empty() {
        return Err(DeployStartError::Unavailable(
            "service lookup contained a rejected row".to_owned(),
        ));
    }
    Ok(report
        .accepted
        .into_iter()
        .filter(|row| &row.value.namespace_id == namespace_id)
        .map(|row| ObservedServiceRow {
            document: row.value,
        })
        .collect())
}

struct ResolvedNamespace {
    id: CorrosionNamespaceName,
    document: NamespaceDocument,
}

fn decode_namespace(
    cluster_id: &ClusterName,
    namespace_name: &CorrosionNamespaceName,
    rows: Vec<StoredRow>,
) -> Result<ResolvedNamespace, DeployStartError> {
    let report = read_named_rows::<NamespaceDocument>(cluster_id, rows);
    if !report.skipped.is_empty() {
        return Err(DeployStartError::Unavailable(
            "namespace lookup contained a rejected row".to_owned(),
        ));
    }
    if report.accepted.is_empty() {
        return Err(DeployStartError::Refused(
            DeployRefusal::namespace_not_found(namespace_name.clone()),
        ));
    }
    if report.accepted.len() != 1 {
        return Err(DeployStartError::Unavailable(
            "namespace lookup returned more than one row for a primary key".to_owned(),
        ));
    }
    let row = report.accepted.into_iter().next().expect("length checked");
    let id = CorrosionNamespaceName::try_new(row.source.key)
        .map_err(|error| DeployStartError::Unavailable(error.to_string()))?;
    Ok(ResolvedNamespace {
        id,
        document: row.value,
    })
}

fn decode_cluster(
    cluster_id: &ClusterName,
    rows: Vec<StoredRow>,
) -> Result<ClusterDocument, String> {
    decode_one::<ClusterDocument>(cluster_id, CorrosionTable::Cluster, rows)?
        .ok_or_else(|| "cluster row is missing or invalid".to_owned())
}

fn decode_one<Document>(
    cluster_id: &ClusterName,
    table: CorrosionTable,
    rows: Vec<StoredRow>,
) -> Result<Option<Document>, String>
where
    Document: ployz_core::corrosion::OrdinaryCorrosionDocument,
{
    let report = read_rows::<Document>(cluster_id, rows);
    if !report.skipped.is_empty() || report.accepted.len() > 1 {
        return Err(format!(
            "{} lookup returned invalid rows (accepted {}, skipped {})",
            table.as_str(),
            report.accepted.len(),
            report.skipped.len(),
        ));
    }
    Ok(report.accepted.into_iter().next().map(|row| row.value))
}

fn desired_routes(
    cluster: &ClusterDocument,
    command: &DeployCommand,
    namespace_id: &CorrosionNamespaceName,
    rows: Vec<StoredRow>,
) -> Result<(Vec<DesiredRouteRow>, bool), String> {
    let report = read_named_rows::<RouteBindingDocument>(&cluster.cluster_id, rows);
    if !report.skipped.is_empty() {
        return Err("route lookup contained a rejected row".to_owned());
    }
    let desired_names = command
        .request
        .services
        .iter()
        .map(|service| service.service_name.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let routes_without_service = report.accepted.iter().any(|row| {
        declared_route_targets_omitted_service(&row.value, namespace_id, &desired_names)
    });
    let mut planned = Vec::new();
    if let AutomaticHostnameMode::Custom { suffix } = &cluster.hostname_mode {
        for service in &command.request.services {
            let hostname = automatic_hostname(namespace_id, &service.service_name, suffix)?;
            match report
                .accepted
                .iter()
                .find(|row| row.value.hostname == hostname)
            {
                Some(row)
                    if automatic_route_matches(
                        &row.value,
                        namespace_id,
                        &service.service_name,
                        &hostname,
                    ) =>
                {
                    planned.push(DesiredRouteRow {
                        id: hostname,
                        document: row.value.clone(),
                    });
                }
                Some(row) => {
                    return Err(format!(
                        "automatic hostname {} conflicts with route {}",
                        hostname.as_str(),
                        row.source.key,
                    ));
                }
                None => {
                    let id = hostname.clone();
                    let written_at = OffsetDateTime::now_utc()
                        .format(&Rfc3339)
                        .map_err(|error| error.to_string())?;
                    let document = RouteBindingDocument {
                        v: CorrosionDocumentVersion::V1,
                        cluster_id: cluster.cluster_id.clone(),
                        provenance: OperatorWriteProvenance {
                            written_by: command.initiator.clone(),
                            written_at: ployz_core::corrosion::CorrosionTimestamp::try_new(
                                written_at,
                            )
                            .map_err(|error| error.to_string())?,
                        },
                        hostname,
                        namespace_id: namespace_id.clone(),
                        service_name: service.service_name.clone(),
                        endpoint_port: RoutePort::try_new(80).map_err(|error| error.to_string())?,
                        origin: RouteBindingOrigin::Automatic,
                        ingress_mode: IngressMode::Direct,
                    };
                    planned.push(DesiredRouteRow { id, document });
                }
            }
        }
    }
    Ok((planned, routes_without_service))
}

fn declared_route_targets_omitted_service(
    route: &RouteBindingDocument,
    namespace_id: &CorrosionNamespaceName,
    desired_names: &std::collections::BTreeSet<CorrosionServiceName>,
) -> bool {
    &route.namespace_id == namespace_id
        && route.origin == RouteBindingOrigin::Declared
        && !desired_names.contains(&route.service_name)
}

fn automatic_hostname(
    namespace_name: &CorrosionNamespaceName,
    service_name: &ployz_core::corrosion::CorrosionServiceName,
    suffix: &RouteHostname,
) -> Result<RouteHostname, String> {
    RouteHostname::try_new(format!(
        "{}.{}.{}",
        service_name.as_str(),
        namespace_name.as_str(),
        suffix.as_str()
    ))
    .map_err(|error| error.to_string())
}

fn automatic_route_matches(
    route: &RouteBindingDocument,
    namespace_id: &CorrosionNamespaceName,
    service_name: &CorrosionServiceName,
    hostname: &RouteHostname,
) -> bool {
    &route.hostname == hostname
        && &route.namespace_id == namespace_id
        && &route.service_name == service_name
        && route.endpoint_port == RoutePort::try_new(80).expect("port 80 is valid")
        && route.origin == RouteBindingOrigin::Automatic
        && route.ingress_mode == IngressMode::Direct
}

fn decode_roster(
    cluster: &ClusterDocument,
    machine_rows: Vec<StoredRow>,
    status_rows: Vec<StoredRow>,
) -> Result<Vec<DeployRosterMachine>, String> {
    let machines = read_named_roster_rows::<MachineDocument>(cluster, machine_rows);
    let statuses = read_rows::<MachineStatusDocument>(&cluster.cluster_id, status_rows);
    let mut status_by_machine = BTreeMap::new();
    for row in statuses.accepted {
        if row.source.key == row.value.machine_id.as_str() {
            status_by_machine.insert(row.value.machine_id.clone(), row.value);
        }
    }
    let mut roster = machines
        .accepted
        .into_iter()
        .map(|row| {
            let id = MachineName::try_new(row.source.key).map_err(|error| error.to_string())?;
            let status = status_by_machine
                .remove(&id)
                .map(|status| DeployMachineStatus {
                    free_disk_bytes: status.free_disk_bytes,
                    load: status.load,
                });
            Ok(DeployRosterMachine {
                id,
                name: row.value.name,
                lifecycle: row.value.lifecycle,
                status,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    roster.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(roster)
}

fn commit_statements(commit: &DeployCommit) -> Result<Vec<Statement>, String> {
    let namespace_id = &commit.namespace_id;
    let mut statements = Vec::with_capacity(
        3 + commit.services.len() + commit.containers.len() + commit.automatic_routes.len(),
    );
    statements.push(Statement::with_params(
        "DELETE FROM services WHERE namespace_id = ?",
        vec![SqliteParameter::Text(namespace_id.as_str().to_owned())],
    ));
    for service in &commit.services {
        statements.push(upsert_document(
            CorrosionTable::Services,
            &service.key,
            &service.document,
        )?);
    }
    statements.push(Statement::with_params(
        "DELETE FROM containers WHERE namespace_id = ?",
        vec![SqliteParameter::Text(namespace_id.as_str().to_owned())],
    ));
    for container in &commit.containers {
        statements.push(insert_document(
            CorrosionTable::Containers,
            &container.id,
            &container.document,
        )?);
    }
    statements.push(Statement::with_params(
        "DELETE FROM route_bindings WHERE namespace_id = ? AND json_extract(document, '$.origin') = 'automatic'",
        vec![SqliteParameter::Text(namespace_id.as_str().to_owned())],
    ));
    for route in &commit.automatic_routes {
        statements.push(insert_document(
            CorrosionTable::RouteBindings,
            route.id.as_str(),
            &route.document,
        )?);
    }
    Ok(statements)
}

fn insert_document<Document>(
    table: CorrosionTable,
    id: &str,
    document: &Document,
) -> Result<Statement, String>
where
    Document: serde::Serialize,
{
    Ok(Statement::with_params(
        format!(
            "INSERT INTO {} (id, document) VALUES (?, ?)",
            table.as_str()
        ),
        document_params(id, document)?,
    ))
}

fn insert_if_absent_document<Document>(
    table: CorrosionTable,
    id: &str,
    document: &Document,
) -> Result<Statement, String>
where
    Document: serde::Serialize,
{
    Ok(Statement::with_params(
        format!(
            "INSERT INTO {} (id, document) SELECT ?, ? WHERE NOT EXISTS (SELECT 1 FROM {} WHERE id = ?)",
            table.as_str(),
            table.as_str()
        ),
        {
            let mut params = document_params(id, document)?;
            params.push(SqliteParameter::Text(id.to_owned()));
            params
        },
    ))
}

fn upsert_document<Document>(
    table: CorrosionTable,
    id: &str,
    document: &Document,
) -> Result<Statement, String>
where
    Document: serde::Serialize,
{
    Ok(Statement::with_params(
        format!(
            "INSERT INTO {} (id, document) VALUES (?, ?) ON CONFLICT(id) DO UPDATE SET document = excluded.document",
            table.as_str()
        ),
        document_params(id, document)?,
    ))
}

fn document_params<Document>(id: &str, document: &Document) -> Result<Vec<SqliteParameter>, String>
where
    Document: serde::Serialize,
{
    Ok(vec![
        SqliteParameter::Text(id.to_owned()),
        SqliteParameter::Text(serde_json::to_string(document).map_err(|error| error.to_string())?),
    ])
}

fn select_by_id(table: CorrosionTable, id: &str) -> Statement {
    Statement::with_params(
        format!("SELECT id, document FROM {} WHERE id = ?", table.as_str()),
        vec![SqliteParameter::Text(id.to_owned())],
    )
}

fn namespace_named(
    cluster_id: &ClusterName,
    namespace_name: &ployz_core::corrosion::CorrosionNamespaceName,
) -> Statement {
    Statement::with_params(
        "SELECT id, document FROM namespaces WHERE json_extract(document, '$.cluster_id') = ? AND name = ?",
        vec![
            SqliteParameter::Text(cluster_id.as_str().to_owned()),
            SqliteParameter::Text(namespace_name.as_str().to_owned()),
        ],
    )
}

fn cluster_rows(table: CorrosionTable) -> Statement {
    Statement::simple(format!("SELECT id, document FROM {}", table.as_str()))
}

fn all_cluster_rows(table: CorrosionTable, cluster_id: &ClusterName) -> Statement {
    Statement::with_params(
        format!(
            "SELECT id, document FROM {} WHERE json_extract(document, '$.cluster_id') = ?",
            table.as_str()
        ),
        vec![SqliteParameter::Text(cluster_id.as_str().to_owned())],
    )
}

fn namespace_rows(table: CorrosionTable, namespace_id: &CorrosionNamespaceName) -> Statement {
    Statement::with_params(
        format!(
            "SELECT id, document FROM {} WHERE namespace_id = ?",
            table.as_str()
        ),
        vec![SqliteParameter::Text(namespace_id.as_str().to_owned())],
    )
}

fn machine_status_rows(cluster_id: &ClusterName) -> Statement {
    Statement::with_params(
        "SELECT machine_id AS id, document FROM machine_status WHERE json_extract(document, '$.cluster_id') = ?",
        vec![SqliteParameter::Text(cluster_id.as_str().to_owned())],
    )
}

#[cfg(test)]
mod tests {
    use ployz_core::corrosion::{
        CorrosionDocumentVersion, CorrosionNamespaceName, CorrosionTimestamp,
        OperatorWriteProvenance, Principal,
    };
    use ployz_core::ids::PeerName;

    use super::*;

    const CLUSTER: &str = "main";
    const NAMESPACE_A: &str = "production";
    const NAMESPACE_B: &str = "staging";

    #[test]
    fn namespace_lookup_is_parameterized_and_bounded_by_name() {
        let name = CorrosionNamespaceName::try_new("production").expect("namespace name");
        let Statement::WithParams(sql, params) = namespace_named(&cluster_id(), &name) else {
            panic!("namespace lookup must be parameterized");
        };
        assert!(sql.contains("name = ?"));
        assert!(!sql.contains("production"));
        assert_eq!(
            params,
            vec![
                SqliteParameter::Text(CLUSTER.to_owned()),
                SqliteParameter::Text("production".to_owned()),
            ]
        );
    }

    #[test]
    fn deploy_name_creation_is_a_one_shot_conditional_insert() {
        let statement = insert_if_absent_document(
            CorrosionTable::Operations,
            "production/release-1",
            &serde_json::json!({ "v": 1 }),
        )
        .expect("conditional operation insert");
        let Statement::WithParams(sql, params) = statement else {
            panic!("operation insert must be parameterized");
        };
        assert!(sql.starts_with("INSERT INTO operations"));
        assert!(sql.contains("WHERE NOT EXISTS"));
        assert_eq!(
            params.last(),
            Some(&SqliteParameter::Text("production/release-1".to_owned()))
        );
    }

    #[test]
    fn namespace_decode_rejects_missing_malformed_and_duplicate_claims() {
        let cluster = cluster_id();
        let name = CorrosionNamespaceName::try_new("production").expect("namespace name");
        assert!(decode_namespace(&cluster, &name, Vec::new()).is_err());
        assert!(
            decode_namespace(
                &cluster,
                &name,
                vec![StoredRow::new(NAMESPACE_A, "not json")]
            )
            .is_err()
        );
        assert!(
            decode_namespace(
                &cluster,
                &name,
                vec![
                    namespace_row(NAMESPACE_A, "production"),
                    namespace_row(NAMESPACE_B, "production"),
                ],
            )
            .is_err()
        );

        let resolved = decode_namespace(
            &cluster,
            &name,
            vec![namespace_row(NAMESPACE_A, "production")],
        )
        .expect("one exact namespace");
        assert_eq!(resolved.id.as_str(), NAMESPACE_A);
    }

    #[test]
    fn automatic_route_keeps_the_deployed_service_name_and_port_80_convention() {
        let service =
            ployz_core::corrosion::CorrosionServiceName::try_new("api").expect("service name");
        let namespace_id = CorrosionNamespaceName::try_new(NAMESPACE_A).expect("namespace id");
        let suffix = RouteHostname::try_new("apps.example.test").expect("suffix");
        let hostname =
            automatic_hostname(&namespace_id, &service, &suffix).expect("automatic hostname");
        assert_eq!(hostname.as_str(), "api.production.apps.example.test");
        let other_namespace = CorrosionNamespaceName::try_new(NAMESPACE_B).expect("namespace");
        assert_ne!(
            hostname,
            automatic_hostname(&other_namespace, &service, &suffix).expect("automatic hostname")
        );

        let service_name = CorrosionServiceName::try_new("api").expect("service name");
        let route = RouteBindingDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: cluster_id(),
            provenance: OperatorWriteProvenance {
                written_by: initiator(),
                written_at: timestamp(),
            },
            hostname: hostname.clone(),
            namespace_id: namespace_id.clone(),
            service_name: service_name.clone(),
            endpoint_port: RoutePort::try_new(80).expect("port"),
            origin: RouteBindingOrigin::Automatic,
            ingress_mode: IngressMode::Direct,
        };
        assert!(automatic_route_matches(
            &route,
            &namespace_id,
            &service_name,
            &hostname,
        ));
    }

    #[test]
    fn omitted_service_only_conflicts_with_operator_declared_routes() {
        let namespace_id = CorrosionNamespaceName::try_new(NAMESPACE_A).expect("namespace");
        let service_name = CorrosionServiceName::try_new("api").expect("service");
        let hostname = RouteHostname::try_new("api.example.test").expect("hostname");
        let mut route = RouteBindingDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: cluster_id(),
            provenance: OperatorWriteProvenance {
                written_by: initiator(),
                written_at: timestamp(),
            },
            hostname,
            namespace_id: namespace_id.clone(),
            service_name,
            endpoint_port: RoutePort::try_new(80).expect("port"),
            origin: RouteBindingOrigin::Automatic,
            ingress_mode: IngressMode::Direct,
        };
        let no_services = std::collections::BTreeSet::new();

        assert!(!declared_route_targets_omitted_service(
            &route,
            &namespace_id,
            &no_services,
        ));
        route.origin = RouteBindingOrigin::Declared;
        assert!(declared_route_targets_omitted_service(
            &route,
            &namespace_id,
            &no_services,
        ));
    }

    #[test]
    fn namespace_commit_replaces_automatic_routes_without_touching_declared_routes() {
        let namespace_id = CorrosionNamespaceName::try_new(NAMESPACE_A).expect("namespace");
        let statements = commit_statements(&DeployCommit {
            namespace_id: namespace_id.clone(),
            services: Vec::new(),
            containers: Vec::new(),
            automatic_routes: Vec::new(),
        })
        .expect("commit statements");

        let automatic_delete = statements
            .iter()
            .find(|statement| match statement {
                Statement::WithParams(sql, _) => sql.starts_with("DELETE FROM route_bindings"),
                Statement::Simple(_) => false,
            })
            .expect("automatic route replacement");
        let Statement::WithParams(sql, params) = automatic_delete else {
            panic!("route replacement must be parameterized")
        };
        assert!(sql.contains("$.origin"));
        assert!(sql.contains("'automatic'"));
        assert_eq!(
            params,
            &vec![SqliteParameter::Text(namespace_id.as_str().to_owned())]
        );
    }

    fn cluster_id() -> ClusterName {
        ClusterName::try_new(CLUSTER).expect("cluster id")
    }

    fn timestamp() -> ployz_core::corrosion::CorrosionTimestamp {
        CorrosionTimestamp::try_new("2026-08-08T00:00:00Z").expect("timestamp")
    }

    fn initiator() -> ployz_core::corrosion::OperationInitiator {
        Principal::Peer {
            peer_id: PeerName::try_new("operator").expect("peer id"),
        }
    }

    fn namespace_row(id: &str, name: &str) -> StoredRow {
        let document = NamespaceDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: cluster_id(),
            provenance: OperatorWriteProvenance {
                written_by: initiator(),
                written_at: timestamp(),
            },
            name: CorrosionNamespaceName::try_new(name).expect("namespace name"),
        };
        StoredRow::new(id, serde_json::to_string(&document).expect("json"))
    }
}
