//! Substrate membership row primitives.
//!
//! These types describe durable rows and queries. They do not decide Ployz
//! product outcomes such as `Joined` or `AlreadyPresent`.

use crate::{Error, IrohEndpointId, StoreParam, StoreResult, StoreStatement, StoreValue};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StoreMachineId(String);

impl StoreMachineId {
    pub fn parse(value: impl Into<String>) -> crate::Result<Self> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IslandId(String);

impl IslandId {
    pub fn parse(value: impl Into<String>) -> crate::Result<Self> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NamespaceId(String);

impl NamespaceId {
    pub fn parse(value: impl Into<String>) -> crate::Result<Self> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NamespaceRole(String);

impl NamespaceRole {
    pub fn parse(value: impl Into<String>) -> crate::Result<Self> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WireGuardPublicKey(String);

impl WireGuardPublicKey {
    pub fn parse(value: impl Into<String>) -> crate::Result<Self> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OverlayIp(String);

impl OverlayIp {
    pub fn parse(value: impl Into<String>) -> crate::Result<Self> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapabilitiesJson(String);

impl CapabilitiesJson {
    pub fn parse(value: impl Into<String>) -> crate::Result<Self> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowEpoch(u64);

impl RowEpoch {
    pub fn new(value: u64) -> crate::Result<Self> {
        if value == 0 || value > i64::MAX as u64 {
            return Err(Error::MalformedPayload);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }

    fn sql_value(self) -> i64 {
        self.0 as i64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MembershipLifecycle {
    Active,
    Removing,
    Tombstoned,
    Conflicted,
    Deleted,
}

impl MembershipLifecycle {
    pub fn parse(value: &str) -> crate::Result<Self> {
        match value {
            "active" => Ok(Self::Active),
            "removing" => Ok(Self::Removing),
            "tombstoned" => Ok(Self::Tombstoned),
            "conflicted" => Ok(Self::Conflicted),
            "deleted" => Ok(Self::Deleted),
            _ => Err(Error::MalformedPayload),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Removing => "removing",
            Self::Tombstoned => "tombstoned",
            Self::Conflicted => "conflicted",
            Self::Deleted => "deleted",
        }
    }
}

pub fn machine_row_from_store_row(row: &[StoreValue]) -> StoreResult<MachineRow> {
    let [
        StoreValue::Text(machine_id),
        StoreValue::Text(island_id),
        StoreValue::Text(iroh_endpoint_id),
        StoreValue::Text(wireguard_public_key),
        StoreValue::Text(overlay_ip),
        StoreValue::Text(capabilities_json),
        StoreValue::Text(lifecycle),
        StoreValue::Integer(epoch),
        StoreValue::Integer(updated_at),
    ] = row
    else {
        return Err(crate::StoreError::MalformedPayload);
    };

    if *epoch <= 0 {
        return Err(crate::StoreError::MalformedPayload);
    }

    Ok(MachineRow::new(
        StoreMachineId::parse(machine_id.clone())
            .map_err(|_| crate::StoreError::MalformedPayload)?,
        IslandId::parse(island_id.clone()).map_err(|_| crate::StoreError::MalformedPayload)?,
        IrohEndpointId::parse(iroh_endpoint_id.clone())
            .map_err(|_| crate::StoreError::MalformedPayload)?,
        WireGuardPublicKey::parse(wireguard_public_key.clone())
            .map_err(|_| crate::StoreError::MalformedPayload)?,
        OverlayIp::parse(overlay_ip.clone()).map_err(|_| crate::StoreError::MalformedPayload)?,
        CapabilitiesJson::parse(capabilities_json.clone())
            .map_err(|_| crate::StoreError::MalformedPayload)?,
        MembershipLifecycle::parse(lifecycle).map_err(|_| crate::StoreError::MalformedPayload)?,
        RowEpoch::new(*epoch as u64).map_err(|_| crate::StoreError::MalformedPayload)?,
        *updated_at,
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineRow {
    machine_id: StoreMachineId,
    island_id: IslandId,
    iroh_endpoint_id: IrohEndpointId,
    wireguard_public_key: WireGuardPublicKey,
    overlay_ip: OverlayIp,
    capabilities_json: CapabilitiesJson,
    lifecycle: MembershipLifecycle,
    epoch: RowEpoch,
    updated_at: i64,
}

impl MachineRow {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        machine_id: StoreMachineId,
        island_id: IslandId,
        iroh_endpoint_id: IrohEndpointId,
        wireguard_public_key: WireGuardPublicKey,
        overlay_ip: OverlayIp,
        capabilities_json: CapabilitiesJson,
        lifecycle: MembershipLifecycle,
        epoch: RowEpoch,
        updated_at: i64,
    ) -> Self {
        Self {
            machine_id,
            island_id,
            iroh_endpoint_id,
            wireguard_public_key,
            overlay_ip,
            capabilities_json,
            lifecycle,
            epoch,
            updated_at,
        }
    }

    #[must_use]
    pub fn machine_id(&self) -> &StoreMachineId {
        &self.machine_id
    }

    #[must_use]
    pub fn endpoint_id(&self) -> &IrohEndpointId {
        &self.iroh_endpoint_id
    }

    #[must_use]
    pub fn wireguard_public_key(&self) -> &WireGuardPublicKey {
        &self.wireguard_public_key
    }

    #[must_use]
    pub fn overlay_ip(&self) -> &OverlayIp {
        &self.overlay_ip
    }

    #[must_use]
    pub fn lifecycle(&self) -> MembershipLifecycle {
        self.lifecycle
    }

    #[must_use]
    pub fn epoch(&self) -> RowEpoch {
        self.epoch
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceRow {
    namespace_id: NamespaceId,
    owner_island_id: IslandId,
    name: String,
    lifecycle: MembershipLifecycle,
    epoch: RowEpoch,
    updated_at: i64,
}

impl NamespaceRow {
    pub fn new(
        namespace_id: NamespaceId,
        owner_island_id: IslandId,
        name: impl Into<String>,
        lifecycle: MembershipLifecycle,
        epoch: RowEpoch,
        updated_at: i64,
    ) -> crate::Result<Self> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(Error::MalformedPayload);
        }
        Ok(Self {
            namespace_id,
            owner_island_id,
            name,
            lifecycle,
            epoch,
            updated_at,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceMembershipRow {
    namespace_id: NamespaceId,
    machine_id: StoreMachineId,
    role: NamespaceRole,
    lifecycle: MembershipLifecycle,
    epoch: RowEpoch,
    updated_at: i64,
}

impl NamespaceMembershipRow {
    #[must_use]
    pub fn new(
        namespace_id: NamespaceId,
        machine_id: StoreMachineId,
        role: NamespaceRole,
        lifecycle: MembershipLifecycle,
        epoch: RowEpoch,
        updated_at: i64,
    ) -> Self {
        Self {
            namespace_id,
            machine_id,
            role,
            lifecycle,
            epoch,
            updated_at,
        }
    }
}

pub fn membership_schema_statements() -> StoreResult<Vec<StoreStatement>> {
    Ok(vec![
        StoreStatement::new(
            "CREATE TABLE IF NOT EXISTS machines (
                machine_id TEXT PRIMARY KEY,
                island_id TEXT NOT NULL,
                iroh_endpoint_id TEXT NOT NULL,
                wireguard_public_key TEXT NOT NULL,
                overlay_ip TEXT NOT NULL,
                capabilities_json TEXT NOT NULL,
                lifecycle TEXT NOT NULL,
                epoch INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )",
        )?,
        StoreStatement::new(
            "CREATE TABLE IF NOT EXISTS namespaces (
                namespace_id TEXT PRIMARY KEY,
                owner_island_id TEXT NOT NULL,
                name TEXT NOT NULL,
                lifecycle TEXT NOT NULL,
                epoch INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )",
        )?,
        StoreStatement::new(
            "CREATE TABLE IF NOT EXISTS namespace_memberships (
                namespace_id TEXT NOT NULL,
                machine_id TEXT NOT NULL,
                role TEXT NOT NULL,
                lifecycle TEXT NOT NULL,
                epoch INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY(namespace_id, machine_id)
            )",
        )?,
    ])
}

pub fn upsert_machine_statement(row: &MachineRow) -> StoreResult<StoreStatement> {
    StoreStatement::with_params(
        "INSERT INTO machines (
            machine_id,
            island_id,
            iroh_endpoint_id,
            wireguard_public_key,
            overlay_ip,
            capabilities_json,
            lifecycle,
            epoch,
            updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
        ON CONFLICT(machine_id) DO UPDATE SET
            island_id = excluded.island_id,
            iroh_endpoint_id = excluded.iroh_endpoint_id,
            wireguard_public_key = excluded.wireguard_public_key,
            overlay_ip = excluded.overlay_ip,
            capabilities_json = excluded.capabilities_json,
            lifecycle = excluded.lifecycle,
            epoch = excluded.epoch,
            updated_at = excluded.updated_at
        WHERE excluded.epoch >= machines.epoch",
        vec![
            StoreParam::Text(row.machine_id.as_str().to_string()),
            StoreParam::Text(row.island_id.as_str().to_string()),
            StoreParam::Text(row.iroh_endpoint_id.as_str().to_string()),
            StoreParam::Text(row.wireguard_public_key.as_str().to_string()),
            StoreParam::Text(row.overlay_ip.as_str().to_string()),
            StoreParam::Text(row.capabilities_json.as_str().to_string()),
            StoreParam::Text(row.lifecycle.as_str().to_string()),
            StoreParam::Integer(row.epoch.sql_value()),
            StoreParam::Integer(row.updated_at),
        ],
    )
}

pub fn select_machine_statement(machine_id: &StoreMachineId) -> StoreResult<StoreStatement> {
    StoreStatement::with_params(
        "SELECT
            machine_id,
            island_id,
            iroh_endpoint_id,
            wireguard_public_key,
            overlay_ip,
            capabilities_json,
            lifecycle,
            epoch,
            updated_at
        FROM machines
        WHERE machine_id = ?1",
        vec![StoreParam::Text(machine_id.as_str().to_string())],
    )
}

pub fn active_machine_statement(machine_id: &StoreMachineId) -> StoreResult<StoreStatement> {
    StoreStatement::with_params(
        "SELECT
            machine_id,
            island_id,
            iroh_endpoint_id,
            wireguard_public_key,
            overlay_ip,
            capabilities_json,
            lifecycle,
            epoch,
            updated_at
        FROM machines
        WHERE machine_id = ?1
          AND lifecycle = 'active'",
        vec![StoreParam::Text(machine_id.as_str().to_string())],
    )
}

pub fn upsert_namespace_statement(row: &NamespaceRow) -> StoreResult<StoreStatement> {
    StoreStatement::with_params(
        "INSERT INTO namespaces (
            namespace_id,
            owner_island_id,
            name,
            lifecycle,
            epoch,
            updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        ON CONFLICT(namespace_id) DO UPDATE SET
            owner_island_id = excluded.owner_island_id,
            name = excluded.name,
            lifecycle = excluded.lifecycle,
            epoch = excluded.epoch,
            updated_at = excluded.updated_at
        WHERE excluded.epoch >= namespaces.epoch",
        vec![
            StoreParam::Text(row.namespace_id.as_str().to_string()),
            StoreParam::Text(row.owner_island_id.as_str().to_string()),
            StoreParam::Text(row.name.clone()),
            StoreParam::Text(row.lifecycle.as_str().to_string()),
            StoreParam::Integer(row.epoch.sql_value()),
            StoreParam::Integer(row.updated_at),
        ],
    )
}

pub fn upsert_namespace_membership_statement(
    row: &NamespaceMembershipRow,
) -> StoreResult<StoreStatement> {
    StoreStatement::with_params(
        "INSERT INTO namespace_memberships (
            namespace_id,
            machine_id,
            role,
            lifecycle,
            epoch,
            updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        ON CONFLICT(namespace_id, machine_id) DO UPDATE SET
            role = excluded.role,
            lifecycle = excluded.lifecycle,
            epoch = excluded.epoch,
            updated_at = excluded.updated_at
        WHERE excluded.epoch >= namespace_memberships.epoch",
        vec![
            StoreParam::Text(row.namespace_id.as_str().to_string()),
            StoreParam::Text(row.machine_id.as_str().to_string()),
            StoreParam::Text(row.role.as_str().to_string()),
            StoreParam::Text(row.lifecycle.as_str().to_string()),
            StoreParam::Integer(row.epoch.sql_value()),
            StoreParam::Integer(row.updated_at),
        ],
    )
}

pub fn wireguard_peers_for_machine_statement(
    machine_id: &StoreMachineId,
) -> StoreResult<StoreStatement> {
    StoreStatement::with_params(
        "SELECT DISTINCT peer.*
        FROM namespace_memberships self_membership
        JOIN namespace_memberships peer_membership
          ON peer_membership.namespace_id = self_membership.namespace_id
        JOIN machines peer
          ON peer.machine_id = peer_membership.machine_id
        WHERE self_membership.machine_id = ?1
          AND self_membership.lifecycle = 'active'
          AND peer_membership.lifecycle = 'active'
          AND peer.machine_id != ?1
          AND peer.lifecycle = 'active'",
        vec![StoreParam::Text(machine_id.as_str().to_string())],
    )
}

fn parse_non_empty<T>(
    value: impl Into<String>,
    build: impl FnOnce(String) -> T,
) -> crate::Result<T> {
    let value = value.into();
    if value.trim().is_empty() {
        return Err(Error::MalformedPayload);
    }
    Ok(build(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_ids_reject_empty_values() {
        assert_eq!(StoreMachineId::parse(""), Err(Error::MalformedPayload));
        assert_eq!(IslandId::parse(" "), Err(Error::MalformedPayload));
    }

    #[test]
    fn row_epoch_rejects_zero_and_out_of_sql_range() {
        assert_eq!(RowEpoch::new(0), Err(Error::MalformedPayload));
        assert_eq!(
            RowEpoch::new(i64::MAX as u64 + 1),
            Err(Error::MalformedPayload)
        );
    }

    #[test]
    fn schema_does_not_store_durable_ticket_text() {
        let schema = membership_schema_statements().expect("schema");
        let sql = schema
            .iter()
            .map(StoreStatement::sql)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(sql.contains("iroh_endpoint_id"));
        assert!(!sql.contains("iroh_ticket"));
    }

    #[test]
    fn schema_creates_only_membership_tables_for_this_slice() {
        let conn = test_connection();
        let mut query = conn
            .prepare(
                "SELECT name FROM sqlite_schema
                 WHERE type = 'table'
                   AND name NOT LIKE 'sqlite_%'
                 ORDER BY name",
            )
            .expect("prepare schema query");
        let tables = query
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query schema")
            .collect::<Result<Vec<_>, _>>()
            .expect("tables");

        assert_eq!(
            tables,
            vec![
                "machines".to_string(),
                "namespace_memberships".to_string(),
                "namespaces".to_string(),
            ]
        );
    }

    #[test]
    fn machine_upsert_is_parameterized() {
        let statement = upsert_machine_statement(&machine_row()).expect("statement");

        assert!(statement.sql().contains("INSERT INTO machines"));
        assert!(statement.sql().contains("ON CONFLICT(machine_id)"));
        assert!(!statement.sql().contains("node-a"));
    }

    #[test]
    fn wireguard_peer_query_uses_namespace_overlap() {
        let statement =
            wireguard_peers_for_machine_statement(&StoreMachineId::parse("node-a").expect("id"))
                .expect("statement");

        assert!(
            statement
                .sql()
                .contains("namespace_memberships self_membership")
        );
        assert!(statement.sql().contains("peer_membership.namespace_id"));
        assert!(statement.sql().contains("peer.lifecycle = 'active'"));
    }

    #[test]
    fn machine_upsert_allows_same_epoch_owner_update() {
        let conn = test_connection();
        execute_machine_upsert(&conn, "node-a", "prod", "endpoint-a", "wg-a", "fd00::1", 2);

        let changed =
            execute_machine_upsert(&conn, "node-a", "prod", "endpoint-b", "wg-b", "fd00::2", 2);
        let endpoint: String = conn
            .query_row(
                "SELECT iroh_endpoint_id FROM machines WHERE machine_id = 'node-a'",
                [],
                |row| row.get(0),
            )
            .expect("endpoint");

        assert_eq!(changed, 1);
        assert_eq!(endpoint, "endpoint-b");
    }

    #[test]
    fn machine_upsert_ignores_lower_epoch_row() {
        let conn = test_connection();
        execute_machine_upsert(&conn, "node-a", "prod", "endpoint-a", "wg-a", "fd00::1", 2);

        let changed =
            execute_machine_upsert(&conn, "node-a", "prod", "endpoint-b", "wg-b", "fd00::2", 1);
        let endpoint: String = conn
            .query_row(
                "SELECT iroh_endpoint_id FROM machines WHERE machine_id = 'node-a'",
                [],
                |row| row.get(0),
            )
            .expect("endpoint");

        assert_eq!(changed, 0);
        assert_eq!(endpoint, "endpoint-a");
    }

    #[test]
    fn namespace_upsert_ignores_lower_epoch_row() {
        let conn = test_connection();
        let original = namespace_row("prod", "Production", 2);
        let stale = namespace_row("prod", "Stale", 1);

        assert_eq!(execute_namespace_upsert(&conn, &original), 1);
        assert_eq!(execute_namespace_upsert(&conn, &stale), 0);
        let name: String = conn
            .query_row(
                "SELECT name FROM namespaces WHERE namespace_id = 'prod'",
                [],
                |row| row.get(0),
            )
            .expect("name");

        assert_eq!(name, "Production");
    }

    #[test]
    fn namespace_membership_upsert_ignores_lower_epoch_row() {
        let conn = test_connection();
        let original = namespace_membership_row("prod", "node-a", "writer", 2);
        let stale = namespace_membership_row("prod", "node-a", "reader", 1);

        assert_eq!(execute_namespace_membership_upsert(&conn, &original), 1);
        assert_eq!(execute_namespace_membership_upsert(&conn, &stale), 0);
        let role: String = conn
            .query_row(
                "SELECT role FROM namespace_memberships
                 WHERE namespace_id = 'prod' AND machine_id = 'node-a'",
                [],
                |row| row.get(0),
            )
            .expect("role");

        assert_eq!(role, "writer");
    }

    #[test]
    fn wireguard_peer_query_returns_only_active_namespace_overlap() {
        let conn = test_connection();
        for (machine, endpoint, overlay, lifecycle) in [
            ("node-a", "endpoint-a", "fd00::1", "active"),
            ("node-b", "endpoint-b", "fd00::2", "active"),
            ("node-c", "endpoint-c", "fd00::3", "active"),
            ("node-d", "endpoint-d", "fd00::4", "deleted"),
            ("node-e", "endpoint-e", "fd00::5", "active"),
        ] {
            execute_machine_upsert_with_lifecycle(
                &conn,
                MachineUpsertInput {
                    machine_id: machine,
                    island_id: "prod",
                    endpoint_id: endpoint,
                    wireguard_public_key: machine,
                    overlay_ip: overlay,
                    epoch: 1,
                    lifecycle,
                },
            );
        }
        insert_namespace_membership(&conn, "prod", "node-a", "active");
        insert_namespace_membership(&conn, "prod", "node-b", "active");
        insert_namespace_membership(&conn, "prod", "node-c", "deleted");
        insert_namespace_membership(&conn, "prod", "node-d", "active");
        insert_namespace_membership(&conn, "other", "node-e", "active");

        let statement =
            wireguard_peers_for_machine_statement(&StoreMachineId::parse("node-a").expect("id"))
                .expect("statement");
        let mut query = conn.prepare(statement.sql()).expect("prepare");
        let peers = query
            .query_map(["node-a"], |row| row.get::<_, String>(0))
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("peers");

        assert_eq!(peers, vec!["node-b"]);
    }

    #[test]
    fn machine_row_decodes_from_store_row_shape() {
        let row = vec![
            StoreValue::Text("node-a".to_string()),
            StoreValue::Text("prod".to_string()),
            StoreValue::Text("endpoint-node-a".to_string()),
            StoreValue::Text("wg-node-a".to_string()),
            StoreValue::Text("fd00::1".to_string()),
            StoreValue::Text("{}".to_string()),
            StoreValue::Text("active".to_string()),
            StoreValue::Integer(7),
            StoreValue::Integer(100),
        ];

        let decoded = machine_row_from_store_row(&row).expect("row");

        assert_eq!(decoded.machine_id().as_str(), "node-a");
        assert_eq!(decoded.lifecycle(), MembershipLifecycle::Active);
        assert_eq!(decoded.epoch(), RowEpoch::new(7).expect("epoch"));
    }

    fn machine_row() -> MachineRow {
        MachineRow::new(
            StoreMachineId::parse("node-a").expect("machine"),
            IslandId::parse("prod").expect("island"),
            IrohEndpointId::parse("endpoint-node-a").expect("endpoint"),
            WireGuardPublicKey::parse("wg-node-a").expect("wireguard"),
            OverlayIp::parse("fd00::1").expect("overlay"),
            CapabilitiesJson::parse("{}").expect("capabilities"),
            MembershipLifecycle::Active,
            RowEpoch::new(1).expect("epoch"),
            100,
        )
    }

    fn namespace_row(namespace_id: &str, name: &str, epoch: u64) -> NamespaceRow {
        NamespaceRow::new(
            NamespaceId::parse(namespace_id).expect("namespace"),
            IslandId::parse("prod").expect("island"),
            name,
            MembershipLifecycle::Active,
            RowEpoch::new(epoch).expect("epoch"),
            100,
        )
        .expect("namespace row")
    }

    fn namespace_membership_row(
        namespace_id: &str,
        machine_id: &str,
        role: &str,
        epoch: u64,
    ) -> NamespaceMembershipRow {
        NamespaceMembershipRow::new(
            NamespaceId::parse(namespace_id).expect("namespace"),
            StoreMachineId::parse(machine_id).expect("machine"),
            NamespaceRole::parse(role).expect("role"),
            MembershipLifecycle::Active,
            RowEpoch::new(epoch).expect("epoch"),
            100,
        )
    }

    fn test_connection() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().expect("sqlite");
        for statement in membership_schema_statements().expect("schema") {
            conn.execute(statement.sql(), []).expect("schema statement");
        }
        conn
    }

    fn execute_machine_upsert(
        conn: &rusqlite::Connection,
        machine_id: &str,
        island_id: &str,
        endpoint_id: &str,
        wireguard_public_key: &str,
        overlay_ip: &str,
        epoch: i64,
    ) -> usize {
        execute_machine_upsert_with_lifecycle(
            conn,
            MachineUpsertInput {
                machine_id,
                island_id,
                endpoint_id,
                wireguard_public_key,
                overlay_ip,
                epoch,
                lifecycle: "active",
            },
        )
    }

    struct MachineUpsertInput<'a> {
        machine_id: &'a str,
        island_id: &'a str,
        endpoint_id: &'a str,
        wireguard_public_key: &'a str,
        overlay_ip: &'a str,
        epoch: i64,
        lifecycle: &'a str,
    }

    fn execute_machine_upsert_with_lifecycle(
        conn: &rusqlite::Connection,
        input: MachineUpsertInput<'_>,
    ) -> usize {
        let statement = upsert_machine_statement(&MachineRow::new(
            StoreMachineId::parse(input.machine_id).expect("machine"),
            IslandId::parse(input.island_id).expect("island"),
            IrohEndpointId::parse(input.endpoint_id).expect("endpoint"),
            WireGuardPublicKey::parse(input.wireguard_public_key).expect("wireguard"),
            OverlayIp::parse(input.overlay_ip).expect("overlay"),
            CapabilitiesJson::parse("{}").expect("capabilities"),
            MembershipLifecycle::parse(input.lifecycle).expect("lifecycle"),
            RowEpoch::new(input.epoch as u64).expect("epoch"),
            100,
        ))
        .expect("statement");

        conn.execute(
            statement.sql(),
            rusqlite::params![
                input.machine_id,
                input.island_id,
                input.endpoint_id,
                input.wireguard_public_key,
                input.overlay_ip,
                "{}",
                input.lifecycle,
                input.epoch,
                100_i64,
            ],
        )
        .expect("upsert")
    }

    fn insert_namespace_membership(
        conn: &rusqlite::Connection,
        namespace_id: &str,
        machine_id: &str,
        lifecycle: &str,
    ) {
        conn.execute(
            "INSERT INTO namespace_memberships (
                namespace_id,
                machine_id,
                role,
                lifecycle,
                epoch,
                updated_at
            ) VALUES (?1, ?2, 'member', ?3, 1, 100)",
            rusqlite::params![namespace_id, machine_id, lifecycle],
        )
        .expect("namespace membership");
    }

    fn execute_namespace_upsert(conn: &rusqlite::Connection, row: &NamespaceRow) -> usize {
        let statement = upsert_namespace_statement(row).expect("statement");
        conn.execute(
            statement.sql(),
            rusqlite::params![
                row.namespace_id.as_str(),
                row.owner_island_id.as_str(),
                row.name,
                row.lifecycle.as_str(),
                row.epoch.sql_value(),
                row.updated_at,
            ],
        )
        .expect("namespace upsert")
    }

    fn execute_namespace_membership_upsert(
        conn: &rusqlite::Connection,
        row: &NamespaceMembershipRow,
    ) -> usize {
        let statement = upsert_namespace_membership_statement(row).expect("statement");
        conn.execute(
            statement.sql(),
            rusqlite::params![
                row.namespace_id.as_str(),
                row.machine_id.as_str(),
                row.role.as_str(),
                row.lifecycle.as_str(),
                row.epoch.sql_value(),
                row.updated_at,
            ],
        )
        .expect("namespace membership upsert")
    }
}
