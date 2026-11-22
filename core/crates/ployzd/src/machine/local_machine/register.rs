//! Register: admit a joiner, assign its subnet, and publish it to the Cluster.

use std::collections::BTreeMap;

use ployz_core::{Machine, RegisterRequest, Registered};

use crate::{corrosion::DeclaredDoors, sealing_key::SealingKey};

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
                let consumption::JoinReply::Register(registration) = stored.reply else {
                    return Err(Error::JoinConsumed(request.machine_id));
                };
                if let Some(confirmation) = &request.join_confirmation {
                    let confirmed = SealingKey::load(self.owner.data_dir())
                        .map_err(Error::JoinRecord)?
                        .is_some_and(|key| key.confirms_join(&stored.approval, confirmation));
                    if !confirmed {
                        return Err(Error::JoinRefused(
                            ployz_core::authority::AuthorityRefusal::Mismatch {
                                field: "a join confirmation",
                            },
                        ));
                    }
                    // The approval must still hold here: its window on the floored clock,
                    // its signer and every key it lists. Once it lapses, a resend of a row
                    // already out changes nothing and still answers.
                    let machine = &registration.assigned_machine;
                    let body = stored.approval.open().map_err(Error::JoinRefused)?;
                    if let Err(refused) =
                        self.verify_join(&stored.approval, &body.machine, body.network)
                    {
                        let published = replicated
                            .machine(machine.id.as_str())
                            .await?
                            .is_some_and(|row| row.public_key == machine.public_key);
                        return if published {
                            Ok(*registration)
                        } else {
                            Err(refused)
                        };
                    }
                    publication.register(machine.clone()).await?;
                    if replicated.machine(machine.id.as_str()).await?.is_none() {
                        self.refuse_removed(machine.id, None).await?;
                        return Err(Error::MemberRemoved(machine.id));
                    }
                }
                return Ok(*registration);
            }
        };
        let subnet = ipnet::Ipv4Net::from(assignment.machine.subnet);
        if snapshot.machines.iter().any(|peer| {
            let other = ipnet::Ipv4Net::from(peer.subnet);
            subnet.contains(&other.network()) || other.contains(&subnet.network())
        }) {
            return Err(Error::JoinRefused(
                ployz_core::authority::AuthorityRefusal::Mismatch {
                    field: "an unused subnet",
                },
            ));
        }
        let approval = self.admit_join(
            &fresh,
            &assignment.machine,
            snapshot.network,
            request.join_approval,
            &snapshot.machines,
        )?;
        assignment.machine.join_approval = Some(approval.clone());
        assignment.machine.runtime.management_identity = None;
        let sealing_key = match SealingKey::load(self.owner.data_dir())
            .map_err(Error::JoinRecord)?
        {
            Some(key) => {
                // The joiner's claim, not the row: Cloud's Register carries no runtime.
                let opens_floors = request
                    .capabilities
                    .iter()
                    .any(|claimed| claimed == ployz_core::FORGET_VERSIONS_CAPABILITY);
                let floors = self.floors_for(&key, opens_floors)?;
                Some(key.seal_join(&assignment.machine.public_key, &approval, floors.as_ref())?)
            }
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
            },
        )?;
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
    }
}
