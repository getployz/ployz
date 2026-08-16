//! Certificate row body held in the replicated store.

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

/// Certificate and pending HTTP-01 challenge for one Ingress Hostname.
#[derive(Clone, Default, Debug, Eq, PartialEq)]
pub struct CertificateRow {
    material: Option<CertificateMaterial>,
    challenge: Option<CertificateChallenge>,
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
        }
    }

    /// Row that holds newly issued material and no challenge.
    #[must_use]
    pub fn issued(material: CertificateMaterial) -> Self {
        Self {
            material: Some(material),
            challenge: None,
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

    /// Take issued material out of the row.
    #[must_use]
    pub fn into_material(self) -> Option<CertificateMaterial> {
        self.material
    }

    /// Keep existing material and set the pending challenge.
    #[must_use]
    pub fn with_challenge(&self, challenge: CertificateChallenge) -> Self {
        Self {
            material: self.material.clone(),
            challenge: Some(challenge),
        }
    }

    pub(crate) fn decode(encoded: &str) -> Result<Self, Error> {
        if encoded.is_empty() {
            return Ok(Self::default());
        }
        let body: CertificateBody = serde_json::from_str(encoded)?;
        Ok(Self {
            material: CertificateMaterial::new(body.certificate, body.private_key),
            challenge: CertificateChallenge::new(body.challenge_token, body.challenge_response),
        })
    }

    pub(crate) fn encode(&self) -> Result<String, Error> {
        Ok(serde_json::to_string(&json!({
            "certificate": self.material.as_ref().map(CertificateMaterial::certificate).unwrap_or(""),
            "private_key": self.material.as_ref().map(CertificateMaterial::private_key).unwrap_or(""),
            "challenge_token": self.challenge.as_ref().map(CertificateChallenge::token).unwrap_or(""),
            "challenge_response": self.challenge.as_ref().map(CertificateChallenge::response).unwrap_or(""),
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
}

#[cfg(test)]
mod tests {
    use super::{CertificateChallenge, CertificateMaterial, CertificateRow};

    fn decode_material(encoded: &str) -> Result<Option<CertificateMaterial>, super::Error> {
        Ok(CertificateRow::decode(encoded)?.material)
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
        let material =
            decode_material(r#"{"certificate":"CERT","private_key":"KEY","last_error":"later"}"#)
                .unwrap()
                .unwrap();
        assert_eq!(material.certificate(), "CERT");
        assert_eq!(material.private_key(), "KEY");
    }

    #[test]
    fn certificate_challenge_reads_from_the_row_body() {
        let row = CertificateRow::decode(
            r#"{"certificate":"","private_key":"","challenge_token":"tok","challenge_response":"tok.thumb"}"#,
        )
        .unwrap();
        assert_eq!(row.material, None);
        let challenge = row.challenge.unwrap();
        assert_eq!(challenge.token(), "tok");
        assert_eq!(challenge.response(), "tok.thumb");
        assert_eq!(CertificateChallenge::new("", "tok.thumb"), None);
        assert_eq!(CertificateRow::decode("{}").unwrap().challenge, None);
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
    }
}
