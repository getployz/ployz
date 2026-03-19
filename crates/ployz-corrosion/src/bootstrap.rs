use async_trait::async_trait;
use std::path::{Path, PathBuf};

use ployz_config::corrosion as corrosion_config;
use ployz_store_api::BootstrapStateReader;
use ployz_types::error::Error;
use ployz_types::model::{MachineId, MachineRecord, OverlayIp, PublicKey};

#[derive(Debug, Clone)]
pub struct CorrosionBootstrapState {
    network_dir: PathBuf,
}

impl CorrosionBootstrapState {
    #[must_use]
    pub fn new(network_dir: impl Into<PathBuf>) -> Self {
        Self {
            network_dir: network_dir.into(),
        }
    }
}

/// Read peer config from corrosion's sqlite DB (bypassing the API).
/// Only fetches the columns needed for WG peer setup.
fn peer_records_from_db(network_dir: &Path) -> Result<Vec<MachineRecord>, String> {
    let db_path = corrosion_config::Paths::new(network_dir).db;
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    let conn = rusqlite::Connection::open(&db_path)
        .map_err(|e| format!("open corrosion db '{}': {e}", db_path.display()))?;
    let mut stmt = match conn.prepare(
        "SELECT id, public_key, overlay_ip, subnet, bridge_ip, endpoints FROM machines ORDER BY id",
    ) {
        Ok(stmt) => stmt,
        Err(rusqlite::Error::SqliteFailure(_, Some(message)))
            if message.contains("no such table: machines") =>
        {
            return Ok(Vec::new());
        }
        Err(e) => {
            return Err(format!(
                "prepare peer_records_from_db query '{}': {e}",
                db_path.display()
            ));
        }
    };

    let rows = stmt
        .query_map([], |row| {
            let id: String = row.get("id")?;
            let public_key: Vec<u8> = row.get("public_key")?;
            let overlay_ip: String = row.get("overlay_ip")?;
            let subnet: String = row.get("subnet")?;
            let bridge_ip: String = row.get("bridge_ip")?;
            let endpoints: String = row.get("endpoints")?;
            Ok((id, public_key, overlay_ip, subnet, bridge_ip, endpoints))
        })
        .map_err(|e| format!("query peer_records_from_db '{}': {e}", db_path.display()))?;

    let mut records = Vec::new();
    for row in rows {
        let (id, public_key, overlay_ip, subnet, bridge_ip, endpoints) =
            row.map_err(|e| format!("read machine row from '{}': {e}", db_path.display()))?;

        if overlay_ip.is_empty() {
            continue;
        }

        let key: [u8; 32] = match public_key.try_into() {
            Ok(k) => k,
            Err(_) => continue,
        };
        let overlay: std::net::Ipv6Addr = match overlay_ip.parse() {
            Ok(ip) => ip,
            Err(_) => continue,
        };
        let subnet_parsed: Option<ipnet::Ipv4Net> = if subnet.is_empty() {
            None
        } else {
            subnet.parse().ok()
        };
        let bridge_parsed: Option<OverlayIp> = if bridge_ip.is_empty() {
            None
        } else {
            bridge_ip.parse::<std::net::Ipv6Addr>().ok().map(OverlayIp)
        };
        let endpoints_parsed: Vec<String> =
            serde_json::from_str(&endpoints).unwrap_or_else(|error| {
                tracing::warn!(%id, ?error, "malformed endpoints JSON in db, treating as empty");
                Vec::new()
            });

        let mut record = MachineRecord::seed(
            MachineId(id),
            PublicKey(key),
            OverlayIp(overlay),
            subnet_parsed,
            endpoints_parsed,
        );
        record.bridge_ip = bridge_parsed;
        records.push(record);
    }

    Ok(records)
}

fn corrosion_bootstrap_from_db(
    network_dir: &Path,
    local_machine_id: &MachineId,
) -> Result<Vec<String>, String> {
    let records = peer_records_from_db(network_dir)?;
    Ok(records
        .into_iter()
        .filter(|machine| machine.id.0 != local_machine_id.0)
        .map(|machine| {
            format!(
                "[{}]:{}",
                machine.overlay_ip.0,
                corrosion_config::DEFAULT_GOSSIP_PORT
            )
        })
        .collect())
}

#[async_trait]
impl BootstrapStateReader for CorrosionBootstrapState {
    async fn seed_machine_records(&self) -> ployz_types::Result<Vec<MachineRecord>> {
        peer_records_from_db(&self.network_dir)
            .map_err(|error| Error::operation("seed_machine_records", error))
    }

    async fn bootstrap_addrs(
        &self,
        local_machine_id: &MachineId,
    ) -> ployz_types::Result<Vec<String>> {
        corrosion_bootstrap_from_db(&self.network_dir, local_machine_id)
            .map_err(|error| Error::operation("bootstrap_addrs", error))
    }
}

#[cfg(test)]
mod tests {
    use super::CorrosionBootstrapState;
    use ployz_config::corrosion as corrosion_config;
    use ployz_store_api::BootstrapStateReader;
    use ployz_types::model::MachineId;
    use std::fs;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_network_dir(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("ployz-corrosion-bootstrap-{label}-{nanos}"))
    }

    fn write_db(
        network_dir: &Path,
        schema_sql: &str,
        rows_sql: &[&str],
    ) -> rusqlite::Result<()> {
        fs::create_dir_all(network_dir).expect("create network dir");
        let db_path = corrosion_config::Paths::new(network_dir).db;
        let db_dir = db_path.parent().expect("db path parent");
        fs::create_dir_all(db_dir).expect("create db dir");
        let conn = rusqlite::Connection::open(db_path)?;
        conn.execute_batch(schema_sql)?;
        for sql in rows_sql {
            conn.execute_batch(sql)?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn missing_db_returns_empty() {
        let network_dir = temp_network_dir("missing-db");
        let reader = CorrosionBootstrapState::new(&network_dir);

        assert!(reader.seed_machine_records().await.expect("seed records").is_empty());
        assert!(reader
            .bootstrap_addrs(&MachineId("self".into()))
            .await
            .expect("bootstrap addrs")
            .is_empty());
    }

    #[tokio::test]
    async fn missing_machines_table_returns_empty() {
        let network_dir = temp_network_dir("missing-table");
        write_db(&network_dir, "CREATE TABLE unrelated (id INTEGER);", &[])
            .expect("write sqlite db");
        let reader = CorrosionBootstrapState::new(&network_dir);

        assert!(reader.seed_machine_records().await.expect("seed records").is_empty());
        assert!(reader
            .bootstrap_addrs(&MachineId("self".into()))
            .await
            .expect("bootstrap addrs")
            .is_empty());
    }

    #[tokio::test]
    async fn malformed_rows_are_skipped() {
        let network_dir = temp_network_dir("malformed");
        write_db(
            &network_dir,
            "CREATE TABLE machines (
                id TEXT NOT NULL,
                public_key BLOB NOT NULL,
                overlay_ip TEXT NOT NULL,
                subnet TEXT NOT NULL,
                bridge_ip TEXT NOT NULL,
                endpoints TEXT NOT NULL
            );",
            &[
                "INSERT INTO machines VALUES (
                    'bad-key',
                    x'0102',
                    'fd00::10',
                    '',
                    '',
                    '[]'
                );",
                "INSERT INTO machines VALUES (
                    'bad-ip',
                    x'0000000000000000000000000000000000000000000000000000000000000000',
                    'not-an-ip',
                    '',
                    '',
                    '[]'
                );",
            ],
        )
        .expect("write sqlite db");
        let reader = CorrosionBootstrapState::new(&network_dir);

        assert!(reader.seed_machine_records().await.expect("seed records").is_empty());
    }

    #[tokio::test]
    async fn local_machine_is_excluded_from_bootstrap_addresses() {
        let network_dir = temp_network_dir("exclude-local");
        write_db(
            &network_dir,
            "CREATE TABLE machines (
                id TEXT NOT NULL,
                public_key BLOB NOT NULL,
                overlay_ip TEXT NOT NULL,
                subnet TEXT NOT NULL,
                bridge_ip TEXT NOT NULL,
                endpoints TEXT NOT NULL
            );",
            &[
                "INSERT INTO machines VALUES (
                    'self',
                    x'0000000000000000000000000000000000000000000000000000000000000001',
                    'fd00::1',
                    '',
                    '',
                    '[]'
                );",
                "INSERT INTO machines VALUES (
                    'peer',
                    x'0000000000000000000000000000000000000000000000000000000000000002',
                    'fd00::2',
                    '',
                    '',
                    '[]'
                );",
            ],
        )
        .expect("write sqlite db");
        let reader = CorrosionBootstrapState::new(&network_dir);

        let addrs = reader
            .bootstrap_addrs(&MachineId("self".into()))
            .await
            .expect("bootstrap addrs");
        assert_eq!(
            addrs,
            vec![format!(
                "[fd00::2]:{}",
                corrosion_config::DEFAULT_GOSSIP_PORT
            )]
        );
    }
}
