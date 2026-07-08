use std::fmt;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::core_types::*;
use crate::machine::{MachineJoinRedeemResult, MachineJoinToken};

pub const CLOUD_BOOTSTRAP_PROTOCOL_VERSION: u16 = 1;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(type = "Brand<string, \"CloudBootstrapSessionSecret\">")]
#[serde(transparent)]
pub struct CloudBootstrapSessionSecret(String);

impl CloudBootstrapSessionSecret {
    pub fn try_new(value: impl Into<String>) -> Result<Self, CloudBootstrapSecretError> {
        let value = value.into();
        validate_cloud_bootstrap_secret(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CloudBootstrapSessionSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CloudBootstrapSessionSecret([redacted])")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(type = "Brand<string, \"CloudBootstrapCallbackToken\">")]
#[serde(transparent)]
pub struct CloudBootstrapCallbackToken(String);

impl CloudBootstrapCallbackToken {
    pub fn try_new(value: impl Into<String>) -> Result<Self, CloudBootstrapSecretError> {
        let value = value.into();
        validate_cloud_bootstrap_secret(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CloudBootstrapCallbackToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CloudBootstrapCallbackToken([redacted])")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(type = "Brand<string, \"CloudBootstrapRedemptionId\">")]
#[serde(transparent)]
pub struct CloudBootstrapRedemptionId(String);

impl CloudBootstrapRedemptionId {
    pub fn try_new(value: impl Into<String>) -> Result<Self, CloudBootstrapSecretError> {
        let value = value.into();
        validate_cloud_bootstrap_secret(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CloudBootstrapRedemptionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("CloudBootstrapRedemptionId")
            .field(&self.0)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(type = "Brand<string, \"CloudBootstrapAttemptId\">")]
#[serde(transparent)]
pub struct CloudBootstrapAttemptId(String);

impl CloudBootstrapAttemptId {
    pub fn try_new(value: impl Into<String>) -> Result<Self, CloudBootstrapSecretError> {
        let value = value.into();
        validate_cloud_bootstrap_secret(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CloudBootstrapAttemptId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("CloudBootstrapAttemptId")
            .field(&self.0)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudBootstrapSecretError {
    Empty,
    Invalid,
}

fn validate_cloud_bootstrap_secret(value: &str) -> Result<(), CloudBootstrapSecretError> {
    if value.is_empty() {
        return Err(CloudBootstrapSecretError::Empty);
    }
    if value
        .chars()
        .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(CloudBootstrapSecretError::Invalid);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CloudBootstrapClientInfo {
    pub protocol_version: u16,
    pub keeper_version: String,
}

impl CloudBootstrapClientInfo {
    #[must_use]
    pub fn current(keeper_version: impl Into<String>) -> Self {
        Self {
            protocol_version: CLOUD_BOOTSTRAP_PROTOCOL_VERSION,
            keeper_version: keeper_version.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CloudBootstrapMachineFacts {
    pub hostname: Option<String>,
    pub os: String,
    pub arch: String,
    pub candidate_runtime_nats_url: Option<MachineJoinRuntimeNatsUrl>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CloudBootstrapSessionCreateRequest {
    pub attempt_id: CloudBootstrapAttemptId,
    pub client: CloudBootstrapClientInfo,
    pub machine: CloudBootstrapMachineFacts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CloudBootstrapSessionCreated {
    pub browser_url: String,
    pub user_code: String,
    pub session_secret: CloudBootstrapSessionSecret,
    pub poll_after_seconds: u16,
    pub expires_at_unix_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CloudBootstrapSessionPollRequest {
    pub attempt_id: CloudBootstrapAttemptId,
    pub session_secret: CloudBootstrapSessionSecret,
    pub machine: CloudBootstrapMachineFacts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudBootstrapDecision {
    Pending {
        retry_after_seconds: u16,
    },
    Ready {
        envelope: Box<CloudBootstrapEnvelope>,
    },
    Expired,
    Failed {
        failure: CloudBootstrapDecisionFailure,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "failure", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudBootstrapDecisionFailure {
    UnsupportedClient {
        message: FailureMessage,
        minimum_protocol_version: u16,
    },
    Unauthorized,
    AlreadyConsumedByPolicy,
    InvalidMachineFacts {
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CloudBootstrapEnvelope {
    pub attempt_id: CloudBootstrapAttemptId,
    pub redemption_id: CloudBootstrapRedemptionId,
    pub callback_url: String,
    pub callback_token: CloudBootstrapCallbackToken,
    pub intent: CloudBootstrapIntent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "intent", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudBootstrapIntent {
    Founder { founder: Box<CloudFounderBootstrap> },
    Joiner { joiner: Box<CloudJoinerBootstrap> },
    WaitForFounder { retry_after_seconds: u16 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CloudFounderBootstrap {
    pub runtime_nats_url: MachineJoinRuntimeNatsUrl,
    pub cloud_nats_user_public_key: NatsUserPublicKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CloudJoinerBootstrap {
    pub runtime_nats_url: MachineJoinRuntimeNatsUrl,
    pub trusted_nats: MachineJoinTrustedNats,
    pub join_token: MachineJoinToken,
    pub join_secret_delivery: MachineJoinSecretDelivery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CloudBootstrapCallbackRequest {
    pub attempt_id: CloudBootstrapAttemptId,
    pub redemption_id: CloudBootstrapRedemptionId,
    pub outcome: CloudBootstrapOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudBootstrapOutcome {
    FounderSucceeded { result: CloudFounderBootstrapResult },
    JoinerSucceeded { result: CloudJoinerBootstrapResult },
    Failed { failure: CloudBootstrapFailure },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CloudFounderBootstrapResult {
    pub machine_id: MachineId,
    pub runtime_nats_url: MachineJoinRuntimeNatsUrl,
    pub trusted_nats: MachineJoinTrustedNats,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CloudJoinerBootstrapResult {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub name: MachineName,
    pub last_event_sequence: EventSequence,
    pub result: MachineJoinRedeemResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "failure", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudBootstrapFailure {
    AlreadyBootstrapped,
    EnvelopeInvalid { message: FailureMessage },
    BootstrapFailed { message: FailureMessage },
    CloudReachabilityFailed { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CloudBootstrapCallbackAccepted {
    pub accepted_at_unix_seconds: u64,
}
