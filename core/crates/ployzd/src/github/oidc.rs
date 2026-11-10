//! Verify a GitHub Actions OIDC token on this Machine, against the keys GitHub's issuer
//! publishes, with nothing from Cloud but the token itself.
//!
//! The issuer is fixed and its keys come over TLS from the issuer's own host, so a token
//! verifies only if GitHub signed it. Only RS256 is accepted: the algorithm is never read
//! from the key set or trusted from the token. No error or log line carries the token.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ployz_core::authority::{
    AuthorityRefusal, ClaimRule, GithubClaims, Timestamp,
    validity::{self, CLOCK_SKEW_MILLIS},
};
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use serde::Deserialize;
use thiserror::Error;
use url::Url;

/// The one issuer a Machine accepts tokens from.
pub const ISSUER: &str = "https://token.actions.githubusercontent.com";
const JWKS_URL: &str = "https://token.actions.githubusercontent.com/.well-known/jwks";
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// How soon an unknown key ID may fetch the key set again, so a stream of made-up key IDs
/// cannot make this Machine hammer GitHub.
const REFETCH_FLOOR: Duration = Duration::from_secs(60);
/// GitHub's tokens live 5 minutes. A longer one is refused rather than trusted for hours.
const MAX_LIFETIME_MILLIS: u64 = 60 * 60 * 1000;
/// Far above GitHub's 2 KB tokens, and bounds the work a made-up one costs.
const MAX_TOKEN_BYTES: usize = 16 * 1024;

/// Why this Machine refused a GitHub token.
#[derive(Debug, Error)]
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
    #[error("the GitHub token is outside its lifetime")]
    Lifetime(#[source] AuthorityRefusal),
    #[error("GitHub's token keys could not be read")]
    Keys(#[source] KeysError),
}

/// Why GitHub's key set could not be read.
#[derive(Debug, Error)]
pub enum KeysError {
    #[error("GitHub did not answer for its token keys")]
    Fetch(#[source] reqwest::Error),
    #[error("GitHub's token keys are not a key set")]
    Malformed,
}

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
    /// Returns [`KeysError::Malformed`] unless `jwks` is a JWKS document whose RSA keys
    /// are well formed.
    pub fn parse(jwks: &[u8]) -> Result<Self, KeysError> {
        let jwks: Jwks = serde_json::from_slice(jwks).map_err(|_| KeysError::Malformed)?;
        let mut keys = BTreeMap::new();
        for jwk in jwks.keys {
            if jwk.kty != "RSA" || jwk.alg.as_deref().is_some_and(|alg| alg != "RS256") {
                continue;
            }
            let (Some(kid), Some(n), Some(e)) = (jwk.kid, jwk.n, jwk.e) else {
                return Err(KeysError::Malformed);
            };
            let decode = |text: &str| {
                URL_SAFE_NO_PAD
                    .decode(text)
                    .map_err(|_| KeysError::Malformed)
            };
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

    fn contains(&self, kid: &str) -> bool {
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

/// Verify `token` against `keys` at `now`, in the order the build proof lists the checks:
/// the RS256 signature by a key `keys` holds, the issuer, then the lifetime.
///
/// # Errors
/// Returns the first [`TokenRefusal`] found.
pub fn verify(token: &str, keys: &KeySet, now: Timestamp) -> Result<GithubClaims, TokenRefusal> {
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
    validity::verify_within(claims.iat, claims.exp, now, MAX_LIFETIME_MILLIS)
        .map_err(TokenRefusal::Lifetime)?;
    if claims.nbf > now.plus(CLOCK_SKEW_MILLIS) {
        return Err(TokenRefusal::Lifetime(AuthorityRefusal::Mismatch {
            field: "a start time",
        }));
    }
    Ok(claims)
}

/// Why a token does not prove a build under a rule.
#[derive(Debug, Error)]
pub enum ProofRefusal {
    #[error(transparent)]
    Token(TokenRefusal),
    #[error("{}", claim_refusal(.0))]
    Claim(AuthorityRefusal),
}

fn claim_refusal(refusal: &AuthorityRefusal) -> String {
    if let AuthorityRefusal::Mismatch { field } = refusal {
        format!("the GitHub token's {field} claim does not match the rule")
    } else {
        format!("the GitHub token does not match the rule: {refusal}")
    }
}

/// GitHub's token keys, fetched over TLS from the fixed issuer and cached. A token naming
/// a key the cache lacks fetches the set again, at most once a minute.
pub struct IssuerKeys {
    client: reqwest::Client,
    url: Url,
    refetch_floor: Duration,
    cached: Mutex<Cached>,
    fetching: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct Cached {
    keys: Arc<KeySet>,
    fetched: Option<Instant>,
}

impl IssuerKeys {
    /// Keys from GitHub's issuer, fetched on first use.
    ///
    /// # Errors
    /// Returns [`KeysError::Fetch`] when the HTTPS client cannot be built.
    pub fn github() -> Result<Self, KeysError> {
        let client = reqwest::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(FETCH_TIMEOUT)
            .user_agent("ployzd")
            .build()
            .map_err(KeysError::Fetch)?;
        let url = Url::parse(JWKS_URL).expect("the JWKS URL is a constant");
        Ok(Self::with(client, url, REFETCH_FLOOR))
    }

    fn with(client: reqwest::Client, url: Url, refetch_floor: Duration) -> Self {
        Self {
            client,
            url,
            refetch_floor,
            cached: Mutex::new(Cached::default()),
            fetching: tokio::sync::Mutex::new(()),
        }
    }

    /// Verify `token` at `now` against GitHub's keys, as [`verify`] does.
    ///
    /// # Errors
    /// Returns the [`TokenRefusal`] from [`verify`], and [`TokenRefusal::Keys`] when the
    /// key set the token needs could not be fetched.
    pub async fn verify(&self, token: &str, now: Timestamp) -> Result<GithubClaims, TokenRefusal> {
        let kid = parse(token)?.kid;
        let keys = self.keys_naming(&kid).await?;
        verify(token, &keys, now)
    }

    /// Verify `token` at `now` and check its claims against `rule`. A refusal is logged
    /// with the rule's ID and the reason, the claim's name for a claim, never the token.
    ///
    /// # Errors
    /// Returns [`ProofRefusal::Token`] when the token does not verify, and
    /// [`ProofRefusal::Claim`] naming the first claim `rule` does not match.
    pub async fn check(
        &self,
        rule: &impl ClaimRule,
        token: &str,
        now: Timestamp,
    ) -> Result<GithubClaims, ProofRefusal> {
        let id = rule.identity().rule;
        let claims = self.verify(token, now).await.map_err(|refusal| {
            tracing::warn!(rule = %id, %refusal, "refused a GitHub token");
            ProofRefusal::Token(refusal)
        })?;
        rule.verify_claims(&claims).map_err(|refusal| {
            if let AuthorityRefusal::Mismatch { field } = &refusal {
                tracing::warn!(rule = %id, claim = field, "refused a GitHub token's claim");
            }
            ProofRefusal::Claim(refusal)
        })?;
        Ok(claims)
    }

    async fn keys_naming(&self, kid: &str) -> Result<Arc<KeySet>, TokenRefusal> {
        if let Some(keys) = self.cached_naming(kid) {
            return Ok(keys);
        }
        let _fetching = self.fetching.lock().await;
        if let Some(keys) = self.cached_naming(kid) {
            return Ok(keys);
        }
        let recent = self
            .lock()
            .fetched
            .is_some_and(|at| at.elapsed() < self.refetch_floor);
        if recent {
            return Ok(Arc::clone(&self.lock().keys));
        }
        let fetched = self.fetch().await;
        // A failed fetch counts toward the floor too, so an outage can't make every token refetch.
        self.lock().fetched = Some(Instant::now());
        let keys = Arc::new(fetched.map_err(TokenRefusal::Keys)?);
        self.lock().keys = Arc::clone(&keys);
        Ok(keys)
    }

    fn cached_naming(&self, kid: &str) -> Option<Arc<KeySet>> {
        let cached = self.lock();
        cached.keys.contains(kid).then(|| Arc::clone(&cached.keys))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Cached> {
        self.cached.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn fetch(&self) -> Result<KeySet, KeysError> {
        let response = self
            .client
            .get(self.url.clone())
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(KeysError::Fetch)?;
        KeySet::parse(&response.bytes().await.map_err(KeysError::Fetch)?)
    }
}

#[cfg(test)]
#[path = "oidc_tests.rs"]
mod tests;
