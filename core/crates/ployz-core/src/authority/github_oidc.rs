//! Check that GitHub Actions signed an OIDC token, against the keys GitHub's issuer
//! publishes. A Machine adds the token's lifetime; a person's client checking an image
//! token from an earlier build does not, since the person, not freshness, is then the
//! authority.
//!
//! Only RS256 is accepted: the algorithm is never read from the key set or trusted from
//! the token. No error carries the token.

#[cfg(any(test, feature = "test-signer"))]
pub mod test_support;

use std::collections::BTreeMap;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use serde::Deserialize;
use thiserror::Error;

use super::GithubClaims;

/// The one issuer whose tokens are accepted.
pub const ISSUER: &str = "https://token.actions.githubusercontent.com";
/// Where that issuer publishes its keys.
pub const JWKS_URL: &str = "https://token.actions.githubusercontent.com/.well-known/jwks";
/// Far above GitHub's 2 KB tokens, and bounds the work a made-up one costs.
pub const MAX_TOKEN_BYTES: usize = 16 * 1024;

/// Why a token is not one GitHub Actions signed.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum TokenRefusal {
    #[error("the GitHub token is not a signed JWT")]
    Malformed,
    #[error("the GitHub token is not signed with RS256")]
    Algorithm,
    #[error("GitHub publishes no key the token names")]
    UnknownKey,
    #[error("the GitHub token's signature does not verify")]
    Signature,
    #[error("the GitHub token's claims are not GitHub Actions claims")]
    Claims,
    #[error("the GitHub token was not issued by GitHub Actions")]
    Issuer,
}

/// A document that is not a JWKS key set, or whose RSA keys are malformed.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("GitHub's token keys are not a key set")]
pub struct MalformedKeys;

/// The RSA keys GitHub's issuer publishes, by key ID.
#[derive(Clone, Debug, Default)]
pub struct KeySet {
    keys: BTreeMap<String, RsaKey>,
}

#[derive(Clone, Debug)]
struct RsaKey {
    modulus: Vec<u8>,
    exponent: Vec<u8>,
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kty: String,
    kid: Option<String>,
    alg: Option<String>,
    n: Option<String>,
    e: Option<String>,
}

impl KeySet {
    /// The RS256 keys of a JWKS document. Keys of another type or algorithm are skipped.
    ///
    /// # Errors
    /// Returns [`MalformedKeys`] unless `jwks` is a JWKS document whose RSA keys are well
    /// formed.
    pub fn parse(jwks: &[u8]) -> Result<Self, MalformedKeys> {
        let jwks: Jwks = serde_json::from_slice(jwks).map_err(|_| MalformedKeys)?;
        let mut keys = BTreeMap::new();
        for jwk in jwks.keys {
            if jwk.kty != "RSA" || jwk.alg.as_deref().is_some_and(|alg| alg != "RS256") {
                continue;
            }
            let (Some(kid), Some(n), Some(e)) = (jwk.kid, jwk.n, jwk.e) else {
                return Err(MalformedKeys);
            };
            let decode = |text: &str| URL_SAFE_NO_PAD.decode(text).map_err(|_| MalformedKeys);
            let mut modulus = decode(&n)?;
            let leading = modulus.iter().take_while(|byte| **byte == 0).count();
            modulus.drain(..leading);
            keys.insert(
                kid,
                RsaKey {
                    modulus,
                    exponent: decode(&e)?,
                },
            );
        }
        Ok(Self { keys })
    }

    /// Whether the set holds key `kid`.
    #[must_use]
    pub fn contains(&self, kid: &str) -> bool {
        self.keys.contains_key(kid)
    }
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    kid: Option<String>,
}

/// A token split into its parts, its header checked to name RS256 and a key.
struct Parsed<'token> {
    kid: String,
    signed: &'token str,
    payload: &'token str,
    signature: &'token str,
}

fn parse(token: &str) -> Result<Parsed<'_>, TokenRefusal> {
    if token.len() > MAX_TOKEN_BYTES {
        return Err(TokenRefusal::Malformed);
    }
    let (signed, signature) = token.rsplit_once('.').ok_or(TokenRefusal::Malformed)?;
    let (header, payload) = signed.split_once('.').ok_or(TokenRefusal::Malformed)?;
    if payload.contains('.') {
        return Err(TokenRefusal::Malformed);
    }
    let header: Header = URL_SAFE_NO_PAD
        .decode(header)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or(TokenRefusal::Malformed)?;
    if header.alg != "RS256" {
        return Err(TokenRefusal::Algorithm);
    }
    Ok(Parsed {
        kid: header.kid.ok_or(TokenRefusal::UnknownKey)?,
        signed,
        payload,
        signature,
    })
}

/// The key ID `token`'s header names, once it names RS256.
///
/// # Errors
/// Returns [`TokenRefusal::Malformed`], [`TokenRefusal::Algorithm`] or
/// [`TokenRefusal::UnknownKey`] for a header that names no RS256 key.
pub fn key_id(token: &str) -> Result<String, TokenRefusal> {
    parse(token).map(|parsed| parsed.kid)
}

/// The claims of `token` once its RS256 signature verifies by a key `keys` holds and
/// GitHub Actions issued it. Its lifetime is not checked.
///
/// # Errors
/// Returns the first [`TokenRefusal`] found.
pub fn signed_claims(token: &str, keys: &KeySet) -> Result<GithubClaims, TokenRefusal> {
    let parsed = parse(token)?;
    let key = keys.keys.get(&parsed.kid).ok_or(TokenRefusal::UnknownKey)?;
    let signature = URL_SAFE_NO_PAD
        .decode(parsed.signature)
        .map_err(|_| TokenRefusal::Signature)?;
    RsaPublicKeyComponents {
        n: &key.modulus,
        e: &key.exponent,
    }
    .verify(
        &RSA_PKCS1_2048_8192_SHA256,
        parsed.signed.as_bytes(),
        &signature,
    )
    .map_err(|_| TokenRefusal::Signature)?;
    let claims: GithubClaims = URL_SAFE_NO_PAD
        .decode(parsed.payload)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or(TokenRefusal::Claims)?;
    if claims.iss != ISSUER {
        return Err(TokenRefusal::Issuer);
    }
    Ok(claims)
}
