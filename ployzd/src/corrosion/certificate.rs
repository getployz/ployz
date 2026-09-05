//! Certificate row body held in the replicated store.

use std::time::SystemTime;

use chrono::{DateTime, SecondsFormat, Utc};
use ployz_core::{IssuanceClock, IssuanceFailure};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::Error;

/// Parseable certificate chain paired with its private key.
///
/// Admission does not prove trust, hostname coverage, validity dates, or proxy adoption.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CertificateMaterial {
    certificate: String,
    private_key: String,
}

impl CertificateMaterial {
    #[must_use]
    pub fn new(certificate: impl Into<String>, private_key: impl Into<String>) -> Option<Self> {
        use rcgen::PublicKeyData as _;
        use x509_parser::pem::Pem;

        let certificate = certificate.into();
        let private_key = private_key.into();
        let key = rcgen::KeyPair::from_pem(&private_key).ok()?;
        let mut chain = Pem::iter_from_buffer(certificate.as_bytes());
        let leaf = chain.next()?.ok()?;
        if leaf.label != "CERTIFICATE"
            || leaf.parse_x509().ok()?.public_key().raw != key.subject_public_key_info()
        {
            return None;
        }
        for certificate in chain {
            let certificate = certificate.ok()?;
            if certificate.label != "CERTIFICATE" {
                return None;
            }
            certificate.parse_x509().ok()?;
        }
        Some(Self {
            certificate,
            private_key,
        })
    }

    #[must_use]
    pub fn certificate(&self) -> &str {
        &self.certificate
    }

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

impl CertificateChallenge {
    #[must_use]
    pub fn new(token: impl Into<String>, response: impl Into<String>) -> Option<Self> {
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
        let (prefix, thumbprint) = response.split_once('.')?;
        // The final base64 symbol of a 32-byte digest has two zero padding bits.
        if token.len() < 22
            || !base64url(&token)
            || prefix != token
            || thumbprint.len() != 43
            || !base64url(thumbprint)
            || !b"AEIMQUYcgkosw048".contains(thumbprint.as_bytes().last()?)
        {
            return None;
        }
        Some(Self { token, response })
    }

    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    #[must_use]
    pub fn response(&self) -> &str {
        &self.response
    }
}

/// Operator-visible reason, with a shared clock only when issuance is backing off.
#[derive(Clone, Debug, Eq, PartialEq)]
struct RecordedRefusal {
    reason: String,
    clock: Option<IssuanceClock>,
}

/// Replicated certificate row for one Ingress Hostname.
#[derive(Clone, Default, Debug, Eq, PartialEq)]
pub struct CertificateRow {
    material: Option<CertificateMaterial>,
    challenge: Option<CertificateChallenge>,
    refusal: Option<RecordedRefusal>,
}

impl CertificateRow {
    /// Stored snapshot for one hostname.
    #[must_use]
    pub fn from_parts(
        material: Option<CertificateMaterial>,
        challenge: Option<CertificateChallenge>,
    ) -> Self {
        Self {
            material,
            challenge,
            refusal: None,
        }
    }

    /// Row that holds newly issued material and no challenge.
    #[must_use]
    pub fn issued(material: CertificateMaterial) -> Self {
        Self {
            material: Some(material),
            challenge: None,
            refusal: None,
        }
    }

    /// Attach a complete refusal clock, or leave the row unchanged if the text is empty.
    #[must_use]
    pub fn with_backoff(self, last_error: impl Into<String>, clock: IssuanceClock) -> Self {
        let reason = last_error.into();
        if reason.is_empty() {
            return self;
        }
        Self {
            refusal: Some(RecordedRefusal {
                reason,
                clock: Some(clock),
            }),
            ..self
        }
    }

    /// Issued material, if any.
    #[must_use]
    pub fn material(&self) -> Option<&CertificateMaterial> {
        self.material.as_ref()
    }

    /// Pending HTTP-01 challenge, if any.
    #[must_use]
    pub fn challenge(&self) -> Option<&CertificateChallenge> {
        self.challenge.as_ref()
    }

    /// Last recorded refusal or issuance error, if any.
    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        self.refusal.as_ref().map(|refusal| refusal.reason.as_str())
    }

    /// Shared backoff clock, if a complete refusal has been recorded.
    #[must_use]
    pub fn clock(&self) -> Option<IssuanceClock> {
        self.refusal.as_ref().and_then(|refusal| refusal.clock)
    }

    /// Take issued material out of the row.
    #[must_use]
    pub fn into_material(self) -> Option<CertificateMaterial> {
        self.material
    }

    /// Keep existing material and set the pending challenge.
    #[must_use]
    pub fn with_challenge(self, challenge: CertificateChallenge) -> Self {
        Self {
            challenge: Some(challenge),
            ..self
        }
    }

    /// Keep existing material and challenge and record a refusal reason.
    #[must_use]
    pub fn with_error(self, reason: impl Into<String>) -> Self {
        Self {
            refusal: Some(RecordedRefusal {
                reason: reason.into(),
                clock: None,
            }),
            ..self
        }
    }

    pub(crate) fn decode(encoded: &str) -> Result<Self, Error> {
        if encoded.is_empty() {
            return Ok(Self::default());
        }
        let body: CertificateBody = serde_json::from_str(encoded)?;
        let clock = decode_clock(&body.next_attempt_at, body.failures, &body.last_failure);
        let has_material = !body.certificate.is_empty() || !body.private_key.is_empty();
        let material = CertificateMaterial::new(body.certificate, body.private_key);
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
        let challenge = CertificateChallenge::new(body.challenge_token, body.challenge_response);
        if has_challenge && challenge.is_none() {
            if !last_error.is_empty() {
                last_error.push_str("; ");
            }
            last_error.push_str("stored HTTP-01 challenge is invalid");
        }
        Ok(Self {
            material,
            challenge,
            refusal: (!last_error.is_empty()).then_some(RecordedRefusal {
                reason: last_error,
                clock,
            }),
        })
    }

    pub(crate) fn encode(&self) -> Result<String, Error> {
        Ok(serde_json::to_string(&json!({
            "certificate": self.material.as_ref().map(CertificateMaterial::certificate).unwrap_or(""),
            "private_key": self.material.as_ref().map(CertificateMaterial::private_key).unwrap_or(""),
            "challenge_token": self.challenge.as_ref().map(CertificateChallenge::token).unwrap_or(""),
            "challenge_response": self.challenge.as_ref().map(CertificateChallenge::response).unwrap_or(""),
            "last_error": self.last_error().unwrap_or(""),
            "next_attempt_at": self.clock().map(|clock| encode_attempt(clock.next_attempt_at())).unwrap_or_default(),
            "failures": self.clock().map(|clock| clock.failures()).unwrap_or(0),
            "last_failure": encode_failure(self.clock().map(|clock| clock.last_failure())),
        }))?)
    }
}

#[derive(Deserialize)]
struct CertificateBody {
    #[serde(default)]
    certificate: String,
    #[serde(default)]
    private_key: String,
    #[serde(default)]
    challenge_token: String,
    #[serde(default)]
    challenge_response: String,
    #[serde(default)]
    last_error: String,
    #[serde(default)]
    next_attempt_at: String,
    #[serde(default)]
    failures: u32,
    #[serde(default)]
    last_failure: String,
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

fn encode_failure(failure: Option<IssuanceFailure>) -> &'static str {
    match failure {
        Some(IssuanceFailure::DoesNotResolve) => "does_not_resolve",
        Some(IssuanceFailure::ResolvesElsewhere) => "resolves_elsewhere",
        Some(IssuanceFailure::Authority) => "authority",
        None => "",
    }
}

fn decode_failure(text: &str) -> Option<IssuanceFailure> {
    match text {
        "does_not_resolve" => Some(IssuanceFailure::DoesNotResolve),
        "resolves_elsewhere" => Some(IssuanceFailure::ResolvesElsewhere),
        "authority" => Some(IssuanceFailure::Authority),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use ployz_core::{IssuanceClock, IssuanceFailure};

    use super::{CertificateChallenge, CertificateMaterial, CertificateRow};

    #[test]
    fn challenge_admission_rejects_invalid_grammar_and_token_mismatch() {
        let token = "LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0";
        let thumbprint = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let response = format!("{token}.{thumbprint}");
        assert!(CertificateChallenge::new(token, &response).is_some());
        let minimum_token = "_-0123456789abcdefghij";
        assert!(
            CertificateChallenge::new(minimum_token, format!("{minimum_token}.{thumbprint}"))
                .is_some()
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
                CertificateChallenge::new(&bad_token, format!("{bad_token}.{thumbprint}"))
                    .is_none(),
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
                CertificateChallenge::new(token, &bad_response).is_none(),
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
        CertificateMaterial::new(pair.cert.pem(), pair.signing_key.serialize_pem()).unwrap()
    }

    fn decode_material(encoded: &str) -> Result<Option<CertificateMaterial>, super::Error> {
        Ok(CertificateRow::decode(encoded)?.into_material())
    }

    #[test]
    fn certificate_material_rejects_garbage_and_mismatched_keys() {
        assert!(CertificateMaterial::new("CERT", "KEY").is_none());
        let first = rcgen::generate_simple_self_signed(["example.com".to_owned()]).unwrap();
        let second = rcgen::generate_simple_self_signed(["example.com".to_owned()]).unwrap();
        assert!(
            CertificateMaterial::new(first.cert.pem(), second.signing_key.serialize_pem())
                .is_none()
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
            assert!(CertificateMaterial::new(certificate.clone(), private_key.clone()).is_some());
            let chain = format!("{certificate}{certificate}");
            assert!(CertificateMaterial::new(chain, private_key.clone()).is_some());
            let broken_chain = format!(
                "{certificate}-----BEGIN CERTIFICATE-----\nZ2FyYmFnZQ==\n-----END CERTIFICATE-----\n"
            );
            assert!(CertificateMaterial::new(broken_chain, private_key).is_none());
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

    #[test]
    fn invalid_certificate_body_is_an_error() {
        assert!(decode_material("{").is_err());
        assert!(decode_material("null").is_err());
    }

    #[test]
    fn empty_certificate_body_is_not_present() {
        assert_eq!(CertificateMaterial::new("", ""), None);
        assert_eq!(CertificateMaterial::new("CERT", ""), None);
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
        assert_eq!(
            CertificateChallenge::new(
                "",
                "LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
            ),
            None
        );
        assert_eq!(CertificateRow::decode("{}").unwrap().challenge(), None);
    }

    #[test]
    fn certificate_row_round_trips_refusal_clock() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let clock = IssuanceClock::new(3, at, IssuanceFailure::DoesNotResolve);
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
                r#"{"certificate":"","private_key":"","last_error":"later","next_attempt_at":"2023-11-14T22:13:20Z","failures":2,"last_failure":"resolves_elsewhere"}"#
            )
            .unwrap()
            .clock()
            .map(|clock| clock.last_failure()),
            Some(IssuanceFailure::ResolvesElsewhere)
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
        let latest = CertificateRow::from_parts(Some(issued.clone()), None);
        let challenge = CertificateChallenge::new("LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0", "LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
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
            IssuanceFailure::DoesNotResolve,
        );
        let row = CertificateRow::issued(material.clone()).with_backoff("authority refused", clock);
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
    fn issued_write_replaces_the_row() {
        let issued = issued_material();
        let row = CertificateRow::issued(issued.clone());
        assert_eq!(row.material(), Some(&issued));
        assert_eq!(row.challenge(), None);
        assert_eq!(row.last_error(), None);
        assert_eq!(row.clock(), None);
    }

    #[test]
    fn error_write_keeps_issued_material() {
        let issued = issued_material();
        let row = CertificateRow::issued(issued.clone()).with_error("refused");
        assert_eq!(row.material(), Some(&issued));
        assert_eq!(row.last_error(), Some("refused"));
        assert_eq!(row.clock(), None);
    }

    #[test]
    fn error_write_clears_a_previous_clock() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let clock = IssuanceClock::new(3, at, IssuanceFailure::DoesNotResolve);
        let row = CertificateRow::from_parts(None, None)
            .with_backoff("does not resolve", clock)
            .with_error("policy refused");
        assert_eq!(row.last_error(), Some("policy refused"));
        assert_eq!(row.clock(), None);
    }
}
