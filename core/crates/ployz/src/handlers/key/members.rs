//! The Members a key change goes to, read over this CLI's own connection, and the bundle
//! this computer's Device signs for them. Cloud never relays a key or a roster here: a
//! Member that never sees a step simply never trusts the key.

use std::collections::BTreeMap;

use ployz_core::authority::{
    KeyChange, KeyMember, KeyStep, ProposalApproval, ProposalRef, ProposalStep, Signed, SignerKey,
    StepDigest, StepKind, Timestamp, TrustedGrant, TrustedKeys,
};
use ployz_core::{
    DeviceCode, DeviceSet, InspectRequest, KEY_PRIOR_CAPABILITY, KEY_STEP_CAPABILITY,
    KeyStepRequest, MachineDetails, MachineId, MachineTarget, Namespace, OperationId,
    PARENT_VERSIONS_CAPABILITY, RpcError, SCOPED_KEYS_CAPABILITY, Sealing, SealingPublicKey, op,
};

use super::super::Error;
use super::super::device::{DeviceSigning, Reach};
use crate::cluster::{Call, fan_out_entry_last};
use crate::connect::Client;
use crate::ui::Gaps;

/// What every Member must speak before a key change goes out.
const NEEDED: [&str; 4] = [
    KEY_STEP_CAPABILITY,
    SCOPED_KEYS_CAPABILITY,
    PARENT_VERSIONS_CAPABILITY,
    KEY_PRIOR_CAPABILITY,
];

/// How long a bundle's trust may be applied after it is signed.
const WINDOW_MILLIS: u64 = 10 * 60 * 1000;

/// One Member as it answered: who it is, and the keys it trusts.
pub(super) struct Member {
    pub(super) key: KeyMember,
    pub(super) name: String,
    pub(super) cluster: Option<SealingPublicKey>,
    pub(super) trusted: TrustedKeys,
}

impl Member {
    /// The grant this Member holds for the key `code` names.
    pub(super) fn grant(&self, code: DeviceCode) -> Option<&TrustedGrant> {
        self.trusted
            .grants()
            .find(|held| held.grant.key.code() == code)
    }

    /// This Member as its own Inspect reports it. The Cluster key is the live `sealing`
    /// field: the record in `machine` is the local one, which never carries it.
    pub(super) fn of(details: MachineDetails) -> Self {
        Self {
            key: KeyMember {
                id: details.id,
                public_key: details.public_key,
            },
            name: String::new(),
            cluster: details.sealing.and_then(Sealing::held_key),
            trusted: TrustedKeys::new(details.devices, details.passkeys, details.grants),
        }
    }
}

/// The Members that answered, and the ones that didn't.
pub(super) struct Roster {
    pub(super) members: Vec<Member>,
    pub(super) gaps: Gaps,
    pub(super) reach: Reach,
}

impl Roster {
    /// Every Member over `client`'s Full connection. A Member it has no Full route to is
    /// among the gaps.
    ///
    /// # Errors
    /// Refuses naming the Members too old for scoped keys, and when the Members name no
    /// Cluster key or two.
    pub(super) async fn read(client: &mut Client) -> Result<Self, Error> {
        let reach = Reach::every_member(client).await?;
        let (answers, gaps) = reach
            .fan_out(|machine_id| inspect(client.clone(), machine_id))
            .await;
        let mut members = Vec::new();
        let mut outdated = Vec::new();
        for (machine_id, seen) in answers {
            match seen {
                Seen::Member(mut member) => {
                    member.name = reach.name(&machine_id);
                    members.push(*member);
                }
                Seen::Outdated => outdated.push(reach.name(&machine_id)),
            }
        }
        if !outdated.is_empty() {
            return Err(Error::conflict(format!(
                "{} need an upgrade for CI and agent keys. Nothing changed.",
                outdated.join(", ")
            )));
        }
        let gaps = reach.named(gaps);
        Ok(Self {
            members,
            gaps,
            reach,
        })
    }

    /// Refuse unless every Member answered: a key change must reach them all.
    ///
    /// # Errors
    /// Returns `unavailable` naming the Members that didn't answer.
    pub(super) fn require_all(&self) -> Result<(), Error> {
        if self.gaps.is_complete() {
            return Ok(());
        }
        let missing: Vec<String> = self
            .gaps
            .failures
            .iter()
            .map(|failure| &failure.machine_id)
            .chain(&self.gaps.omitted)
            .map(|machine_id| self.reach.name(machine_id))
            .collect();
        Err(Error::unavailable(format!(
            "{} didn't answer over your own connection. A key change needs every server, so connect to each one. Nothing changed.",
            missing.join(", ")
        )))
    }

    /// The Cluster key every answering Member holds.
    ///
    /// # Errors
    /// Refuses when none holds one, or two Members hold different ones.
    pub(super) fn cluster(&self) -> Result<SealingPublicKey, Error> {
        let mut held = self.members.iter().filter_map(|member| member.cluster);
        let Some(first) = held.next() else {
            return Err(Error::conflict(
                "No server holds the Cluster key yet. Nothing changed.",
            ));
        };
        if held.any(|other| other != first) {
            return Err(Error::conflict(
                "Your servers hold different Cluster keys. Nothing changed.",
            ));
        }
        Ok(first)
    }

    /// The keys the first Member that answered trusts, which a Volume's version must
    /// check under.
    ///
    /// # Errors
    /// Returns `not_found` when no Member answered.
    pub(super) fn trusted(&self) -> Result<&TrustedKeys, Error> {
        self.members
            .first()
            .map(|member| &member.trusted)
            .ok_or_else(|| Error::not_found("No servers answered. Nothing changed."))
    }

    pub(super) fn names(&self) -> String {
        self.members
            .iter()
            .map(|member| member.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

enum Seen {
    Member(Box<Member>),
    Outdated,
}

async fn inspect(client: Client, machine_id: MachineId) -> Result<Option<Seen>, RpcError> {
    let target = MachineTarget::from(&machine_id);
    for capability in NEEDED {
        let held = client
            .supports(Some(&target), capability)
            .await
            .map_err(crate::connect::rpc_error)?;
        if !held {
            return Ok(Some(Seen::Outdated));
        }
    }
    let details = client
        .call_shared::<op::Inspect>(InspectRequest::default(), Call::once().to(&target))
        .await
        .map_err(crate::connect::rpc_error)?;
    Ok(Some(Seen::Member(Box::new(Member::of(details)))))
}

/// The steps of one bundle this computer's Device signed.
pub(super) struct Bundle {
    steps: Vec<Signed<KeyStep>>,
}

impl Bundle {
    /// `changes` as the steps of one bundle `signing` signs for `cluster`, issued now.
    ///
    /// # Errors
    /// Returns a command error when a body has no canonical form.
    pub(super) fn sign(
        signing: &DeviceSigning,
        cluster: SealingPublicKey,
        changes: Vec<KeyChange>,
    ) -> Result<Self, Error> {
        let issued_at = Timestamp::now();
        let expires_at = Timestamp::from_millis(issued_at.as_millis() + WINDOW_MILLIS)
            .map_err(Error::command)?;
        let operation =
            OperationId::from_uuid(&uuid::Uuid::new_v4().to_string()).map_err(Error::from)?;
        let signer = SignerKey::Device(signing.device_key());
        let mut bodies = Vec::new();
        let mut steps = Vec::new();
        for (index, change) in (0u32..).zip(changes) {
            let body = KeyStep {
                cluster,
                change,
                signer,
                proposal: Some(ProposalRef { operation, index }),
            };
            let unsigned = Signed::unsigned(&body).map_err(Error::command)?;
            steps.push(ProposalStep {
                index,
                kind: StepKind::Key,
                digest: StepDigest::of(&unsigned.body),
            });
            bodies.push(body);
        }
        let bundle = signing.sign(&ProposalApproval {
            cluster,
            environment: Namespace::system(),
            operation,
            steps,
            issued_at,
            expires_at,
            signer,
        })?;
        let steps = bodies
            .iter()
            .map(|body| Signed::proposed(body, bundle.clone()).map_err(Error::command))
            .collect::<Result<_, _>>()?;
        Ok(Self { steps })
    }

    /// Hand each Member the steps that name it, in order.
    pub(super) async fn send(
        &self,
        client: &Client,
        reach: &Reach,
    ) -> (Vec<(MachineId, DeviceSet)>, Gaps) {
        let mut by_member: BTreeMap<MachineId, Vec<Signed<KeyStep>>> = BTreeMap::new();
        for step in &self.steps {
            let members = match step.open().map(|body| body.change) {
                Ok(KeyChange::Trust { members, .. }) => members,
                Ok(KeyChange::Remove { .. }) | Err(_) => continue,
            };
            for member in members {
                by_member.entry(member.id).or_default().push(step.clone());
            }
        }
        let entry = reach.entry();
        let result = fan_out_entry_last(by_member.keys().copied(), entry, |machine_id| {
            let client = client.clone();
            let steps = by_member.get(&machine_id).cloned().unwrap_or_default();
            async move {
                let target = MachineTarget::from(&machine_id);
                let mut last = None;
                for step in steps {
                    last = Some(
                        client
                            .call_shared::<op::KeyStep>(
                                KeyStepRequest { step },
                                Call::once().to(&target),
                            )
                            .await
                            .map_err(crate::connect::rpc_error)?,
                    );
                }
                Ok::<_, RpcError>(last)
            }
        })
        .await;
        let gaps = reach.named(Gaps::of(&result));
        let answers = result
            .successes
            .into_iter()
            .map(|success| (success.machine_id, success.value))
            .collect();
        (answers, gaps)
    }
}

#[cfg(test)]
#[path = "members_tests.rs"]
mod tests;
