//! Machine-local join proposals and Device-approved admission.

use std::collections::BTreeMap;

use ployz_core::{
    ApproveJoinRequest, DeviceKey, JoinApproved, Machine, MachineId, PendingJoins,
    authority::{AuthorityRefusal, JoinApproval, Purpose, Signed, Timestamp, join},
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
}

/// What entering a join found for one Machine.
pub(crate) enum Entered {
    /// An earlier entry already answered this request; the answer stands.
    Replayed(super::restart_approval::JoinResult),
    /// Nothing is stored for the Machine, so this entry may admit it.
    Fresh(FreshJoin),
}

/// The fact that no answer is stored for `machine`, which only
/// [`LocalMachine::enter_join`] can state and which consuming a join takes.
pub(crate) struct FreshJoin {
    machine: MachineId,
}

/// Drop each approved proposal in `data_dir`'s join record whose signer is not one of
/// `trusted`, and name the Machines dropped. A proposal nobody approved admits nothing and
/// stays.
pub(crate) fn reverify_join_proposals(
    data_dir: &std::path::Path,
    trusted: &[DeviceKey],
) -> std::io::Result<Vec<MachineId>> {
    let path = data_dir.join("join-approvals.json");
    let mut joins: Joins = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(std::io::Error::other)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut dropped = Vec::new();
    joins.proposals.retain(|id, proposal| {
        let kept = proposal.approval.as_ref().is_none_or(|approval| {
            approval
                .verified_signers(trusted.iter().copied().map(Into::into))
                .is_ok_and(|signers| signers.contains(&proposal.body.signer.into()))
                && approval.open().is_ok_and(|body| body == proposal.body)
        });
        if !kept {
            dropped.push(*id);
        }
        kept
    });
    if !dropped.is_empty() {
        let bytes = serde_json::to_vec(&joins).map_err(std::io::Error::other)?;
        crate::filesystem::atomic_write(&path, &bytes, 0o600)?;
    }
    Ok(dropped)
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

    pub(crate) fn pending_joins(&self) -> Result<PendingJoins, Error> {
        let now = Timestamp::now();
        Ok(PendingJoins {
            joins: self
                .read_joins()?
                .proposals
                .into_values()
                .filter(|proposal| proposal.body.expires_at >= now && proposal.approval.is_none())
                .map(|proposal| proposal.body)
                .collect(),
        })
    }

    pub(crate) async fn approve_join(
        &self,
        request: ApproveJoinRequest,
    ) -> Result<JoinApproved, Error> {
        let local = self.clone();
        self.finish_mutation(async move {
            let body = request.approval.open().map_err(Error::JoinRefused)?;
            local.verify_join(&request.approval, &body.machine, body.network)?;
            let mut joins = local.read_joins()?;
            local.require_unused_join(body.machine.id)?;
            joins.proposals.insert(
                body.machine.id,
                Proposal {
                    body,
                    approval: Some(request.approval),
                },
            );
            local.save_joins(joins)?;
            Ok(JoinApproved {})
        })
        .await
    }

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
        same_entry: impl FnOnce(&super::restart_approval::JoinResult) -> Result<bool, Error>,
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
        result: super::restart_approval::JoinResult,
    ) -> Result<(), Error> {
        let machine = result
            .approval
            .open()
            .map_err(Error::JoinRefused)?
            .machine
            .id;
        if machine != fresh.machine {
            return Err(Error::JoinRefused(AuthorityRefusal::Mismatch {
                field: "a joining Machine",
            }));
        }
        self.approvals.consume_join(result, Timestamp::now())?;
        let mut joins = self.read_joins()?;
        joins.proposals.remove(&machine);
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
            &record.devices().collect::<Vec<_>>(),
            &record
                .passkeys()
                .iter()
                .map(|trusted| trusted.passkey.clone())
                .collect::<Vec<_>>(),
            machine,
            network,
            Timestamp::now(),
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
    ) -> Result<Signed<JoinApproval>, Error> {
        debug_assert_eq!(
            fresh.machine, machine.id,
            "a join is admitted as it was entered"
        );
        let mut joins = self.read_joins()?;
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
                    field: "bootstrap peers",
                }));
            }
            return Ok(approval);
        }
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
                field: "pending join capacity",
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
                    admitting_machines: vec![record.id()],
                    bootstrap: peers.iter().map(Into::into).collect(),
                    issued_at,
                    expires_at: issued_at
                        .plus(ployz_core::authority::validity::MAX_LIFETIME_MILLIS),
                    signer: DeviceKey::from_bytes([0; 32]),
                },
                approval: None,
            },
        );
        self.save_joins(joins)?;
        Err(Error::JoinRefused(AuthorityRefusal::Unsigned {
            purpose: Purpose::Join,
        }))
    }
}
