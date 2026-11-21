use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ployz_core::{MachineId, separation::OTLP_TOKEN_SIGNATURE};
use serde::Serialize;

use crate::management::ManagementSecret;

const PREFIX: &str = "ployz-otlp-v1";

#[derive(Serialize)]
struct Claims<'a> {
    machine: &'a str,
    iat: i64,
    exp: i64,
}

pub(super) fn mint(
    secret: &ManagementSecret,
    machine: &MachineId,
    issued: i64,
    lifetime_seconds: i64,
) -> String {
    let claims = Claims {
        machine: machine.as_str(),
        iat: issued,
        exp: issued + lifetime_seconds,
    };
    let payload = URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&claims).expect("token claims serialize to JSON"));
    let mut signed = OTLP_TOKEN_SIGNATURE.to_vec();
    signed.extend_from_slice(payload.as_bytes());
    let signature = URL_SAFE_NO_PAD.encode(secret.sign(&signed));
    format!("{PREFIX}.{payload}.{signature}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret() -> ManagementSecret {
        ManagementSecret::generate()
    }

    #[test]
    fn a_token_is_claims_and_a_signature_the_management_key_verifies() {
        let secret = secret();
        let machine = MachineId::parse("a".repeat(32)).unwrap();
        let token = mint(&secret, &machine, 1_000, 3_600);
        let [prefix, payload, signature] = token.split('.').collect::<Vec<_>>()[..] else {
            panic!("a token has three segments: {token}");
        };
        assert_eq!(prefix, PREFIX);
        assert_eq!(signature.len(), 86);
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap();
        assert_eq!(
            claims,
            serde_json::json!({"machine": "a".repeat(32), "iat": 1_000, "exp": 4_600})
        );
        let mut signed = OTLP_TOKEN_SIGNATURE.to_vec();
        signed.extend_from_slice(payload.as_bytes());
        let signature = iroh::Signature::from_bytes(
            &URL_SAFE_NO_PAD
                .decode(signature)
                .unwrap()
                .try_into()
                .unwrap(),
        );
        iroh::PublicKey::from_bytes(secret.public_key().as_bytes())
            .unwrap()
            .verify(&signed, &signature)
            .unwrap();
    }
}
