use crate::mesh_state::invite::{
    issue_invite_token, parse_and_verify_invite_token, peer_hint_from_record,
};
use crate::mesh_state::network::NetworkConfig;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use ployz_orchestrator::network::endpoints::detect_advertised_endpoints;
use ployz_store_api::{InviteStore, MachineStore};
use ployz_types::model::InviteRecord;
use ployz_types::time::now_unix_secs;
use x25519_dalek::StaticSecret;

use ployz_api::{DaemonPayload, DaemonResponse, MachineInviteInfo, MachineInviteListPayload};

use super::super::DaemonState;

impl DaemonState {
    pub(crate) async fn handle_machine_invite_create(&self, ttl_secs: u64) -> DaemonResponse {
        let active_config = match self.active.as_ref() {
            Some(active) => active.config.clone(),
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

        let (token, invite) = match self.do_issue_invite_token(&active_config, ttl_secs).await {
            Ok(value) => value,
            Err(err) => return self.err("INVITE_CREATE_FAILED", err),
        };

        self.ok(format!(
            "invite token created\n  network: {}\n  invite:  {}\n  ttl:     {}s\n  token:   {}",
            active_config.name, invite.invite_id, ttl_secs, token
        ))
    }

    pub(crate) async fn handle_machine_invite_revoke(&self, invite_id: &str) -> DaemonResponse {
        let Some(active) = self.active.as_ref() else {
            return self.err(
                "NO_RUNNING_NETWORK",
                "machine invite revoke requires a running network",
            );
        };

        match active
            .mesh
            .store
            .revoke_invite(invite_id, now_unix_secs())
            .await
        {
            Ok(_) => self.ok(format!("invite revoked\n  invite:  {invite_id}")),
            Err(err) => self.err(
                "INVITE_REVOKE_FAILED",
                format!("failed to revoke invite: {err}"),
            ),
        }
    }

    pub(crate) async fn handle_machine_invite_list(&self) -> DaemonResponse {
        let Some(active) = self.active.as_ref() else {
            return self.err(
                "NO_RUNNING_NETWORK",
                "machine invite list requires a running network",
            );
        };

        let invites = match active.mesh.store.list_invites().await {
            Ok(invites) => invites,
            Err(err) => {
                return self.err(
                    "INVITE_LIST_FAILED",
                    format!("failed to list invites: {err}"),
                );
            }
        };

        let now = now_unix_secs();
        let payload = MachineInviteListPayload {
            invites: invites
                .iter()
                .map(|invite| MachineInviteInfo {
                    invite_id: invite.invite_id.clone(),
                    expires_at: invite.expires_at,
                    status: invite_status(invite, now).to_string(),
                    consumed_by: invite
                        .consumed_by
                        .as_ref()
                        .map(|machine_id| machine_id.0.clone()),
                })
                .collect(),
        };

        let message = if payload.invites.is_empty() {
            String::from("no invites")
        } else {
            payload
                .invites
                .iter()
                .map(|invite| format!("{}  {}", invite.invite_id, invite.status))
                .collect::<Vec<_>>()
                .join("\n")
        };

        self.ok_with_payload(message, Some(DaemonPayload::MachineInviteList(payload)))
    }

    pub(crate) async fn handle_machine_invite_import(&self, token: &str) -> DaemonResponse {
        let invite = match parse_and_verify_invite_token(token) {
            Ok(invite) => invite,
            Err(err) => return self.err("INVALID_INVITE_TOKEN", err),
        };
        self.err(
            "UNSUPPORTED",
            format!(
                "invite import is no longer supported\n  invite:  {}\n  network: {}",
                invite.invite_id, invite.network_name
            ),
        )
    }

    pub(crate) async fn do_issue_invite_token(
        &self,
        network: &NetworkConfig,
        ttl_secs: u64,
    ) -> Result<(String, InviteRecord), String> {
        let mesh = self
            .active
            .as_ref()
            .map(|a| &a.mesh)
            .ok_or_else(|| "no running network".to_string())?;

        let now = now_unix_secs();
        let expires_at = now
            .checked_add(ttl_secs)
            .ok_or_else(|| "ttl overflow".to_string())?;
        let invite_id = random_hex_id();

        let endpoints = detect_advertised_endpoints(51820).await;
        let overlay_ip = Some(network.overlay_ip.0.to_string());

        let wg_secret = StaticSecret::from(self.identity.private_key.0);
        let wg_public = x25519_dalek::PublicKey::from(&wg_secret);
        let wg_public_key = Some(URL_SAFE_NO_PAD.encode(wg_public.as_bytes()));

        let machines = mesh
            .store
            .list_machines()
            .await
            .map_err(|err| format!("list machines for invite hints: {err}"))?;
        let bootstrap_peers = machines
            .iter()
            .filter(|machine| machine.id != self.identity.machine_id)
            .filter_map(peer_hint_from_record)
            .collect::<Vec<_>>();

        let mut invite = InviteRecord {
            invite_id: invite_id.clone(),
            network_id: network.id.clone(),
            issuer_machine_id: self.identity.machine_id.clone(),
            issuer_verify_key: issuer_verify_key(&self.identity.private_key.0),
            expires_at,
            consumed_by: None,
            consumed_at: None,
            revoked_at: None,
            signature: String::new(),
        };
        invite.signature = sign_invite_record(&self.identity.private_key.0, &invite)?;

        mesh.store
            .create_invite(&invite)
            .await
            .map_err(|err| format!("store invite: {err}"))?;

        let (token, _) = issue_invite_token(
            &self.identity,
            network,
            invite_id,
            ttl_secs,
            now,
            endpoints,
            overlay_ip,
            wg_public_key,
            bootstrap_peers,
        )?;

        Ok((token, invite))
    }
}

fn invite_status(invite: &InviteRecord, now: u64) -> &'static str {
    if invite.revoked_at.is_some() {
        return "revoked";
    }
    if invite.consumed_by.is_some() {
        return "consumed";
    }
    if invite.expires_at <= now {
        return "expired";
    }
    "active"
}

fn random_hex_id() -> String {
    let mut bytes = [0u8; 16];
    rand::fill(&mut bytes);
    let mut value = String::with_capacity(32);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut value, "{byte:02x}");
    }
    value
}

fn issuer_verify_key(private_key: &[u8; 32]) -> String {
    let signing_key = SigningKey::from_bytes(private_key);
    URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes())
}

fn sign_invite_record(private_key: &[u8; 32], invite: &InviteRecord) -> Result<String, String> {
    let mut unsigned = invite.clone();
    unsigned.signature.clear();
    let payload =
        serde_json::to_vec(&unsigned).map_err(|error| format!("encode invite record: {error}"))?;
    let signing_key = SigningKey::from_bytes(private_key);
    Ok(URL_SAFE_NO_PAD.encode(signing_key.sign(&payload).to_bytes()))
}
