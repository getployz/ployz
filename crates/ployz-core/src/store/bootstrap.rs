use std::net::Ipv6Addr;
use std::path::Path;

use crate::corrosion_config;
use crate::model::{MachineId, MachineRecord, MachineStatus, OverlayIp, Participation, PublicKey};
use crate::network::endpoints::detect_endpoints;
use crate::node::identity::Identity;
use crate::node::invite::InviteClaims;
use crate::store::network::NetworkConfig;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

const BOOTSTRAP_PEERS_FILE: &str = "bootstrap-peers.json";

pub struct BootstrapInfo {
    pub peer_id: String,
    pub peer_wg_public_key: [u8; 32],
    pub peer_overlay_ip: Ipv6Addr,
    pub peer_endpoints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapPeerRecord {
    pub machine_id: MachineId,
    pub public_key: PublicKey,
    pub overlay_ip: OverlayIp,
    pub endpoints: Vec<String>,
}

impl BootstrapPeerRecord {
    #[must_use]
    pub fn into_machine_record(self) -> MachineRecord {
        MachineRecord {
            id: self.machine_id,
            public_key: self.public_key,
            overlay_ip: self.overlay_ip,
            subnet: None,
            bridge_ip: None,
            endpoints: self.endpoints,
            status: MachineStatus::Unknown,
            participation: Participation::Disabled,
            last_heartbeat: 0,
            created_at: 0,
            updated_at: 0,
            labels: std::collections::BTreeMap::new(),
        }
    }

    pub fn from_invite(invite: &InviteClaims) -> Option<Self> {
        let Some(overlay_str) = invite.issuer_overlay_ip.as_deref() else {
            return None;
        };
        let Some(public_key_b64) = invite.issuer_wg_public_key.as_deref() else {
            return None;
        };
        if invite.issuer_endpoints.is_empty() {
            return None;
        }

        let key_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(public_key_b64)
            .ok()?;
        let public_key: [u8; 32] = key_bytes.as_slice().try_into().ok()?;
        let overlay_ip = overlay_str.parse().ok()?;

        Some(Self {
            machine_id: MachineId(invite.issued_by.clone()),
            public_key: PublicKey(public_key),
            overlay_ip: OverlayIp(overlay_ip),
            endpoints: invite.issuer_endpoints.clone(),
        })
    }
}

#[must_use]
pub fn bootstrap_peers_path(network_dir: &Path) -> std::path::PathBuf {
    network_dir.join(BOOTSTRAP_PEERS_FILE)
}

pub fn load_bootstrap_peer_records(network_dir: &Path) -> Result<Vec<BootstrapPeerRecord>, String> {
    let path = bootstrap_peers_path(network_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }

    let data = std::fs::read_to_string(&path)
        .map_err(|error| format!("read bootstrap peers '{}': {error}", path.display()))?;
    serde_json::from_str(&data)
        .map_err(|error| format!("parse bootstrap peers '{}': {error}", path.display()))
}

pub fn write_bootstrap_peer_record(
    network_dir: &Path,
    peer: &BootstrapPeerRecord,
) -> Result<(), String> {
    let path = bootstrap_peers_path(network_dir);
    std::fs::create_dir_all(network_dir).map_err(|error| {
        format!(
            "create bootstrap peer dir '{}': {error}",
            network_dir.display()
        )
    })?;
    let mut peers = load_bootstrap_peer_records(network_dir)?;
    if let Some(existing) = peers
        .iter_mut()
        .find(|existing| existing.machine_id == peer.machine_id)
    {
        *existing = peer.clone();
    } else {
        peers.push(peer.clone());
    }
    peers.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));

    let body = serde_json::to_string_pretty(&peers)
        .map_err(|error| format!("encode bootstrap peers '{}': {error}", path.display()))?;
    std::fs::write(&path, body)
        .map_err(|error| format!("write bootstrap peers '{}': {error}", path.display()))
}

/// Read peer config from corrosion's sqlite DB (bypassing the API).
/// Only fetches the columns needed for WG peer setup — avoids breaking on
/// schema migrations (new columns won't exist until corrosion starts).
pub(crate) fn peer_records_from_db(network_dir: &Path) -> Result<Vec<MachineRecord>, String> {
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
        let endpoints_parsed: Vec<String> = serde_json::from_str(&endpoints).unwrap_or_default();

        records.push(MachineRecord {
            id: MachineId(id),
            public_key: PublicKey(key),
            overlay_ip: OverlayIp(overlay),
            subnet: subnet_parsed,
            bridge_ip: bridge_parsed,
            endpoints: endpoints_parsed,
            status: MachineStatus::Unknown,
            participation: Participation::Disabled,
            last_heartbeat: 0,
            created_at: 0,
            updated_at: 0,
            labels: std::collections::BTreeMap::new(),
        });
    }

    Ok(records)
}

pub(crate) fn corrosion_bootstrap_from_db(
    network_dir: &Path,
    local_machine_id: &MachineId,
) -> Result<Vec<String>, String> {
    let records = peer_records_from_db(network_dir)?;
    Ok(records
        .into_iter()
        .filter(|m| m.id.0 != local_machine_id.0)
        .map(|m| {
            format!(
                "[{}]:{}",
                m.overlay_ip.0,
                corrosion_config::DEFAULT_GOSSIP_PORT
            )
        })
        .collect())
}

pub fn resolve_bootstrap_addrs(
    network_dir: &Path,
    machine_id: &MachineId,
    bootstrap: &Option<BootstrapInfo>,
) -> Result<Vec<String>, String> {
    Ok(bootstrap
        .as_ref()
        .map(|bs| {
            vec![format!(
                "[{}]:{}",
                bs.peer_overlay_ip,
                corrosion_config::DEFAULT_GOSSIP_PORT
            )]
        })
        .unwrap_or(corrosion_bootstrap_from_db(network_dir, machine_id)?))
}

pub async fn build_seed_records(
    network_dir: &Path,
    identity: &Identity,
    net_config: &NetworkConfig,
    bootstrap: Option<&BootstrapInfo>,
    listen_port: u16,
) -> Vec<MachineRecord> {
    let mut seed_records: Vec<MachineRecord> = load_bootstrap_peer_records(network_dir)
        .unwrap_or_else(|error| {
            tracing::warn!(
                ?error,
                "failed to load local bootstrap peers, starting fresh"
            );
            Vec::new()
        })
        .into_iter()
        .map(BootstrapPeerRecord::into_machine_record)
        .collect();

    let db_records = peer_records_from_db(network_dir).unwrap_or_else(|e| {
        tracing::warn!(?e, "failed to pre-load machines from db, starting fresh");
        Vec::new()
    });
    for record in db_records {
        if let Some(existing) = seed_records
            .iter_mut()
            .find(|machine| machine.id == record.id)
        {
            *existing = record;
        } else {
            seed_records.push(record);
        }
    }

    if let Some(bs) = bootstrap {
        let bootstrap_record = MachineRecord {
            id: MachineId(bs.peer_id.clone()),
            public_key: PublicKey(bs.peer_wg_public_key),
            overlay_ip: OverlayIp(bs.peer_overlay_ip),
            subnet: None,
            bridge_ip: None,
            endpoints: bs.peer_endpoints.clone(),
            status: MachineStatus::Unknown,
            participation: Participation::Disabled,
            last_heartbeat: 0,
            created_at: 0,
            updated_at: 0,
            labels: std::collections::BTreeMap::new(),
        };
        if !seed_records
            .iter()
            .any(|machine| machine.id == bootstrap_record.id)
        {
            seed_records.push(bootstrap_record);
        }
    }

    let endpoints = detect_endpoints(listen_port).await;
    let self_record = MachineRecord {
        id: identity.machine_id.clone(),
        public_key: identity.public_key.clone(),
        overlay_ip: net_config.overlay_ip,
        subnet: Some(net_config.subnet),
        bridge_ip: None,
        endpoints,
        status: MachineStatus::Unknown,
        participation: Participation::Disabled,
        last_heartbeat: 0,
        created_at: 0,
        updated_at: 0,
        labels: std::collections::BTreeMap::new(),
    };
    if let Some(existing) = seed_records.iter_mut().find(|m| m.id == self_record.id) {
        *existing = self_record;
    } else {
        seed_records.push(self_record);
    }

    seed_records
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NetworkName;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    fn temp_network_dir(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ployz-bootstrap-{name}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&root).expect("create temp bootstrap dir");
        root
    }

    fn sample_invite() -> InviteClaims {
        InviteClaims {
            invite_id: "invite".into(),
            network_id: crate::model::NetworkId("network".into()),
            network_name: "alpha".into(),
            issued_by: "founder".into(),
            issuer_verify_key: "verify".into(),
            expires_at: 1,
            nonce: "nonce".into(),
            issuer_endpoints: vec!["10.0.0.1:51820".into()],
            issuer_overlay_ip: Some("fd00::1".into()),
            issuer_wg_public_key: Some(URL_SAFE_NO_PAD.encode([7u8; 32])),
            issuer_subnet: Some("10.210.0.0/24".into()),
            allocated_subnet: "10.210.1.0/24".into(),
        }
    }

    #[test]
    fn bootstrap_peer_roundtrip_from_invite() {
        let network_dir = temp_network_dir("roundtrip");
        let invite = sample_invite();
        let peer = BootstrapPeerRecord::from_invite(&invite).expect("bootstrap peer");

        write_bootstrap_peer_record(&network_dir, &peer).expect("persist bootstrap peer");
        let loaded = load_bootstrap_peer_records(&network_dir).expect("load bootstrap peers");

        assert_eq!(loaded, vec![peer]);
        let _ = std::fs::remove_dir_all(&network_dir);
    }

    #[tokio::test]
    async fn build_seed_records_prefers_db_over_bootstrap_and_self() {
        let network_dir = temp_network_dir("merge");
        let identity = Identity::generate(MachineId("joiner".into()), [3; 32]);
        let net_config = NetworkConfig::new(
            NetworkName("alpha".into()),
            &identity.public_key,
            "10.210.0.0/16",
            "10.210.1.0/24".parse().expect("valid subnet"),
        );
        net_config
            .save(&NetworkConfig::path(&network_dir, "alpha"))
            .expect("save network config");

        write_bootstrap_peer_record(
            &network_dir,
            &BootstrapPeerRecord {
                machine_id: MachineId("founder".into()),
                public_key: PublicKey([1; 32]),
                overlay_ip: OverlayIp("fd00::1".parse().expect("valid overlay")),
                endpoints: vec!["bootstrap:51820".into()],
            },
        )
        .expect("persist bootstrap founder");

        let db_path = corrosion_config::Paths::new(&network_dir).db;
        let parent = db_path.parent().expect("db parent");
        std::fs::create_dir_all(parent).expect("create db parent");
        let conn = rusqlite::Connection::open(&db_path).expect("open db");
        conn.execute(
            "CREATE TABLE machines (
                id TEXT PRIMARY KEY,
                public_key BLOB NOT NULL,
                overlay_ip TEXT NOT NULL,
                subnet TEXT NOT NULL,
                bridge_ip TEXT NOT NULL,
                endpoints TEXT NOT NULL
            )",
            [],
        )
        .expect("create machines table");
        conn.execute(
            "INSERT INTO machines (id, public_key, overlay_ip, subnet, bridge_ip, endpoints)
             VALUES (?1, ?2, ?3, '', '', ?4)",
            rusqlite::params![
                "founder",
                vec![2u8; 32],
                "fd00::2",
                serde_json::to_string(&vec!["db:51820"]).expect("encode endpoints"),
            ],
        )
        .expect("insert founder db record");

        let seed_records =
            build_seed_records(&network_dir, &identity, &net_config, None, 51820).await;

        let founder = seed_records
            .iter()
            .find(|record| record.id.0 == "founder")
            .expect("founder record present");
        assert_eq!(founder.public_key, PublicKey([2; 32]));
        assert_eq!(founder.endpoints, vec!["db:51820".to_string()]);

        let joiner = seed_records
            .iter()
            .find(|record| record.id == identity.machine_id)
            .expect("self record present");
        assert_eq!(joiner.subnet, Some(net_config.subnet));

        let _ = std::fs::remove_dir_all(&network_dir);
    }
}
