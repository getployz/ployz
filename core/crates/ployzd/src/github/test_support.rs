//! A test RSA signer GitHub's key set is extended with, so tests sign tokens of their own.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    rand::SystemRandom,
    signature::{RSA_PKCS1_SHA256, RsaKeyPair, RsaPublicKeyComponents},
};
use serde_json::{Value, json};

use super::oidc::KeySet;

const JWKS: &[u8] = include_bytes!("fixtures/jwks.json");
/// An RSA key made with `openssl genpkey` for these tests. It signs nothing outside them.
const TEST_SIGNER: &[u8] = include_bytes!("fixtures/test-signer.pk8");
pub(crate) const TEST_KID: &str = "test-signer";
/// The real token's claims, which tests change one at a time.
pub(crate) const CLAIMS: &str = include_str!("fixtures/claims.json");

pub(crate) fn encode(value: &Value) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).expect("JSON encodes"))
}

pub(crate) fn test_signer() -> RsaKeyPair {
    RsaKeyPair::from_pkcs8(TEST_SIGNER).expect("the test signer is PKCS#8")
}

/// GitHub's keys with the test signer's added under [`TEST_KID`].
#[expect(clippy::indexing_slicing, reason = "JSON fixture")]
pub(crate) fn keys_with_test_signer() -> KeySet {
    let public = RsaPublicKeyComponents::<Vec<u8>>::from(test_signer().public());
    let mut jwks: Value = serde_json::from_slice(JWKS).expect("the fixture JWKS is JSON");
    jwks["keys"]
        .as_array_mut()
        .expect("a JWKS holds keys")
        .push(json!({
            "kty": "RSA",
            "alg": "RS256",
            "kid": TEST_KID,
            "n": URL_SAFE_NO_PAD.encode(&public.n),
            "e": URL_SAFE_NO_PAD.encode(&public.e),
        }));
    KeySet::parse(&serde_json::to_vec(&jwks).expect("JSON encodes")).expect("the JWKS parses")
}

/// A token over `claims`, RS256-signed by the test signer and naming `kid`.
pub(crate) fn signed_by_test_signer(kid: &str, claims: &Value) -> String {
    let signer = test_signer();
    let signed = format!(
        "{}.{}",
        encode(&json!({"alg": "RS256", "kid": kid, "typ": "JWT"})),
        encode(claims)
    );
    let mut signature = vec![0; signer.public().modulus_len()];
    signer
        .sign(
            &RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            signed.as_bytes(),
            &mut signature,
        )
        .expect("the test signer signs");
    format!("{signed}.{}", URL_SAFE_NO_PAD.encode(signature))
}
