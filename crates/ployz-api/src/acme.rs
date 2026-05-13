use ployz_model::{AcmeChallengeReadinessRecord, AcmeChallengeRecord, CertificateRecord};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcmeHttp01StatusPayload {
    pub hostname: String,
    pub certificate: Option<CertificateRecord>,
    pub challenges: Vec<AcmeHttp01ChallengeStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcmeHttp01ChallengeStatus {
    pub challenge: AcmeChallengeRecord,
    pub readiness: Vec<AcmeChallengeReadinessRecord>,
}
