use std::path::Path;

use ployz_types::model::{MachineId, MachineRecord, OverlayIp, PublicKey};

use crate::config as corrosion_config;

/// Read peer config from corrosion's sqlite DB (bypassing the API).
/// Only fetches the columns needed for WG peer setup.
pub fn peer_records_from_db(network_dir: &Path) -> Result<Vec<MachineRecord>, String> {
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

pub fn corrosion_bootstrap_from_db(
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
