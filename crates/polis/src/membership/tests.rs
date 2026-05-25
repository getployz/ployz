use super::model::machine_row_from_store_row;
use super::*;
use crate::{Error, IrohEndpointId, StoreRow, StoreStatement, StoreValue};

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

    assert!(sql.contains("iroh_endpoint_id TEXT"));
    assert!(!sql.contains("iroh_ticket"));
}

#[test]
fn schema_creates_membership_table_only() {
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

    assert_eq!(tables, vec!["machines".to_string()]);
}

#[test]
fn schema_requires_complete_machine_rows() {
    let schema = membership_schema_statements()
        .expect("schema")
        .into_iter()
        .map(|statement| statement.sql().to_string())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(!schema.contains(" DEFAULT "));
    assert!(!schema.contains("row_kind"));
}

#[test]
fn replication_schema_allows_corrosion_fragments_without_sentinel_columns() {
    let conn = rusqlite::Connection::open_in_memory().expect("sqlite");
    conn.execute_batch(&membership_replication_schema_sql())
        .expect("replication schema");

    conn.execute(
        "INSERT INTO machines (machine_id, island_id) VALUES ('node-a', 'prod')",
        [],
    )
    .expect("partial replicated fragment");
    let schema = membership_replication_schema_sql();
    assert!(!schema.contains(" DEFAULT "));
    assert!(!schema.contains("row_kind"));
}

#[test]
fn machine_query_only_surfaces_complete_rows() {
    let query = MachineRowQuery::by_island_machine_id(
        &IslandId::parse("prod").expect("island"),
        &StoreMachineId::parse("node-a").expect("machine"),
    )
    .expect("query");

    assert!(
        query
            .statement()
            .sql()
            .contains("iroh_endpoint_id IS NOT NULL")
    );
    assert!(
        query
            .statement()
            .sql()
            .contains("wireguard_public_key IS NOT NULL")
    );
    assert!(query.statement().sql().contains("overlay_ip IS NOT NULL"));
    assert!(query.statement().sql().contains("lifecycle IS NOT NULL"));
    assert!(query.statement().sql().contains("epoch IS NOT NULL"));
}

#[test]
fn schema_rejects_rows_outside_typed_membership_invariants() {
    let conn = test_connection();

    assert!(
        conn.execute(
            "INSERT INTO machines (
                machine_id,
                island_id,
                iroh_endpoint_id,
                wireguard_public_key,
                overlay_ip,
                lifecycle,
                epoch
            ) VALUES ('', 'prod', 'endpoint-a', 'wg-a', 'fd00::1', 'active', 1)",
            [],
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "INSERT INTO machines (
                machine_id,
                island_id,
                iroh_endpoint_id,
                wireguard_public_key,
                overlay_ip,
                lifecycle,
                epoch
            ) VALUES ('node-a', 'prod', 'endpoint-a', 'wg-a', 'fd00::1', 'unknown', 1)",
            [],
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "INSERT INTO machines (
                machine_id,
                island_id,
                iroh_endpoint_id,
                wireguard_public_key,
                overlay_ip,
                lifecycle,
                epoch
            ) VALUES ('node-a', 'prod', 'endpoint-a', 'wg-a', 'fd00::1', 'active', 0)",
            [],
        )
        .is_err()
    );
    assert!(
        conn.execute("INSERT INTO machines (machine_id) VALUES ('node-a')", [])
            .is_err()
    );
}

#[test]
fn machine_upsert_is_parameterized() {
    let statement = upsert_machine_statement(&machine_row()).expect("statement");

    assert!(statement.sql().contains("INSERT INTO machines"));
    assert!(
        statement
            .sql()
            .contains("ON CONFLICT(island_id, machine_id)")
    );
    assert!(!statement.sql().contains("node-a"));
}

#[test]
fn machine_upsert_accepts_same_owner_row() {
    let conn = test_connection();
    execute_machine_upsert(&conn, "node-a", "prod", "endpoint-a", "wg-a", "fd00::1", 2);

    let changed =
        execute_machine_upsert(&conn, "node-a", "prod", "endpoint-a", "wg-a", "fd00::1", 2);
    let endpoint: String = conn
        .query_row(
            "SELECT iroh_endpoint_id FROM machines WHERE machine_id = 'node-a'",
            [],
            |row| row.get(0),
        )
        .expect("endpoint");

    assert_eq!(changed, 1);
    assert_eq!(endpoint, "endpoint-a");
}

#[test]
fn machine_upsert_keeps_same_machine_id_separate_by_island() {
    let conn = test_connection();
    execute_machine_upsert(&conn, "node-a", "prod", "endpoint-a", "wg-a", "fd00::1", 2);
    execute_machine_upsert(
        &conn,
        "node-a",
        "staging",
        "endpoint-b",
        "wg-b",
        "fd00::2",
        3,
    );

    let rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM machines WHERE machine_id = 'node-a'",
            [],
            |row| row.get(0),
        )
        .expect("row count");
    let staging_endpoint: String = conn
        .query_row(
            "SELECT iroh_endpoint_id FROM machines
             WHERE island_id = 'staging' AND machine_id = 'node-a'",
            [],
            |row| row.get(0),
        )
        .expect("staging endpoint");

    assert_eq!(rows, 2);
    assert_eq!(staging_endpoint, "endpoint-b");
}

#[test]
fn machine_upsert_rejects_different_owner_row() {
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
fn machine_row_decodes_from_store_row_shape() {
    let row = StoreRow::from_fields([
        ("machine_id", StoreValue::Text("node-a".to_string())),
        ("island_id", StoreValue::Text("prod".to_string())),
        (
            "iroh_endpoint_id",
            StoreValue::Text("endpoint-node-a".to_string()),
        ),
        (
            "wireguard_public_key",
            StoreValue::Text("wg-node-a".to_string()),
        ),
        ("overlay_ip", StoreValue::Text("fd00::1".to_string())),
        ("lifecycle", StoreValue::Text("active".to_string())),
        ("epoch", StoreValue::Integer(7)),
    ])
    .expect("store row");

    let decoded = machine_row_from_store_row(&row).expect("row");

    assert_eq!(decoded.machine_id().as_str(), "node-a");
    assert_eq!(decoded.island_id().as_str(), "prod");
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
        MembershipLifecycle::Active,
        RowEpoch::new(1).expect("epoch"),
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
        MembershipLifecycle::parse(input.lifecycle).expect("lifecycle"),
        RowEpoch::new(input.epoch as u64).expect("epoch"),
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
            input.lifecycle,
            input.epoch,
        ],
    )
    .expect("upsert")
}
