//! Certificate row body held in the replicated store.

use std::{collections::BTreeMap, time::SystemTime};

use chrono::{DateTime, SecondsFormat, Utc};
use ployz_core::{
    CertificateFailureKind, CertificateHost, ClusterRoute, IngressHost, IssuanceClock,
    IssuanceFailure,
};
use serde::{Deserialize, Serialize};

use super::Error;

/// Parseable certificate chain paired with its private key.
///
/// Admission does not prove trust, hostname coverage, validity dates, or proxy adoption.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CertificateMaterial {
    certificate: String,
    private_key: String,
}

/// Why certificate material cannot be admitted; errors never include key material.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CertificateMaterialError {
    /// The private key is not supported PEM key material.
    #[error("invalid certificate private key")]
    InvalidPrivateKey,
    /// The certificate chain is empty or malformed.
    #[error("invalid certificate chain")]
    InvalidChain,
    /// The leaf certificate belongs to another key.
    #[error("certificate does not match its private key")]
    KeyMismatch,
    /// No DNS name on the leaf certificate serves the hostname.
    #[error("certificate does not cover the hostname")]
    HostnameNotCovered,
}

impl CertificateMaterial {
    /// Admit a parseable chain whose leaf matches the private key.
    ///
    /// # Errors
    /// Returns the invalid component or key mismatch without exposing supplied material.
    pub fn parse(
        certificate: impl Into<String>,
        private_key: impl Into<String>,
    ) -> Result<Self, CertificateMaterialError> {
        use CertificateMaterialError::{InvalidChain, InvalidPrivateKey, KeyMismatch};
        use rcgen::PublicKeyData as _;
        use x509_parser::pem::Pem;

        let certificate = certificate.into();
        let private_key = private_key.into();
        let key = rcgen::KeyPair::from_pem(&private_key).map_err(|_| InvalidPrivateKey)?;
        let mut chain = Pem::iter_from_buffer(certificate.as_bytes());
        let leaf = chain
            .next()
            .ok_or(InvalidChain)?
            .map_err(|_| InvalidChain)?;
        if leaf.label != "CERTIFICATE" {
            return Err(InvalidChain);
        }
        if leaf
            .parse_x509()
            .map_err(|_| InvalidChain)?
            .public_key()
            .raw
            != key.subject_public_key_info()
        {
            return Err(KeyMismatch);
        }
        for certificate in chain {
            let certificate = certificate.map_err(|_| InvalidChain)?;
            if certificate.label != "CERTIFICATE" {
                return Err(InvalidChain);
            }
            certificate.parse_x509().map_err(|_| InvalidChain)?;
        }
        Ok(Self {
            certificate,
            private_key,
        })
    }

    /// Keep this material only if a DNS subject alternative name on its leaf
    /// serves `hostname`: the same name, or a wildcard one label above it.
    ///
    /// # Errors
    /// Returns `HostnameNotCovered` when no leaf DNS name serves the hostname.
    pub fn covering(self, hostname: &CertificateHost) -> Result<Self, CertificateMaterialError> {
        use x509_parser::extensions::GeneralName;

        // A wildcard target is only served by the same wildcard.
        let host = IngressHost::parse(hostname.as_str()).ok();
        let serves = |name: &str| {
            CertificateHost::parse(name.to_ascii_lowercase()).is_ok_and(|name| {
                name == *hostname || host.as_ref().is_some_and(|host| name.covers(host))
            })
        };
        let (_, leaf) = x509_parser::pem::parse_x509_pem(self.certificate.as_bytes())
            .map_err(|_| CertificateMaterialError::InvalidChain)?;
        let leaf = leaf
            .parse_x509()
            .map_err(|_| CertificateMaterialError::InvalidChain)?;
        let covered = leaf
            .subject_alternative_name()
            .ok()
            .flatten()
            .is_some_and(|names| {
                names
                    .value
                    .general_names
                    .iter()
                    .any(|name| matches!(name, GeneralName::DNSName(name) if serves(name)))
            });
        if covered {
            Ok(self)
        } else {
            Err(CertificateMaterialError::HostnameNotCovered)
        }
    }

    /// Borrow the admitted PEM certificate chain.
    #[must_use]
    pub fn certificate(&self) -> &str {
        &self.certificate
    }

    /// Borrow the corresponding PEM private key.
    #[must_use]
    pub fn private_key(&self) -> &str {
        &self.private_key
    }
}

/// HTTP-01 token and key authorization held in cluster state for one Ingress Hostname.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CertificateChallenge {
    token: String,
    response: String,
}

/// Why an HTTP-01 challenge cannot be admitted, without its supplied values.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CertificateChallengeError {
    /// The token is too short or contains forbidden characters.
    #[error("invalid HTTP-01 token")]
    InvalidToken,
    /// The key authorization does not contain a valid thumbprint.
    #[error("invalid HTTP-01 key authorization")]
    InvalidResponse,
    /// The key authorization names a different token.
    #[error("HTTP-01 key authorization token mismatch")]
    TokenMismatch,
}

impl CertificateChallenge {
    /// Admit an HTTP-01 token and matching key authorization.
    ///
    /// # Errors
    /// Rejects malformed tokens, malformed thumbprints, or mismatched token prefixes.
    pub fn parse(
        token: impl Into<String>,
        response: impl Into<String>,
    ) -> Result<Self, CertificateChallengeError> {
        let token = token.into();
        let response = response.into();
        // RFC 8555 §§8.1, 8.3: token + '.' + base64url(SHA-256 JWK thumbprint).
        // Length bounds token capacity; neither entropy nor account-key ownership
        // can be established from the stored challenge alone.
        let base64url = |value: &str| {
            value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        };
        let (prefix, thumbprint) = response
            .split_once('.')
            .ok_or(CertificateChallengeError::InvalidResponse)?;
        // The final base64 symbol of a 32-byte digest has two zero padding bits.
        if token.len() < 22 || !base64url(&token) {
            return Err(CertificateChallengeError::InvalidToken);
        }
        if prefix != token {
            return Err(CertificateChallengeError::TokenMismatch);
        }
        if thumbprint.len() != 43
            || !base64url(thumbprint)
            || !b"AEIMQUYcgkosw048".contains(
                thumbprint
                    .as_bytes()
                    .last()
                    .ok_or(CertificateChallengeError::InvalidResponse)?,
            )
        {
            return Err(CertificateChallengeError::InvalidResponse);
        }
        Ok(Self { token, response })
    }

    /// Borrow the admitted HTTP-01 token.
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Borrow the matching key authorization.
    #[must_use]
    pub fn response(&self) -> &str {
        &self.response
    }
}

/// Operator-visible reason, with a shared clock only when issuance is backing off.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedRefusal {
    reason: String,
    clock: Option<IssuanceClock>,
}

/// Replicated certificate row for one certificate hostname.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CertificateRow {
    /// Material, pending challenge and refusal that ACME owns.
    Acme {
        /// Issued material and the route its order validated along.
        material: Option<(CertificateMaterial, ClusterRoute)>,
        challenge: Option<CertificateChallenge>,
        refusal: Option<RecordedRefusal>,
    },
    /// Operator- or Cloud-supplied material that ACME never orders, renews, or overwrites.
    Published(CertificateMaterial),
}

impl Default for CertificateRow {
    fn default() -> Self {
        Self::from_parts(None, None)
    }
}

impl CertificateRow {
    /// ACME-owned snapshot for one hostname.
    #[must_use]
    pub fn from_parts(
        material: Option<(CertificateMaterial, ClusterRoute)>,
        challenge: Option<CertificateChallenge>,
    ) -> Self {
        Self::Acme {
            material,
            challenge,
            refusal: None,
        }
    }

    /// ACME row that holds material newly issued along `route`, and no challenge.
    #[must_use]
    pub fn issued(material: CertificateMaterial, route: ClusterRoute) -> Self {
        Self::from_parts(Some((material, route)), None)
    }

    /// The route ACME-issued material validated along. `None` without ACME material.
    #[must_use]
    pub fn route(&self) -> Option<ClusterRoute> {
        match self {
            Self::Acme { material, .. } => material.as_ref().map(|(_, route)| *route),
            Self::Published(_) => None,
        }
    }

    /// Published material, if this row holds it.
    #[must_use]
    pub fn published(&self) -> Option<&CertificateMaterial> {
        match self {
            Self::Published(material) => Some(material),
            Self::Acme { .. } => None,
        }
    }

    /// Attach a complete refusal clock, or leave the row unchanged if the text is empty.
    /// A published row takes no refusal.
    #[must_use]
    pub fn with_backoff(self, last_error: impl Into<String>, clock: IssuanceClock) -> Self {
        let reason = last_error.into();
        if reason.is_empty() {
            return self;
        }
        self.with_refusal(RecordedRefusal {
            reason,
            clock: Some(clock),
        })
    }

    /// Material served for the hostname, published or ACME-issued.
    #[must_use]
    pub fn material(&self) -> Option<&CertificateMaterial> {
        match self {
            Self::Acme { material, .. } => material.as_ref().map(|(material, _)| material),
            Self::Published(material) => Some(material),
        }
    }

    /// Pending HTTP-01 challenge, if any.
    #[must_use]
    pub fn challenge(&self) -> Option<&CertificateChallenge> {
        match self {
            Self::Acme { challenge, .. } => challenge.as_ref(),
            Self::Published(_) => None,
        }
    }

    fn refusal(&self) -> Option<&RecordedRefusal> {
        match self {
            Self::Acme { refusal, .. } => refusal.as_ref(),
            Self::Published(_) => None,
        }
    }

    /// Last recorded refusal or issuance error, if any.
    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        self.refusal().map(|refusal| refusal.reason.as_str())
    }

    /// Shared backoff clock, if a complete refusal has been recorded.
    #[must_use]
    pub fn clock(&self) -> Option<IssuanceClock> {
        self.refusal().and_then(|refusal| refusal.clock)
    }

    /// Take the served material out of the row.
    #[must_use]
    pub fn into_material(self) -> Option<CertificateMaterial> {
        match self {
            Self::Acme { material, .. } => material.map(|(material, _)| material),
            Self::Published(material) => Some(material),
        }
    }

    /// Keep existing material and set the pending challenge. A published row takes no challenge.
    #[must_use]
    pub fn with_challenge(self, challenge: CertificateChallenge) -> Self {
        match self {
            Self::Acme {
                material, refusal, ..
            } => Self::Acme {
                material,
                challenge: Some(challenge),
                refusal,
            },
            published @ Self::Published(_) => published,
        }
    }

    /// Keep existing material and challenge and record a refusal reason.
    /// A published row takes no refusal.
    #[must_use]
    pub fn with_error(self, reason: impl Into<String>) -> Self {
        self.with_refusal(RecordedRefusal {
            reason: reason.into(),
            clock: None,
        })
    }

    fn with_refusal(self, refusal: RecordedRefusal) -> Self {
        match self {
            Self::Acme {
                material,
                challenge,
                ..
            } => Self::Acme {
                material,
                challenge,
                refusal: Some(refusal),
            },
            published @ Self::Published(_) => published,
        }
    }

    pub(crate) fn decode(encoded: &str) -> Result<Self, Error> {
        if encoded.is_empty() {
            return Ok(Self::default());
        }
        let body: CertificateBody = serde_json::from_str(encoded)?;
        let clock = decode_clock(&body.next_attempt_at, body.failures, &body.last_failure);
        let has_material = !body.certificate.is_empty() || !body.private_key.is_empty();
        let material = CertificateMaterial::parse(body.certificate, body.private_key).ok();
        let mut last_error = body.last_error;
        if has_material && material.is_none() {
            if !last_error.is_empty() {
                last_error.push_str("; ");
            }
            last_error.push_str(
                "stored certificate material is invalid or does not match its private key",
            );
        }
        let has_challenge = !body.challenge_token.is_empty() || !body.challenge_response.is_empty();
        let challenge =
            CertificateChallenge::parse(body.challenge_token, body.challenge_response).ok();
        if has_challenge && challenge.is_none() {
            if !last_error.is_empty() {
                last_error.push_str("; ");
            }
            last_error.push_str("stored HTTP-01 challenge is invalid");
        }
        if body.published {
            // Published material is supplied whole; a published row without it is corrupt.
            return material.map(Self::Published).ok_or_else(|| {
                Error::Protocol("published certificate row has no valid material".into())
            });
        }
        let route = body.route.unwrap_or(ClusterRoute::Direct);
        Ok(Self::Acme {
            material: material.map(|material| (material, route)),
            challenge,
            refusal: (!last_error.is_empty()).then_some(RecordedRefusal {
                reason: last_error,
                clock,
            }),
        })
    }

    pub(crate) fn encode(&self) -> Result<String, Error> {
        let clock = self.clock();
        Ok(serde_json::to_string(&CertificateBody {
            certificate: self
                .material()
                .map_or("", CertificateMaterial::certificate)
                .into(),
            private_key: self
                .material()
                .map_or("", CertificateMaterial::private_key)
                .into(),
            challenge_token: self
                .challenge()
                .map_or("", CertificateChallenge::token)
                .into(),
            challenge_response: self
                .challenge()
                .map_or("", CertificateChallenge::response)
                .into(),
            last_error: self.last_error().unwrap_or_default().into(),
            next_attempt_at: clock
                .map(|clock| encode_attempt(clock.next_attempt_at()))
                .unwrap_or_default(),
            failures: clock.map_or(0, |clock| clock.failures()),
            last_failure: encode_failure(clock.map(|clock| clock.last_failure())),
            published: self.published().is_some(),
            route: self.route(),
        })?)
    }
}

/// Published material that serves `hostname`: its own published row, else a
/// published wildcard one label above it. ACME never orders a covered hostname.
#[must_use]
pub fn published_cover<'rows>(
    hostname: &IngressHost,
    rows: &'rows BTreeMap<CertificateHost, CertificateRow>,
) -> Option<&'rows CertificateMaterial> {
    rows.get(hostname.as_str())
        .and_then(CertificateRow::published)
        .or_else(|| {
            rows.iter()
                .find_map(|(name, row)| row.published().filter(|_| name.covers(hostname)))
        })
}

/// Frozen JSON body of a `certificates` row. Every field defaults and unknown
/// fields are ignored, so later releases may only add optional fields.
#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct CertificateBody {
    certificate: String,
    private_key: String,
    challenge_token: String,
    challenge_response: String,
    last_error: String,
    next_attempt_at: String,
    failures: u32,
    last_failure: String,
    published: bool,
    /// The route ACME material validated along; absent without ACME material.
    route: Option<ClusterRoute>,
}

fn encode_attempt(time: SystemTime) -> String {
    DateTime::<Utc>::from(time).to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn decode_attempt(text: &str) -> Option<SystemTime> {
    if text.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|time| SystemTime::from(time.with_timezone(&Utc)))
}

fn decode_clock(next_attempt_at: &str, failures: u32, last_failure: &str) -> Option<IssuanceClock> {
    let last_failure = decode_failure(last_failure)?;
    let next_attempt_at = decode_attempt(next_attempt_at)?;
    Some(IssuanceClock::new(failures, next_attempt_at, last_failure))
}

fn encode_failure(failure: Option<IssuanceFailure>) -> String {
    failure
        .map(|failure| CertificateFailureKind::from(failure).as_str().to_owned())
        .unwrap_or_default()
}

fn decode_failure(text: &str) -> Option<IssuanceFailure> {
    CertificateFailureKind::from(text).issuance_failure()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use ployz_core::{ClusterRoute, IssuanceClock, IssuanceFailure};

    use super::{
        CertificateChallenge, CertificateChallengeError, CertificateMaterial,
        CertificateMaterialError, CertificateRow,
    };

    #[test]
    fn challenge_admission_rejects_invalid_grammar_and_token_mismatch() {
        let token = "LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0";
        let thumbprint = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let response = format!("{token}.{thumbprint}");
        assert!(CertificateChallenge::parse(token, &response).is_ok());
        assert_eq!(
            CertificateChallenge::parse("short", &response),
            Err(CertificateChallengeError::InvalidToken)
        );
        assert_eq!(
            CertificateChallenge::parse(token, format!("other.{thumbprint}")),
            Err(CertificateChallengeError::TokenMismatch)
        );
        assert_eq!(
            CertificateChallenge::parse(token, format!("{token}.short")),
            Err(CertificateChallengeError::InvalidResponse)
        );
        let minimum_token = "_-0123456789abcdefghij";
        assert!(
            CertificateChallenge::parse(minimum_token, format!("{minimum_token}.{thumbprint}"))
                .is_ok()
        );
        for bad_token in [
            String::new(),
            "short".to_owned(),
            format!("../{token}"),
            format!("{token}\n}}"),
            format!("{token}\""),
            format!("{token}="),
            format!("{token}é"),
        ] {
            assert!(
                CertificateChallenge::parse(&bad_token, format!("{bad_token}.{thumbprint}"))
                    .is_err(),
                "{bad_token:?}"
            );
        }
        for bad_response in [
            String::new(),
            format!("other.{thumbprint}"),
            format!("{token}.short"),
            format!("{response}="),
            format!("{response}\n"),
            format!("{response}.extra"),
            format!("{token}.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB"),
        ] {
            assert!(
                CertificateChallenge::parse(token, &bad_response).is_err(),
                "{bad_response:?}"
            );
            let encoded = serde_json::json!({"challenge_token":token,"challenge_response":bad_response,"last_error":"authority refused"}).to_string();
            let row = CertificateRow::decode(&encoded).unwrap();
            assert!(row.challenge().is_none());
            assert!(row.last_error().unwrap().contains("authority refused"));
        }
    }

    fn issued_material() -> CertificateMaterial {
        let pair = rcgen::generate_simple_self_signed(["example.com".to_owned()]).unwrap();
        CertificateMaterial::parse(pair.cert.pem(), pair.signing_key.serialize_pem()).unwrap()
    }

    fn decode_material(encoded: &str) -> Result<Option<CertificateMaterial>, super::Error> {
        Ok(CertificateRow::decode(encoded)?.into_material())
    }

    #[test]
    fn certificate_material_rejects_garbage_and_mismatched_keys() {
        assert_eq!(
            CertificateMaterial::parse("CERT", "KEY"),
            Err(CertificateMaterialError::InvalidPrivateKey)
        );
        let first = rcgen::generate_simple_self_signed(["example.com".to_owned()]).unwrap();
        let second = rcgen::generate_simple_self_signed(["example.com".to_owned()]).unwrap();
        assert_eq!(
            CertificateMaterial::parse(first.cert.pem(), second.signing_key.serialize_pem()),
            Err(CertificateMaterialError::KeyMismatch)
        );
        assert_eq!(
            CertificateMaterial::parse("CERT", first.signing_key.serialize_pem()),
            Err(CertificateMaterialError::InvalidChain)
        );
    }

    #[test]
    fn certificate_material_accepts_policy_key_types_and_parseable_chains() {
        let keys = [
            rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap(),
            rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P384_SHA384).unwrap(),
            rcgen::KeyPair::from_pem(include_str!(
                "../../tests/fixtures/certificate-test-rsa-key.pem"
            ))
            .unwrap(),
        ];
        for key in keys {
            let params = rcgen::CertificateParams::new(vec!["example.com".to_owned()]).unwrap();
            let certificate = params.self_signed(&key).unwrap().pem();
            let private_key = key.serialize_pem();
            assert!(CertificateMaterial::parse(certificate.clone(), private_key.clone()).is_ok());
            let chain = format!("{certificate}{certificate}");
            assert!(CertificateMaterial::parse(chain, private_key.clone()).is_ok());
            let broken_chain = format!(
                "{certificate}-----BEGIN CERTIFICATE-----\nZ2FyYmFnZQ==\n-----END CERTIFICATE-----\n"
            );
            assert!(CertificateMaterial::parse(broken_chain, private_key).is_err());
        }
    }

    #[test]
    fn invalid_stored_material_keeps_challenge_and_refusal_evidence() {
        let row = CertificateRow::decode(r#"{"certificate":"CERT","private_key":"KEY",
            "challenge_token":"LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0","challenge_response":"LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","last_error":"authority refused"}"#).unwrap();
        assert!(row.material().is_none());
        assert!(row.challenge().is_some());
        assert!(row.last_error().unwrap().contains("authority refused"));
        assert!(row.last_error().unwrap().contains("invalid"));
        assert_eq!(CertificateRow::decode(&row.encode().unwrap()).unwrap(), row);
    }

    fn material_for(names: &[&str]) -> CertificateMaterial {
        let pair = rcgen::generate_simple_self_signed(
            names
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        CertificateMaterial::parse(pair.cert.pem(), pair.signing_key.serialize_pem()).unwrap()
    }

    #[test]
    fn material_covers_its_names_and_one_label_under_its_wildcards() {
        let covers = |names: &[&str], hostname: &str| {
            material_for(names)
                .covering(&ployz_core::CertificateHost::parse(hostname).unwrap())
                .is_ok()
        };
        assert!(covers(&["app.example.com"], "app.example.com"));
        assert!(covers(&["*.example.com"], "app.example.com"));
        assert!(covers(&["*.example.com"], "*.example.com"));
        assert!(covers(&["other.test", "*.example.com"], "api.example.com"));
        assert!(!covers(&["*.example.com"], "example.com"));
        assert!(!covers(&["*.example.com"], "deep.app.example.com"));
        assert!(!covers(&["*.example.com"], "*.app.example.com"));
        assert!(!covers(&["app.example.com"], "*.example.com"));
        assert_eq!(
            material_for(&["app.example.com"])
                .covering(&ployz_core::CertificateHost::parse("web.example.com").unwrap()),
            Err(CertificateMaterialError::HostnameNotCovered)
        );
    }

    #[test]
    fn published_flag_round_trips_and_defaults_to_acme() {
        let material = issued_material();
        let row = CertificateRow::Published(material.clone());
        assert_eq!(CertificateRow::decode(&row.encode().unwrap()).unwrap(), row);
        let body = serde_json::json!({
            "certificate": material.certificate(), "private_key": material.private_key()
        });
        assert!(
            CertificateRow::decode(&body.to_string())
                .unwrap()
                .published()
                .is_none()
        );
    }

    #[test]
    fn issued_route_round_trips_and_needs_material() {
        for route in [ClusterRoute::Direct, ClusterRoute::ViaProxy] {
            let row = CertificateRow::issued(issued_material(), route);
            assert_eq!(
                CertificateRow::decode(&row.encode().unwrap())
                    .unwrap()
                    .route(),
                Some(route)
            );
        }
        assert_eq!(CertificateRow::default().route(), None);
    }

    #[test]
    fn invalid_certificate_body_is_an_error() {
        assert!(decode_material("{").is_err());
        assert!(decode_material("null").is_err());
        assert!(decode_material(r#"{"published":true}"#).is_err());
        assert!(
            decode_material(r#"{"published":true,"certificate":"CERT","private_key":"KEY"}"#)
                .is_err()
        );
    }

    #[test]
    fn empty_certificate_body_is_not_present() {
        assert!(CertificateMaterial::parse("", "").is_err());
        assert!(CertificateMaterial::parse("CERT", "").is_err());
        assert_eq!(decode_material("").unwrap(), None);
        assert_eq!(decode_material("{}").unwrap(), None);
        assert_eq!(
            decode_material(r#"{"certificate":"CERT","private_key":""}"#).unwrap(),
            None
        );
    }

    #[test]
    fn certificate_material_reads_known_fields_and_ignores_the_rest() {
        let issued = issued_material();
        let row = CertificateRow::decode(
            &serde_json::json!({
                "certificate": issued.certificate(), "private_key": issued.private_key(),
                "last_error": "refused", "future": 1
            })
            .to_string(),
        )
        .unwrap();
        let material = row.material().unwrap();
        assert_eq!(material.certificate(), issued.certificate());
        assert_eq!(material.private_key(), issued.private_key());
        assert_eq!(row.last_error(), Some("refused"));
    }

    #[test]
    fn certificate_row_round_trips_last_error() {
        let row = CertificateRow::default().with_error(
            "certificate policy names challenge kind dns-01 which this daemon cannot perform",
        );
        let encoded = row.encode().unwrap();
        let decoded = CertificateRow::decode(&encoded).unwrap();
        assert_eq!(decoded.last_error(), row.last_error());
        assert_eq!(CertificateRow::decode("{}").unwrap().last_error(), None);
    }

    #[test]
    fn certificate_challenge_reads_from_the_row_body() {
        let row = CertificateRow::decode(
            r#"{"certificate":"","private_key":"","challenge_token":"LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0","challenge_response":"LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}"#,
        )
        .unwrap();
        assert_eq!(row.material(), None);
        let challenge = row.challenge().unwrap();
        assert_eq!(
            challenge.token(),
            "LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0"
        );
        assert_eq!(
            challenge.response(),
            "LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        );
        assert!(CertificateChallenge::parse("", "").is_err());
        assert_eq!(CertificateRow::decode("{}").unwrap().challenge(), None);
    }

    #[test]
    fn certificate_row_round_trips_refusal_clock() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let clock = IssuanceClock::new(
            3,
            at,
            IssuanceFailure::Refused(ployz_core::Refusal::DoesNotResolve),
        );
        let row = CertificateRow::from_parts(None, None).with_backoff(
            "Ingress Hostname app.example.com does not resolve; it should resolve to 192.0.2.1.",
            clock,
        );
        let encoded = row.encode().unwrap();
        let decoded = CertificateRow::decode(&encoded).unwrap();
        assert_eq!(decoded.last_error(), row.last_error());
        assert_eq!(decoded.clock(), Some(clock));
        assert_eq!(
            CertificateRow::decode(
                r#"{"certificate":"","private_key":"","last_error":"later","next_attempt_at":"2023-11-14T22:13:20Z","failures":2,"last_failure":"reaches_elsewhere"}"#
            )
            .unwrap()
            .clock()
            .map(|clock| clock.last_failure()),
            Some(IssuanceFailure::Refused(ployz_core::Refusal::ReachesElsewhere))
        );
        assert_eq!(
            CertificateRow::decode(r#"{"last_failure":"authority"}"#)
                .unwrap()
                .clock(),
            None
        );
    }

    #[test]
    fn challenge_write_keeps_issued_material() {
        let issued = issued_material();
        let latest = CertificateRow::from_parts(
            Some((issued.clone(), ployz_core::ClusterRoute::Direct)),
            None,
        );
        let challenge = CertificateChallenge::parse("LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0", "LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        let row = latest.with_challenge(challenge.clone());
        assert_eq!(row.material(), Some(&issued));
        assert_eq!(row.challenge(), Some(&challenge));
    }

    #[test]
    fn invalid_stored_challenge_keeps_material_and_refusal_clock() {
        let material = issued_material();
        let clock = IssuanceClock::new(
            3,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            IssuanceFailure::Refused(ployz_core::Refusal::DoesNotResolve),
        );
        let row = CertificateRow::issued(material.clone(), ployz_core::ClusterRoute::Direct)
            .with_backoff("authority refused", clock);
        let mut body: serde_json::Value = serde_json::from_str(&row.encode().unwrap()).unwrap();
        let fields = body.as_object_mut().unwrap();
        fields.insert("challenge_token".to_owned(), "../escape".into());
        fields.insert("challenge_response".to_owned(), "injected\n}".into());
        let decoded = CertificateRow::decode(&body.to_string()).unwrap();
        assert_eq!(decoded.material(), Some(&material));
        assert!(decoded.challenge().is_none());
        assert_eq!(decoded.clock(), Some(clock));
        assert_eq!(
            decoded.last_error(),
            Some("authority refused; stored HTTP-01 challenge is invalid")
        );
    }

    #[test]
    fn error_write_keeps_issued_material() {
        let issued = issued_material();
        let row = CertificateRow::issued(issued.clone(), ployz_core::ClusterRoute::Direct)
            .with_error("refused");
        assert_eq!(row.material(), Some(&issued));
        assert_eq!(row.last_error(), Some("refused"));
        assert_eq!(row.clock(), None);
    }

    #[test]
    fn error_write_clears_a_previous_clock() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let clock = IssuanceClock::new(
            3,
            at,
            IssuanceFailure::Refused(ployz_core::Refusal::DoesNotResolve),
        );
        let row = CertificateRow::from_parts(None, None)
            .with_backoff("does not resolve", clock)
            .with_error("policy refused");
        assert_eq!(row.last_error(), Some("policy refused"));
        assert_eq!(row.clock(), None);
    }
}
