use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use ployz_api::{
    DaemonPayload, DaemonRequest, DaemonResponse, MembershipAttestation, MeshJoinPayload,
};
use ployz_types::model::MachineRecord;
use serde::Deserialize;
use std::io::{Read, Write};

#[derive(Deserialize)]
struct InviteClaimsSubset {
    invite_id: String,
    challenge_nonce: String,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("attestor error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| format!("read stdin: {error}"))?;

    let request: DaemonRequest =
        serde_json::from_str(&input).map_err(|error| format!("parse request: {error}"))?;
    let DaemonRequest::MeshJoin { token } = request else {
        return Err("expected MeshJoin request".into());
    };

    let (claims_b64, _sig_b64) = token
        .split_once('.')
        .ok_or_else(|| "invalid invite token format".to_string())?;
    let claims_json = URL_SAFE_NO_PAD
        .decode(claims_b64)
        .map_err(|error| format!("decode claims: {error}"))?;
    let claims: InviteClaimsSubset =
        serde_json::from_slice(&claims_json).map_err(|error| format!("parse claims: {error}"))?;

    let seed_hex = std::env::var("PLOYZ_TEST_ATTESTOR_SEED")
        .map_err(|_| "PLOYZ_TEST_ATTESTOR_SEED not set".to_string())?;
    let seed_bytes = decode_hex_32(&seed_hex)?;
    let record_json = std::env::var("PLOYZ_TEST_ATTESTOR_RECORD")
        .map_err(|_| "PLOYZ_TEST_ATTESTOR_RECORD not set".to_string())?;
    let encoded = std::env::var("PLOYZ_TEST_ATTESTOR_ENCODED")
        .map_err(|_| "PLOYZ_TEST_ATTESTOR_ENCODED not set".to_string())?;

    let record: MachineRecord =
        serde_json::from_str(&record_json).map_err(|error| format!("parse record: {error}"))?;

    let signing_key = SigningKey::from_bytes(&seed_bytes);
    let verify_key = signing_key.verifying_key();

    let mut message = Vec::with_capacity(
        claims.invite_id.len() + claims.challenge_nonce.len() + record.id.0.len() + 2,
    );
    message.extend_from_slice(claims.invite_id.as_bytes());
    message.push(0);
    message.extend_from_slice(claims.challenge_nonce.as_bytes());
    message.push(0);
    message.extend_from_slice(record.id.0.as_bytes());
    let signature = signing_key.sign(&message);

    let attestation = MembershipAttestation {
        challenge_nonce: claims.challenge_nonce,
        ed25519_public_key: URL_SAFE_NO_PAD.encode(verify_key.to_bytes()),
        signature: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
    };

    let response = DaemonResponse {
        ok: true,
        code: "OK".into(),
        message: encoded.clone(),
        payload: Some(DaemonPayload::MeshJoin(MeshJoinPayload {
            encoded,
            record,
            attestation,
        })),
    };
    let out =
        serde_json::to_string(&response).map_err(|error| format!("encode response: {error}"))?;
    std::io::stdout()
        .write_all(out.as_bytes())
        .map_err(|error| format!("write stdout: {error}"))?;
    Ok(())
}

fn decode_hex_32(value: &str) -> Result<[u8; 32], String> {
    if value.len() != 64 {
        return Err(format!("expected 64 hex chars, got {}", value.len()));
    }
    let mut out = [0u8; 32];
    for (slot, chunk) in out.iter_mut().zip(value.as_bytes().chunks(2)) {
        let [hi, lo] = chunk else {
            return Err("odd number of hex chars".into());
        };
        let hi = hex_nibble(*hi)?;
        let lo = hex_nibble(*lo)?;
        *slot = (hi << 4) | lo;
    }
    Ok(out)
}

fn hex_nibble(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(format!("invalid hex char: {byte}")),
    }
}
