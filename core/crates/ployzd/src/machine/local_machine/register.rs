//! Register: admit a joiner, assign its subnet, and publish it to the Cluster.

use ployz_core::authority::MismatchField;
use std::collections::BTreeMap;

use ployz_core::{Machine, RegisterRequest, Registered};

use crate::{
    corrosion::{DeclaredDoors, ReplicatedStore},
    machine::LocalMachineRecord,
    sealing_key::SealingKey,
};

use super::{Error, LocalMachine, consumption, join_approval, published_network};

impl LocalMachine {
    /// Publish a client-selected assignment from a participating Entry Machine.
    ///
    /// # Errors
    /// Rejects absent assignments, invalid assignments, unavailable storage, or
    /// a Cluster network not yet published.
    pub async fn register(&self, request: RegisterRequest) -> Result<Registered, Error> {
        self.finish_mutation(move |local| async move {
            local.record().require_participating()?;
            if request.assigned_subnet.is_none() {
                return Err(Error::MissingAssignment);
            }
            local.publish_assignment(request).await
        })
        .await
    }

    /// Admit a joiner, or publish the row of one already admitted. A Forget here never
    /// lands between the removal check and the row write, since both take this Machine's
    /// exclusive mutation admission. A delete gossiped from another Member can, so the
    /// write itself refuses a row this store deleted, and a write that left no row is
    /// refused as a removal.
    async fn publish_assignment(&self, request: RegisterRequest) -> Result<Registered, Error> {
        let replicated = self.replicated()?;
        let declared = replicated
            .cluster_doors()
            .await
            .map_err(Error::DeclaredDoorsUnread)?;
        admit_joiner(&declared, &request)?;
        let publication = replicated.machine_publication().await;
        self.refuse_removed(request.machine_id, Some(request.public_key))
            .await?;
        let snapshot = ployz_core::EnrollmentSnapshot {
            network: published_network(&self.record(), replicated).await?,
            machines: replicated.machines().await?.observations,
            target_versions: replicated.version().await?,
            doors: declared.followed(),
        };
        let mut assignment = ployz_core::allocate_enrollment(
            &request,
            &ployz_core::EnrollmentSnapshot {
                machines: Vec::new(),
                ..snapshot.clone()
            },
            &[],
        )?;
        let confirmed = replicated
            .machine(request.machine_id.as_str())
            .await?
            .is_some_and(|row| row.public_key == request.public_key);
        self.give_way_to_a_fresh_approval(request.machine_id, confirmed)?;
        let fresh = match self.enter_join(request.machine_id, |stored| {
            Ok(
                ployz_core::authority::join::JoinAllocation::from(&assignment.machine)
                    == stored.approval.open().map_err(Error::JoinRefused)?.machine
                    && request
                        .join_approval
                        .as_ref()
                        .is_none_or(|approval| approval == &stored.approval),
            )
        })? {
            join_approval::Entered::Fresh(fresh) => fresh,
            join_approval::Entered::Replayed(stored) => {
                if !confirmed && request.join_confirmation.is_none() {
                    self.redraft_refused_join(
                        &stored,
                        &assignment.machine,
                        snapshot.network,
                        &snapshot.machines,
                        request.first_key,
                    )?;
                }
                return self
                    .replay_join(stored, &request, replicated, &publication)
                    .await;
            }
        };
        let subnet = ipnet::Ipv4Net::from(assignment.machine.subnet);
        if snapshot.machines.iter().any(|peer| {
            let other = ipnet::Ipv4Net::from(peer.subnet);
            subnet.contains(&other.network()) || other.contains(&subnet.network())
        }) {
            return Err(Error::JoinRefused(
                ployz_core::authority::AuthorityRefusal::Mismatch {
                    field: MismatchField::UnusedSubnet,
                },
            ));
        }
        let approval = self.admit_join(
            &fresh,
            &assignment.machine,
            snapshot.network,
            request.join_approval.clone(),
            &snapshot.machines,
            request.first_key,
        )?;
        assignment.machine.join_approval = Some(approval.clone());
        assignment.machine.runtime.management_identity = None;
        let seals_trust = claims(&request, ployz_core::JOIN_TRUST_CAPABILITY);
        let sealing_key =
            match SealingKey::load(self.owner.data_dir()).map_err(Error::JoinRecord)? {
                Some(key) => Some(self.seal_capsule(
                    &key,
                    &assignment.machine.public_key,
                    &approval,
                    &request,
                )?),
                None => None,
            };
        let registration = registered(
            assignment.machine,
            snapshot.machines,
            snapshot.target_versions,
            sealing_key,
            self.record().devices().collect(),
            self.record()
                .grants()
                .iter()
                .map(|held| held.grant.clone())
                .collect(),
        );
        self.consume_join(
            fresh,
            consumption::JoinResult {
                approval,
                reply: consumption::JoinReply::Register(Box::new(registration.clone())),
                sealed_trust: seals_trust,
            },
        )?;
        Ok(registration)
    }

    /// The capsule for a joiner: the Sealing Key and `approval`, then this Machine's floors
    /// for a joiner that opens them, then for one that claims `join-trust.v1` the keys this
    /// Machine trusts and removed, read from its record under this same admission.
    fn seal_capsule(
        &self,
        key: &SealingKey,
        recipient: &ployz_core::WireGuardPublicKey,
        approval: &ployz_core::authority::Signed<ployz_core::authority::JoinApproval>,
        request: &RegisterRequest,
    ) -> Result<ployz_core::SealedSealingKey, Error> {
        // The joiner's claim, not the row: Cloud's Register carries no runtime.
        let trust =
            claims(request, ployz_core::JOIN_TRUST_CAPABILITY).then(|| join_trust(&self.record()));
        let floors = self.floors_for(
            key,
            trust.is_some() || claims(request, ployz_core::FORGET_VERSIONS_CAPABILITY),
        )?;
        Ok(key.seal_join(recipient, approval, floors.as_ref(), trust.as_ref())?)
    }

    /// Answer a Register for a join this Machine already answered. A confirmation of the
    /// capsule publishes the joiner's row, and the answer then carries this Machine's
    /// restrictions as they stand now, read after the row went out, so a removal either
    /// lands before them or finds the joiner listed. A stored answer from before
    /// `join-trust.v1` is sealed again with current trust for a joiner that now claims it and
    /// has not confirmed, so a refused legacy capsule never blocks the join for good.
    async fn replay_join(
        &self,
        stored: consumption::JoinResult,
        request: &RegisterRequest,
        replicated: &ReplicatedStore,
        publication: &crate::corrosion::MachinePublicationGuard<'_>,
    ) -> Result<Registered, Error> {
        let consumption::JoinReply::Register(registration) = &stored.reply else {
            return Err(Error::JoinConsumed(request.machine_id));
        };
        let mut registration = registration.as_ref().clone();
        let Some(confirmation) = &request.join_confirmation else {
            if stored.sealed_trust || !claims(request, ployz_core::JOIN_TRUST_CAPABILITY) {
                return Ok(registration);
            }
            let Some(key) = SealingKey::load(self.owner.data_dir()).map_err(Error::JoinRecord)?
            else {
                return Ok(registration);
            };
            registration.sealing_key = Some(self.seal_capsule(
                &key,
                &registration.assigned_machine.public_key,
                &stored.approval,
                request,
            )?);
            self.approvals
                .replace_join_result(consumption::JoinResult {
                    approval: stored.approval,
                    reply: consumption::JoinReply::Register(Box::new(registration.clone())),
                    sealed_trust: true,
                })?;
            return Ok(registration);
        };
        let key = SealingKey::load(self.owner.data_dir()).map_err(Error::JoinRecord)?;
        let confirmed = key
            .as_ref()
            .is_some_and(|key| key.confirms_join(&stored.approval, confirmation));
        if !confirmed {
            return Err(Error::JoinRefused(
                ployz_core::authority::AuthorityRefusal::Mismatch {
                    field: MismatchField::JoinConfirmation,
                },
            ));
        }
        // The approval must still hold here: its window on the floored clock, its signer
        // and every key it lists. Once it lapses, a resend of a row already out changes
        // nothing and still answers.
        let machine = registration.assigned_machine.clone();
        let body = stored.approval.open().map_err(Error::JoinRefused)?;
        if let Err(refused) = self.verify_join(&stored.approval, &body.machine, body.network) {
            let published = replicated
                .machine(machine.id.as_str())
                .await?
                .is_some_and(|row| row.public_key == machine.public_key);
            if !published {
                return Err(refused);
            }
        } else {
            publication.register(machine.clone()).await?;
            if replicated.machine(machine.id.as_str()).await?.is_none() {
                self.refuse_removed(machine.id, None).await?;
                return Err(Error::MemberRemoved(machine.id));
            }
        }
        self.settle_join(machine.id)?;
        if let Some(key) = key {
            let record = self.record();
            let set = ployz_core::JoinRestrictionSet {
                removed_keys: record.removed_keys().to_vec(),
                removed_members: record.removed_members().to_vec(),
                floors: self.approvals.floors_in(key.public_key())?,
            };
            registration.restrictions =
                Some(key.restrict_join(&stored.approval, confirmation, set));
        }
        Ok(registration)
    }
}

/// Refuse a joiner whose client can't follow the Cluster's declared Doors, judged on
/// this Machine's own replica. Such a client would open the Public Door, so only an
/// undeclared Cluster or one declared Public admits it.
fn admit_joiner(declared: &DeclaredDoors, request: &RegisterRequest) -> Result<(), Error> {
    let follows = request
        .capabilities
        .iter()
        .any(|claimed| claimed == ployz_core::SET_CLUSTER_DOORS_CAPABILITY);
    let admitted = follows
        || match declared {
            DeclaredDoors::Undeclared => true,
            DeclaredDoors::Declared(doors) => doors.is_public_only(),
            DeclaredDoors::Unreadable => false,
        };
    if admitted {
        Ok(())
    } else {
        Err(Error::JoinerCannotFollowDoors {
            joiner: request.name.clone(),
        })
    }
}

/// Whether the joiner `request` registers claims `capability`.
fn claims(request: &RegisterRequest, capability: &str) -> bool {
    request
        .capabilities
        .iter()
        .any(|claimed| claimed == capability)
}

/// The trust this Machine seals for a joiner: the keys it trusts and the ones it removed.
fn join_trust(record: &LocalMachineRecord) -> crate::sealing_key::JoinTrust {
    crate::sealing_key::JoinTrust {
        devices: record.devices().collect(),
        passkeys: record
            .passkeys()
            .iter()
            .map(|held| held.passkey.clone())
            .collect(),
        grants: record
            .grants()
            .iter()
            .map(|held| held.grant.clone())
            .collect(),
        removed_keys: record.removed_keys().to_vec(),
        removed_members: record.removed_members().to_vec(),
    }
}

fn registered(
    assigned_machine: Machine,
    machines: Vec<Machine>,
    target_versions: BTreeMap<String, i64>,
    sealing_key: Option<ployz_core::SealedSealingKey>,
    devices: Vec<ployz_core::DeviceKey>,
    grants: Vec<ployz_core::authority::Grant>,
) -> Registered {
    let assigned_id = assigned_machine.id;
    Registered {
        assigned_machine,
        visible_peers: machines
            .into_iter()
            .filter(|machine| machine.id != assigned_id)
            .collect(),
        target_versions,
        sealing_key,
        devices,
        grants,
        restrictions: None,
    }
}
