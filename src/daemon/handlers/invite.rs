use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use corro_api_types::Statement;
use x25519_dalek::StaticSecret;

use crate::drivers::StoreDriver;
use crate::model::InviteRecord;
use crate::network::endpoints::detect_endpoints;
use crate::node::invite::{issue_invite_token, parse_and_verify_invite_token};
use crate::store::InviteStore;
use crate::store::network::NetworkConfig;
use crate::transport::DaemonResponse;

use super::super::DaemonState;
use super::super::ssh::now_unix_secs;

impl DaemonState {
    pub(crate) async fn handle_machine_invite_create(&self, ttl_secs: u64) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => {
                return self.err(
                    "NO_RUNNING_NETWORK",
                    "machine invite create requires a running network",
                );
            }
        };

        if ttl_secs == 0 {
            return self.err("INVALID_ARGUMENT", "ttl_secs must be greater than zero");
        }

        let token = match self.do_issue_invite_token(&active.config, ttl_secs).await {
            Ok(token) => token,
            Err(err) => return self.err("INVITE_CREATE_FAILED", err),
        };

        self.ok(format!(
            "invite token created\n  network: {}\n  ttl:     {}s\n  token:   {}",
            active.config.name, ttl_secs, token
        ))
    }

    pub(crate) async fn handle_machine_invite_import(&self, token: &str) -> DaemonResponse {
        let store = match self.active.as_ref() {
            Some(a) => a.mesh.store(),
            None => {
                return self.err(
                    "NO_RUNNING_NETWORK",
                    "invite import requires a running network",
                );
            }
        };

        let invite = match parse_and_verify_invite_token(token) {
            Ok(invite) => invite,
            Err(err) => return self.err("INVALID_INVITE_TOKEN", err),
        };

        if now_unix_secs() > invite.expires_at {
            return self.err("INVITE_EXPIRED", "invite token has expired");
        }

        let record = InviteRecord {
            id: invite.invite_id.clone(),
            expires_at: invite.expires_at,
        };

        match store.create_invite(&record).await {
            Ok(()) => self.ok(format!(
                "invite imported\n  network: {}\n  invite:  {}",
                invite.network_name, record.id
            )),
            Err(crate::Error::Operation { operation, .. }) if operation == "invite_exists" => self
                .ok(format!(
                    "invite already present\n  network: {}\n  invite:  {}",
                    invite.network_name, record.id
                )),
            Err(err) => self.err(
                "INVITE_IMPORT_FAILED",
                format!("failed to import invite: {err}"),
            ),
        }
    }

    pub(crate) async fn do_issue_invite_token(
        &self,
        network: &NetworkConfig,
        ttl_secs: u64,
    ) -> Result<String, String> {
        let store = self
            .active
            .as_ref()
            .map(|a| a.mesh.store())
            .ok_or_else(|| "no running network".to_string())?;

        let endpoints = detect_endpoints(51820).await;
        let overlay_ip = Some(network.overlay_ip.0.to_string());

        let wg_secret = StaticSecret::from(self.identity.private_key.0);
        let wg_public = x25519_dalek::PublicKey::from(&wg_secret);
        let wg_public_key = Some(URL_SAFE_NO_PAD.encode(wg_public.as_bytes()));

        let (token, claims) = issue_invite_token(
            &self.identity,
            network,
            ttl_secs,
            now_unix_secs(),
            endpoints,
            overlay_ip,
            wg_public_key,
        )?;

        let record = InviteRecord {
            id: claims.invite_id,
            expires_at: claims.expires_at,
        };

        store
            .create_invite(&record)
            .await
            .map_err(|e| format!("store invite: {e}"))?;

        Ok(token)
    }

    pub(crate) async fn handle_debug_seed_invites(&self, count: u64) -> DaemonResponse {
        let store = match self.active.as_ref() {
            Some(a) => a.mesh.store(),
            None => {
                return self.err(
                    "NO_RUNNING_NETWORK",
                    "debug seed invites requires a running network",
                );
            }
        };

        let client = match &store {
            StoreDriver::Corrosion { store, .. }
            | StoreDriver::CorrosionHost { store, .. } => store.client(),
            StoreDriver::Memory { .. } => {
                return self.err("UNSUPPORTED", "debug seed invites only works with corrosion");
            }
        };

        let now = now_unix_secs();
        let expires_at = now + 86400;

        // Batch into transactions of 500
        let batch_size = 500u64;
        let mut inserted = 0u64;
        while inserted < count {
            let n = std::cmp::min(batch_size, count - inserted);
            let stmts: Vec<Statement> = (0..n)
                .map(|i| {
                    let id = format!("debug-{}-{}", now, inserted + i);
                    Statement::WithParams(
                        "INSERT INTO invites (id, expires_at) VALUES (?, ?)".to_string(),
                        vec![id.into(), (expires_at as i64).into()],
                    )
                })
                .collect();

            match client.execute(&stmts, None).await {
                Ok(_) => inserted += n,
                Err(e) => {
                    return self.err(
                        "SEED_FAILED",
                        format!("failed after {inserted} rows: {e}"),
                    );
                }
            }
        }

        self.ok(format!("seeded {inserted} invite rows"))
    }
}
