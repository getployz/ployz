//! Certificate renewal planning.

use ployz_core::cert::{
    AcmeChallengeStateKey, AcmeHttp01Challenge, AcmeLockKey, ActiveCertState, CertBundleRef,
    CertStateKey, CertValidityWindow,
};
use ployz_core::ids::CertId;
use ployz_core::ops::{CertOperationFailure, FailureMessage};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertChallengeCommand {
    pub cert_id: CertId,
    pub challenge: AcmeHttp01Challenge,
    pub current: Option<ActiveCertState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertFinishCommand {
    pub cert_id: CertId,
    pub hostname: ployz_core::ops::RouteHostname,
    pub current: Option<ActiveCertState>,
    pub acme_result: AcmeFinishResult,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcmeFinishResult {
    Issued {
        bundle_ref: CertBundleRef,
        validity: CertValidityWindow,
    },
    Failed {
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertChallengePlan {
    pub lock_key: AcmeLockKey,
    pub challenge_write: AcmeChallengeWrite,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertFinishPlan {
    Completed { active_cert_write: ActiveCertWrite },
    Failed { failure: CertOperationFailure },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcmeChallengeWrite {
    pub key: AcmeChallengeStateKey,
    pub challenge: AcmeHttp01Challenge,
}

impl AcmeChallengeWrite {
    #[must_use]
    pub const fn ttl_seconds(&self) -> ployz_core::cert::AcmeChallengeTtlSeconds {
        self.challenge.ttl_seconds()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveCertWrite {
    pub key: CertStateKey,
    pub state: ActiveCertState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertRenewalPlanError {
    CurrentCertMismatch {
        expected: CertId,
        actual: CertId,
    },
    CurrentHostnameMismatch {
        expected: ployz_core::ops::RouteHostname,
        actual: ployz_core::ops::RouteHostname,
    },
}

pub fn plan_cert_challenge(
    command: CertChallengeCommand,
) -> Result<CertChallengePlan, CertRenewalPlanError> {
    validate_current_cert(
        &command.cert_id,
        command.challenge.hostname(),
        command.current.as_ref(),
    )?;

    let lock_key = AcmeLockKey::from_cert_id(&command.cert_id);
    let challenge_write = AcmeChallengeWrite {
        key: AcmeChallengeStateKey::from_cert_id(&command.cert_id),
        challenge: command.challenge.clone(),
    };
    Ok(CertChallengePlan {
        lock_key,
        challenge_write,
    })
}

pub fn plan_cert_finish(
    command: CertFinishCommand,
) -> Result<CertFinishPlan, CertRenewalPlanError> {
    validate_current_cert(
        &command.cert_id,
        &command.hostname,
        command.current.as_ref(),
    )?;

    match command.acme_result {
        AcmeFinishResult::Issued {
            bundle_ref,
            validity,
        } => {
            let state = ActiveCertState {
                cert_id: command.cert_id.clone(),
                hostname: command.hostname,
                bundle_ref,
                validity,
            };
            Ok(CertFinishPlan::Completed {
                active_cert_write: ActiveCertWrite {
                    key: CertStateKey::from_cert_id(&command.cert_id),
                    state,
                },
            })
        }
        AcmeFinishResult::Failed { message } => Ok(CertFinishPlan::Failed {
            failure: CertOperationFailure::AcmeValidationFailed {
                cert_id: command.cert_id,
                message,
                retained_active_cert: command.current,
            },
        }),
    }
}

fn validate_current_cert(
    cert_id: &CertId,
    hostname: &ployz_core::ops::RouteHostname,
    current: Option<&ActiveCertState>,
) -> Result<(), CertRenewalPlanError> {
    let Some(current) = current else {
        return Ok(());
    };

    if current.cert_id != *cert_id {
        return Err(CertRenewalPlanError::CurrentCertMismatch {
            expected: cert_id.clone(),
            actual: current.cert_id.clone(),
        });
    }

    if current.hostname != *hostname {
        return Err(CertRenewalPlanError::CurrentHostnameMismatch {
            expected: hostname.clone(),
            actual: current.hostname.clone(),
        });
    }

    Ok(())
}
