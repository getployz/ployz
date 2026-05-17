use std::collections::BTreeSet;
use std::fmt::{self, Display, Formatter};

use mvp_lease::{
    LeaseAcquirePolicy, LeaseBook, LeaseCommandContext, LeaseContentHash, LeaseEpoch, LeaseError,
    LeaseGuard, LeaseHolder, LeaseRelease, LeaseResource, LeaseState, LeaseTimestamp, VisibleNode,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MIN_ACME_TOKEN_LEN: usize = 22;
const MAX_ACME_KEY_AUTHORIZATION_LEN: usize = 512;
const MAX_HOSTNAME_LEN: usize = 253;
const MAX_HOSTNAME_LABEL_LEN: usize = 63;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct AcmeHostname(String);

impl AcmeHostname {
    pub fn parse(value: impl Into<String>) -> Result<Self, AcmeCoordinationError> {
        let value = value.into();
        let normalized = canonicalize_hostname(&value)?;
        if normalized.is_empty() {
            return Err(AcmeCoordinationError::EmptyHostname);
        }
        Ok(Self(normalized))
    }

    pub fn parse_host_header(value: impl Into<String>) -> Result<Self, AcmeCoordinationError> {
        let value = value.into();
        let host = strip_host_port(value.trim());
        Self::parse(host)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for AcmeHostname {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for AcmeHostname {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct AcmeChallengeToken(String);

impl AcmeChallengeToken {
    pub fn parse(value: impl Into<String>) -> Result<Self, AcmeCoordinationError> {
        let value = value.into();
        if value.is_empty() {
            return Err(AcmeCoordinationError::EmptyChallengeToken);
        }
        if value.len() < MIN_ACME_TOKEN_LEN {
            return Err(AcmeCoordinationError::ChallengeTokenTooShort {
                min_len: MIN_ACME_TOKEN_LEN,
                actual_len: value.len(),
            });
        }
        if !value.bytes().all(is_acme_token_byte) {
            return Err(AcmeCoordinationError::InvalidChallengeToken { token: value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for AcmeChallengeToken {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for AcmeChallengeToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AcmeKeyAuthorization(String);

impl AcmeKeyAuthorization {
    pub fn parse_for_token(
        token: &AcmeChallengeToken,
        value: impl Into<String>,
    ) -> Result<Self, AcmeCoordinationError> {
        let value = value.into();
        if value.is_empty() {
            return Err(AcmeCoordinationError::EmptyKeyAuthorization);
        }
        if value.len() > MAX_ACME_KEY_AUTHORIZATION_LEN {
            return Err(AcmeCoordinationError::KeyAuthorizationTooLong {
                max_len: MAX_ACME_KEY_AUTHORIZATION_LEN,
                actual_len: value.len(),
            });
        }
        if !value.is_ascii()
            || value
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        {
            return Err(AcmeCoordinationError::InvalidKeyAuthorization { value });
        }
        let Some((actual_token, thumbprint)) = value.split_once('.') else {
            return Err(AcmeCoordinationError::InvalidKeyAuthorization { value });
        };
        if actual_token != token.as_str() {
            return Err(AcmeCoordinationError::KeyAuthorizationTokenMismatch {
                expected: token.as_str().to_string(),
                actual: actual_token.to_string(),
            });
        }
        if thumbprint.is_empty() || !thumbprint.bytes().all(is_acme_token_byte) {
            return Err(AcmeCoordinationError::InvalidKeyAuthorization { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AcmeChallengeId {
    hostname: AcmeHostname,
    token: AcmeChallengeToken,
    lease_resource: LeaseResource,
}

impl AcmeChallengeId {
    #[must_use]
    pub fn new(hostname: AcmeHostname, token: AcmeChallengeToken) -> Self {
        let lease_resource =
            LeaseResource::from_segments(["acme", "http01", hostname.as_str(), token.as_str()]);
        Self {
            hostname,
            token,
            lease_resource,
        }
    }

    #[must_use]
    pub fn hostname(&self) -> &AcmeHostname {
        &self.hostname
    }

    #[must_use]
    pub fn token(&self) -> &AcmeChallengeToken {
        &self.token
    }

    #[must_use]
    pub fn lease_resource(&self) -> &LeaseResource {
        &self.lease_resource
    }

    #[must_use]
    pub fn lease_claimed_fact_key(&self, epoch: LeaseEpoch) -> String {
        format!("/facts/lease/{}/claimed/{epoch}", self.lease_resource)
    }

    #[must_use]
    pub fn lease_renewed_fact_key(
        &self,
        epoch: LeaseEpoch,
        claim_hash: LeaseContentHash,
        renewed_at: LeaseTimestamp,
    ) -> String {
        format!(
            "/facts/lease/{}/renewed/{epoch}/{}/{renewed_at}",
            self.lease_resource, claim_hash
        )
    }

    #[must_use]
    pub fn lease_released_fact_key(
        &self,
        epoch: LeaseEpoch,
        claim_hash: LeaseContentHash,
        release: LeaseRelease,
    ) -> String {
        let release_segment = match release {
            LeaseRelease::At(released_at) => released_at.to_string(),
            LeaseRelease::DroppedWithoutTimestamp => "drop".to_string(),
        };
        format!(
            "/facts/lease/{}/released/{epoch}/{}/{release_segment}",
            self.lease_resource, claim_hash
        )
    }

    #[must_use]
    pub fn presented_fact_key(&self, epoch: LeaseEpoch) -> String {
        format!(
            "/facts/acme/http01/{}/{}/presented/{epoch}",
            self.hostname, self.token
        )
    }

    #[must_use]
    pub fn cleared_fact_key(&self, epoch: LeaseEpoch, claim_hash: LeaseContentHash) -> String {
        format!(
            "/facts/acme/http01/{}/{}/cleared/{epoch}/{}",
            self.hostname, self.token, claim_hash
        )
    }
}

#[derive(Debug)]
pub struct AcmeChallengeLease {
    id: AcmeChallengeId,
    guard: LeaseGuard,
    visible_nodes: BTreeSet<VisibleNode>,
}

impl AcmeChallengeLease {
    fn new(
        id: AcmeChallengeId,
        guard: LeaseGuard,
        visible_nodes: BTreeSet<VisibleNode>,
    ) -> Result<Self, AcmeCoordinationError> {
        let expected = id.lease_resource().clone();
        if guard.resource() != &expected {
            return Err(AcmeCoordinationError::LeaseResourceMismatch {
                expected,
                actual: guard.resource().clone(),
            });
        }
        Ok(Self {
            id,
            guard,
            visible_nodes,
        })
    }

    #[must_use]
    pub fn id(&self) -> &AcmeChallengeId {
        &self.id
    }

    #[must_use]
    pub fn visible_nodes(&self) -> &BTreeSet<VisibleNode> {
        &self.visible_nodes
    }

    #[must_use]
    pub fn holder(&self) -> &LeaseHolder {
        self.guard.holder()
    }

    #[must_use]
    pub fn epoch(&self) -> LeaseEpoch {
        self.guard.epoch()
    }

    #[must_use]
    pub fn claim_hash(&self) -> LeaseContentHash {
        self.guard.claim_hash()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcmeChallengeRecord {
    key_authorization: AcmeKeyAuthorization,
    holder: LeaseHolder,
    epoch: LeaseEpoch,
    claim_hash: LeaseContentHash,
    published_at: LeaseTimestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcmeHttp01PresentedFact {
    id: AcmeChallengeId,
    key_authorization: AcmeKeyAuthorization,
    holder: LeaseHolder,
    epoch: LeaseEpoch,
    claim_hash: LeaseContentHash,
    published_at: LeaseTimestamp,
}

impl AcmeHttp01PresentedFact {
    pub fn new(
        lease: &AcmeChallengeLease,
        key_authorization: AcmeKeyAuthorization,
        published_at: LeaseTimestamp,
    ) -> Self {
        Self {
            id: lease.id().clone(),
            key_authorization,
            holder: lease.holder().clone(),
            epoch: lease.epoch(),
            claim_hash: lease.claim_hash(),
            published_at,
        }
    }

    pub fn from_parts(
        id: AcmeChallengeId,
        key_authorization: AcmeKeyAuthorization,
        holder: LeaseHolder,
        epoch: LeaseEpoch,
        claim_hash: LeaseContentHash,
        published_at: LeaseTimestamp,
    ) -> Result<Self, AcmeCoordinationError> {
        AcmeKeyAuthorization::parse_for_token(id.token(), key_authorization.as_str().to_string())?;
        Ok(Self {
            id,
            key_authorization,
            holder,
            epoch,
            claim_hash,
            published_at,
        })
    }

    #[must_use]
    pub fn id(&self) -> &AcmeChallengeId {
        &self.id
    }

    #[must_use]
    pub fn key_authorization(&self) -> &AcmeKeyAuthorization {
        &self.key_authorization
    }

    #[must_use]
    pub fn holder(&self) -> &LeaseHolder {
        &self.holder
    }

    #[must_use]
    pub fn epoch(&self) -> LeaseEpoch {
        self.epoch
    }

    #[must_use]
    pub fn claim_hash(&self) -> LeaseContentHash {
        self.claim_hash
    }

    #[must_use]
    pub fn published_at(&self) -> LeaseTimestamp {
        self.published_at
    }
}

impl<'de> Deserialize<'de> for AcmeHttp01PresentedFact {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WirePresentedFact {
            id: AcmeChallengeId,
            key_authorization: AcmeKeyAuthorization,
            holder: LeaseHolder,
            epoch: LeaseEpoch,
            claim_hash: LeaseContentHash,
            published_at: LeaseTimestamp,
        }

        let wire = WirePresentedFact::deserialize(deserializer)?;
        Self::from_parts(
            wire.id,
            wire.key_authorization,
            wire.holder,
            wire.epoch,
            wire.claim_hash,
            wire.published_at,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcmeHttp01ClearedFact {
    id: AcmeChallengeId,
    holder: LeaseHolder,
    epoch: LeaseEpoch,
    claim_hash: LeaseContentHash,
    cleared_at: LeaseTimestamp,
}

impl AcmeHttp01ClearedFact {
    #[must_use]
    pub fn new(lease: &AcmeChallengeLease, cleared_at: LeaseTimestamp) -> Self {
        Self {
            id: lease.id().clone(),
            holder: lease.holder().clone(),
            epoch: lease.epoch(),
            claim_hash: lease.claim_hash(),
            cleared_at,
        }
    }

    #[must_use]
    pub fn from_parts(
        id: AcmeChallengeId,
        holder: LeaseHolder,
        epoch: LeaseEpoch,
        claim_hash: LeaseContentHash,
        cleared_at: LeaseTimestamp,
    ) -> Self {
        Self {
            id,
            holder,
            epoch,
            claim_hash,
            cleared_at,
        }
    }

    #[must_use]
    pub fn id(&self) -> &AcmeChallengeId {
        &self.id
    }

    #[must_use]
    pub fn holder(&self) -> &LeaseHolder {
        &self.holder
    }

    #[must_use]
    pub fn epoch(&self) -> LeaseEpoch {
        self.epoch
    }

    #[must_use]
    pub fn claim_hash(&self) -> LeaseContentHash {
        self.claim_hash
    }

    #[must_use]
    pub fn cleared_at(&self) -> LeaseTimestamp {
        self.cleared_at
    }
}

impl AcmeChallengeRecord {
    #[must_use]
    pub fn key_authorization(&self) -> &AcmeKeyAuthorization {
        &self.key_authorization
    }

    #[must_use]
    pub fn holder(&self) -> &LeaseHolder {
        &self.holder
    }

    #[must_use]
    pub fn epoch(&self) -> LeaseEpoch {
        self.epoch
    }

    #[must_use]
    pub fn claim_hash(&self) -> LeaseContentHash {
        self.claim_hash
    }

    #[must_use]
    pub fn published_at(&self) -> LeaseTimestamp {
        self.published_at
    }
}

#[derive(Debug, Error)]
pub enum AcmeCoordinationError {
    #[error("ACME hostname is empty")]
    EmptyHostname,
    #[error("ACME hostname is too long: expected at most {max_len}, got {actual_len}")]
    HostnameTooLong { max_len: usize, actual_len: usize },
    #[error("ACME hostname label is invalid: {label}")]
    InvalidHostnameLabel { label: String },
    #[error("ACME hostname label is too long: expected at most {max_len}, got {actual_len}")]
    HostnameLabelTooLong { max_len: usize, actual_len: usize },
    #[error("ACME challenge token is empty")]
    EmptyChallengeToken,
    #[error("ACME challenge token is too short: expected at least {min_len}, got {actual_len}")]
    ChallengeTokenTooShort { min_len: usize, actual_len: usize },
    #[error("ACME challenge token contains unsupported characters: {token}")]
    InvalidChallengeToken { token: String },
    #[error("ACME key authorization is empty")]
    EmptyKeyAuthorization,
    #[error("ACME key authorization is too long: expected at most {max_len}, got {actual_len}")]
    KeyAuthorizationTooLong { max_len: usize, actual_len: usize },
    #[error("ACME key authorization does not match token: expected {expected}, got {actual}")]
    KeyAuthorizationTokenMismatch { expected: String, actual: String },
    #[error("ACME key authorization is invalid: {value}")]
    InvalidKeyAuthorization { value: String },
    #[error("ACME challenge lease expected resource {expected}, got {actual}")]
    LeaseResourceMismatch {
        expected: LeaseResource,
        actual: LeaseResource,
    },
    #[error(transparent)]
    Lease(#[from] LeaseError),
}

#[derive(Debug)]
pub struct AcmeChallengeCoordinator {
    leases: LeaseBook,
    policy: LeaseAcquirePolicy,
    challenges: std::collections::BTreeMap<AcmeChallengeId, AcmeChallengeRecord>,
}

impl AcmeChallengeCoordinator {
    #[must_use]
    pub fn new(policy: LeaseAcquirePolicy) -> Self {
        Self {
            leases: LeaseBook::new(),
            policy,
            challenges: std::collections::BTreeMap::new(),
        }
    }

    pub fn active_challenge(
        &self,
        id: &AcmeChallengeId,
        now: LeaseTimestamp,
    ) -> Option<&AcmeChallengeRecord> {
        let record = self.challenges.get(id)?;
        let LeaseState::Active { current, .. } = self.lease_state(id, now) else {
            return None;
        };
        if current.holder() == record.holder()
            && current.epoch() == record.epoch()
            && current.content_hash() == record.claim_hash()
        {
            return Some(record);
        }
        None
    }

    fn lease_state(&self, id: &AcmeChallengeId, now: LeaseTimestamp) -> LeaseState {
        self.leases.state(id.lease_resource(), now)
    }

    pub fn acquire_challenge(
        &self,
        holder: LeaseHolder,
        hostname: AcmeHostname,
        token: AcmeChallengeToken,
        now: LeaseTimestamp,
        context: LeaseCommandContext,
    ) -> Result<AcmeChallengeLease, AcmeCoordinationError> {
        let id = AcmeChallengeId::new(hostname, token);
        let acquired = self
            .leases
            .try_acquire(
                id.lease_resource().clone(),
                holder,
                now,
                &self.policy,
                context,
            )?
            .into_acquired()?;
        let (guard, visible_nodes) = acquired.into_parts();
        AcmeChallengeLease::new(id, guard, visible_nodes)
    }

    pub fn publish_challenge(
        &mut self,
        lease: &AcmeChallengeLease,
        key_authorization: AcmeKeyAuthorization,
        now: LeaseTimestamp,
    ) -> Result<(), AcmeCoordinationError> {
        self.leases.assert_current(&lease.guard, now)?;
        self.challenges.insert(
            lease.id().clone(),
            AcmeChallengeRecord {
                key_authorization,
                holder: lease.guard.holder().clone(),
                epoch: lease.guard.epoch(),
                claim_hash: lease.guard.claim_hash(),
                published_at: now,
            },
        );
        Ok(())
    }

    pub fn delete_challenge(
        &mut self,
        lease: &AcmeChallengeLease,
        now: LeaseTimestamp,
    ) -> Result<(), AcmeCoordinationError> {
        self.leases.assert_current(&lease.guard, now)?;
        self.challenges.remove(lease.id());
        Ok(())
    }

    pub fn release_challenge(
        &mut self,
        lease: &mut AcmeChallengeLease,
        now: LeaseTimestamp,
    ) -> Result<(), AcmeCoordinationError> {
        self.leases.release(&mut lease.guard, now)?;
        self.challenges.remove(lease.id());
        Ok(())
    }
}

#[cfg(any(test, feature = "harness"))]
pub mod harness {
    use mvp_lease::{LeaseFact, LeaseState, LeaseTimestamp};

    use super::{AcmeChallengeCoordinator, AcmeChallengeId};

    pub fn record_observed_lease_fact(coordinator: &AcmeChallengeCoordinator, fact: LeaseFact) {
        coordinator.leases.importer().record(fact);
    }

    #[must_use]
    pub fn lease_state(
        coordinator: &AcmeChallengeCoordinator,
        id: &AcmeChallengeId,
        now: LeaseTimestamp,
    ) -> LeaseState {
        coordinator.lease_state(id, now)
    }

    #[must_use]
    pub fn lease_fact_count(coordinator: &AcmeChallengeCoordinator) -> usize {
        coordinator.leases.fact_count()
    }

    #[must_use]
    pub fn challenge_count(coordinator: &AcmeChallengeCoordinator) -> usize {
        coordinator.challenges.len()
    }
}

fn is_acme_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

fn canonicalize_hostname(value: &str) -> Result<String, AcmeCoordinationError> {
    let normalized = value.trim_end_matches('.').to_ascii_lowercase();
    if normalized.is_empty() {
        return Err(AcmeCoordinationError::EmptyHostname);
    }
    if normalized.len() > MAX_HOSTNAME_LEN {
        return Err(AcmeCoordinationError::HostnameTooLong {
            max_len: MAX_HOSTNAME_LEN,
            actual_len: normalized.len(),
        });
    }
    for label in normalized.split('.') {
        validate_hostname_label(label)?;
    }
    Ok(normalized)
}

fn validate_hostname_label(label: &str) -> Result<(), AcmeCoordinationError> {
    if label.is_empty() {
        return Err(AcmeCoordinationError::InvalidHostnameLabel {
            label: label.to_string(),
        });
    }
    if label.len() > MAX_HOSTNAME_LABEL_LEN {
        return Err(AcmeCoordinationError::HostnameLabelTooLong {
            max_len: MAX_HOSTNAME_LABEL_LEN,
            actual_len: label.len(),
        });
    }
    if label.starts_with('-')
        || label.ends_with('-')
        || !label
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(AcmeCoordinationError::InvalidHostnameLabel {
            label: label.to_string(),
        });
    }
    Ok(())
}

fn strip_host_port(value: &str) -> &str {
    let Some((host, port)) = value.rsplit_once(':') else {
        return value;
    };
    if host.contains(':') || port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return value;
    }
    host
}

#[cfg(test)]
mod tests {
    use super::*;
    use mvp_lease::{
        LeaseClaimed, LeaseDuration, LeaseFact, LeasePolicyError, harness::visible_nodes,
    };

    const TOKEN_A: &str = "tokA0123456789abcdefgh";
    const TOKEN_B: &str = "tokB0123456789abcdefgh";

    fn hostname(value: &str) -> AcmeHostname {
        AcmeHostname::parse(value).expect("hostname parses")
    }

    fn token(value: &str) -> AcmeChallengeToken {
        AcmeChallengeToken::parse(value).expect("token parses")
    }

    fn key_auth(token: &AcmeChallengeToken, thumbprint: &str) -> AcmeKeyAuthorization {
        AcmeKeyAuthorization::parse_for_token(token, format!("{}.{thumbprint}", token.as_str()))
            .expect("key authorization parses")
    }

    fn holder(value: &str) -> LeaseHolder {
        LeaseHolder::new(value)
    }

    fn at(value: u64) -> LeaseTimestamp {
        LeaseTimestamp::from_secs(value)
    }

    fn context() -> LeaseCommandContext {
        LeaseCommandContext::new(visible_nodes(["node-a", "node-b"]))
    }

    fn coordinator() -> AcmeChallengeCoordinator {
        AcmeChallengeCoordinator::new(policy().expect("policy"))
    }

    fn policy() -> Result<LeaseAcquirePolicy, LeasePolicyError> {
        Ok(LeaseAcquirePolicy::new(LeaseDuration::from_secs(10)?))
    }

    fn second_epoch() -> LeaseEpoch {
        LeaseEpoch::from_u64(2).expect("second epoch")
    }

    #[test]
    fn first_issuer_acquires_and_publishes_challenge_state() {
        let mut coordinator = coordinator();
        let lease = coordinator
            .acquire_challenge(
                holder("issuer-a"),
                hostname("Example.COM."),
                token(TOKEN_A),
                at(100),
                context(),
            )
            .expect("issuer acquires challenge");

        coordinator
            .publish_challenge(&lease, key_auth(lease.id().token(), "keyauthA"), at(101))
            .expect("publish succeeds");

        let record = coordinator
            .active_challenge(lease.id(), at(101))
            .expect("challenge is published");
        assert_eq!(record.holder(), &holder("issuer-a"));
        assert_eq!(record.epoch(), LeaseEpoch::first());
        assert_eq!(lease.id().hostname().as_str(), "example.com");
        assert_eq!(lease.visible_nodes().len(), 2);
    }

    #[test]
    fn second_issuer_gets_conflict_while_first_lease_is_active() {
        let coordinator = coordinator();
        let lease = coordinator
            .acquire_challenge(
                holder("issuer-a"),
                hostname("example.com"),
                token(TOKEN_A),
                at(100),
                context(),
            )
            .expect("issuer a acquires");
        let error = coordinator
            .acquire_challenge(
                holder("issuer-b"),
                hostname("example.com"),
                token(TOKEN_A),
                at(101),
                context(),
            )
            .expect_err("issuer b is vetoed");

        assert!(matches!(
            error,
            AcmeCoordinationError::Lease(LeaseError::Conflict(conflict))
                if conflict.conflicting_holder() == &holder("issuer-a")
        ));
        std::mem::forget(lease);
    }

    #[test]
    fn expired_lease_allows_second_issuer_and_fences_first() {
        let mut coordinator = coordinator();
        let first = coordinator
            .acquire_challenge(
                holder("issuer-a"),
                hostname("example.com"),
                token(TOKEN_A),
                at(100),
                context(),
            )
            .expect("issuer a acquires");
        coordinator
            .publish_challenge(&first, key_auth(first.id().token(), "keyauthA"), at(101))
            .expect("issuer a publishes");
        let second = coordinator
            .acquire_challenge(
                holder("issuer-b"),
                hostname("example.com"),
                token(TOKEN_A),
                at(111),
                context(),
            )
            .expect("issuer b acquires after expiry");
        coordinator
            .publish_challenge(&second, key_auth(second.id().token(), "keyauthB"), at(112))
            .expect("issuer b publishes");

        let stale = coordinator
            .publish_challenge(
                &first,
                key_auth(first.id().token(), "keyauthStale"),
                at(113),
            )
            .expect_err("stale issuer is fenced");

        assert!(matches!(
            stale,
            AcmeCoordinationError::Lease(LeaseError::Superseded { .. })
        ));
        let record = coordinator
            .active_challenge(second.id(), at(112))
            .expect("challenge remains");
        assert_eq!(record.holder(), &holder("issuer-b"));
        assert_eq!(record.epoch(), second_epoch());
        assert_eq!(
            record.key_authorization().as_str(),
            format!("{}.keyauthB", second.id().token().as_str())
        );
    }

    #[test]
    fn stale_issuer_cannot_delete_new_holder_challenge() {
        let mut coordinator = coordinator();
        let first = coordinator
            .acquire_challenge(
                holder("issuer-a"),
                hostname("example.com"),
                token(TOKEN_A),
                at(100),
                context(),
            )
            .expect("issuer a acquires");
        let second = coordinator
            .acquire_challenge(
                holder("issuer-b"),
                hostname("example.com"),
                token(TOKEN_A),
                at(111),
                context(),
            )
            .expect("issuer b acquires");
        coordinator
            .publish_challenge(&second, key_auth(second.id().token(), "keyauthB"), at(112))
            .expect("issuer b publishes");

        let error = coordinator
            .delete_challenge(&first, at(113))
            .expect_err("stale issuer cannot delete");

        assert!(matches!(
            error,
            AcmeCoordinationError::Lease(LeaseError::Superseded { .. })
        ));
        let record = coordinator
            .active_challenge(second.id(), at(113))
            .expect("new holder challenge remains active");
        assert_eq!(record.holder(), &holder("issuer-b"));
        assert_eq!(record.epoch(), second_epoch());
        assert_eq!(
            record.key_authorization().as_str(),
            format!("{}.keyauthB", second.id().token().as_str())
        );
    }

    #[test]
    fn deterministic_supersession_is_reported_for_conflicting_claims() {
        let coordinator = coordinator();
        let id = AcmeChallengeId::new(hostname("example.com"), token(TOKEN_A));
        let resource = id.lease_resource().clone();
        let first_claim = LeaseClaimed::new(
            resource.clone(),
            holder("issuer-a"),
            LeaseEpoch::first(),
            at(100),
            at(110),
        );
        let second_claim = LeaseClaimed::new(
            resource,
            holder("issuer-b"),
            LeaseEpoch::first(),
            at(100),
            at(110),
        );
        let first_hash = LeaseFact::Claimed(first_claim.clone()).content_hash();
        let second_hash = LeaseFact::Claimed(second_claim.clone()).content_hash();
        let winner_hash = first_hash.min(second_hash);
        let loser_hash = first_hash.max(second_hash);
        harness::record_observed_lease_fact(&coordinator, LeaseFact::Claimed(first_claim));
        harness::record_observed_lease_fact(&coordinator, LeaseFact::Claimed(second_claim));

        let LeaseState::Active {
            current,
            superseded,
        } = harness::lease_state(&coordinator, &id, at(101))
        else {
            panic!("expected deterministic active state");
        };

        assert_eq!(superseded.len(), 1);
        assert_eq!(current.content_hash(), winner_hash);
        assert_eq!(superseded[0].content_hash(), loser_hash);
        assert_eq!(superseded[0].by_epoch(), LeaseEpoch::first());
    }

    #[test]
    fn superseded_same_epoch_local_holder_cannot_mutate_challenge_record() {
        let mut coordinator = coordinator();
        let local = coordinator
            .acquire_challenge(
                holder("issuer-a"),
                hostname("example.com"),
                token(TOKEN_B),
                at(100),
                context(),
            )
            .expect("local issuer acquires");
        coordinator
            .publish_challenge(&local, key_auth(local.id().token(), "keyauthA"), at(101))
            .expect("local publish succeeds");
        let challenge_count = harness::challenge_count(&coordinator);
        let winning_foreign = winning_claim_against(local.id(), local.claim_hash());
        harness::record_observed_lease_fact(&coordinator, LeaseFact::Claimed(winning_foreign));

        let publish_error = coordinator
            .publish_challenge(
                &local,
                key_auth(local.id().token(), "keyauthStale"),
                at(102),
            )
            .expect_err("superseded local lease cannot publish");
        let delete_error = coordinator
            .delete_challenge(&local, at(102))
            .expect_err("superseded local lease cannot delete");

        assert!(matches!(
            publish_error,
            AcmeCoordinationError::Lease(LeaseError::Superseded { .. })
        ));
        assert!(matches!(
            delete_error,
            AcmeCoordinationError::Lease(LeaseError::Superseded { .. })
        ));
        assert_eq!(harness::challenge_count(&coordinator), challenge_count);
        assert!(coordinator.active_challenge(local.id(), at(102)).is_none());
    }

    fn winning_claim_against(id: &AcmeChallengeId, local_hash: LeaseContentHash) -> LeaseClaimed {
        for suffix in 0..10_000 {
            let claim = LeaseClaimed::new(
                id.lease_resource().clone(),
                holder(&format!("foreign-{suffix}")),
                LeaseEpoch::first(),
                at(100),
                at(110),
            );
            let hash = LeaseFact::Claimed(claim.clone()).content_hash();
            if hash < local_hash {
                return claim;
            }
        }
        panic!("expected to find deterministic lower hash claim");
    }

    #[test]
    fn invalid_token_characters_are_rejected() {
        let error =
            AcmeChallengeToken::parse("bad.token.0123456789abcdef").expect_err("dot is rejected");

        assert!(matches!(
            error,
            AcmeCoordinationError::InvalidChallengeToken { .. }
        ));
    }

    #[test]
    fn short_challenge_tokens_are_rejected() {
        let error = AcmeChallengeToken::parse("token-a").expect_err("short token is rejected");

        assert!(matches!(
            error,
            AcmeCoordinationError::ChallengeTokenTooShort { .. }
        ));
    }

    #[test]
    fn key_authorization_must_match_token_and_shape() {
        let token = token(TOKEN_A);
        let mismatch = AcmeKeyAuthorization::parse_for_token(&token, "other.keyauthA")
            .expect_err("wrong token is rejected");
        let control =
            AcmeKeyAuthorization::parse_for_token(&token, format!("{}.\nkeyauthA", token.as_str()))
                .expect_err("control character is rejected");

        assert!(matches!(
            mismatch,
            AcmeCoordinationError::KeyAuthorizationTokenMismatch { .. }
        ));
        assert!(matches!(
            control,
            AcmeCoordinationError::InvalidKeyAuthorization { .. }
        ));
    }

    #[test]
    fn hostnames_reject_bytes_outside_rfc1035_label_shape() {
        for value in [
            "bad/name.example",
            " bad.example",
            "bad.example ",
            "bad name.example",
            "bad\nname.example",
            "\"bad.example",
            "-bad.example",
            "bad-.example",
            "bad..example",
        ] {
            let error = AcmeHostname::parse(value).expect_err("hostname is rejected");
            assert!(
                matches!(error, AcmeCoordinationError::InvalidHostnameLabel { .. }),
                "{value} produced {error:?}"
            );
            let json_error = serde_json::from_value::<AcmeHostname>(serde_json::json!(value))
                .expect_err("hostname deserialization is rejected");
            assert!(
                json_error
                    .to_string()
                    .contains("ACME hostname label is invalid"),
                "{value} produced {json_error:?}"
            );
        }
    }

    #[test]
    fn presented_fact_deserialization_revalidates_key_authorization_token() {
        let id = AcmeChallengeId::new(hostname("example.test"), token(TOKEN_A));
        let claim = LeaseClaimed::new(
            id.lease_resource().clone(),
            holder("issuer-a"),
            LeaseEpoch::first(),
            at(100),
            at(160),
        );
        let fact = AcmeHttp01PresentedFact::from_parts(
            id,
            key_auth(&token(TOKEN_A), "thumbprintA"),
            holder("issuer-a"),
            LeaseEpoch::first(),
            LeaseFact::Claimed(claim).content_hash(),
            at(110),
        )
        .expect("presented fact");
        let mut value = serde_json::to_value(&fact).expect("serialize fact");
        value["key_authorization"] = serde_json::json!("other.thumbprintA");

        let error =
            serde_json::from_value::<AcmeHttp01PresentedFact>(value).expect_err("token mismatch");

        assert!(error.to_string().contains("does not match token"));
    }

    #[test]
    fn challenge_resource_uses_segment_encoding() {
        let id = AcmeChallengeId::new(hostname("example.com"), token(TOKEN_A));

        assert_eq!(
            id.lease_resource().as_str(),
            format!("acme.http01.example~2Ecom.{}", TOKEN_A)
        );
    }

    #[test]
    fn explicit_release_removes_challenge_ownership() {
        let mut coordinator = coordinator();
        let mut lease = coordinator
            .acquire_challenge(
                holder("issuer-a"),
                hostname("example.com"),
                token(TOKEN_A),
                at(100),
                context(),
            )
            .expect("issuer acquires");
        coordinator
            .publish_challenge(&lease, key_auth(lease.id().token(), "keyauthA"), at(101))
            .expect("issuer publishes");

        coordinator
            .release_challenge(&mut lease, at(101))
            .expect("release succeeds");

        assert!(matches!(
            harness::lease_state(&coordinator, lease.id(), at(102)),
            LeaseState::Released { .. }
        ));
        assert!(coordinator.active_challenge(lease.id(), at(102)).is_none());

        let next = coordinator
            .acquire_challenge(
                holder("issuer-b"),
                hostname("example.com"),
                token(TOKEN_A),
                at(102),
                context(),
            )
            .expect("second issuer acquires released challenge");
        coordinator
            .publish_challenge(&next, key_auth(next.id().token(), "keyauthB"), at(103))
            .expect("second issuer publishes");
        assert!(matches!(
            coordinator.active_challenge(next.id(), at(103)),
            Some(record) if record.holder() == &holder("issuer-b") && record.epoch() == second_epoch()
        ));
    }
}
