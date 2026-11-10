//! The bundle a person signs once over an ordered plan of steps, and the check a Machine
//! runs when one step of that plan reaches it.
//!
//! Each step is a [`RestartApproval`] or [`super::DeployApproval`] body that nobody
//! signed. What vouches for it is the bundle in its envelope: a signed list of step
//! digests in order. A Machine checks that its step sits in the bundle at the index the
//! step names, with the same kind, and that the body it was handed hashes to that digest.
//! Cloud can carry a step but cannot add or alter one. A restart step runs only once,
//! since each step consumes its own place in the Machine's record; a deploy step is
//! consumed as a signed deploy approval is. A Machine sees only its own step, so the
//! order Cloud runs the steps in, and whether it runs them all, stays Cloud's.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use ts_rs::TS;

use super::{
    canonical::{Timestamp, hex_bytes, write_hex},
    keys::{Approver, Purpose, Signed, Statement},
    refusal::{AuthorityRefusal, field, first_mismatch},
    restart::{Approved, RestartApproval, RestartFacts, SignerKey, WorkflowKind},
    session::GrantScope,
    trusted_key::TrustedKeys,
};
use crate::{Namespace, OperationId, SealingPublicKey};

/// SHA-256 of one step's canonical body, written as 64 lowercase hex digits.
#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct StepDigest([u8; 32]);

impl StepDigest {
    #[must_use]
    pub fn of(body: &str) -> Self {
        Self(Sha256::digest(body.as_bytes()).into())
    }

    /// # Errors
    /// Returns a [`crate::ValueError`] unless `value` is 64 lowercase hex digits.
    pub fn parse(value: &str) -> Result<Self, crate::ValueError> {
        hex_bytes(value)
            .map(Self)
            .ok_or_else(|| crate::ValueError::new("step digest", value, "64 lowercase hex digits"))
    }
}

impl fmt::Display for StepDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_hex(&self.0, formatter)
    }
}

impl fmt::Debug for StepDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "StepDigest({self})")
    }
}

impl TryFrom<String> for StepDigest {
    type Error = crate::ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<StepDigest> for String {
    fn from(digest: StepDigest) -> Self {
        digest.to_string()
    }
}

/// What one step of a proposal runs. The first three are a [`RestartApproval`]'s
/// [`WorkflowKind`], written the same way; a deploy is a [`super::DeployApproval`]. A
/// daemon from before deploy steps refuses a bundle that lists one.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    Restart,
    Stop,
    Start,
    Deploy,
}

impl From<WorkflowKind> for StepKind {
    fn from(kind: WorkflowKind) -> Self {
        match kind {
            WorkflowKind::Restart => Self::Restart,
            WorkflowKind::Stop => Self::Stop,
            WorkflowKind::Start => Self::Start,
        }
    }
}

/// One step of a proposal: its place in the order, what it runs, and the digest of the
/// step body a Machine must be handed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ProposalStep {
    pub index: u32,
    pub kind: StepKind,
    pub digest: StepDigest,
}

/// A person approves an ordered plan of steps in one Cluster and Environment, under one
/// operation with a bounded lifetime. Every step shares this window.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ProposalApproval {
    pub cluster: SealingPublicKey,
    pub environment: Namespace,
    pub operation: OperationId,
    /// Non-empty, with `steps[i].index == i`.
    #[serde(deserialize_with = "dense_steps")]
    pub steps: Vec<ProposalStep>,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
    pub signer: SignerKey,
}

fn dense_steps<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<ProposalStep>, D::Error> {
    let steps = Vec::<ProposalStep>::deserialize(deserializer)?;
    if steps.is_empty() {
        return Err(serde::de::Error::custom("a proposal needs a step"));
    }
    if steps
        .iter()
        .enumerate()
        .any(|(place, step)| u32::try_from(place).ok() != Some(step.index))
    {
        return Err(serde::de::Error::custom(
            "steps must be numbered from 0 in order",
        ));
    }
    Ok(steps)
}

impl Statement for ProposalApproval {
    const PURPOSE: Purpose = Purpose::Workflow;
}

/// Where a step body says it sits: which proposal, and at which index.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ProposalRef {
    pub operation: OperationId,
    pub index: u32,
}

/// A statement a proposal may list as one of its steps.
pub(super) trait StepBody: Statement {
    fn kind(&self) -> StepKind;
    /// Where the body says it sits, absent from a single approval.
    fn place(&self) -> Option<ProposalRef>;
    fn signer(&self) -> SignerKey;
    fn cluster(&self) -> SealingPublicKey;
    fn namespace(&self) -> &Namespace;
    fn window(&self) -> (Timestamp, Timestamp);
}

impl StepBody for RestartApproval {
    fn kind(&self) -> StepKind {
        self.kind.into()
    }
    fn place(&self) -> Option<ProposalRef> {
        self.proposal
    }
    fn signer(&self) -> SignerKey {
        self.signer
    }
    fn cluster(&self) -> SealingPublicKey {
        self.cluster
    }
    fn namespace(&self) -> &Namespace {
        &self.namespace
    }
    fn window(&self) -> (Timestamp, Timestamp) {
        (self.issued_at, self.expires_at)
    }
}

/// Open `step`, an unsigned step body, as the step of `bundle` it names, signed by one of
/// this Machine's Devices or a browser key under a trusted passkey's grant. Returns the
/// body, its approver, and this Machine's Cluster key. The caller checks the body against
/// its request, the grant's scope and the window.
///
/// # Errors
/// Returns the first [`AuthorityRefusal`] found: a step that carries its own signatures
/// or grants, a bundle nested in a bundle, a signature or grant problem on the bundle, a
/// malformed body, or a step outside the bundle, of another kind, differing from its
/// digest, or naming another Cluster, Environment or window than the bundle.
pub(super) fn open_step<T: StepBody>(
    bundle: &Signed<ProposalApproval>,
    step: &Signed<T>,
    trusted: &TrustedKeys,
    cluster: Option<SealingPublicKey>,
    now: Timestamp,
) -> Result<(T, Approver, SealingPublicKey), AuthorityRefusal> {
    if !step.signatures.is_empty() || !step.grants.is_empty() || bundle.proposal.is_some() {
        return Err(AuthorityRefusal::Mismatch {
            field: "a proposal",
        });
    }
    let approvers = bundle.approvers(trusted, now)?;
    let plan = bundle.open()?;
    let body = step.open()?;
    let Some(cluster) = cluster else {
        return Err(AuthorityRefusal::NoClusterIdentity);
    };
    let by = approvers
        .into_iter()
        .find(|approver| approver.signs_as(plan.signer));
    let place = body
        .place()
        .filter(|place| place.operation == plan.operation);
    let listed = place.and_then(|place| plan.steps.get(place.index as usize));
    first_mismatch([
        ("the signer", by.is_some() && body.signer() == plan.signer),
        ("a proposal", place.is_some()),
        (
            "a step",
            listed.is_some_and(|listed| {
                listed.kind == body.kind() && listed.digest == StepDigest::of(&step.body)
            }),
        ),
        (field::CLUSTER, plan.cluster == body.cluster()),
        (field::ENVIRONMENT, &plan.environment == body.namespace()),
        (
            field::WINDOW,
            (plan.issued_at, plan.expires_at) == body.window(),
        ),
    ])?;
    let by = by.expect("the signer check passed");
    Ok((body, by, cluster))
}

/// Check that `step`, an unsigned restart step, is the step of `bundle` it names, and
/// that the bundle approves exactly this request.
///
/// # Errors
/// Returns the first [`AuthorityRefusal`] [`open_step`] finds, a request the step does
/// not match, a grant that does not cover it, or a bundle whose window has passed.
pub fn verify_step(
    bundle: &Signed<ProposalApproval>,
    step: &Signed<RestartApproval>,
    facts: &RestartFacts<'_>,
) -> Result<Approved, AuthorityRefusal> {
    let (body, by, cluster) = open_step(bundle, step, facts.trusted, facts.cluster, facts.now)?;
    body.matches(cluster, facts)?;
    if let Approver::Browser { grant, .. } = &by {
        grant.body.covers(&GrantScope {
            cluster,
            environment: &body.namespace,
            machines: &body.machines,
            family: body.kind.family(),
            issued_at: body.issued_at,
            expires_at: body.expires_at,
        })?;
    }
    super::validity::verify(body.issued_at, body.expires_at, facts.now)?;
    Ok(Approved { body, by })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ContainerId, ContainerKind, MachineId, ServiceName,
        authority::{
            ActionFamily, GrantBody, RestartStep, SessionGrant, Signer, TrustedKeys, canonical,
            restart::{self, verify},
            session::Delegation,
            trusted_key::tests::{Answer, Authenticator, HONEST, trusting},
        },
    };

    const ISSUED: u64 = 1_000_000;
    const MINUTE: u64 = 60_000;

    fn at(millis: u64) -> Timestamp {
        Timestamp::from_millis(millis).unwrap()
    }

    fn cluster() -> SealingPublicKey {
        SealingPublicKey::from_bytes([9; 32])
    }

    fn machine(tag: char) -> MachineId {
        MachineId::parse(tag.to_string().repeat(32)).unwrap()
    }

    fn production() -> Namespace {
        Namespace::parse("production").unwrap()
    }

    fn container(tag: char) -> ContainerId {
        ContainerId::parse(tag.to_string().repeat(64)).unwrap()
    }

    fn operation(tag: char) -> OperationId {
        OperationId::parse(tag.to_string().repeat(32)).unwrap()
    }

    fn device() -> Signer {
        Signer::from_seed(&[1; 32])
    }

    fn browser() -> Signer {
        Signer::from_seed(&[7; 32])
    }

    fn step_body(signer: SignerKey, tag: char, kind: WorkflowKind, index: u32) -> RestartApproval {
        RestartApproval {
            cluster: cluster(),
            operation: operation(tag),
            kind,
            namespace: production(),
            service: ServiceName::parse("web").unwrap(),
            machines: vec![machine('2')],
            container: container(tag),
            issued_at: at(ISSUED),
            expires_at: at(ISSUED + 10 * MINUTE),
            signer,
            proposal: Some(ProposalRef {
                operation: operation('e'),
                index,
            }),
        }
    }

    /// A three-step plan: stop `a`, start `a`, restart `b`.
    fn plan(signer: SignerKey) -> [RestartApproval; 3] {
        [
            step_body(signer, 'a', WorkflowKind::Stop, 0),
            step_body(signer, 'a', WorkflowKind::Start, 1),
            step_body(signer, 'b', WorkflowKind::Restart, 2),
        ]
    }

    fn bundle_body(signer: SignerKey, steps: &[RestartApproval]) -> ProposalApproval {
        ProposalApproval {
            cluster: cluster(),
            environment: production(),
            operation: operation('e'),
            steps: steps
                .iter()
                .enumerate()
                .map(|(index, step)| ProposalStep {
                    index: u32::try_from(index).unwrap(),
                    kind: step.kind.into(),
                    digest: StepDigest::of(&canonical::to_canonical(step).unwrap()),
                })
                .collect(),
            issued_at: at(ISSUED),
            expires_at: at(ISSUED + 10 * MINUTE),
            signer,
        }
    }

    fn device_bundle() -> ([RestartApproval; 3], Signed<ProposalApproval>) {
        let signer = SignerKey::Device(device().public().into());
        let steps = plan(signer);
        let bundle = Signed::sign(&bundle_body(signer, &steps), &device()).unwrap();
        (steps, bundle)
    }

    fn envelope(
        step: &RestartApproval,
        bundle: &Signed<ProposalApproval>,
    ) -> Signed<RestartApproval> {
        Signed::proposed(step, bundle.clone()).unwrap()
    }

    fn trusted_device() -> TrustedKeys {
        TrustedKeys {
            devices: vec![device().public().into()],
            passkeys: Vec::new(),
        }
    }

    fn check(
        signed: &Signed<RestartApproval>,
        trusted: &TrustedKeys,
        tag: char,
        step: RestartStep,
        now: u64,
    ) -> Result<Approved, AuthorityRefusal> {
        let own = machine('2');
        let service = ServiceName::parse("web").unwrap();
        let container = container(tag);
        verify(
            signed,
            &RestartFacts {
                cluster: Some(cluster()),
                machine: &own,
                trusted,
                namespace: &production(),
                service: &service,
                container: &container,
                kind: ContainerKind::ServiceContainer,
                step,
                now: at(now),
            },
        )
    }

    #[test]
    fn each_step_of_a_device_signed_bundle_passes_at_its_own_index() {
        let (steps, bundle) = device_bundle();
        for (step, lifecycle, tag) in [
            (&steps[0], RestartStep::Stop, 'a'),
            (&steps[1], RestartStep::Start, 'a'),
            (&steps[2], RestartStep::Stop, 'b'),
        ] {
            let approved = check(
                &envelope(step, &bundle),
                &trusted_device(),
                tag,
                lifecycle,
                ISSUED + MINUTE,
            )
            .unwrap();
            assert_eq!(approved.body, *step);
            assert_eq!(approved.by, Approver::Device(device().public()));
        }
    }

    #[test]
    fn a_step_whose_body_differs_from_its_digest_is_refused() {
        let (steps, bundle) = device_bundle();
        let mut signed = envelope(&steps[2], &bundle);
        signed.body = signed
            .body
            .replace(r#""service":"web""#, r#""service":"db""#);
        let own = machine('2');
        let service = ServiceName::parse("db").unwrap();
        let container = container('b');
        let refused = verify(
            &signed,
            &RestartFacts {
                cluster: Some(cluster()),
                machine: &own,
                trusted: &trusted_device(),
                namespace: &production(),
                service: &service,
                container: &container,
                kind: ContainerKind::ServiceContainer,
                step: RestartStep::Stop,
                now: at(ISSUED + MINUTE),
            },
        )
        .unwrap_err();
        assert_eq!(refused, AuthorityRefusal::Mismatch { field: "a step" });
    }

    #[test]
    fn a_step_presented_under_another_steps_index_is_refused() {
        let (steps, bundle) = device_bundle();
        let swapped = RestartApproval {
            proposal: Some(ProposalRef {
                operation: operation('e'),
                index: 0,
            }),
            ..steps[2].clone()
        };
        assert_eq!(
            check(
                &envelope(&swapped, &bundle),
                &trusted_device(),
                'b',
                RestartStep::Stop,
                ISSUED + MINUTE,
            )
            .unwrap_err(),
            AuthorityRefusal::Mismatch { field: "a step" }
        );
    }

    #[test]
    fn a_step_whose_kind_differs_from_the_bundles_is_refused() {
        let signer = SignerKey::Device(device().public().into());
        let steps = plan(signer);
        let mut listed = bundle_body(signer, &steps);
        listed.steps.first_mut().unwrap().kind = StepKind::Start;
        let bundle = Signed::sign(&listed, &device()).unwrap();
        assert_eq!(
            check(
                &envelope(&steps[0], &bundle),
                &trusted_device(),
                'a',
                RestartStep::Stop,
                ISSUED + MINUTE,
            )
            .unwrap_err(),
            AuthorityRefusal::Mismatch { field: "a step" }
        );
    }

    #[test]
    fn an_index_outside_the_bundle_is_refused() {
        let (steps, bundle) = device_bundle();
        let beyond = RestartApproval {
            proposal: Some(ProposalRef {
                operation: operation('e'),
                index: 3,
            }),
            ..steps[0].clone()
        };
        assert_eq!(
            check(
                &envelope(&beyond, &bundle),
                &trusted_device(),
                'a',
                RestartStep::Stop,
                ISSUED + MINUTE,
            )
            .unwrap_err(),
            AuthorityRefusal::Mismatch { field: "a step" }
        );
    }

    #[test]
    fn a_step_naming_another_proposal_is_refused() {
        let (steps, bundle) = device_bundle();
        let elsewhere = RestartApproval {
            proposal: Some(ProposalRef {
                operation: operation('f'),
                index: 0,
            }),
            ..steps[0].clone()
        };
        assert_eq!(
            check(
                &envelope(&elsewhere, &bundle),
                &trusted_device(),
                'a',
                RestartStep::Stop,
                ISSUED + MINUTE,
            )
            .unwrap_err(),
            AuthorityRefusal::Mismatch {
                field: "a proposal"
            }
        );
    }

    #[test]
    fn a_step_that_names_no_proposal_but_rides_in_one_is_refused() {
        let (steps, bundle) = device_bundle();
        let bare = RestartApproval {
            proposal: None,
            ..steps[0].clone()
        };
        assert_eq!(
            check(
                &envelope(&bare, &bundle),
                &trusted_device(),
                'a',
                RestartStep::Stop,
                ISSUED + MINUTE,
            )
            .unwrap_err(),
            AuthorityRefusal::Mismatch {
                field: "a proposal"
            }
        );
    }

    #[test]
    fn a_signed_step_that_also_names_a_proposal_is_refused() {
        let (steps, _) = device_bundle();
        let signed = Signed::sign(&steps[0], &device()).unwrap();
        assert_eq!(
            check(
                &signed,
                &trusted_device(),
                'a',
                RestartStep::Stop,
                ISSUED + MINUTE,
            )
            .unwrap_err(),
            AuthorityRefusal::Mismatch {
                field: "a proposal"
            }
        );
    }

    #[test]
    fn a_step_that_carries_its_own_signature_beside_a_bundle_is_refused() {
        let (steps, bundle) = device_bundle();
        let mut signed = Signed::sign(&steps[0], &device()).unwrap();
        signed.proposal = Some(Box::new(bundle));
        assert_eq!(
            check(
                &signed,
                &trusted_device(),
                'a',
                RestartStep::Stop,
                ISSUED + MINUTE,
            )
            .unwrap_err(),
            AuthorityRefusal::Mismatch {
                field: "a proposal"
            }
        );
    }

    #[test]
    fn a_bundle_nested_in_a_bundle_is_refused() {
        let (steps, mut bundle) = device_bundle();
        bundle.proposal = Some(Box::new(bundle.clone()));
        assert_eq!(
            check(
                &envelope(&steps[0], &bundle),
                &trusted_device(),
                'a',
                RestartStep::Stop,
                ISSUED + MINUTE,
            )
            .unwrap_err(),
            AuthorityRefusal::Mismatch {
                field: "a proposal"
            }
        );
    }

    #[test]
    fn a_bundle_whose_window_passed_is_refused() {
        let (steps, bundle) = device_bundle();
        assert_eq!(
            check(
                &envelope(&steps[0], &bundle),
                &trusted_device(),
                'a',
                RestartStep::Stop,
                ISSUED + 13 * MINUTE,
            )
            .unwrap_err(),
            AuthorityRefusal::Expired
        );
    }

    #[test]
    fn a_step_whose_window_differs_from_the_bundles_is_refused() {
        let signer = SignerKey::Device(device().public().into());
        let mut steps = plan(signer);
        steps.first_mut().unwrap().expires_at = at(ISSUED + 5 * MINUTE);
        let bundle = Signed::sign(&bundle_body(signer, &steps), &device()).unwrap();
        assert_eq!(
            check(
                &envelope(&steps[0], &bundle),
                &trusted_device(),
                'a',
                RestartStep::Stop,
                ISSUED + MINUTE,
            )
            .unwrap_err(),
            AuthorityRefusal::Mismatch { field: "a window" }
        );
    }

    #[test]
    fn a_bundle_signed_by_a_stranger_is_unknown() {
        let stranger = Signer::from_seed(&[2; 32]);
        let signer = SignerKey::Device(stranger.public().into());
        let steps = plan(signer);
        let bundle = Signed::sign(&bundle_body(signer, &steps), &stranger).unwrap();
        assert_eq!(
            check(
                &envelope(&steps[0], &bundle),
                &trusted_device(),
                'a',
                RestartStep::Stop,
                ISSUED + MINUTE,
            )
            .unwrap_err(),
            AuthorityRefusal::UnknownKey {
                key: stranger.public()
            }
        );
    }

    #[test]
    fn a_step_still_permits_only_its_own_lifecycle_steps() {
        let (steps, bundle) = device_bundle();
        assert_eq!(
            check(
                &envelope(&steps[1], &bundle),
                &trusted_device(),
                'a',
                RestartStep::Stop,
                ISSUED + MINUTE,
            )
            .unwrap_err(),
            AuthorityRefusal::Mismatch {
                field: "a workflow"
            }
        );
    }

    #[test]
    fn a_bundle_lists_no_step_twice_and_counts_from_zero() {
        let signer = SignerKey::Device(device().public().into());
        let steps = plan(signer);
        let mut listed = bundle_body(signer, &steps);
        listed.steps.get_mut(1).unwrap().index = 2;
        let json = serde_json::to_string(&listed).unwrap();
        assert!(serde_json::from_str::<ProposalApproval>(&json).is_err());
        listed.steps.clear();
        let json = serde_json::to_string(&listed).unwrap();
        assert!(serde_json::from_str::<ProposalApproval>(&json).is_err());
    }

    #[test]
    fn a_browser_key_under_a_grant_naming_every_steps_family_passes() {
        let passkey = Authenticator::es256(4);
        let grant_body = GrantBody {
            browser_key: browser().public(),
            cluster: cluster(),
            environment: production(),
            environment_id: uuid::Uuid::nil(),
            machines: vec![machine('2')],
            families: vec![
                ActionFamily::Restart,
                ActionFamily::Start,
                ActionFamily::Stop,
            ],
            issued_at: at(ISSUED),
            expires_at: at(ISSUED + 60 * MINUTE),
            delegation: Delegation::None,
        };
        let grant_text = canonical::to_canonical(&grant_body).unwrap();
        let assertion = passkey.answer(&Answer {
            kind: "webauthn.get",
            challenge: &GrantBody::challenge(&grant_text),
            ..HONEST
        });
        let grant = SessionGrant {
            body: grant_text,
            assertion,
        };
        let signer = SignerKey::Browser {
            browser: browser().public(),
        };
        let steps = plan(signer);
        let bundle = Signed::sign_under(&bundle_body(signer, &steps), &browser(), grant).unwrap();
        let trusted = trusting(&passkey);
        for (step, lifecycle, tag) in [
            (&steps[0], RestartStep::Stop, 'a'),
            (&steps[1], RestartStep::Start, 'a'),
            (&steps[2], RestartStep::Stop, 'b'),
        ] {
            let approved = check(
                &envelope(step, &bundle),
                &trusted,
                tag,
                lifecycle,
                ISSUED + MINUTE,
            )
            .unwrap();
            assert!(matches!(approved.by, Approver::Browser { .. }));
        }
        let mut narrower = grant_body.clone();
        narrower.families = vec![ActionFamily::Stop];
        let narrower_text = canonical::to_canonical(&narrower).unwrap();
        let assertion = passkey.answer(&Answer {
            kind: "webauthn.get",
            challenge: &GrantBody::challenge(&narrower_text),
            ..HONEST
        });
        let bundle = Signed::sign_under(
            &bundle_body(signer, &steps),
            &browser(),
            SessionGrant {
                body: narrower_text,
                assertion,
            },
        )
        .unwrap();
        assert_eq!(
            check(
                &envelope(&steps[1], &bundle),
                &trusted,
                'a',
                RestartStep::Start,
                ISSUED + MINUTE,
            )
            .unwrap_err(),
            AuthorityRefusal::Mismatch { field: "an action" }
        );
    }

    #[test]
    fn a_step_envelope_travels_with_its_bundle_and_without_signatures() {
        let (steps, bundle) = device_bundle();
        let signed = envelope(&steps[0], &bundle);
        let json = serde_json::to_string(&signed).unwrap();
        assert!(json.contains(r#""signatures":[]"#), "{json}");
        assert!(json.contains(r#""proposal":{"body":"#), "{json}");
        assert!(!json.contains(r#""grants""#), "{json}");
        assert_eq!(
            serde_json::from_str::<Signed<RestartApproval>>(&json).unwrap(),
            signed
        );
        assert!(restart::verify_time(&steps[0], at(ISSUED + MINUTE)).is_ok());
    }
}
