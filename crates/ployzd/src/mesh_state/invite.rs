use super::network::NetworkConfig;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use ployz_model::{MachineId, NetworkId, PublicKey};
use ployz_runtime_api::Identity;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerHint {
    pub machine_id: MachineId,
    pub endpoints: Vec<String>,
    pub wg_public_key: String,
    pub overlay_ip: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteToken {
    pub invite_id: String,
    pub network_id: NetworkId,
    pub network_name: String,
    pub issuer_machine_id: MachineId,
    pub issuer_verify_key: String,
    pub expires_at: u64,
    #[serde(default)]
    pub issuer_endpoints: Vec<String>,
    #[serde(default)]
    pub issuer_overlay_ip: Option<String>,
    #[serde(default)]
    pub issuer_wg_public_key: Option<String>,
    #[serde(default)]
    pub bootstrap_peers: Vec<PeerHint>,
}

#[allow(clippy::too_many_arguments)]
pub fn issue_invite_token(
    identity: &Identity,
    network: &NetworkConfig,
    invite_id: String,
    ttl_secs: u64,
    now_unix_secs: u64,
    issuer_endpoints: Vec<String>,
    issuer_overlay_ip: Option<String>,
    issuer_wg_public_key: Option<String>,
    bootstrap_peers: Vec<PeerHint>,
) -> Result<(String, InviteToken), String> {
    let expires_at = now_unix_secs
        .checked_add(ttl_secs)
        .ok_or_else(|| "ttl overflow".to_string())?;

    let signing_key = SigningKey::from_bytes(&identity.private_key.0);
    let issuer_verify_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

    let token = InviteToken {
        invite_id,
        network_id: network.id.clone(),
        network_name: network.name.0.clone(),
        issuer_machine_id: identity.machine_id.clone(),
        issuer_verify_key,
        expires_at,
        issuer_endpoints,
        issuer_overlay_ip,
        issuer_wg_public_key,
        bootstrap_peers,
    };

    let token_json =
        serde_json::to_vec(&token).map_err(|error| format!("encode invite: {error}"))?;
    let signature = signing_key.sign(&token_json);
    let token_encoded = URL_SAFE_NO_PAD.encode(&token_json);
    let sig_encoded = URL_SAFE_NO_PAD.encode(signature.to_bytes());

    Ok((format!("{token_encoded}.{sig_encoded}"), token))
}

pub fn parse_and_verify_invite_token(encoded: &str) -> Result<InviteToken, String> {
    let (token_b64, sig_b64) = encoded
        .split_once('.')
        .ok_or_else(|| "invalid invite token format".to_string())?;

    let token_json = URL_SAFE_NO_PAD
        .decode(token_b64)
        .map_err(|error| format!("decode invite token: {error}"))?;
    let token: InviteToken = serde_json::from_slice(&token_json)
        .map_err(|error| format!("parse invite token: {error}"))?;

    let sig_bytes = URL_SAFE_NO_PAD
        .decode(sig_b64)
        .map_err(|error| format!("decode invite signature: {error}"))?;
    let sig_arr: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| "invalid invite signature length".to_string())?;
    let signature = Signature::from_bytes(&sig_arr);

    let key_bytes = URL_SAFE_NO_PAD
        .decode(&token.issuer_verify_key)
        .map_err(|error| format!("decode issuer verify key: {error}"))?;
    let key_arr: [u8; 32] = key_bytes
        .as_slice()
        .try_into()
        .map_err(|_| "invalid issuer verify key length".to_string())?;
    let verify_key = VerifyingKey::from_bytes(&key_arr)
        .map_err(|error| format!("parse issuer verify key: {error}"))?;

    verify_key
        .verify(&token_json, &signature)
        .map_err(|error| format!("verify invite signature: {error}"))?;

    Ok(token)
}

#[must_use]
pub fn peer_hint_from_record(record: &ployz_model::MachineMembership) -> Option<PeerHint> {
    if record.endpoints.is_empty() {
        return None;
    }
    Some(PeerHint {
        machine_id: record.id.clone(),
        endpoints: record.endpoints.clone(),
        wg_public_key: URL_SAFE_NO_PAD.encode(PublicKey(record.public_key.0).0),
        overlay_ip: record.overlay_ip.0.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_model::{MachineId, NetworkName};
    use ployz_runtime_api::Identity;

    #[test]
    fn invite_roundtrip_preserves_bootstrap_hints() {
        let identity = Identity::generate(MachineId::new("founder"), [7; 32]);
        let subnet = "10.210.1.0/24".parse().expect("valid subnet");
        let network = NetworkConfig::new(
            NetworkName("alpha".into()),
            &identity.public_key,
            "10.210.0.0/16",
            subnet,
        );

        let (token, claims) = issue_invite_token(
            &identity,
            &network,
            "invite-1".into(),
            600,
            1_700_000_000,
            vec!["1.2.3.4:51820".into()],
            Some(network.overlay_ip.0.to_string()),
            Some("wg-public".into()),
            vec![PeerHint {
                machine_id: MachineId::new("peer-1"),
                endpoints: vec!["5.6.7.8:51820".into()],
                wg_public_key: "wg-peer".into(),
                overlay_ip: "fd00::2".into(),
            }],
        )
        .expect("issue invite");

        assert_eq!(claims.bootstrap_peers.len(), 1);

        let parsed = parse_and_verify_invite_token(&token).expect("parse invite");
        assert_eq!(parsed.bootstrap_peers.len(), 1);
    }
}
