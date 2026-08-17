//! Certificate row body held in the replicated store.

use std::time::SystemTime;

use chrono::{DateTime, SecondsFormat, Utc};
use ployz_core::{IssuanceClock, IssuanceFailure};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::Error;

/// Certificate and private key held in cluster state for one Ingress Hostname.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CertificateMaterial {
    certificate: String,
    private_key: String,
}

impl CertificateMaterial {
    #[must_use]
    pub fn new(certificate: impl Into<String>, private_key: impl Into<String>) -> Option<Self> {
        let certificate = certificate.into();
        let private_key = private_key.into();
        (!certificate.is_empty() && !private_key.is_empty()).then_some(Self {
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertificateChallenge {
    token: String,
    response: String,
}

impl CertificateChallenge {
    #[must_use]
    pub fn new(token: impl Into<String>, response: impl Into<String>) -> Option<Self> {
        let token = token.into();
        let response = response.into();
        (!token.is_empty() && !response.is_empty()).then_some(Self { token, response })
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
        Ok(Self {
            material: CertificateMaterial::new(body.certificate, body.private_key),
            challenge: CertificateChallenge::new(body.challenge_token, body.challenge_response),
            refusal: (!body.last_error.is_empty()).then_some(RecordedRefusal {
                reason: body.last_error,
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

    fn decode_material(encoded: &str) -> Result<Option<CertificateMaterial>, super::Error> {
        Ok(CertificateRow::decode(encoded)?.into_material())
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
        let row = CertificateRow::decode(
            r#"{"certificate":"CERT","private_key":"KEY","last_error":"refused","future":1}"#,
        )
        .unwrap();
        let material = row.material().unwrap();
        assert_eq!(material.certificate(), "CERT");
        assert_eq!(material.private_key(), "KEY");
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
            r#"{"certificate":"","private_key":"","challenge_token":"tok","challenge_response":"tok.thumb"}"#,
        )
        .unwrap();
        assert_eq!(row.material(), None);
        let challenge = row.challenge().unwrap();
        assert_eq!(challenge.token(), "tok");
        assert_eq!(challenge.response(), "tok.thumb");
        assert_eq!(CertificateChallenge::new("", "tok.thumb"), None);
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
        let issued = CertificateMaterial::new("CERT", "KEY").unwrap();
        let latest = CertificateRow::from_parts(Some(issued.clone()), None);
        let challenge = CertificateChallenge::new("tok", "tok.thumb").unwrap();
        let row = latest.with_challenge(challenge.clone());
        assert_eq!(row.material(), Some(&issued));
        assert_eq!(row.challenge(), Some(&challenge));
    }

    #[test]
    fn issued_write_replaces_the_row() {
        let issued = CertificateMaterial::new("CERT", "KEY").unwrap();
        let row = CertificateRow::issued(issued.clone());
        assert_eq!(row.material(), Some(&issued));
        assert_eq!(row.challenge(), None);
        assert_eq!(row.last_error(), None);
        assert_eq!(row.clock(), None);
    }

    #[test]
    fn error_write_keeps_issued_material() {
        let issued = CertificateMaterial::new("CERT", "KEY").unwrap();
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
