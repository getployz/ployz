use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::model::NetworkId;
use crate::node::identity::Identity;
use crate::store::network::NetworkConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteClaims {
    pub invite_id: String,
    pub network_id: NetworkId,
    pub network_name: String,
    pub issued_by: String,
    pub issuer_verify_key: String,
    pub expires_at: u64,
    pub nonce: String,
    #[serde(default)]
    pub issuer_endpoints: Vec<String>,
    #[serde(default)]
    pub issuer_overlay_ip: Option<String>,
    #[serde(default)]
    pub issuer_wg_public_key: Option<String>,
    /// The issuer's IPv4 subnet so the joiner can avoid allocating it.
    #[serde(default)]
    pub issuer_subnet: Option<String>,
    pub allocated_subnet: String,
}

#[allow(clippy::too_many_arguments)]
pub fn issue_invite_token(
    identity: &Identity,
    network: &NetworkConfig,
    ttl_secs: u64,
    now_unix_secs: u64,
    issuer_endpoints: Vec<String>,
    issuer_overlay_ip: Option<String>,
    issuer_wg_public_key: Option<String>,
    issuer_subnet: Option<String>,
    allocated_subnet: String,
) -> Result<(String, InviteClaims), String> {
    let expires_at = now_unix_secs
        .checked_add(ttl_secs)
        .ok_or_else(|| "ttl overflow".to_string())?;

    let mut nonce_bytes = [0u8; 16];
    rand::fill(&mut nonce_bytes);
    let mut nonce = String::with_capacity(32);
    for b in nonce_bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut nonce, "{b:02x}");
    }

    let mut invite_id_bytes = [0u8; 16];
    rand::fill(&mut invite_id_bytes);
    let mut invite_id = String::with_capacity(32);
    for b in invite_id_bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut invite_id, "{b:02x}");
    }

    let signing_key = SigningKey::from_bytes(&identity.private_key.0);
    let issuer_verify_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

    let claims = InviteClaims {
        invite_id: invite_id.clone(),
        network_id: network.id.clone(),
        network_name: network.name.0.clone(),
        issued_by: identity.machine_id.0.clone(),
        issuer_verify_key,
        expires_at,
        nonce,
        issuer_endpoints,
        issuer_overlay_ip,
        issuer_wg_public_key,
        issuer_subnet,
        allocated_subnet,
    };

    let claims_json = serde_json::to_vec(&claims).map_err(|e| format!("encode invite: {e}"))?;
    let signature = signing_key.sign(&claims_json);
    let claims_encoded = URL_SAFE_NO_PAD.encode(&claims_json);
    let sig_encoded = URL_SAFE_NO_PAD.encode(signature.to_bytes());

    let token = format!("{claims_encoded}.{sig_encoded}");
    Ok((token, claims))
}

pub fn parse_and_verify_invite_token(encoded: &str) -> Result<InviteClaims, String> {
    let (claims_b64, sig_b64) = encoded
        .split_once('.')
        .ok_or_else(|| "invalid invite token format".to_string())?;

    let claims_json = URL_SAFE_NO_PAD
        .decode(claims_b64)
        .map_err(|e| format!("decode invite claims: {e}"))?;
    let claims: InviteClaims =
        serde_json::from_slice(&claims_json).map_err(|e| format!("parse invite claims: {e}"))?;

    let sig_bytes = URL_SAFE_NO_PAD
        .decode(sig_b64)
        .map_err(|e| format!("decode invite signature: {e}"))?;
    let sig_arr: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| "invalid invite signature length".to_string())?;
    let signature = Signature::from_bytes(&sig_arr);

    let key_bytes = URL_SAFE_NO_PAD
        .decode(&claims.issuer_verify_key)
        .map_err(|e| format!("decode issuer verify key: {e}"))?;
    let key_arr: [u8; 32] = key_bytes
        .as_slice()
        .try_into()
        .map_err(|_| "invalid issuer verify key length".to_string())?;
    let verify_key =
        VerifyingKey::from_bytes(&key_arr).map_err(|e| format!("parse issuer verify key: {e}"))?;

    verify_key
        .verify(&claims_json, &signature)
        .map_err(|e| format!("verify invite signature: {e}"))?;

    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MachineId, NetworkName};

    #[test]
    fn invite_roundtrip_preserves_allocated_subnet() {
        let identity = Identity::generate(MachineId("founder".into()), [7; 32]);
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
            600,
            1_700_000_000,
            vec!["1.2.3.4:51820".into()],
            Some(network.overlay_ip.0.to_string()),
            Some("wg-public".into()),
            Some(network.subnet.to_string()),
            "10.210.99.0/24".into(),
        )
        .expect("issue invite");

        assert_eq!(claims.allocated_subnet, "10.210.99.0/24");

        let parsed = parse_and_verify_invite_token(&token).expect("parse invite");
        assert_eq!(parsed.allocated_subnet, "10.210.99.0/24");
    }
}
