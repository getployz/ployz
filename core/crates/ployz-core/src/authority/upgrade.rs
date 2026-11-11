//! The Release Channel a person approves for their Cluster, once, so a Machine that
//! enforces Device approval admits an Operator's upgrade only along it.
//!
//! The statement names the Cluster and the channel, and nothing about Machines or
//! releases: each Machine resolves the channel itself and checks Ployz's own signature on
//! what it installs. It never expires. A newer generation replaces it, and a Machine
//! refuses any generation below the highest it has accepted.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    canonical::Timestamp,
    keys::{Purpose, Signed, Statement},
    refusal::{AuthorityRefusal, field, first_mismatch},
    restart::SignerKey,
    rule::{RuleStatement, verify_signature},
    trusted_key::TrustedKeys,
};
use crate::{ReleaseChannel, SealingPublicKey};

/// A person's standing approval that their Cluster upgrades along `channel`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct UpgradeChannel {
    /// The Cluster's Sealing public key, which every Machine holds and Cloud cannot mint.
    pub cluster: SealingPublicKey,
    pub channel: ReleaseChannel,
    /// Each approval adds one. A Machine refuses one lower than it has accepted.
    pub generation: u64,
    pub signer: SignerKey,
}

impl Statement for UpgradeChannel {
    const PURPOSE: Purpose = Purpose::Rule;
}

impl RuleStatement for UpgradeChannel {
    fn signer(&self) -> SignerKey {
        self.signer
    }
}

/// Open a channel statement for `cluster`, signed by a Device this Machine trusts or
/// answered by a trusted passkey's own prompt. A browser key is refused even under a
/// valid session grant: the statement stands for every future upgrade.
///
/// # Errors
/// Returns the refusal of [`verify_signature`], and [`AuthorityRefusal::Mismatch`] naming
/// a Cluster for a statement about another one.
pub fn verify(
    signed: &Signed<UpgradeChannel>,
    trusted: &TrustedKeys,
    cluster: SealingPublicKey,
    now: Timestamp,
) -> Result<UpgradeChannel, AuthorityRefusal> {
    let (body, _) = verify_signature(signed, trusted, now)?;
    first_mismatch([(field::CLUSTER, body.cluster == cluster)])?;
    Ok(body)
}

/// Refuse `generation` when it is below `newest`, the highest this Machine has accepted:
/// a person approved another channel since. An equal generation passes, so a resend of the
/// same request is admitted.
///
/// # Errors
/// Returns [`AuthorityRefusal::Mismatch`] naming a generation.
pub fn admit_generation(generation: u64, newest: Option<u64>) -> Result<(), AuthorityRefusal> {
    first_mismatch([(
        "a generation",
        newest.is_none_or(|newest| generation >= newest),
    )])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DeviceKey;
    use crate::authority::{
        canonical,
        keys::Signer,
        restart::OwnPrompt,
        session::{ActionFamily, Delegation, GrantBody, SessionGrant},
        trusted_key::tests::{Answer, Authenticator, HONEST, trusting},
    };

    fn device() -> Signer {
        Signer::from_seed(&[7; 32])
    }

    fn cluster() -> SealingPublicKey {
        SealingPublicKey::from_bytes([9; 32])
    }

    fn statement() -> UpgradeChannel {
        UpgradeChannel {
            cluster: cluster(),
            channel: ReleaseChannel::Stable,
            generation: 2,
            signer: SignerKey::Device(DeviceKey::from(device().public())),
        }
    }

    fn devices(signer: &Signer) -> TrustedKeys {
        TrustedKeys {
            devices: vec![DeviceKey::from(signer.public())],
            passkeys: Vec::new(),
        }
    }

    fn now() -> Timestamp {
        Timestamp::from_millis(2_000).unwrap()
    }

    #[test]
    fn the_statement_is_pinned() {
        let body = canonical::to_canonical(&statement()).unwrap();
        assert_eq!(
            body,
            format!(
                r#"{{"channel":"stable","cluster":"{}","generation":2,"signer":"{}"}}"#,
                "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=",
                "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c",
            )
        );
        let signed = Signed::sign(&statement(), &device()).unwrap();
        assert!(
            signed
                .signing_input()
                .starts_with(b"ployz-authority/v1\nrule\n"),
            "a channel signs under the rule purpose, which no session grant covers"
        );
    }

    #[test]
    fn a_device_or_a_passkeys_own_prompt_approves_a_channel() {
        let signed = Signed::sign(&statement(), &device()).unwrap();
        assert_eq!(
            verify(&signed, &devices(&device()), cluster(), now()).unwrap(),
            statement()
        );

        let passkey = Authenticator::es256(4);
        let answered = UpgradeChannel {
            signer: SignerKey::Passkey(OwnPrompt::Passkey),
            ..statement()
        };
        let unsigned = Signed::unsigned(&answered).unwrap();
        // The body the dashboard drafts for the passkey, byte for byte (`channelApprovalBody`).
        assert_eq!(
            unsigned.body,
            r#"{"channel":"stable","cluster":"CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=","generation":2,"signer":"passkey"}"#
        );
        let assertion = passkey.answer(&Answer {
            challenge: &unsigned.challenge(),
            ..HONEST
        });
        let signed = unsigned.answered(assertion);
        assert_eq!(
            verify(&signed, &trusting(&passkey), cluster(), now()).unwrap(),
            answered
        );
    }

    #[test]
    fn another_clusters_statement_is_refused() {
        let signed = Signed::sign(&statement(), &device()).unwrap();
        assert_eq!(
            verify(
                &signed,
                &devices(&device()),
                SealingPublicKey::from_bytes([8; 32]),
                now()
            )
            .unwrap_err(),
            AuthorityRefusal::Mismatch {
                field: field::CLUSTER
            }
        );
    }

    #[test]
    fn an_unknown_key_is_refused() {
        let signed = Signed::sign(&statement(), &device()).unwrap();
        let stranger = Signer::from_seed(&[3; 32]);
        assert_eq!(
            verify(&signed, &devices(&stranger), cluster(), now()).unwrap_err(),
            AuthorityRefusal::UnknownKey {
                key: device().public()
            }
        );
    }

    #[test]
    fn a_browser_key_under_a_valid_session_grant_cannot_approve_a_channel() {
        let browser = Signer::from_seed(&[4; 32]);
        let passkey = Authenticator::es256(4);
        let grant = GrantBody {
            browser_key: browser.public(),
            cluster: cluster(),
            environment: crate::Namespace::parse("app").unwrap(),
            environment_id: uuid::Uuid::from_u128(1),
            machines: vec![crate::MachineId::parse("1".repeat(32)).unwrap()],
            families: vec![ActionFamily::Deploy],
            issued_at: Timestamp::from_millis(0).unwrap(),
            expires_at: Timestamp::from_millis(3_000_000).unwrap(),
            delegation: Delegation::None,
        };
        let body = canonical::to_canonical(&grant).unwrap();
        let assertion = passkey.answer(&Answer {
            challenge: &GrantBody::challenge(&body),
            ..HONEST
        });
        let session = UpgradeChannel {
            signer: SignerKey::Browser {
                browser: browser.public(),
            },
            ..statement()
        };
        let signed =
            Signed::sign_under(&session, &browser, SessionGrant { body, assertion }).unwrap();
        let trusted = trusting(&passkey);
        assert!(
            signed.approvers(&trusted, now()).is_ok(),
            "the grant itself verifies"
        );
        assert_eq!(
            verify(&signed, &trusted, cluster(), now()).unwrap_err(),
            AuthorityRefusal::Mismatch {
                field: "the signer"
            }
        );
    }

    #[test]
    fn a_lower_generation_is_refused_and_an_equal_one_passes() {
        assert!(admit_generation(2, None).is_ok());
        assert!(admit_generation(2, Some(2)).is_ok());
        assert!(admit_generation(3, Some(2)).is_ok());
        assert_eq!(
            admit_generation(1, Some(2)).unwrap_err(),
            AuthorityRefusal::Mismatch {
                field: "a generation"
            }
        );
    }
}
