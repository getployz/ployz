//! Activation: turning Device enforcement on once, across a restart, with no off.
//!
//! Admission verifies the signed starting set and writes a pending activation. The commit
//! happens in the next process, before it serves anything, so no connection, stream,
//! grant, worker or accepted want opened before the commit can act after it. See
//! [`crate::daemon`]'s boot commit.

use std::{io, path::Path};

use ployz_core::{
    ActivationReport, ActivationState, CheckCode, DeviceCode, DeviceEnforcementSet, DeviceKey,
    LegacyKey, LocalMachinePhase, Machine, MachineId, OperationId, Sealing, TrustedKeyKind,
    authority::{
        Approver, AuthorityRefusal, Member, Signed, StartingSet, Timestamp, TrustedKeys,
        starting_set,
    },
};
use serde::{Deserialize, Serialize};
use thiserror::Error as ThisError;

use super::{Error, LocalMachine};
use crate::machine::{Evidence, LocalMachineRecord};

const PENDING_FILE: &str = "activation-pending.json";

/// Why a starting set was not admitted. Each names what a person can check.
#[derive(Debug, ThisError)]
pub enum Refusal {
    #[error("{0}")]
    Authority(AuthorityRefusal),
    #[error(
        "Server {0} joined while Device enforcement was off, and this Server holds no evidence a person approved it. Activate with `ployz device enforce` from a Device, which pins each Server"
    )]
    LegacyMember(MachineId),
    #[error(
        "key {0} was added while Device enforcement was off, and this Server holds no evidence a person installed it. Activate with `ployz device enforce` from a Device, over SSH"
    )]
    LegacyKey(DeviceCode),
    #[error("Device enforcement can't be turned off once this Server is activated")]
    NoOff,
    #[error("this Server is already activated under another starting set")]
    Activated,
    #[error("this Server enforces from its first start, so it has nothing to activate")]
    BornEnforcing,
}

/// An admitted starting set, written before the restart that commits it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct PendingActivation {
    pub(crate) operation: OperationId,
    pub(crate) members: Vec<Member>,
    pub(crate) signer: DeviceCode,
    pub(crate) signer_evidence: Evidence,
    #[serde(default)]
    pub(crate) kept_keys: Vec<CheckCode>,
}

impl PendingActivation {
    /// The keys the commit puts in the snapshot: the signer, and each key of `held` whose
    /// full code the person confirmed. A key held under a kept name with another full code
    /// stays out.
    pub(crate) fn kept(
        &self,
        held: &TrustedKeys,
    ) -> impl Iterator<Item = (DeviceCode, Evidence)> + use<> {
        let kept = Evidence::Activation {
            operation: self.operation,
        };
        let held: Vec<CheckCode> = held
            .devices
            .iter()
            .map(DeviceKey::check_code)
            .chain(
                held.passkeys
                    .iter()
                    .map(|trusted| trusted.passkey.public_key.check_code()),
            )
            .collect();
        let confirmed: Vec<_> = self
            .kept_keys
            .iter()
            .filter(|code| held.contains(code))
            .map(|code| (code.code(), kept))
            .collect();
        std::iter::once((self.signer, self.signer_evidence)).chain(confirmed)
    }

    pub(crate) fn read(data_dir: &Path) -> io::Result<Option<Self>> {
        match std::fs::read(data_dir.join(PENDING_FILE)) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Write and sync the file, then its directory, so a crash after this returns leaves
    /// the next boot to finish the activation.
    pub(crate) fn write(&self, data_dir: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        crate::filesystem::atomic_write(&data_dir.join(PENDING_FILE), &bytes, 0o600)?;
        std::fs::File::open(data_dir)?.sync_all()
    }

    pub(crate) fn remove(data_dir: &Path) -> io::Result<()> {
        match std::fs::remove_file(data_dir.join(PENDING_FILE)) {
            Ok(()) => std::fs::File::open(data_dir)?.sync_all(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// What the commit retired, for the journal line and the activation's output.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Retired {
    pub(crate) devices: Vec<DeviceCode>,
    pub(crate) passkeys: Vec<DeviceCode>,
}

fn approver_code(approver: &Approver) -> Option<DeviceCode> {
    match approver {
        Approver::Device(key) => Some(DeviceKey::from(*key).code()),
        Approver::Passkey(code) => Some(*code),
        Approver::Browser { .. } => None,
    }
}

/// The first Member of `set` this Machine holds no evidence for.
fn legacy_member(
    record: &LocalMachineRecord,
    set: &StartingSet,
    rows: &[Machine],
    cluster: Option<ployz_core::SealingPublicKey>,
) -> Option<MachineId> {
    set.members
        .iter()
        .find(|member| {
            !record.machine().is_some_and(|own| member.matches(own))
                && !record
                    .trusted_snapshot()
                    .is_some_and(|snapshot| snapshot.holds_member(member))
                && !rows
                    .iter()
                    .any(|row| member.matches(row) && record.evidences_member(row, cluster))
        })
        .map(|member| member.id)
}

impl LocalMachine {
    /// The Cluster's identity: the Sealing public key this Machine's key file holds.
    fn cluster_identity(&self, phase: LocalMachinePhase) -> Option<ployz_core::SealingPublicKey> {
        match self.held_sealing(phase) {
            Some(Sealing::Held { public_key }) => Some(public_key),
            _ => None,
        }
    }

    /// Admit a starting set and restart to commit it. Over this Server's local socket the
    /// set is a person's own word at the console or over SSH, so any key the record holds
    /// may sign it, the signer gains evidence, and it may keep keys the person checked. Any
    /// other caller needs the signer and every Member in the trusted snapshot, and keeps no
    /// key this Machine holds outside it. `capability` is the route refusals name.
    ///
    /// # Errors
    /// Returns [`Error::ActivationRefused`] naming a legacy Member or key, an authority
    /// refusal, or an activation under another set, [`Error::Activating`] while a commit is
    /// pending, and the record and admission errors of a mutation.
    pub(crate) async fn activate(
        &self,
        signed: Signed<StartingSet>,
        capability: &'static str,
    ) -> Result<DeviceEnforcementSet, Error> {
        let local = self.local_socket;
        let phase = self.record().phase();
        let cluster = self.cluster_identity(phase);
        let rows = match (local, self.replicated()) {
            (false, Ok(replicated)) => replicated.machines().await?.observations,
            _ => Vec::new(),
        };
        let approvals = self.approvals.clone();
        let owner = self.owner.clone();
        let data_dir = owner.data_dir().to_path_buf();
        let refused = move |reason| Error::ActivationRefused { capability, reason };
        let admitted = self
            .finish_mutation(async move {
                owner
                    .mutate(move |store| {
                        let record = store.record();
                        if record.phase() != LocalMachinePhase::Participating {
                            return Err(Error::NotParticipating);
                        }
                        let body = signed
                            .open()
                            .map_err(|refusal| refused(Refusal::Authority(refusal)))?;
                        if record.born_enforcing() {
                            return Err(refused(Refusal::BornEnforcing));
                        }
                        if let Some(operation) = record.activation() {
                            return if operation == body.operation {
                                Ok(None)
                            } else {
                                Err(refused(Refusal::Activated))
                            };
                        }
                        if PendingActivation::read(&data_dir)
                            .map_err(Error::ActivationRecord)?
                            .is_some()
                        {
                            return Err(Error::Activating);
                        }
                        let own = record.machine().ok_or(Error::NotParticipating)?.into();
                        let now = Timestamp::now();
                        let (body, approver) =
                            starting_set::verify(&signed, &record.held_keys(), &own, now)
                                .map_err(|refusal| refused(Refusal::Authority(refusal)))?;
                        let signer = approver_code(&approver).ok_or_else(|| {
                            refused(Refusal::Authority(AuthorityRefusal::Mismatch {
                                field: "the signer",
                            }))
                        })?;
                        let snapshot = record.trusted_snapshot();
                        let signer_evidence = if local {
                            Evidence::LocalSocket
                        } else {
                            let Some(evidence) =
                                snapshot.and_then(|snapshot| snapshot.keys().get(&signer))
                            else {
                                return Err(refused(Refusal::LegacyKey(signer)));
                            };
                            if let Some(kept) = legacy_keys(record)
                                .into_iter()
                                .find(|legacy| body.kept_keys.contains(&legacy.code))
                            {
                                return Err(refused(Refusal::LegacyKey(kept.code.code())));
                            }
                            if let Some(member) = legacy_member(record, &body, &rows, cluster) {
                                return Err(refused(Refusal::LegacyMember(member)));
                            }
                            *evidence
                        };
                        approvals.consume_starting_set(&body, now)?;
                        let pending = PendingActivation {
                            operation: body.operation,
                            members: body.members,
                            signer,
                            signer_evidence,
                            kept_keys: body.kept_keys,
                        };
                        pending.write(&data_dir).map_err(Error::ActivationRecord)?;
                        Ok(Some(pending))
                    })
                    .await?
            })
            .await?;
        if let Some(pending) = admitted {
            self.owner.begin_activating();
            tracing::info!(
                operation = %pending.operation,
                members = pending.members.len(),
                signer = %pending.signer,
                "activation admitted; restarting to commit it"
            );
            self.owner.request_restart();
        }
        Ok(DeviceEnforcementSet { enforced: true })
    }

    /// This Machine's own account of its activation, for Inspect.
    pub(crate) async fn activation_report(&self) -> ActivationReport {
        let record = self.record();
        let state = if record.activation().is_some() || record.born_enforcing() {
            ActivationState::Activated
        } else if self.owner.activating()
            || PendingActivation::read(self.owner.data_dir())
                .ok()
                .flatten()
                .is_some()
        {
            ActivationState::Activating
        } else {
            ActivationState::Awaiting
        };
        let cluster = self.cluster_identity(record.phase());
        let legacy_members = match self.replicated() {
            Ok(replicated) if record.awaits_activation() => replicated
                .machines()
                .await
                .map(|rows| {
                    rows.observations
                        .iter()
                        .filter(|row| !record.evidences_member(row, cluster))
                        .map(|row| row.id)
                        .collect()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        ActivationReport {
            state,
            operation: record.activation(),
            legacy_members,
            legacy_keys: legacy_keys(&record),
            private_access: self
                .containers
                .as_ref()
                .filter(|containers| containers.private_access_state_kept())
                .map(|containers| ployz_core::PrivateAccessKept {
                    tailnet: containers.private_access_tailnet(),
                }),
        }
    }
}

#[cfg(test)]
impl LocalMachine {
    /// Commit an admitted activation in place, as the next boot does.
    pub(crate) async fn commit_activation_as_if_restarted(&self) {
        self.owner
            .mutate(|store| {
                crate::activation::commit_pending(store, &crate::activation::NoPlugin).unwrap();
            })
            .await
            .unwrap();
        self.owner.end_activating();
    }
}

/// Every key the record holds that the snapshot does not.
fn legacy_keys(record: &LocalMachineRecord) -> Vec<LegacyKey> {
    if record.born_enforcing() {
        return Vec::new();
    }
    let evidenced = |code: DeviceCode| {
        record
            .trusted_snapshot()
            .is_some_and(|snapshot| snapshot.holds_key(code))
    };
    let held = record.held_keys();
    held.devices
        .iter()
        .filter(|key| !evidenced(key.code()))
        .map(|key| LegacyKey {
            code: key.check_code(),
            kind: TrustedKeyKind::Device,
            account: None,
        })
        .chain(
            held.passkeys
                .iter()
                .filter(|trusted| !evidenced(trusted.passkey.code()))
                .map(|trusted| LegacyKey {
                    code: trusted.passkey.public_key.check_code(),
                    kind: TrustedKeyKind::Passkey,
                    account: Some(trusted.passkey.account.to_string()),
                }),
        )
        .collect()
}
