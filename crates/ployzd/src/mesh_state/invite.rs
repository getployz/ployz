use super::network::NetworkConfig;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use ployz_runtime_api::Identity;
use ployz_types::model::NetworkId;
use serde::{Deserialize, Serialize};

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
    #[serde(default)]
    pub issuer_subnet: Option<String>,
    pub allocated_subnet: String,
}

pub struct InviteTokenContext {
    pub issuer_endpoints: Vec<String>,
    pub issuer_overlay_ip: Option<String>,
    pub issuer_wg_public_key: Option<String>,
    pub issuer_subnet: Option<String>,
    pub allocated_subnet: String,
}

pub fn issue_invite_token(
    identity: &Identity,
    network: &NetworkConfig,
    ttl_secs: u64,
    now_unix_secs: u64,
    context: InviteTokenContext,
) -> Result<(String, InviteClaims), String> {
    let InviteTokenContext {
        issuer_endpoints,
        issuer_overlay_ip,
        issuer_wg_public_key,
        issuer_subnet,
        allocated_subnet,
    } = context;
    let expires_at = now_unix_secs
        .checked_add(ttl_secs)
        .ok_or_else(|| "ttl overflow".to_string())?;

    let mut nonce_bytes = [0u8; 16];
    rand::fill(&mut nonce_bytes);
    let nonce = hex_string(nonce_bytes);

    let mut invite_id_bytes = [0u8; 16];
    rand::fill(&mut invite_id_bytes);
    let invite_id = hex_string(invite_id_bytes);

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

    let claims_json =
        serde_json::to_vec(&claims).map_err(|error| format!("encode invite: {error}"))?;
    let signature = signing_key.sign(&claims_json);
    let claims_encoded = URL_SAFE_NO_PAD.encode(&claims_json);
    let sig_encoded = URL_SAFE_NO_PAD.encode(signature.to_bytes());

    Ok((format!("{claims_encoded}.{sig_encoded}"), claims))
}

pub fn parse_and_verify_invite_token(encoded: &str) -> Result<InviteClaims, String> {
    let (claims_b64, sig_b64) = encoded
        .split_once('.')
        .ok_or_else(|| "invalid invite token format".to_string())?;

    let claims_json = URL_SAFE_NO_PAD
        .decode(claims_b64)
        .map_err(|error| format!("decode invite claims: {error}"))?;
    let claims: InviteClaims = serde_json::from_slice(&claims_json)
        .map_err(|error| format!("parse invite claims: {error}"))?;

    let sig_bytes = URL_SAFE_NO_PAD
        .decode(sig_b64)
        .map_err(|error| format!("decode invite signature: {error}"))?;
    let sig_arr: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| "invalid invite signature length".to_string())?;
    let signature = Signature::from_bytes(&sig_arr);

    let key_bytes = URL_SAFE_NO_PAD
        .decode(&claims.issuer_verify_key)
        .map_err(|error| format!("decode issuer verify key: {error}"))?;
    let key_arr: [u8; 32] = key_bytes
        .as_slice()
        .try_into()
        .map_err(|_| "invalid issuer verify key length".to_string())?;
    let verify_key = VerifyingKey::from_bytes(&key_arr)
        .map_err(|error| format!("parse issuer verify key: {error}"))?;

    verify_key
        .verify(&claims_json, &signature)
        .map_err(|error| format!("verify invite signature: {error}"))?;

    Ok(claims)
}

fn hex_string(bytes: [u8; 16]) -> String {
    let mut value = String::with_capacity(32);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut value, "{byte:02x}");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_runtime_api::Identity;
    use ployz_types::model::{MachineId, NetworkName};

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
            InviteTokenContext {
                issuer_endpoints: vec!["1.2.3.4:51820".into()],
                issuer_overlay_ip: Some(network.overlay_ip.0.to_string()),
                issuer_wg_public_key: Some("wg-public".into()),
                issuer_subnet: Some(network.subnet.to_string()),
                allocated_subnet: "10.210.99.0/24".into(),
            },
        )
        .expect("issue invite");

        assert_eq!(claims.allocated_subnet, "10.210.99.0/24");

        let parsed = parse_and_verify_invite_token(&token).expect("parse invite");
        assert_eq!(parsed.allocated_subnet, "10.210.99.0/24");
    }
}
