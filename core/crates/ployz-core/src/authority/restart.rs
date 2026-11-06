//! The approval a Device signs for one Service restart, and the checks a Machine runs
//! before it acts on one.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    canonical::Timestamp,
    keys::{Purpose, Signed, SigningPublicKey, Statement},
    refusal::AuthorityRefusal,
};
use crate::{
    ContainerId, ContainerLocator, ContainerWant, DeviceKey, MachineId, Namespace, OperationId,
    RunTarget, SealingPublicKey, ServiceName,
};

/// The workflow an approval is for. One approval never serves another kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowKind {
    Restart,
    Stop,
}

impl WorkflowKind {
    /// Whether this workflow authorizes the requested lifecycle step.
    #[must_use]
    pub fn permits(self, step: RestartStep) -> bool {
        match self {
            Self::Restart => matches!(
                step,
                RestartStep::Hold | RestartStep::Stop | RestartStep::Start
            ),
            Self::Stop => matches!(step, RestartStep::Hold | RestartStep::Stop),
        }
    }
}

/// A Device approves one Container's lifecycle steps in one Cluster and Service,
/// under one operation with a bounded lifetime.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RestartApproval {
    /// The Cluster's Sealing public key, which every Machine holds and Cloud cannot mint.
    pub cluster: SealingPublicKey,
    pub operation: OperationId,
    pub kind: WorkflowKind,
    pub namespace: Namespace,
    pub service: ServiceName,
    /// Sorted and distinct.
    #[serde(deserialize_with = "ordered_machines")]
    pub machines: Vec<MachineId>,
    pub container: ContainerId,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
    /// The Device that signed, which the signature must come from.
    pub signer: DeviceKey,
}

/// Maximum signed lifetime, excluding clock skew.
pub const MAX_LIFETIME_MILLIS: u64 = 15 * 60 * 1000;
/// Clock tolerance in milliseconds on either side of the signed interval.
pub const CLOCK_SKEW_MILLIS: u64 = 2 * 60 * 1000;

fn ordered_machines<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<MachineId>, D::Error> {
    let machines = Vec::<MachineId>::deserialize(deserializer)?;
    if machines
        .iter()
        .zip(machines.iter().skip(1))
        .any(|(left, right)| left >= right)
    {
        return Err(serde::de::Error::custom(
            "Machines must be sorted and distinct",
        ));
    }
    Ok(machines)
}

impl Statement for RestartApproval {
    const PURPOSE: Purpose = Purpose::Workflow;
}

/// The steps a restart takes on one Container, in the order they run. A Machine records
/// the furthest step each operation reached per Container, so a step never runs twice
/// and an earlier step never runs after a later one.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartStep {
    Hold,
    Out,
    Stop,
    Start,
    Remove,
}

impl RestartStep {
    /// The lifecycle step on an existing Container. `None` requires deploy authorization,
    /// even when a new-creation request would adopt an existing Container.
    #[must_use]
    pub fn of(want: &ContainerWant) -> Option<(Self, &ContainerLocator)> {
        match want {
            ContainerWant::Held { container, .. } => Some((Self::Hold, container)),
            ContainerWant::Out { container, .. } => Some((Self::Out, container)),
            ContainerWant::Stopped { container, .. } => Some((Self::Stop, container)),
            ContainerWant::Gone { container, .. } => Some((Self::Remove, container)),
            ContainerWant::Running {
                container: RunTarget::Existing(container),
                ..
            } => Some((Self::Start, container)),
            ContainerWant::Running {
                container: RunTarget::New(_),
                ..
            } => None,
        }
    }
}

/// What a Machine knows when it checks an approval against one request.
#[derive(Clone, Debug)]
pub struct RestartFacts<'a> {
    /// The Sealing public key this Machine holds, or `None` while it holds none.
    pub cluster: Option<SealingPublicKey>,
    pub machine: &'a MachineId,
    pub devices: &'a [DeviceKey],
    /// The Service the requested Container belongs to.
    pub namespace: &'a Namespace,
    pub service: &'a ServiceName,
    pub container: &'a ContainerId,
    pub step: RestartStep,
    pub now: Timestamp,
}

/// Check `approval` is one of this Machine's Devices approving exactly this request.
///
/// # Errors
/// Returns the first [`AuthorityRefusal`] found: a signature problem, a malformed body,
/// a body naming a different Cluster, workflow, Service, Machine or signer, or an
/// expired approval. Replay is the Machine's consumption record to refuse.
pub fn verify(
    approval: &Signed<RestartApproval>,
    facts: &RestartFacts<'_>,
) -> Result<RestartApproval, AuthorityRefusal> {
    let signers =
        approval.verified_signers(facts.devices.iter().copied().map(SigningPublicKey::from))?;
    let body = approval.open()?;
    let Some(cluster) = facts.cluster else {
        return Err(AuthorityRefusal::NoClusterIdentity);
    };
    let checks = [
        ("the signer", signers.contains(&body.signer.into())),
        ("a Cluster", body.cluster == cluster),
        ("a workflow", body.kind.permits(facts.step)),
        ("an Environment", &body.namespace == facts.namespace),
        ("a Service", &body.service == facts.service),
        ("Machines", body.machines.contains(facts.machine)),
        ("a Container", &body.container == facts.container),
    ];
    if let Some((field, _)) = checks.iter().find(|(_, holds)| !holds) {
        return Err(AuthorityRefusal::Mismatch { field });
    }
    verify_time(&body, facts.now)?;
    Ok(body)
}

/// Check the signed lifetime and the Machine's clock, including its persisted time floor.
///
/// # Errors
/// Returns a mismatch for an invalid lifetime or future issue time, or an expired refusal.
pub fn verify_time(body: &RestartApproval, now: Timestamp) -> Result<(), AuthorityRefusal> {
    if body.expires_at < body.issued_at
        || body.expires_at.as_millis() - body.issued_at.as_millis() > MAX_LIFETIME_MILLIS
    {
        return Err(AuthorityRefusal::Mismatch {
            field: "a lifetime",
        });
    }
    if body.issued_at > now.plus(CLOCK_SKEW_MILLIS) {
        return Err(AuthorityRefusal::Mismatch {
            field: "an issue time",
        });
    }
    if now > body.expires_at.plus(CLOCK_SKEW_MILLIS) {
        return Err(AuthorityRefusal::Expired);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::keys::Signer;

    fn signer(seed: u8) -> Signer {
        Signer::from_seed(&[seed; 32])
    }

    fn device(signer: &Signer) -> DeviceKey {
        signer.public().into()
    }

    fn approval(signer: &Signer) -> RestartApproval {
        RestartApproval {
            cluster: SealingPublicKey::from_bytes([9; 32]),
            operation: OperationId::parse("0f8fad5bd9cb469fa16570867728950e").unwrap(),
            kind: WorkflowKind::Restart,
            namespace: Namespace::parse("app").unwrap(),
            service: ServiceName::parse("api").unwrap(),
            machines: vec![
                MachineId::parse("1111111111111111111111111111111a").unwrap(),
                MachineId::parse("2222222222222222222222222222222b").unwrap(),
            ],
            container: ContainerId::parse("a".repeat(64)).unwrap(),
            issued_at: Timestamp::from_millis(100_000).unwrap(),
            expires_at: Timestamp::from_millis(1_000_000).unwrap(),
            signer: device(signer),
        }
    }

    struct Machine {
        id: MachineId,
        devices: Vec<DeviceKey>,
        namespace: Namespace,
        service: ServiceName,
        container: ContainerId,
    }

    impl Machine {
        fn facts(&self) -> RestartFacts<'_> {
            RestartFacts {
                cluster: Some(SealingPublicKey::from_bytes([9; 32])),
                machine: &self.id,
                devices: &self.devices,
                namespace: &self.namespace,
                service: &self.service,
                container: &self.container,
                step: RestartStep::Stop,
                now: Timestamp::from_millis(999_999).unwrap(),
            }
        }
    }

    fn machine(approved: &[&Signer]) -> Machine {
        Machine {
            id: MachineId::parse("2222222222222222222222222222222b").unwrap(),
            devices: approved.iter().map(|signer| device(signer)).collect(),
            namespace: Namespace::parse("app").unwrap(),
            service: ServiceName::parse("api").unwrap(),
            container: ContainerId::parse("a".repeat(64)).unwrap(),
        }
    }

    #[test]
    fn an_approved_device_signing_this_request_passes() {
        let signer = signer(1);
        let signed = Signed::sign(&approval(&signer), &signer).unwrap();
        let machine = machine(&[&signer]);
        assert_eq!(
            verify(&signed, &machine.facts()).unwrap(),
            approval(&signer)
        );
    }

    #[test]
    fn the_body_travels_canonically_with_a_hex_signature() {
        let signer = signer(1);
        let signed = Signed::sign(&approval(&signer), &signer).unwrap();
        assert!(
            signed.body.starts_with(r#"{"cluster":""#),
            "{}",
            signed.body
        );
        assert!(signed.body.contains(r#""kind":"restart""#));
        let [signature] = signed.signatures.as_slice() else {
            panic!("one signature, found {:?}", signed.signatures);
        };
        assert_eq!(signature.signature.len(), 64);
        let json = serde_json::to_string(&signed).unwrap();
        assert_eq!(
            serde_json::from_str::<Signed<RestartApproval>>(&json).unwrap(),
            signed
        );
    }

    #[test]
    fn a_signed_workflow_permits_only_its_own_steps() {
        let signer = signer(1);
        let machine = machine(&[&signer]);
        for kind in [WorkflowKind::Restart, WorkflowKind::Stop] {
            let signed = Signed::sign(
                &RestartApproval {
                    kind,
                    ..approval(&signer)
                },
                &signer,
            )
            .unwrap();
            for (step, allowed) in [
                (RestartStep::Hold, true),
                (RestartStep::Stop, true),
                (RestartStep::Start, kind == WorkflowKind::Restart),
                (RestartStep::Out, false),
                (RestartStep::Remove, false),
            ] {
                let mut facts = machine.facts();
                facts.step = step;
                assert_eq!(
                    verify(&signed, &facts).is_ok(),
                    allowed,
                    "{kind:?} {step:?}"
                );
            }
        }
    }

    #[test]
    fn a_key_the_machine_has_not_approved_is_unknown() {
        let stranger = signer(2);
        let signed = Signed::sign(&approval(&stranger), &stranger).unwrap();
        let machine = machine(&[&signer(1)]);
        assert_eq!(
            verify(&signed, &machine.facts()).unwrap_err(),
            AuthorityRefusal::UnknownKey {
                key: stranger.public()
            }
        );
    }

    #[test]
    fn a_body_changed_after_signing_fails_its_signature() {
        let signer = signer(1);
        let mut signed = Signed::sign(&approval(&signer), &signer).unwrap();
        signed.body = signed
            .body
            .replace(r#""service":"api""#, r#""service":"db""#);
        let machine = machine(&[&signer]);
        assert_eq!(
            verify(&signed, &machine.facts()).unwrap_err(),
            AuthorityRefusal::BadSignature {
                key: signer.public()
            }
        );
    }

    #[test]
    fn a_signature_by_another_device_over_a_body_naming_this_one_mismatches_the_signer() {
        let named = signer(1);
        let other = signer(2);
        let signed = Signed::sign(&approval(&named), &other).unwrap();
        let machine = machine(&[&named, &other]);
        assert_eq!(
            verify(&signed, &machine.facts()).unwrap_err(),
            AuthorityRefusal::Mismatch {
                field: "the signer"
            }
        );
    }

    #[test]
    fn no_signature_is_unsigned() {
        let signer = signer(1);
        let mut signed = Signed::sign(&approval(&signer), &signer).unwrap();
        signed.signatures.clear();
        let machine = machine(&[&signer]);
        assert_eq!(
            verify(&signed, &machine.facts()).unwrap_err(),
            AuthorityRefusal::Unsigned {
                purpose: Purpose::Workflow
            }
        );
    }

    #[test]
    fn each_bound_field_must_match_the_request() {
        let signer = signer(1);
        let mut machine = machine(&[&signer]);
        let signed = Signed::sign(&approval(&signer), &signer).unwrap();

        let mut facts = machine.facts();
        facts.cluster = Some(SealingPublicKey::from_bytes([8; 32]));
        assert_eq!(
            verify(&signed, &facts).unwrap_err(),
            AuthorityRefusal::Mismatch { field: "a Cluster" }
        );
        facts.cluster = None;
        assert_eq!(
            verify(&signed, &facts).unwrap_err(),
            AuthorityRefusal::NoClusterIdentity
        );

        machine.namespace = Namespace::parse("other").unwrap();
        assert_eq!(
            verify(&signed, &machine.facts()).unwrap_err(),
            AuthorityRefusal::Mismatch {
                field: "an Environment"
            }
        );
        machine.namespace = Namespace::parse("app").unwrap();

        machine.service = ServiceName::parse("db").unwrap();
        assert_eq!(
            verify(&signed, &machine.facts()).unwrap_err(),
            AuthorityRefusal::Mismatch { field: "a Service" }
        );
        machine.service = ServiceName::parse("api").unwrap();

        machine.id = MachineId::parse("3333333333333333333333333333333c").unwrap();
        assert_eq!(
            verify(&signed, &machine.facts()).unwrap_err(),
            AuthorityRefusal::Mismatch { field: "Machines" }
        );
    }

    #[test]
    fn an_approval_past_its_expiry_is_refused() {
        let signer = signer(1);
        let signed = Signed::sign(&approval(&signer), &signer).unwrap();
        let machine = machine(&[&signer]);
        let mut facts = machine.facts();
        facts.now = Timestamp::from_millis(1_000_000).unwrap();
        assert!(
            verify(&signed, &facts).is_ok(),
            "the last millisecond still counts"
        );
        facts.now = Timestamp::from_millis(1_000_001 + CLOCK_SKEW_MILLIS).unwrap();
        assert_eq!(
            verify(&signed, &facts).unwrap_err(),
            AuthorityRefusal::Expired
        );
    }

    #[test]
    fn lifetime_and_clock_skew_are_bounded() {
        let signer = signer(1);
        let mut body = approval(&signer);
        body.issued_at = Timestamp::from_millis(100_000).unwrap();
        body.expires_at = body.issued_at.plus(MAX_LIFETIME_MILLIS);
        assert!(verify_time(&body, body.expires_at.plus(CLOCK_SKEW_MILLIS)).is_ok());
        assert_eq!(
            verify_time(&body, body.expires_at.plus(CLOCK_SKEW_MILLIS + 1)),
            Err(AuthorityRefusal::Expired)
        );
        assert!(verify_time(&body, Timestamp::from_millis(0).unwrap()).is_ok());
        body.issued_at = Timestamp::from_millis(CLOCK_SKEW_MILLIS + 1).unwrap();
        assert!(matches!(
            verify_time(&body, Timestamp::from_millis(0).unwrap()),
            Err(AuthorityRefusal::Mismatch {
                field: "an issue time"
            })
        ));
        body.issued_at = Timestamp::from_millis(0).unwrap();
        body.expires_at = body.issued_at.plus(MAX_LIFETIME_MILLIS + 1);
        assert!(matches!(
            verify_time(&body, body.issued_at),
            Err(AuthorityRefusal::Mismatch {
                field: "a lifetime"
            })
        ));
    }

    #[test]
    fn unsorted_or_repeated_machines_and_malformed_signatures_do_not_parse() {
        let signer = signer(1);
        let mut body = approval(&signer);
        body.machines.reverse();
        assert!(Signed::sign(&body, &signer).unwrap().open().is_err());
        let machine = *body.machines.first().unwrap();
        body.machines = vec![machine, machine];
        assert!(Signed::sign(&body, &signer).unwrap().open().is_err());
        let envelope = serde_json::json!({ "body":"{}", "signatures":[{ "key": signer.public(), "signature":"00" }] });
        assert!(serde_json::from_value::<Signed<RestartApproval>>(envelope).is_err());
    }

    #[test]
    fn a_body_with_a_field_this_machine_cannot_read_is_malformed() {
        let signer = signer(1);
        let mut signed = Signed::sign(&approval(&signer), &signer).unwrap();
        signed.body = signed.body.replacen('{', r#"{"also":"x","#, 1);
        let machine = machine(&[&signer]);
        assert_eq!(
            verify(&signed, &machine.facts()).unwrap_err(),
            AuthorityRefusal::BadSignature {
                key: signer.public()
            },
            "a changed body never reaches the parser"
        );
        let mut unchecked = Signed::sign(&approval(&signer), &signer).unwrap();
        unchecked.body = signed.body.clone();
        assert!(matches!(
            unchecked.open().unwrap_err(),
            AuthorityRefusal::Malformed { .. }
        ));
    }

    #[test]
    fn restart_steps_are_the_wants_that_act_on_an_existing_container() {
        use crate::{ContainerId, RunRotation};
        let id = ContainerId::parse("a".repeat(64)).unwrap();
        let locator = ContainerLocator::Id(id);
        let held = ContainerWant::Held {
            container: locator.clone(),
            run: crate::DeploymentLogId::parse("1111111111111111111111111111111a").unwrap(),
            expect: None,
        };
        let stopped = ContainerWant::Stopped {
            container: locator.clone(),
            drain: crate::Drain::NONE,
            stop: crate::StopOptions::default(),
        };
        let started = ContainerWant::Running {
            container: RunTarget::Existing(locator.clone()),
            rotation: RunRotation::Pending,
        };
        let out = ContainerWant::Out {
            container: locator.clone(),
            allow_empty: false,
            expect: None,
        };
        assert_eq!(RestartStep::of(&held), Some((RestartStep::Hold, &locator)));
        assert_eq!(
            RestartStep::of(&stopped),
            Some((RestartStep::Stop, &locator))
        );
        assert_eq!(
            RestartStep::of(&started),
            Some((RestartStep::Start, &locator))
        );
        assert_eq!(RestartStep::of(&out), Some((RestartStep::Out, &locator)));
        assert!(RestartStep::Hold < RestartStep::Stop && RestartStep::Stop < RestartStep::Start);
    }
}
