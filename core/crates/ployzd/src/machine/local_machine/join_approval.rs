//! Machine-local join proposals and Device-approved admission.

use ployz_core::authority::MismatchField;
use std::collections::BTreeMap;

use ployz_core::{
    ApproveJoinRequest, ConfirmJoinRequest, DeviceKey, JoinApproved, JoinConfirmed, Machine,
    MachineId, PendingJoins, WireGuardPublicKey,
    authority::{AuthorityRefusal, JoinApproval, Purpose, Signed, SignerKey, Timestamp, join},
    first_key::FirstKeyHint,
};
use serde::{Deserialize, Serialize};

use super::{Error, LocalMachine};

#[derive(Default, Deserialize, Serialize)]
struct Joins {
    proposals: BTreeMap<MachineId, Proposal>,
}

#[derive(Deserialize, Serialize)]
struct Proposal {
    body: JoinApproval,
    approval: Option<Signed<JoinApproval>>,
    /// The key the joiner said its First Key file waits for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    first_key: Option<FirstKeyHint>,
}

/// What entering a join found for one Machine.
pub(crate) enum Entered {
    /// An earlier entry already answered this request; the answer stands.
    Replayed(super::consumption::JoinResult),
    /// Nothing is stored for the Machine, so this entry may admit it.
    Fresh(FreshJoin),
}

/// The fact that no answer is stored for `machine`, which only
/// [`LocalMachine::enter_join`] can state and which consuming a join takes.
pub(crate) struct FreshJoin {
    machine: MachineId,
}

impl LocalMachine {
    fn read_joins(&self) -> Result<Joins, Error> {
        match std::fs::read(self.owner.data_dir().join("join-approvals.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(std::io::Error::other)
                .map_err(Error::JoinRecord),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Joins::default()),
            Err(error) => Err(Error::JoinRecord(error)),
        }
    }

    fn save_joins(&self, mut joins: Joins) -> Result<(), Error> {
        let now = Timestamp::now();
        joins
            .proposals
            .retain(|_, proposal| proposal.body.expires_at >= now);
        let bytes = serde_json::to_vec(&joins)
            .map_err(std::io::Error::other)
            .map_err(Error::JoinRecord)?;
        crate::filesystem::atomic_write(
            &self.owner.data_dir().join("join-approvals.json"),
            &bytes,
            0o600,
        )
        .map_err(Error::JoinRecord)
    }

    /// The joins waiting for a person's approval: drafted and not yet approved, or answered
    /// under a key other than the one the joiner said it waits for, which that joiner refuses.
    pub(crate) fn pending_joins(&self) -> Result<PendingJoins, Error> {
        let now = Timestamp::now();
        let mut pending = PendingJoins {
            joins: Vec::new(),
            first_keys: std::collections::BTreeMap::new(),
        };
        for (machine, proposal) in self.read_joins()?.proposals {
            if proposal.body.expires_at < now || proposal.approval.is_some() {
                continue;
            }
            if let Some(stored) = self.approvals.join_result(machine)?
                && !self.answered_with_another_key(&stored, proposal.first_key)
            {
                continue;
            }
            if let Some(hint) = proposal.first_key {
                pending.first_keys.insert(machine, hint);
            }
            pending.joins.push(proposal.body);
        }
        Ok(pending)
    }

    /// Whether `stored` answered a joiner that waits for `first_key` under an approval some
    /// other key signed. Such a joiner refuses the capsule, unless the person's CLI pinned
    /// that key, so a fresh approval may take its place until the joiner confirms.
    fn answered_with_another_key(
        &self,
        stored: &super::consumption::JoinResult,
        first_key: Option<FirstKeyHint>,
    ) -> bool {
        first_key.is_some_and(|hint| {
            join::approver(&stored.approval, &self.record().trusted_keys())
                .is_none_or(|approver| !hint.names(&approver))
        })
    }

    pub(crate) async fn approve_join(
        &self,
        request: ApproveJoinRequest,
    ) -> Result<JoinApproved, Error> {
        self.finish_mutation(move |local| async move {
            let body = request.approval.open().map_err(Error::JoinRefused)?;
            local
                .refuse_removed(body.machine.id, Some(body.machine.public_key))
                .await?;
            local.verify_join(&request.approval, &body.machine, body.network)?;
            let mut joins = local.read_joins()?;
            let first_key = joins
                .proposals
                .get(&body.machine.id)
                .and_then(|proposal| proposal.first_key);
            if let Some(stored) = local.approvals.join_result(body.machine.id)? {
                local
                    .require_replaceable(&stored, first_key, &request.approval)
                    .await?;
            }
            joins.proposals.insert(
                body.machine.id,
                Proposal {
                    body,
                    approval: Some(request.approval),
                    first_key,
                },
            );
            local.save_joins(joins)?;
            Ok(JoinApproved {})
        })
        .await
    }

    /// Refuse a fresh approval for a join already answered, unless the answer was approved
    /// under a key the joiner doesn't wait for, the joiner hasn't confirmed it, and the fresh
    /// one is signed by the key it does wait for. A joiner is confirmed once its row is out.
    async fn require_replaceable(
        &self,
        stored: &super::consumption::JoinResult,
        first_key: Option<FirstKeyHint>,
        fresh: &Signed<JoinApproval>,
    ) -> Result<(), Error> {
        let body = stored.approval.open().map_err(Error::JoinRefused)?;
        let confirmed = self
            .replicated()?
            .machine(body.machine.id.as_str())
            .await?
            .is_some_and(|row| row.public_key == body.machine.public_key);
        let Some(hint) =
            first_key.filter(|_| !confirmed && self.answered_with_another_key(stored, first_key))
        else {
            return Err(Error::JoinConsumed(body.machine.id));
        };
        let trusted = self.record().trusted_keys();
        if join::approver(fresh, &trusted).is_none_or(|approver| !hint.names(&approver)) {
            return Err(Error::JoinRefused(AuthorityRefusal::Mismatch {
                field: MismatchField::FirstKey,
            }));
        }
        Ok(())
    }

    /// Before a joiner registers again: when a person approved its join afresh to replace
    /// an answer the joiner refused, drop that answer so the fresh approval admits it. An
    /// approval that no longer verifies here leaves the answer as it is. `confirmed` when
    /// the joiner's row is out, which keeps the answer it confirmed.
    pub(crate) fn give_way_to_a_fresh_approval(
        &self,
        machine: MachineId,
        confirmed: bool,
    ) -> Result<(), Error> {
        if confirmed {
            return Ok(());
        }
        let Some(fresh) = self
            .read_joins()?
            .proposals
            .get(&machine)
            .and_then(|proposal| proposal.approval.clone())
        else {
            return Ok(());
        };
        let body = fresh.open().map_err(Error::JoinRefused)?;
        if self
            .verify_join(&fresh, &body.machine, body.network)
            .is_err()
        {
            return Ok(());
        }
        self.approvals.release_join(machine, &fresh)
    }

    /// Stop offering `machine`'s join for approval once it confirmed its answer.
    pub(crate) fn settle_join(&self, machine: MachineId) -> Result<(), Error> {
        let mut joins = self.read_joins()?;
        if joins.proposals.remove(&machine).is_some() {
            self.save_joins(joins)?;
        }
        Ok(())
    }

    /// Refuse a join for a Member this Machine saw removed: in its own record, or by its
    /// store showing the Member's row deleted, which it then records. Every join entry runs
    /// it before [`Self::enter_join`], so neither a fresh join nor a stored answer brings a
    /// removed Member back. `public_key` comes from the request, so it only refuses: the
    /// removal is recorded with the key this Machine verified for the Member, if any, so a
    /// request can't pair a deleted ID with a live Server's key.
    pub(crate) async fn refuse_removed(
        &self,
        machine: MachineId,
        public_key: Option<WireGuardPublicKey>,
    ) -> Result<(), Error> {
        if self.record().removes_member(machine, public_key) {
            return Err(Error::MemberRemoved(machine));
        }
        if self
            .replicated()?
            .removed_machines(&[machine])
            .await?
            .contains(&machine)
        {
            let removed = crate::machine::RemovedMember {
                id: machine,
                public_key: self.joining_key(machine)?,
            };
            self.owner
                .mutate(move |store| store.remove_members(&[removed]))
                .await??;
            return Err(Error::MemberRemoved(machine));
        }
        Ok(())
    }

    /// The WireGuard key a signed approval gave a joining `machine` here: from its stored
    /// answer or its approved pending join. `None` when it holds neither.
    pub(crate) fn joining_key(
        &self,
        machine: MachineId,
    ) -> Result<Option<WireGuardPublicKey>, Error> {
        if let Some(stored) = self.approvals.join_result(machine)? {
            return Ok(stored
                .approval
                .open()
                .ok()
                .map(|body| body.machine.public_key));
        }
        Ok(self
            .read_joins()?
            .proposals
            .get(&machine)
            .filter(|proposal| proposal.approval.is_some())
            .map(|proposal| proposal.body.machine.public_key))
    }

    /// Take the restrictions this joiner's admitting Machine answered its confirmation with,
    /// once their MAC shows a holder of the Cluster's Sealing Key made them for this join:
    /// raise the floors, hold each removed key and Member removed, and stop waiting, so
    /// catch-up may finish. A joiner refuses anything else, so a relay that drops or forges
    /// them only keeps it from participating.
    pub(crate) async fn confirm_join(
        &self,
        request: ConfirmJoinRequest,
    ) -> Result<JoinConfirmed, Error> {
        self.finish_mutation(move |local| async move {
            let record = local.record();
            let approval = record
                .machine()
                .and_then(|machine| machine.join_approval.clone())
                .ok_or(Error::Store(crate::machine::StoreError::NotJoining))?;
            let key = crate::sealing_key::SealingKey::load(local.owner.data_dir())
                .map_err(Error::JoinRecord)?
                .ok_or(Error::JoinRefused(AuthorityRefusal::NoClusterIdentity))?;
            let confirmation = key.confirm_join(&approval);
            if !key.restricts_join(&approval, &confirmation, &request.restrictions) {
                return Err(Error::JoinRefused(AuthorityRefusal::Mismatch {
                    field: MismatchField::JoinRestrictions,
                }));
            }
            let set = request.restrictions.set;
            local
                .approvals
                .install_floors(key.public_key(), &set.floors)?;
            local
                .owner
                .mutate(move |store| store.apply_join_restrictions(&set))
                .await??;
            Ok(JoinConfirmed {})
        })
        .await
    }

    #[cfg(test)]
    pub(crate) fn require_unused_join(&self, machine: MachineId) -> Result<(), Error> {
        self.approvals.require_unused_join(machine)
    }

    /// Where a join stands for `machine` as this Machine is asked to admit it: already
    /// answered, or untouched. A stored answer is replayed only when `same_entry` says the
    /// request repeats the one it answered; any other request for that Machine is
    /// [`Error::JoinConsumed`].
    pub(crate) fn enter_join(
        &self,
        machine: MachineId,
        same_entry: impl FnOnce(&super::consumption::JoinResult) -> Result<bool, Error>,
    ) -> Result<Entered, Error> {
        let Some(stored) = self.approvals.join_result(machine)? else {
            return Ok(Entered::Fresh(FreshJoin { machine }));
        };
        if same_entry(&stored)? {
            Ok(Entered::Replayed(stored))
        } else {
            Err(Error::JoinConsumed(machine))
        }
    }

    /// Record the answer to a join [`enter_join`](Self::enter_join) found untouched, so no
    /// join is consumed without having been entered.
    pub(crate) fn consume_join(
        &self,
        fresh: FreshJoin,
        result: super::consumption::JoinResult,
    ) -> Result<(), Error> {
        let machine = result
            .approval
            .open()
            .map_err(Error::JoinRefused)?
            .machine
            .id;
        if machine != fresh.machine {
            return Err(Error::JoinRefused(AuthorityRefusal::Mismatch {
                field: MismatchField::JoiningMachine,
            }));
        }
        self.approvals.consume_join(result, Timestamp::now())?;
        // A joiner that named the key it waits for keeps its draft until it confirms, so if
        // it refuses this answer it can still be approved with that key.
        let mut joins = self.read_joins()?;
        match joins.proposals.get_mut(&machine) {
            Some(proposal) if proposal.first_key.is_some() => proposal.approval = None,
            _ => {
                joins.proposals.remove(&machine);
            }
        }
        self.save_joins(joins)
    }

    pub(crate) fn verify_join(
        &self,
        approval: &Signed<JoinApproval>,
        machine: &join::JoinAllocation,
        network: ipnet::Ipv4Net,
    ) -> Result<JoinApproval, Error> {
        let record = self.record();
        let cluster = crate::sealing_key::SealingKey::load(self.owner.data_dir())
            .map_err(Error::JoinRecord)?
            .map(|key| key.public_key())
            .ok_or(Error::JoinRefused(AuthorityRefusal::NoClusterIdentity))?;
        join::verify(
            approval,
            cluster,
            record.id(),
            &record.trusted_keys(),
            machine,
            network,
            self.approvals.observed_now(Timestamp::now())?,
        )
        .map_err(Error::JoinRefused)
    }

    pub(crate) fn admit_join(
        &self,
        fresh: &FreshJoin,
        machine: &Machine,
        network: ipnet::Ipv4Net,
        approval: Option<Signed<JoinApproval>>,
        peers: &[Machine],
        first_key: Option<FirstKeyHint>,
    ) -> Result<Signed<JoinApproval>, Error> {
        debug_assert_eq!(
            fresh.machine, machine.id,
            "a join is admitted as it was entered"
        );
        let joins = self.read_joins()?;
        let approval = approval.or_else(|| {
            joins
                .proposals
                .get(&machine.id)
                .and_then(|proposal| proposal.approval.clone())
        });
        if let Some(approval) = approval {
            let body =
                self.verify_join(&approval, &join::JoinAllocation::from(machine), network)?;
            // Cloud relays the body a person signs, so it could add a Machine of its own that
            // the joiner would then trust as a peer. Seal only to a joiner whose starting
            // peers are this Machine's own.
            let ours = body.bootstrap.iter().all(|allocation| {
                peers.iter().any(|peer| {
                    peer.id == allocation.id && peer.public_key == allocation.public_key
                })
            });
            if !ours {
                return Err(Error::JoinRefused(AuthorityRefusal::Mismatch {
                    field: MismatchField::BootstrapPeers,
                }));
            }
            return Ok(approval);
        }
        self.draft_join(joins, machine, network, peers, first_key)?;
        Err(Error::JoinRefused(AuthorityRefusal::Unsigned {
            purpose: Purpose::Join,
        }))
    }

    /// Offer `machine`'s join for a person's approval again when its answer was approved
    /// under a key other than the one it waits for, the joiner hasn't confirmed, and no draft
    /// waits for a signature: its window closed, the answer predates kept drafts, or the
    /// approval that would replace the answer no longer verifies. The fresh draft gets a new
    /// window and the key the joiner named; the old answer stands until an approval under
    /// that key replaces it.
    pub(crate) fn redraft_refused_join(
        &self,
        stored: &super::consumption::JoinResult,
        machine: &Machine,
        network: ipnet::Ipv4Net,
        peers: &[Machine],
        first_key: Option<FirstKeyHint>,
    ) -> Result<(), Error> {
        if !self.answered_with_another_key(stored, first_key) {
            return Ok(());
        }
        let joins = self.read_joins()?;
        let now = Timestamp::now();
        if joins
            .proposals
            .get(&machine.id)
            .is_some_and(|proposal| proposal.approval.is_none() && proposal.body.expires_at >= now)
        {
            return Ok(());
        }
        self.draft_join(joins, machine, network, peers, first_key)
    }

    /// Save a draft of `machine`'s join, with a fresh window and the trust this Machine
    /// holds now, for a person to sign.
    fn draft_join(
        &self,
        mut joins: Joins,
        machine: &Machine,
        network: ipnet::Ipv4Net,
        peers: &[Machine],
        first_key: Option<FirstKeyHint>,
    ) -> Result<(), Error> {
        let record = self.record();
        let cluster = crate::sealing_key::SealingKey::load(self.owner.data_dir())
            .map_err(Error::JoinRecord)?
            .map(|key| key.public_key())
            .ok_or(Error::JoinRefused(AuthorityRefusal::NoClusterIdentity))?;
        let issued_at = Timestamp::now();
        joins
            .proposals
            .retain(|_, proposal| proposal.body.expires_at >= issued_at);
        if !joins.proposals.contains_key(&machine.id) && joins.proposals.len() >= 128 {
            return Err(Error::JoinRefused(AuthorityRefusal::Mismatch {
                field: MismatchField::PendingJoinCapacity,
            }));
        }
        joins.proposals.insert(
            machine.id,
            Proposal {
                body: JoinApproval {
                    cluster,
                    machine: machine.into(),
                    network,
                    sealing_recipient: machine.public_key,
                    devices: record.devices().collect(),
                    passkeys: record
                        .passkeys()
                        .iter()
                        .map(|trusted| trusted.passkey.clone())
                        .collect(),
                    grants: record
                        .grants()
                        .iter()
                        .map(|held| held.grant.clone())
                        .collect(),
                    admitting_machines: vec![record.id()],
                    bootstrap: peers.iter().map(Into::into).collect(),
                    issued_at,
                    expires_at: issued_at
                        .plus(ployz_core::authority::validity::MAX_LIFETIME_MILLIS),
                    signer: SignerKey::Device(DeviceKey::from_bytes([0; 32])),
                    proposal: None,
                },
                approval: None,
                first_key,
            },
        );
        self.save_joins(joins)
    }
}
