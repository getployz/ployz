//! Corrosion rows used by one preferred-controller deploy attempt.
//!
//! This adapter reads converged intent and publishes one complete namespace
//! serving projection in a Corrosion transaction. Docker remains execution
//! reality and is inspected through target-node RPC before planning.

use async_trait::async_trait;
use ployz_core::DeployRefusal;
use ployz_core::corrosion::{
    AutomaticHostnameMode, ClusterDocument, ControllerDocument, CorrosionDocumentVersion,
    CorrosionNamespaceName, CorrosionServiceName, CorrosionTable, IngressMode, MachineDocument,
    NamespaceDocument, OperationDocument, OperatorWriteProvenance, RouteBindingDocument,
    SqliteParameter, Statement, StoredRow, TransactionResult, read_named_roster_rows,
    read_named_rows, read_rows,
};
use ployz_core::ids::ClusterName;
use ployz_core::ingress::RouteBindingOrigin;
use ployz_core::operation::{RouteHostname, RoutePort};

use crate::corrosion::{CorrosionClient, StoredRowLimit, collect_stored_rows};

use super::simple_deploy::{
    DeployCommand, DeployCommit, DeployProjection, DeployRosterMachine, DeployStartError,
    SimpleDeployStore,
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

    async fn observe(&self, command: &DeployCommand) -> Result<DeployProjection, DeployStartError> {
        let matching_namespaces = self
            .query(
                select_by_id(
                    CorrosionTable::Namespaces,
                    command.request.namespace_name.as_str(),
                ),
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
        let machines = self.query(all_rows(CorrosionTable::Machines), MAX_DEPLOY_ROWS);
        let routes = self.query(all_rows(CorrosionTable::RouteBindings), MAX_DEPLOY_ROWS);
        let (cluster, machines, routes) =
            tokio::try_join!(cluster, machines, routes).map_err(|error| error.to_string())?;

        let cluster = decode_cluster(&self.cluster_id, cluster)?;
        let missing_automatic_routes =
            desired_routes(&cluster, command, &command.request.namespace_name, routes)?;
        let roster = decode_roster(&cluster, machines)?;

        Ok(DeployProjection {
            namespace,
            missing_automatic_routes,
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

fn decode_namespace(
    cluster_id: &ClusterName,
    namespace_name: &CorrosionNamespaceName,
    rows: Vec<StoredRow>,
) -> Result<NamespaceDocument, DeployStartError> {
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
    Ok(report
        .accepted
        .into_iter()
        .next()
        .expect("length checked")
        .value)
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
) -> Result<Vec<RouteBindingDocument>, DeployStartError> {
    let report = read_named_rows::<RouteBindingDocument>(&cluster.cluster_id, rows);
    let mut planned = Vec::new();
    if let AutomaticHostnameMode::Custom { suffix } = &cluster.hostname_mode {
        for service_name in command.request.services.keys() {
            let hostname = automatic_hostname(namespace_id, service_name, suffix)
                .map_err(DeployStartError::Unavailable)?;
            if report
                .skipped
                .iter()
                .any(|row| row.source.key == hostname.as_str())
            {
                return Err(DeployStartError::Refused(
                    DeployRefusal::AutomaticHostnameConflict { hostname },
                ));
            }
            match report
                .accepted
                .iter()
                .find(|row| row.value.hostname == hostname)
            {
                Some(row)
                    if automatic_route_matches(
                        &row.value,
                        namespace_id,
                        service_name,
                        &hostname,
                    ) => {}
                Some(_) => {
                    return Err(DeployStartError::Refused(
                        DeployRefusal::AutomaticHostnameConflict { hostname },
                    ));
                }
                None => {
                    let document = RouteBindingDocument {
                        v: CorrosionDocumentVersion::V1,
                        cluster_id: cluster.cluster_id.clone(),
                        provenance: OperatorWriteProvenance {
                            written_by: command.initiator.clone(),
                            written_at: ployz_core::corrosion::CorrosionTimestamp::now_utc(),
                        },
                        hostname,
                        namespace_id: namespace_id.clone(),
                        service_name: service_name.clone(),
                        endpoint_port: RoutePort::try_new(80).map_err(|error| error.to_string())?,
                        origin: RouteBindingOrigin::Automatic,
                        ingress_mode: IngressMode::Direct,
                    };
                    planned.push(document);
                }
            }
        }
    }
    Ok(planned)
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
) -> Result<Vec<DeployRosterMachine>, String> {
    let machines = read_named_roster_rows::<MachineDocument>(cluster, machine_rows);
    let mut roster = machines
        .accepted
        .into_iter()
        .map(|row| DeployRosterMachine {
            name: row.value.name,
            lifecycle: row.value.lifecycle,
        })
        .collect::<Vec<_>>();
    roster.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(roster)
}

fn commit_statements(commit: &DeployCommit) -> Result<Vec<Statement>, String> {
    let mut statements = Vec::with_capacity(1 + commit.missing_automatic_routes.len());
    statements.push(upsert_document(
        CorrosionTable::Namespaces,
        commit.namespace.name.as_str(),
        &commit.namespace,
    )?);
    for route in &commit.missing_automatic_routes {
        statements.push(insert_if_absent_document(
            CorrosionTable::RouteBindings,
            route.hostname.as_str(),
            route,
        )?);
    }
    Ok(statements)
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

fn all_rows(table: CorrosionTable) -> Statement {
    Statement::simple(format!("SELECT id, document FROM {}", table.as_str()))
}

#[cfg(test)]
mod tests {
    use ployz_core::corrosion::{
        AutomaticHostnameMode, CorrosionDocumentVersion, CorrosionNamespaceName,
        CorrosionTimestamp, OperatorWriteProvenance, Principal, StorageMode,
    };
    use ployz_core::deploy::{ContainerRuntimeSpec, ImageReference};
    use ployz_core::ids::{DeployName, PeerName};
    use ployz_core::network::MachineEndpointSupernet;
    use ployz_core::{DeployRequest, DeployServiceRequest, HealthGatePolicy};

    use super::*;

    const CLUSTER: &str = "main";
    const NAMESPACE_A: &str = "production";
    const NAMESPACE_B: &str = "staging";

    #[test]
    fn namespace_lookup_uses_the_canonical_name_as_its_primary_key() {
        let name = CorrosionNamespaceName::try_new("production").expect("namespace name");
        let Statement::WithParams(sql, params) =
            select_by_id(CorrosionTable::Namespaces, name.as_str())
        else {
            panic!("namespace lookup must be parameterized");
        };
        assert_eq!(sql, "SELECT id, document FROM namespaces WHERE id = ?");
        assert_eq!(params, vec![SqliteParameter::Text("production".to_owned())]);
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
        assert_eq!(resolved.name.as_str(), NAMESPACE_A);
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
    fn namespace_commit_does_not_delete_route_bindings() {
        let statements = commit_statements(&DeployCommit {
            namespace: namespace_document(NAMESPACE_A),
            missing_automatic_routes: Vec::new(),
        })
        .expect("commit statements");

        assert!(statements.iter().all(|statement| match statement {
            Statement::WithParams(sql, _) | Statement::Simple(sql) => {
                !sql.contains("route_bindings")
            }
        }));
    }

    #[test]
    fn namespace_commit_is_an_unconditional_local_upsert() {
        let namespace = namespace_document(NAMESPACE_A);
        let statements = commit_statements(&DeployCommit {
            namespace: namespace.clone(),
            missing_automatic_routes: Vec::new(),
        })
        .expect("commit statements");

        assert_eq!(
            statements,
            vec![
                upsert_document(CorrosionTable::Namespaces, NAMESPACE_A, &namespace)
                    .expect("namespace upsert")
            ]
        );
        assert!(statements.iter().all(|statement| match statement {
            Statement::WithParams(sql, _) | Statement::Simple(sql) => {
                !sql.contains("WHERE namespace_id")
            }
        }));
    }

    #[test]
    fn deploy_inserts_only_a_missing_automatic_route() {
        let namespace_id = CorrosionNamespaceName::try_new(NAMESPACE_A).expect("namespace");
        let command = deploy_command();
        let cluster = cluster_document();

        let missing = desired_routes(&cluster, &command, &namespace_id, Vec::new())
            .expect("missing route plans");
        let [route] = missing.as_slice() else {
            panic!("one missing automatic route expected")
        };
        assert_eq!(route.hostname.as_str(), "api.production.apps.example.test");

        let existing = StoredRow::new(
            route.hostname.as_str(),
            serde_json::to_string(route).expect("route serializes"),
        );
        assert!(
            desired_routes(&cluster, &command, &namespace_id, vec![existing])
                .expect("existing route is accepted")
                .is_empty()
        );
    }

    #[test]
    fn deploy_ignores_unrelated_rejected_routes_but_refuses_exact_hostname_occupancy() {
        let namespace_id = CorrosionNamespaceName::try_new(NAMESPACE_A).expect("namespace");
        let command = deploy_command();
        let cluster = cluster_document();
        let rejected_document = serde_json::json!({
            "v": 2,
            "cluster_id": CLUSTER,
        })
        .to_string();

        let planned = desired_routes(
            &cluster,
            &command,
            &namespace_id,
            vec![StoredRow::new(
                "unrelated.apps.example.test",
                rejected_document.clone(),
            )],
        )
        .expect("unrelated rejected route is diagnostic evidence only");
        assert_eq!(planned.len(), 1);

        for exact_occupant in [
            rejected_document,
            serde_json::json!({"v": 1, "cluster_id": "another-cluster"}).to_string(),
            "not json".to_owned(),
        ] {
            let error = desired_routes(
                &cluster,
                &command,
                &namespace_id,
                vec![StoredRow::new(
                    "api.production.apps.example.test",
                    exact_occupant,
                )],
            )
            .expect_err("exact rejected hostname occupant conflicts");
            assert!(matches!(
                error,
                DeployStartError::Refused(DeployRefusal::AutomaticHostnameConflict {
                    hostname
                }) if hostname.as_str() == "api.production.apps.example.test"
            ));
        }
    }

    #[test]
    fn disabled_automatic_hostnames_ignore_all_rejected_route_evidence() {
        let namespace_id = CorrosionNamespaceName::try_new(NAMESPACE_A).expect("namespace");
        let command = deploy_command();
        let mut cluster = cluster_document();
        cluster.hostname_mode = AutomaticHostnameMode::Disabled;

        let planned = desired_routes(
            &cluster,
            &command,
            &namespace_id,
            vec![StoredRow::new(
                "api.production.apps.example.test",
                serde_json::json!({"v": 2, "cluster_id": CLUSTER}).to_string(),
            )],
        )
        .expect("disabled automatic hostnames do not inspect route occupancy");
        assert!(planned.is_empty());
    }

    fn deploy_command() -> DeployCommand {
        DeployCommand {
            request: DeployRequest {
                namespace_name: CorrosionNamespaceName::try_new(NAMESPACE_A).expect("namespace"),
                deploy_name: DeployName::try_new("release-1").expect("deploy"),
                services: [(
                    CorrosionServiceName::try_new("api").expect("service"),
                    DeployServiceRequest {
                        image: ImageReference::try_new("nginx:latest").expect("image"),
                        credential: None,
                        runtime: ContainerRuntimeSpec::image_defaults(),
                        health_gate: HealthGatePolicy::Enforce,
                        placement: None,
                        machines: None,
                    },
                )]
                .into_iter()
                .collect(),
            },
            initiator: initiator(),
        }
    }

    fn cluster_document() -> ClusterDocument {
        ClusterDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: cluster_id(),
            provenance: OperatorWriteProvenance {
                written_by: initiator(),
                written_at: timestamp(),
            },
            name: CLUSTER.to_owned(),
            storage_default: StorageMode::Plain,
            hostname_mode: AutomaticHostnameMode::Custom {
                suffix: RouteHostname::try_new("apps.example.test").expect("suffix"),
            },
            prefix: MachineEndpointSupernet::default_v1(),
            provider: ployz_core::corrosion::MeshProvider::BuiltinWireguard,
            acme_directory_url: "https://acme.example/directory".to_owned(),
            acme_contact: None,
        }
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
        let document = namespace_document(name);
        StoredRow::new(id, serde_json::to_string(&document).expect("json"))
    }

    fn namespace_document(name: &str) -> NamespaceDocument {
        NamespaceDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: cluster_id(),
            provenance: OperatorWriteProvenance {
                written_by: initiator(),
                written_at: timestamp(),
            },
            name: CorrosionNamespaceName::try_new(name).expect("namespace name"),
            services: Default::default(),
        }
    }
}
