use std::fs;
use std::path::{Path, PathBuf};

use mvp_acme::{AcmeChallengeToken, AcmeHostname, AcmeKeyAuthorization};
use mvp_bus::IslandId;
use mvp_lease::{LeaseContentHash, LeaseEpoch, LeaseHolder, LeaseTimestamp};
use rusqlite::{Connection, OptionalExtension, params, types::Type};
use tempfile::NamedTempFile;

use crate::error::{ProjectionError, ProjectionResult};
use crate::facts::{BackendEndpoint, DnsRecordFact, NodeId, RouteId, ServiceName};
use crate::model::{
    AcmeHttp01ChallengeKey, AcmeHttp01ChallengeProjection, DnsProjection, DnsRecordProjection,
    GatewayProjection, GatewayRouteProjection, NodeProjection, ProjectionIgnoreReason,
    ProjectionState, ProjectionStatus, ServiceProjection,
};

#[derive(Debug, Clone)]
pub struct SqliteProjectionStore {
    path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionRowCounts {
    pub nodes: usize,
    pub tombstoned_nodes: usize,
    pub services: usize,
    pub gateway_routes: usize,
    pub dns_records: usize,
    pub acme_http01_challenges: usize,
    pub statuses: usize,
}

#[derive(Debug)]
pub(crate) struct StagedSqliteProjection {
    target_path: PathBuf,
    file: NamedTempFile,
}

impl StagedSqliteProjection {
    pub(crate) fn promote(self) -> ProjectionResult<()> {
        self.file.as_file().sync_all()?;
        self.file
            .persist(&self.target_path)
            .map_err(|error| error.error)?;
        Ok(())
    }
}

impl SqliteProjectionStore {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn rebuild(&self, state: &ProjectionState) -> ProjectionResult<()> {
        self.stage_rebuild(state)?.promote()
    }

    pub(crate) fn stage_rebuild(
        &self,
        state: &ProjectionState,
    ) -> ProjectionResult<StagedSqliteProjection> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        let file = NamedTempFile::new_in(parent)?;
        self.rebuild_to_path(file.path(), state)?;
        Ok(StagedSqliteProjection {
            target_path: self.path.clone(),
            file,
        })
    }

    fn rebuild_to_path(&self, path: &Path, state: &ProjectionState) -> ProjectionResult<()> {
        let mut connection = Connection::open(path)?;
        create_schema(&connection)?;
        let transaction = connection.transaction()?;
        clear_projection_tables(&transaction)?;
        write_metadata(&transaction, state)?;
        write_nodes(&transaction, state)?;
        write_tombstoned_nodes(&transaction, state)?;
        write_services(&transaction, state)?;
        write_gateway(&transaction, state)?;
        write_dns(&transaction, state)?;
        write_acme_http01(&transaction, state)?;
        write_statuses(&transaction, state)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn load(&self) -> ProjectionResult<ProjectionState> {
        let connection = Connection::open(&self.path)?;
        create_schema(&connection)?;
        let Some(island) = connection
            .query_row(
                "SELECT value FROM projection_metadata WHERE key = 'island'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(IslandId::new)
        else {
            return Err(ProjectionError::ProjectionStore {
                path: self.path.clone(),
                reason: "missing island metadata".to_string(),
            });
        };
        let mut state = ProjectionState::for_island(island);
        load_nodes(&connection, &mut state)?;
        load_tombstoned_nodes(&connection, &mut state)?;
        load_services(&connection, &mut state)?;
        load_gateway(&connection, &mut state)?;
        load_dns(&connection, &mut state)?;
        load_acme_http01(&connection, &mut state)?;
        load_statuses(&connection, &mut state)?;
        Ok(state)
    }

    pub fn row_counts(&self) -> ProjectionResult<ProjectionRowCounts> {
        let connection = Connection::open(&self.path)?;
        create_schema(&connection)?;
        Ok(ProjectionRowCounts {
            nodes: count_rows(&connection, "nodes")?,
            tombstoned_nodes: count_rows(&connection, "tombstoned_nodes")?,
            services: count_rows(&connection, "services")?,
            gateway_routes: count_rows(&connection, "gateway_routes")?,
            dns_records: count_rows(&connection, "dns_records")?,
            acme_http01_challenges: count_rows(&connection, "acme_http01_challenges")?,
            statuses: count_rows(&connection, "projection_statuses")?,
        })
    }
}

fn create_schema(connection: &Connection) -> ProjectionResult<()> {
    connection.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS projection_metadata (
          key TEXT PRIMARY KEY,
          value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS nodes (
          node_id TEXT PRIMARY KEY,
          epoch INTEGER NOT NULL,
          overlay_ip TEXT NOT NULL,
          iroh_endpoint_id TEXT NOT NULL,
          wg_public_key TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS tombstoned_nodes (
          node_id TEXT PRIMARY KEY,
          epoch INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS services (
          service TEXT NOT NULL,
          node_id TEXT NOT NULL,
          version TEXT NOT NULL,
          endpoint_subject TEXT NOT NULL,
          epoch INTEGER NOT NULL,
          PRIMARY KEY (service, node_id)
        );
        CREATE TABLE IF NOT EXISTS gateway_routes (
          route_id TEXT PRIMARY KEY,
          gateway_commit_id TEXT NOT NULL,
          route_commit_id TEXT NOT NULL,
          hostnames_json TEXT NOT NULL,
          backends_json TEXT NOT NULL,
          old_backends_json TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS dns_records (
          dns_commit_id TEXT NOT NULL,
          name TEXT NOT NULL,
          record_type TEXT NOT NULL,
          value TEXT NOT NULL,
          ttl_seconds INTEGER NOT NULL,
          PRIMARY KEY (dns_commit_id, name, record_type, value)
        );
        CREATE TABLE IF NOT EXISTS acme_http01_challenges (
          hostname TEXT NOT NULL,
          token TEXT NOT NULL,
          key_authorization TEXT NOT NULL,
          holder TEXT NOT NULL,
          lease_epoch INTEGER NOT NULL,
          claim_hash TEXT NOT NULL,
          published_at INTEGER NOT NULL,
          PRIMARY KEY (hostname, token)
        );
        CREATE TABLE IF NOT EXISTS projection_statuses (
          reason TEXT PRIMARY KEY,
          count INTEGER NOT NULL
        );
        ",
    )?;
    Ok(())
}

fn count_rows(connection: &Connection, table: &'static str) -> ProjectionResult<usize> {
    let count = connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get::<_, i64>(0)
    })?;
    Ok(count as usize)
}

fn clear_projection_tables(connection: &Connection) -> ProjectionResult<()> {
    for table in [
        "projection_metadata",
        "nodes",
        "tombstoned_nodes",
        "services",
        "gateway_routes",
        "dns_records",
        "acme_http01_challenges",
        "projection_statuses",
    ] {
        connection.execute(&format!("DELETE FROM {table}"), [])?;
    }
    Ok(())
}

fn write_metadata(connection: &Connection, state: &ProjectionState) -> ProjectionResult<()> {
    connection.execute(
        "INSERT INTO projection_metadata (key, value) VALUES ('island', ?1)",
        params![state.island.as_str()],
    )?;
    Ok(())
}

fn write_nodes(connection: &Connection, state: &ProjectionState) -> ProjectionResult<()> {
    let mut statement = connection.prepare(
        "
        INSERT INTO nodes (node_id, epoch, overlay_ip, iroh_endpoint_id, wg_public_key)
        VALUES (?1, ?2, ?3, ?4, ?5)
        ",
    )?;
    for node in state.nodes.values() {
        statement.execute(params![
            node.node_id.as_str(),
            node.epoch as i64,
            node.overlay_ip.as_str(),
            node.iroh_endpoint_id.as_str(),
            node.wg_public_key.as_str()
        ])?;
    }
    Ok(())
}

fn write_tombstoned_nodes(
    connection: &Connection,
    state: &ProjectionState,
) -> ProjectionResult<()> {
    let mut statement = connection.prepare(
        "
        INSERT INTO tombstoned_nodes (node_id, epoch)
        VALUES (?1, ?2)
        ",
    )?;
    for (node_id, epoch) in &state.tombstoned_nodes {
        statement.execute(params![node_id.as_str(), *epoch as i64])?;
    }
    Ok(())
}

fn write_services(connection: &Connection, state: &ProjectionState) -> ProjectionResult<()> {
    let mut statement = connection.prepare(
        "
        INSERT INTO services (service, node_id, version, endpoint_subject, epoch)
        VALUES (?1, ?2, ?3, ?4, ?5)
        ",
    )?;
    for service in state.services.values() {
        statement.execute(params![
            service.service.as_str(),
            service.node_id.as_str(),
            service.version.as_str(),
            service.endpoint_subject.as_str(),
            service.epoch as i64
        ])?;
    }
    Ok(())
}

fn write_gateway(connection: &Connection, state: &ProjectionState) -> ProjectionResult<()> {
    let Some(gateway) = &state.gateway else {
        return Ok(());
    };
    let mut statement = connection.prepare(
        "
        INSERT INTO gateway_routes (
          route_id,
          gateway_commit_id,
          route_commit_id,
          hostnames_json,
          backends_json,
          old_backends_json
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        ",
    )?;
    for route in &gateway.routes {
        statement.execute(params![
            route.route_id.as_str(),
            gateway.gateway_commit_id.as_str(),
            gateway.route_commit_id.as_str(),
            serde_json::to_string(&route.hostnames)?,
            serde_json::to_string(&route.backends)?,
            serde_json::to_string(&route.old_backends_to_drain)?
        ])?;
    }
    Ok(())
}

fn write_dns(connection: &Connection, state: &ProjectionState) -> ProjectionResult<()> {
    let Some(dns) = &state.dns else {
        return Ok(());
    };
    let mut statement = connection.prepare(
        "
        INSERT INTO dns_records (dns_commit_id, name, record_type, value, ttl_seconds)
        VALUES (?1, ?2, ?3, ?4, ?5)
        ",
    )?;
    for record in &dns.records {
        statement.execute(params![
            dns.dns_commit_id.as_str(),
            record.name.as_str(),
            record.record_type.as_str(),
            record.value.as_str(),
            record.ttl_seconds as i64
        ])?;
    }
    Ok(())
}

fn write_acme_http01(connection: &Connection, state: &ProjectionState) -> ProjectionResult<()> {
    let mut statement = connection.prepare(
        "
        INSERT INTO acme_http01_challenges (
          hostname,
          token,
          key_authorization,
          holder,
          lease_epoch,
          claim_hash,
          published_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        ",
    )?;
    for challenge in state.acme_http01.values() {
        statement.execute(params![
            challenge.hostname.as_str(),
            challenge.token.as_str(),
            challenge.key_authorization.as_str(),
            challenge.holder.as_str(),
            challenge.lease_epoch.value() as i64,
            challenge.claim_hash.as_hex(),
            challenge.published_at.value() as i64
        ])?;
    }
    Ok(())
}

fn write_statuses(connection: &Connection, state: &ProjectionState) -> ProjectionResult<()> {
    let mut statement =
        connection.prepare("INSERT INTO projection_statuses (reason, count) VALUES (?1, ?2)")?;
    for status in &state.statuses {
        statement.execute(params![reason_name(&status.reason), status.count as i64])?;
    }
    Ok(())
}

fn load_nodes(connection: &Connection, state: &mut ProjectionState) -> ProjectionResult<()> {
    let mut statement = connection.prepare(
        "
        SELECT node_id, epoch, overlay_ip, iroh_endpoint_id, wg_public_key
        FROM nodes
        ORDER BY node_id
        ",
    )?;
    let rows = statement.query_map([], |row| {
        let node_id = NodeId::new(row.get::<_, String>(0)?);
        Ok(NodeProjection {
            node_id,
            epoch: row.get::<_, i64>(1)? as u64,
            overlay_ip: row.get(2)?,
            iroh_endpoint_id: row.get(3)?,
            wg_public_key: row.get(4)?,
        })
    })?;
    for row in rows {
        let node = row?;
        state.nodes.insert(node.node_id.clone(), node);
    }
    Ok(())
}

fn load_tombstoned_nodes(
    connection: &Connection,
    state: &mut ProjectionState,
) -> ProjectionResult<()> {
    let mut statement = connection.prepare(
        "
        SELECT node_id, epoch
        FROM tombstoned_nodes
        ORDER BY node_id
        ",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            NodeId::new(row.get::<_, String>(0)?),
            row.get::<_, i64>(1)? as u64,
        ))
    })?;
    for row in rows {
        let (node_id, epoch) = row?;
        state.tombstoned_nodes.insert(node_id, epoch);
    }
    Ok(())
}

fn load_services(connection: &Connection, state: &mut ProjectionState) -> ProjectionResult<()> {
    let mut statement = connection.prepare(
        "
        SELECT service, node_id, version, endpoint_subject, epoch
        FROM services
        ORDER BY service, node_id
        ",
    )?;
    let rows = statement.query_map([], |row| {
        let service = ServiceName::new(row.get::<_, String>(0)?);
        let node_id = NodeId::new(row.get::<_, String>(1)?);
        Ok(ServiceProjection {
            service,
            node_id,
            version: row.get(2)?,
            endpoint_subject: row.get(3)?,
            epoch: row.get::<_, i64>(4)? as u64,
        })
    })?;
    for row in rows {
        let service = row?;
        state
            .services
            .insert((service.service.clone(), service.node_id.clone()), service);
    }
    Ok(())
}

fn load_gateway(connection: &Connection, state: &mut ProjectionState) -> ProjectionResult<()> {
    let mut statement = connection.prepare(
        "
        SELECT route_id, gateway_commit_id, route_commit_id, hostnames_json, backends_json,
               old_backends_json
        FROM gateway_routes
        ORDER BY route_id
        ",
    )?;
    let rows = statement.query_map([], |row| {
        let hostnames_json: String = row.get(3)?;
        let backends_json: String = row.get(4)?;
        let old_backends_json: String = row.get(5)?;
        Ok((
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            GatewayRouteProjection {
                route_id: RouteId::new(row.get::<_, String>(0)?),
                hostnames: serde_json::from_str(&hostnames_json).map_err(to_sql_error)?,
                backends: serde_json::from_str::<Vec<BackendEndpoint>>(&backends_json)
                    .map_err(to_sql_error)?,
                old_backends_to_drain: serde_json::from_str::<Vec<BackendEndpoint>>(
                    &old_backends_json,
                )
                .map_err(to_sql_error)?,
            },
        ))
    })?;
    let mut gateway_commit_id = None;
    let mut route_commit_id = None;
    let mut routes = Vec::new();
    for row in rows {
        let (gateway_id, route_id, route) = row?;
        gateway_commit_id = Some(gateway_id);
        route_commit_id = Some(route_id);
        routes.push(route);
    }
    if let (Some(gateway_commit_id), Some(route_commit_id)) = (gateway_commit_id, route_commit_id) {
        state.gateway = Some(GatewayProjection {
            gateway_commit_id,
            route_commit_id,
            routes,
        });
    }
    Ok(())
}

fn load_dns(connection: &Connection, state: &mut ProjectionState) -> ProjectionResult<()> {
    let mut statement = connection.prepare(
        "
        SELECT dns_commit_id, name, record_type, value, ttl_seconds
        FROM dns_records
        ORDER BY name, record_type, value
        ",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            DnsRecordProjection::from(DnsRecordFact {
                name: row.get(1)?,
                record_type: row.get(2)?,
                value: row.get(3)?,
                ttl_seconds: row.get::<_, i64>(4)? as u32,
            }),
        ))
    })?;
    let mut dns_commit_id = None;
    let mut records = Vec::new();
    for row in rows {
        let (commit_id, record) = row?;
        dns_commit_id = Some(commit_id);
        records.push(record);
    }
    if let Some(dns_commit_id) = dns_commit_id {
        state.dns = Some(DnsProjection {
            dns_commit_id,
            records,
        });
    }
    Ok(())
}

fn load_acme_http01(connection: &Connection, state: &mut ProjectionState) -> ProjectionResult<()> {
    let mut statement = connection.prepare(
        "
        SELECT hostname, token, key_authorization, holder, lease_epoch, claim_hash,
               published_at
        FROM acme_http01_challenges
        ORDER BY hostname, token
        ",
    )?;
    let rows = statement.query_map([], |row| {
        let hostname = AcmeHostname::parse(row.get::<_, String>(0)?).map_err(to_sql_error)?;
        let token = AcmeChallengeToken::parse(row.get::<_, String>(1)?).map_err(to_sql_error)?;
        let key_authorization =
            AcmeKeyAuthorization::parse_for_token(&token, row.get::<_, String>(2)?)
                .map_err(to_sql_error)?;
        let holder = LeaseHolder::new(row.get::<_, String>(3)?);
        let lease_epoch =
            LeaseEpoch::from_u64(read_u64(row.get::<_, i64>(4)?, 4)?).map_err(to_sql_error)?;
        let claim_hash =
            LeaseContentHash::from_hex(&row.get::<_, String>(5)?).map_err(to_sql_error)?;
        let published_at = LeaseTimestamp::from_secs(read_u64(row.get::<_, i64>(6)?, 6)?);
        Ok(AcmeHttp01ChallengeProjection {
            hostname,
            token,
            key_authorization,
            holder,
            lease_epoch,
            claim_hash,
            published_at,
        })
    })?;
    for row in rows {
        let challenge = row?;
        let key = AcmeHttp01ChallengeKey::new(challenge.hostname.clone(), challenge.token.clone());
        state.acme_http01.insert(key, challenge);
    }
    Ok(())
}

fn load_statuses(connection: &Connection, state: &mut ProjectionState) -> ProjectionResult<()> {
    let mut statement =
        connection.prepare("SELECT reason, count FROM projection_statuses ORDER BY reason")?;
    let rows = statement.query_map([], |row| {
        let reason_name: String = row.get(0)?;
        Ok(ProjectionStatus {
            reason: parse_reason(&reason_name).map_err(to_sql_error)?,
            count: row.get::<_, i64>(1)? as usize,
        })
    })?;
    for row in rows {
        state.statuses.push(row?);
    }
    Ok(())
}

fn reason_name(reason: &ProjectionIgnoreReason) -> &'static str {
    match reason {
        ProjectionIgnoreReason::CrossIsland => "cross_island",
        ProjectionIgnoreReason::Unverified => "unverified",
        ProjectionIgnoreReason::Unauthorized => "unauthorized",
        ProjectionIgnoreReason::Conflict => "conflict",
        ProjectionIgnoreReason::Superseded => "superseded",
        ProjectionIgnoreReason::MissingPayload => "missing_payload",
        ProjectionIgnoreReason::MalformedPayload => "malformed_payload",
        ProjectionIgnoreReason::UnsupportedFactKind => "unsupported_fact_kind",
    }
}

fn parse_reason(value: &str) -> Result<ProjectionIgnoreReason, serde_json::Error> {
    let reason = match value {
        "cross_island" => ProjectionIgnoreReason::CrossIsland,
        "unverified" => ProjectionIgnoreReason::Unverified,
        "unauthorized" => ProjectionIgnoreReason::Unauthorized,
        "conflict" => ProjectionIgnoreReason::Conflict,
        "superseded" => ProjectionIgnoreReason::Superseded,
        "missing_payload" => ProjectionIgnoreReason::MissingPayload,
        "malformed_payload" => ProjectionIgnoreReason::MalformedPayload,
        "unsupported_fact_kind" => ProjectionIgnoreReason::UnsupportedFactKind,
        other => return serde_json::from_str(other),
    };
    Ok(reason)
}

fn to_sql_error(error: impl std::error::Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(error))
}

fn read_u64(value: i64, column: usize) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(column, Type::Integer, Box::new(error))
    })
}

#[cfg(test)]
mod tests {
    use super::SqliteProjectionStore;
    use crate::facts::{BackendEndpoint, DnsRecordFact, NodeId, RouteId, ServiceName};
    use crate::model::{
        DnsProjection, DnsRecordProjection, GatewayProjection, GatewayRouteProjection,
        NodeProjection, ProjectionIgnoreReason, ProjectionState, ProjectionStatus,
        ServiceProjection,
    };
    use mvp_bus::IslandId;

    fn sample_state() -> ProjectionState {
        let mut state = ProjectionState::for_island(IslandId::new("prod"));
        let node_id = NodeId::new("node-1");
        state.nodes.insert(
            node_id.clone(),
            NodeProjection {
                node_id: node_id.clone(),
                epoch: 1,
                overlay_ip: "fd00::1".to_string(),
                iroh_endpoint_id: "iroh-test".to_string(),
                wg_public_key: "wg-test".to_string(),
            },
        );
        state.tombstoned_nodes.insert(NodeId::new("node-2"), 3);
        state.services.insert(
            (ServiceName::new("web"), node_id.clone()),
            ServiceProjection {
                service: ServiceName::new("web"),
                node_id: node_id.clone(),
                version: "1.0.0".to_string(),
                endpoint_subject: "node.node-1.rpc.inspect".to_string(),
                epoch: 1,
            },
        );
        let backend = BackendEndpoint {
            node_id,
            address: "10.0.0.1:8080".to_string(),
        };
        state.gateway = Some(GatewayProjection {
            gateway_commit_id: "gateway-1".to_string(),
            route_commit_id: "route-1".to_string(),
            routes: vec![GatewayRouteProjection {
                route_id: RouteId::new("web-http"),
                hostnames: vec!["web.example.com".to_string()],
                backends: vec![backend],
                old_backends_to_drain: Vec::new(),
            }],
        });
        state.dns = Some(DnsProjection {
            dns_commit_id: "dns-1".to_string(),
            records: vec![DnsRecordProjection::from(DnsRecordFact {
                name: "web.example.com".to_string(),
                record_type: "AAAA".to_string(),
                value: "fd00::1".to_string(),
                ttl_seconds: 30,
            })],
        });
        state.statuses.push(ProjectionStatus {
            reason: ProjectionIgnoreReason::Unauthorized,
            count: 2,
        });
        state
    }

    #[test]
    fn rebuild_persists_and_loads_projection_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SqliteProjectionStore::new(dir.path().join("projections.sqlite"));
        let state = sample_state();

        store.rebuild(&state).expect("rebuild projection");
        let loaded = store.load().expect("load projection");

        assert_eq!(loaded, state);
    }

    #[test]
    fn deleting_database_and_rebuilding_preserves_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("projections.sqlite");
        let store = SqliteProjectionStore::new(&path);
        let state = sample_state();

        store.rebuild(&state).expect("rebuild projection");
        std::fs::remove_file(&path).expect("delete projection db");
        store.rebuild(&state).expect("rebuild projection again");

        assert_eq!(store.load().expect("load projection"), state);
    }

    #[test]
    fn corrupt_database_is_rebuilt_from_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("projections.sqlite");
        let store = SqliteProjectionStore::new(&path);
        let state = sample_state();
        std::fs::write(&path, b"not sqlite").expect("write corrupt db");

        store.rebuild(&state).expect("rebuild corrupt projection");

        assert_eq!(store.load().expect("load projection"), state);
    }

    #[test]
    fn rebuild_replaces_old_rows_transactionally() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SqliteProjectionStore::new(dir.path().join("projections.sqlite"));
        let mut state = sample_state();
        store.rebuild(&state).expect("rebuild projection");
        state.nodes.clear();
        state.tombstoned_nodes.clear();
        state.services.clear();
        state.gateway = None;
        state.dns = None;
        state.statuses.clear();

        store.rebuild(&state).expect("replace projection");
        let loaded = store.load().expect("load projection");

        assert_eq!(loaded, state);
        assert!(loaded.nodes.is_empty());
        assert!(loaded.gateway.is_none());
    }
}
