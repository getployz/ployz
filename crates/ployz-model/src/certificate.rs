use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcmeAccountRecord {
    pub account_id: String,
    pub issuer_url: String,
    pub contact_email: Option<String>,
    // SECURITY: serialized `instant_acme::AccountCredentials` containing the
    // account private key. Safe only while replication stays inside the
    // WireGuard mesh and local store files are not backed up unencrypted;
    // revisit if either assumption changes.
    pub account_credentials_json: String,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
pub enum CertificateState {
    #[display("pending")]
    #[strum(serialize = "pending")]
    Pending,
    #[display("issuing")]
    #[strum(serialize = "issuing")]
    Issuing,
    #[display("active")]
    #[strum(serialize = "active")]
    Active,
    #[display("renewal_due")]
    #[strum(serialize = "renewal_due")]
    RenewalDue,
    #[display("failed")]
    #[strum(serialize = "failed")]
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CertificateLifecycle {
    Pending {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_error: Option<String>,
    },
    Issuing {
        order_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_version_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_error: Option<String>,
    },
    Active {
        active_version_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_renewal_at: Option<u64>,
    },
    RenewalDue {
        active_version_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_renewal_at: Option<u64>,
    },
    Failed {
        last_error: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_version_id: Option<String>,
    },
}

impl CertificateLifecycle {
    #[must_use]
    pub fn phase(&self) -> CertificateState {
        match self {
            Self::Pending { .. } => CertificateState::Pending,
            Self::Issuing { .. } => CertificateState::Issuing,
            Self::Active { .. } => CertificateState::Active,
            Self::RenewalDue { .. } => CertificateState::RenewalDue,
            Self::Failed { .. } => CertificateState::Failed,
        }
    }

    #[must_use]
    pub fn active_version_id(&self) -> Option<&str> {
        match self {
            Self::Pending { .. } => None,
            Self::Issuing {
                active_version_id, ..
            }
            | Self::Failed {
                active_version_id, ..
            } => active_version_id.as_deref(),
            Self::Active {
                active_version_id, ..
            }
            | Self::RenewalDue {
                active_version_id, ..
            } => Some(active_version_id.as_str()),
        }
    }

    #[must_use]
    pub fn order_url(&self) -> Option<&str> {
        match self {
            Self::Issuing { order_url, .. } => Some(order_url.as_str()),
            Self::Pending { .. }
            | Self::Active { .. }
            | Self::RenewalDue { .. }
            | Self::Failed { .. } => None,
        }
    }

    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        match self {
            Self::Pending { last_error } | Self::Issuing { last_error, .. } => {
                last_error.as_deref()
            }
            Self::Failed { last_error, .. } => Some(last_error.as_str()),
            Self::Active { .. } | Self::RenewalDue { .. } => None,
        }
    }

    #[must_use]
    pub fn next_renewal_at(&self) -> Option<u64> {
        match self {
            Self::Active {
                next_renewal_at, ..
            }
            | Self::RenewalDue {
                next_renewal_at, ..
            } => *next_renewal_at,
            Self::Pending { .. } | Self::Issuing { .. } | Self::Failed { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificateStateTransition {
    pub goal: CertificateStateGoal,
    pub evidence: CertificateTransitionEvidence,
    pub at_unix_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "goal", rename_all = "snake_case")]
pub enum CertificateStateGoal {
    StartIssuing {
        order_url: String,
    },
    MarkOrderFailed {
        error: String,
    },
    FinalizeActive {
        active_version_id: String,
        next_renewal_at: Option<u64>,
    },
    KeepIssuingAfterRetryableFailure {
        error: String,
    },
    MarkFinalizeFailed {
        error: String,
        previous_active_version_id: Option<String>,
    },
    MarkRenewalDue,
    ResetStalledIssuing {
        error: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CertificateTransitionEvidence {
    AcmeOrderStart { hostname: String },
    AcmeFinalize { hostname: String },
    RenewalScheduler { hostname: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertificateTransitionOutcome {
    Applied,
    AlreadyInState,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct CertificateTransitionError {
    code: &'static str,
    message: String,
}

impl CertificateTransitionError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "INVALID_TRANSITION",
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificateVersion {
    pub version_id: String,
    pub fullchain_pem: String,
    // SECURITY: leaf private key in PEM form, replicated as plaintext JSON
    // through the certificate store. Safe only under the WireGuard-only
    // replication + no-unencrypted-backup assumption documented on the
    // schema; revisit if either assumption changes.
    pub private_key_pem: String,
    pub not_before: Option<u64>,
    pub not_after: Option<u64>,
    pub issued_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificateRecord {
    pub hostname: String,
    pub issuer_url: String,
    pub account_id: String,
    pub lifecycle: CertificateLifecycle,
    pub versions: Vec<CertificateVersion>,
    pub requested_at: u64,
    pub updated_at: u64,
}

impl CertificateRecord {
    #[must_use]
    pub fn state(&self) -> CertificateState {
        self.lifecycle.phase()
    }

    #[must_use]
    pub fn active_version_id(&self) -> Option<&str> {
        self.lifecycle.active_version_id()
    }

    #[must_use]
    pub fn order_url(&self) -> Option<&str> {
        self.lifecycle.order_url()
    }

    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        self.lifecycle.last_error()
    }

    #[must_use]
    pub fn next_renewal_at(&self) -> Option<u64> {
        self.lifecycle.next_renewal_at()
    }

    pub fn apply_state_transition(
        &mut self,
        transition: CertificateStateTransition,
    ) -> Result<CertificateTransitionOutcome, CertificateTransitionError> {
        let CertificateStateTransition {
            goal,
            evidence: _,
            at_unix_secs,
        } = transition;
        match goal {
            CertificateStateGoal::StartIssuing { order_url } => {
                if self.state() == CertificateState::Issuing
                    && self.order_url() == Some(order_url.as_str())
                {
                    return Ok(CertificateTransitionOutcome::AlreadyInState);
                }
                match self.lifecycle.clone() {
                    CertificateLifecycle::Pending { .. }
                    | CertificateLifecycle::Failed { .. }
                    | CertificateLifecycle::RenewalDue { .. } => {
                        let active_version_id =
                            self.lifecycle.active_version_id().map(ToString::to_string);
                        self.lifecycle = CertificateLifecycle::Issuing {
                            order_url,
                            active_version_id,
                            last_error: None,
                        };
                    }
                    CertificateLifecycle::Issuing { .. } | CertificateLifecycle::Active { .. } => {
                        return Err(CertificateTransitionError::invalid(format!(
                            "certificate '{}' cannot start issuing from state {}",
                            self.hostname,
                            self.state()
                        )));
                    }
                }
            }
            CertificateStateGoal::MarkOrderFailed { error } => {
                self.lifecycle = CertificateLifecycle::Failed {
                    last_error: error,
                    active_version_id: self.active_version_id().map(ToString::to_string),
                };
            }
            CertificateStateGoal::FinalizeActive {
                active_version_id,
                next_renewal_at,
            } => {
                if self.state() != CertificateState::Issuing {
                    return Err(CertificateTransitionError::invalid(format!(
                        "certificate '{}' must be issuing before finalize; current state is {}",
                        self.hostname,
                        self.state()
                    )));
                }
                self.lifecycle = CertificateLifecycle::Active {
                    active_version_id,
                    next_renewal_at,
                };
            }
            CertificateStateGoal::KeepIssuingAfterRetryableFailure { error } => {
                let CertificateLifecycle::Issuing {
                    order_url,
                    active_version_id,
                    ..
                } = self.lifecycle.clone()
                else {
                    return Err(CertificateTransitionError::invalid(format!(
                        "certificate '{}' must be issuing before retryable finalize failure; current state is {}",
                        self.hostname,
                        self.state()
                    )));
                };
                self.lifecycle = CertificateLifecycle::Issuing {
                    order_url,
                    active_version_id,
                    last_error: Some(error),
                };
            }
            CertificateStateGoal::MarkFinalizeFailed {
                error,
                previous_active_version_id,
            } => {
                if self.state() != CertificateState::Issuing {
                    return Err(CertificateTransitionError::invalid(format!(
                        "certificate '{}' must be issuing before finalize failure; current state is {}",
                        self.hostname,
                        self.state()
                    )));
                }
                self.lifecycle = CertificateLifecycle::Failed {
                    last_error: error,
                    active_version_id: previous_active_version_id,
                };
            }
            CertificateStateGoal::MarkRenewalDue => {
                if self.state() == CertificateState::RenewalDue {
                    return Ok(CertificateTransitionOutcome::AlreadyInState);
                }
                let CertificateLifecycle::Active {
                    active_version_id,
                    next_renewal_at,
                } = self.lifecycle.clone()
                else {
                    return Err(CertificateTransitionError::invalid(format!(
                        "certificate '{}' must be active before renewal due; current state is {}",
                        self.hostname,
                        self.state()
                    )));
                };
                self.lifecycle = CertificateLifecycle::RenewalDue {
                    active_version_id,
                    next_renewal_at,
                };
            }
            CertificateStateGoal::ResetStalledIssuing { error } => {
                if self.state() != CertificateState::Issuing {
                    return Err(CertificateTransitionError::invalid(format!(
                        "certificate '{}' must be issuing before stalled reset; current state is {}",
                        self.hostname,
                        self.state()
                    )));
                }
                self.lifecycle = CertificateLifecycle::Pending {
                    last_error: Some(error),
                };
            }
        }
        self.updated_at = at_unix_secs;
        Ok(CertificateTransitionOutcome::Applied)
    }

    /// The currently-installable version, if any. Independent of `state`:
    /// renewal transitions a healthy cert through `RenewalDue → Issuing` and
    /// `active_version_id` keeps pointing at the existing leaf the whole way;
    /// a non-retryable finalize failure explicitly restores the previous
    /// `active_version_id` so callers can keep serving the old cert. TLS
    /// consumers should ask the type for material here, not gate on `state`.
    #[must_use]
    pub fn installed_version(&self) -> Option<&CertificateVersion> {
        let id = self.active_version_id()?;
        self.versions
            .iter()
            .find(|version| version.version_id == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcmeChallengeRecord {
    pub hostname: String,
    pub token: String,
    // SECURITY: HTTP-01 key authorization is the secret an ACME verifier must
    // echo back. Replicated as plaintext JSON. Safe only under the WireGuard-
    // only replication + no-unencrypted-backup assumption documented on the
    // schema; revisit if either assumption changes.
    pub key_authorization: String,
    pub expires_at: u64,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcmeChallengeReadinessRecord {
    pub hostname: String,
    pub token: String,
    pub machine_id: MachineId,
    pub observed_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainDnsAdvice {
    pub hostname: String,
    pub resolved_ips: Vec<IpAddr>,
    pub recommended_ips: Vec<IpAddr>,
}
