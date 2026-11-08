//! Device-signed restart approvals: the switch that requires them, the gate an Operator
//! caller's restart step passes under it, and the record of steps already taken.

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use ployz_core::{
    ContainerId, ContainerLocator, ContainerNow, ContainerWant, DeviceEnforcementSet, DeviceKey,
    LocalMachinePhase, ManagementScope, OperationId, RunTarget, Sealing,
    SetDeviceEnforcementRequest,
    authority::{
        AuthorityRefusal, Purpose, RestartApproval, RestartFacts, RestartStep, Signed, StartingSet,
        Timestamp, restart, starting_set,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{Error, LocalMachine};

const FILE_NAME: &str = "restart-approvals.json";

impl LocalMachine {
    /// Turn Device enforcement on or off under mutation admission, at Full scope only.
    /// Turning it on trusts only the Members of a Device-signed starting set that names
    /// this Machine, used once.
    ///
    /// # Errors
    /// Returns [`Error::NeedsFullScope`] for an Operator caller, [`Error::NotParticipating`]
    /// when this Machine is not participating, [`Error::EnforcementRefused`] for a missing,
    /// untrusted, expired or reused starting set, admission failures,
    /// [`Error::RecordOwner`] when the record owner has stopped, and [`Error::Store`] when
    /// the record cannot be written.
    pub async fn set_device_enforcement(
        &self,
        request: SetDeviceEnforcementRequest,
    ) -> Result<DeviceEnforcementSet, Error> {
        let client = self.management_client;
        let approvals = self.approvals.clone();
        let owner = self.owner.clone();
        self.finish_mutation(async move {
            owner
                .mutate(move |store| {
                    let record = store.record();
                    if record.phase() != LocalMachinePhase::Participating {
                        return Err(Error::NotParticipating);
                    }
                    if client.is_some_and(|remote| {
                        record.management_scope(&remote) != Some(ManagementScope::Full)
                    }) {
                        return Err(Error::NeedsFullScope);
                    }
                    let starting = match (request.enforced, &request.starting_set) {
                        (false, _) => Vec::new(),
                        (true, None) => {
                            return Err(Error::EnforcementRefused(AuthorityRefusal::Unsigned {
                                purpose: Purpose::Enforce,
                            }));
                        }
                        (true, Some(signed)) => {
                            let own = record.machine().ok_or(Error::NotParticipating)?.into();
                            let devices: Vec<DeviceKey> = record.devices().collect();
                            let now = Timestamp::now();
                            let body = starting_set::verify(signed, &devices, &own, now)
                                .map_err(Error::EnforcementRefused)?;
                            approvals.consume_starting_set(&body, now)?;
                            body.members
                        }
                    };
                    store.set_device_enforcement(request.enforced, starting)?;
                    Ok(DeviceEnforcementSet {
                        enforced: request.enforced,
                    })
                })
                .await?
        })
        .await
    }

    /// Under Device enforcement an Operator caller's deploy, which creates or replaces
    /// Containers, is refused: only a Device-approved restart step passes.
    ///
    /// # Errors
    /// Returns [`Error::RestartRefused`] under enforcement.
    pub(crate) fn admit_operator_deploy(&self) -> Result<(), Error> {
        if self.record().device_enforcement() {
            return Err(AuthorityRefusal::Mismatch {
                field: "a workflow",
            }
            .into());
        }
        Ok(())
    }

    /// Under Device enforcement, let an Operator caller's restart step through only with
    /// an approved Device's approval of exactly this restart, or a browser key's under a
    /// trusted passkey's session grant that covers it, and record the step so the
    /// same approval never runs it again. New-creation requests need deploy authorization
    /// and are refused, including adoption. Returns a want pinned to the verified
    /// Container ID. An absent target is refused. A Machine not enforcing passes untouched.
    ///
    /// # Errors
    /// Returns [`Error::RestartRefused`] naming what the approval lacks,
    /// [`Error::ApprovalRecord`] when the record of taken steps cannot be read or written,
    /// and [`Error::DockerUnavailable`] or [`Error::Docker`] when the Container cannot be
    /// read.
    pub async fn admit_operator_restart(
        &self,
        mut want: ContainerWant,
        approval: Option<&Signed<RestartApproval>>,
    ) -> Result<ContainerWant, Error> {
        let record = self.record();
        if !record.device_enforcement() {
            return Ok(want);
        }
        let Some((step, locator)) = RestartStep::of(&want) else {
            self.admit_operator_deploy()?;
            return Ok(want);
        };
        let approval = approval.ok_or(AuthorityRefusal::Unsigned {
            purpose: Purpose::Workflow,
        })?;
        let ContainerNow::Present(observed) = self.container_now(locator).await? else {
            return Err(AuthorityRefusal::Mismatch {
                field: "a Container",
            }
            .into());
        };
        let trusted = record.trusted_keys();
        let identity = observed.identity();
        let machine = record.id();
        let now = Timestamp::now();
        let cluster = match self.held_sealing(record.phase()) {
            Some(Sealing::Held { public_key }) => Some(public_key),
            _ => None,
        };
        let approved = restart::verify(
            approval,
            &RestartFacts {
                cluster,
                machine: &machine,
                trusted: &trusted,
                namespace: &identity.namespace,
                service: &identity.name,
                container: &observed.container_id,
                kind: observed.kind,
                step,
                now,
            },
        )?;
        self.approvals
            .consume(&approved.body, &observed.container_id, step, now)?;
        tracing::info!(
            operation = %approved.body.operation,
            proposal = ?approved.body.proposal,
            ?step,
            "{} approved by {}",
            approved.body.kind,
            approved.by
        );
        let locator = match &mut want {
            ContainerWant::Held { container, .. }
            | ContainerWant::Out { container, .. }
            | ContainerWant::Stopped { container, .. }
            | ContainerWant::Gone { container, .. }
            | ContainerWant::Running {
                container: RunTarget::Existing(container),
                ..
            } => container,
            ContainerWant::Running {
                container: RunTarget::New(_),
                ..
            } => unreachable!("new creations were refused"),
        };
        *locator = ContainerLocator::Id(observed.container_id);
        Ok(want)
    }
}

/// Device approval consumption in the existing `restart-approvals.json` record.
/// Restart operations expire, while admitted Machine IDs remain consumed across daemon
/// restarts and clock rollback.
pub(crate) struct ConsumptionLog {
    path: PathBuf,
    operations: Mutex<Option<Consumption>>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Consumption {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    joins: BTreeMap<ployz_core::MachineId, JoinResult>,
    #[serde(default)]
    expiry_time_floor: Option<Timestamp>,
    #[serde(flatten)]
    operations: BTreeMap<OperationId, Operation>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct JoinResult {
    pub approval: Signed<ployz_core::authority::JoinApproval>,
    pub reply: JoinReply,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", content = "result", rename_all = "snake_case")]
pub(crate) enum JoinReply {
    Register(Box<ployz_core::Registered>),
    Share(ployz_core::SealedSealingKey),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Operation {
    expires_at: Timestamp,
    containers: BTreeMap<ContainerId, RestartStep>,
}

/// Where `approval`'s consumption is kept. A proposal step gets an entry of its own, so a plan
/// that restarts one Service at step 0 and again at step 2 takes each once, while a resent
/// envelope for the same step still replays. The entry's name is 32 hex digits of the hash of
/// the operation and the step's index, which no real operation ID collides with in practice.
fn consumption_key(approval: &RestartApproval) -> OperationId {
    let Some(step) = &approval.proposal else {
        return approval.operation;
    };
    let mut hasher = Sha256::new();
    hasher.update(step.operation.as_str().as_bytes());
    hasher.update(step.index.to_be_bytes());
    let digest = hasher.finalize();
    let (head, _) = digest.split_at(16);
    OperationId::parse(hex::encode(head)).expect("the hash is lowercase hex")
}

impl ConsumptionLog {
    pub(crate) fn new(data_dir: &Path) -> Arc<Self> {
        Arc::new(Self {
            path: data_dir.join(FILE_NAME),
            operations: Mutex::new(None),
        })
    }

    /// Record `step` of `approval` on `container`, refusing a step the operation already
    /// reached or passed there. The record is written before the caller acts, so a crash
    /// after it costs a new approval, never a second effect.
    fn consume(
        &self,
        approval: &RestartApproval,
        container: &ContainerId,
        step: RestartStep,
        now: Timestamp,
    ) -> Result<(), Error> {
        self.consume_with(approval, container, step, now, |path, contents| {
            crate::filesystem::atomic_write(path, contents, 0o600)
        })
    }

    fn consume_with(
        &self,
        approval: &RestartApproval,
        container: &ContainerId,
        step: RestartStep,
        now: Timestamp,
        write: impl FnOnce(&Path, &[u8]) -> io::Result<()>,
    ) -> Result<(), Error> {
        let mut guard = self
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = Some(self.load().map_err(Error::ApprovalRecord)?);
        }
        let operations = guard.as_mut().expect("loaded above");
        let now = operations
            .expiry_time_floor
            .map_or(now, |highest| now.max(highest));
        restart::verify_time(approval, now)?;
        let mut updated = operations.clone();
        updated.expiry_time_floor = Some(now);
        updated
            .operations
            .retain(|_, operation| operation.expires_at.plus(restart::CLOCK_SKEW_MILLIS) >= now);
        let operation = updated
            .operations
            .entry(consumption_key(approval))
            .or_insert_with(|| Operation {
                expires_at: approval.expires_at,
                containers: BTreeMap::new(),
            });
        operation.expires_at = operation.expires_at.max(approval.expires_at);
        let replayed = operation
            .containers
            .get(container)
            .is_some_and(|reached| *reached >= step);
        if !replayed {
            operation.containers.insert(*container, step);
        }
        let contents = serde_json::to_vec(&updated).map_err(io::Error::other);
        let written = contents
            .and_then(|contents| write(&self.path, &contents))
            .map_err(Error::ApprovalRecord);
        if let Err(error) = written {
            *guard = None;
            return Err(error);
        }
        *operations = updated;
        if replayed {
            return Err(AuthorityRefusal::Replayed {
                operation: approval.operation,
            }
            .into());
        }
        Ok(())
    }

    pub(crate) fn consume_starting_set(
        &self,
        set: &StartingSet,
        now: Timestamp,
    ) -> Result<(), Error> {
        let mut guard = self
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = Some(self.load().map_err(Error::ApprovalRecord)?);
        }
        let consumption = guard.as_ref().expect("loaded above");
        let now = consumption
            .expiry_time_floor
            .map_or(now, |floor| now.max(floor));
        ployz_core::authority::validity::verify(set.issued_at, set.expires_at, now)
            .map_err(Error::EnforcementRefused)?;
        if consumption.operations.contains_key(&set.operation) {
            return Err(Error::EnforcementRefused(AuthorityRefusal::Replayed {
                operation: set.operation,
            }));
        }
        let mut updated = consumption.clone();
        updated.expiry_time_floor = Some(now);
        updated.operations.insert(
            set.operation,
            Operation {
                expires_at: set.expires_at,
                containers: BTreeMap::new(),
            },
        );
        let written = serde_json::to_vec(&updated)
            .map_err(io::Error::other)
            .and_then(|contents| crate::filesystem::atomic_write(&self.path, &contents, 0o600))
            .map_err(Error::ApprovalRecord);
        if let Err(error) = written {
            *guard = None;
            return Err(error);
        }
        *guard = Some(updated);
        Ok(())
    }

    pub(crate) fn join_result(
        &self,
        machine: ployz_core::MachineId,
    ) -> Result<Option<JoinResult>, Error> {
        let mut guard = self
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = Some(self.load().map_err(Error::JoinRecord)?);
        }
        Ok(guard
            .as_ref()
            .expect("loaded above")
            .joins
            .get(&machine)
            .cloned())
    }

    pub(crate) fn require_unused_join(&self, machine: ployz_core::MachineId) -> Result<(), Error> {
        let mut guard = self
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = Some(self.load().map_err(Error::JoinRecord)?);
        }
        if guard
            .as_ref()
            .expect("loaded above")
            .joins
            .contains_key(&machine)
        {
            return Err(Error::JoinConsumed(machine));
        }
        Ok(())
    }

    pub(crate) fn consume_join(&self, result: JoinResult, now: Timestamp) -> Result<(), Error> {
        let mut guard = self
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = Some(self.load().map_err(Error::JoinRecord)?);
        }
        let consumption = guard.as_ref().expect("loaded above");
        let now = consumption
            .expiry_time_floor
            .map_or(now, |floor| now.max(floor));
        let approval = result.approval.open().map_err(Error::JoinRefused)?;
        ployz_core::authority::validity::verify(approval.issued_at, approval.expires_at, now)
            .map_err(Error::JoinRefused)?;
        let mut updated = consumption.clone();
        updated.expiry_time_floor = Some(now);
        if updated.joins.contains_key(&approval.machine.id) {
            return Err(Error::JoinConsumed(approval.machine.id));
        }
        updated.joins.insert(approval.machine.id, result);
        let written = serde_json::to_vec(&updated)
            .map_err(io::Error::other)
            .and_then(|contents| crate::filesystem::atomic_write(&self.path, &contents, 0o600))
            .map_err(Error::JoinRecord);
        if let Err(error) = written {
            *guard = None;
            return Err(error);
        }
        *guard = Some(updated);
        Ok(())
    }

    fn load(&self) -> io::Result<Consumption> {
        match std::fs::read(&self.path) {
            Ok(contents) => serde_json::from_slice(&contents).map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Consumption::default()),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approval(operation: &str, expires_at: u64) -> RestartApproval {
        RestartApproval {
            cluster: ployz_core::SealingPublicKey::from_bytes([1; 32]),
            operation: OperationId::parse(operation).unwrap(),
            kind: ployz_core::authority::WorkflowKind::Restart,
            namespace: ployz_core::Namespace::parse("app").unwrap(),
            service: ployz_core::ServiceName::parse("api").unwrap(),
            machines: vec![],
            container: container('a'),
            issued_at: at(0),
            expires_at: Timestamp::from_millis(expires_at).unwrap(),
            signer: ployz_core::authority::SignerKey::Device(DeviceKey::from_bytes([2; 32])),
            proposal: None,
        }
    }

    fn container(fill: char) -> ContainerId {
        ContainerId::parse(fill.to_string().repeat(64)).unwrap()
    }

    fn at(millis: u64) -> Timestamp {
        Timestamp::from_millis(millis).unwrap()
    }

    fn replayed(result: Result<(), Error>) -> bool {
        matches!(
            result,
            Err(Error::RestartRefused(AuthorityRefusal::Replayed { .. }))
        )
    }

    #[test]
    fn each_step_runs_once_per_container_in_order_and_survives_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let log = ConsumptionLog::new(dir.path());
        let first = approval(&"1".repeat(32), 10_000);

        log.consume(&first, &container('a'), RestartStep::Hold, at(1))
            .unwrap();
        log.consume(&first, &container('a'), RestartStep::Stop, at(2))
            .unwrap();
        assert!(replayed(log.consume(
            &first,
            &container('a'),
            RestartStep::Stop,
            at(3)
        )));
        assert!(replayed(log.consume(
            &first,
            &container('a'),
            RestartStep::Hold,
            at(3)
        )));
        log.consume(&first, &container('b'), RestartStep::Stop, at(3))
            .unwrap();

        let reopened = ConsumptionLog::new(dir.path());
        assert!(replayed(reopened.consume(
            &first,
            &container('a'),
            RestartStep::Stop,
            at(4)
        )));
        reopened
            .consume(&first, &container('a'), RestartStep::Start, at(4))
            .unwrap();
        assert!(replayed(reopened.consume(
            &first,
            &container('a'),
            RestartStep::Start,
            at(5)
        )));

        let second = approval(&"2".repeat(32), 10_000);
        reopened
            .consume(&second, &container('a'), RestartStep::Hold, at(5))
            .unwrap();
    }

    #[test]
    fn a_proposal_that_restarts_one_container_at_two_steps_takes_each_once() {
        let dir = tempfile::tempdir().unwrap();
        let log = ConsumptionLog::new(dir.path());
        let operation = "1".repeat(32);
        let step = |index: u32| RestartApproval {
            proposal: Some(ployz_core::authority::ProposalRef {
                operation: OperationId::parse(&operation).unwrap(),
                index,
            }),
            ..approval(&operation, 10_000)
        };

        log.consume(&step(0), &container('a'), RestartStep::Hold, at(1))
            .unwrap();
        log.consume(&step(0), &container('a'), RestartStep::Start, at(2))
            .unwrap();
        log.consume(&step(1), &container('b'), RestartStep::Stop, at(3))
            .unwrap();
        log.consume(&step(2), &container('a'), RestartStep::Hold, at(4))
            .unwrap();
        log.consume(&step(3), &container('b'), RestartStep::Start, at(5))
            .unwrap();

        assert!(replayed(log.consume(
            &step(0),
            &container('a'),
            RestartStep::Hold,
            at(6)
        )));
        assert!(replayed(log.consume(
            &step(2),
            &container('a'),
            RestartStep::Hold,
            at(6)
        )));
        log.consume(
            &approval(&operation, 10_000),
            &container('a'),
            RestartStep::Hold,
            at(7),
        )
        .unwrap();
        let reopened = ConsumptionLog::new(dir.path());
        assert!(replayed(reopened.consume(
            &step(2),
            &container('a'),
            RestartStep::Hold,
            at(8)
        )));
    }

    #[test]
    fn pruning_then_clock_rollback_cannot_replay_even_after_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let log = ConsumptionLog::new(dir.path());
        let short = approval(&"1".repeat(32), 100);
        log.consume(&short, &container('a'), RestartStep::Start, at(50))
            .unwrap();
        assert!(replayed(log.consume(
            &short,
            &container('a'),
            RestartStep::Start,
            at(100)
        )));
        let later = approval(&"2".repeat(32), 200_000);
        log.consume(&later, &container('a'), RestartStep::Start, at(120_101))
            .unwrap();
        let stored = std::fs::read_to_string(dir.path().join(FILE_NAME)).unwrap();
        assert!(!stored.contains(&"1".repeat(32)), "{stored}");
        let reopened = ConsumptionLog::new(dir.path());
        assert!(matches!(
            reopened.consume(&short, &container('a'), RestartStep::Start, at(50)),
            Err(Error::RestartRefused(AuthorityRefusal::Expired))
        ));
        assert!(matches!(
            reopened.consume(&short, &container('b'), RestartStep::Hold, at(50)),
            Err(Error::RestartRefused(AuthorityRefusal::Expired))
        ));
    }

    #[test]
    fn the_previous_log_shape_still_refuses_consumed_steps() {
        let dir = tempfile::tempdir().unwrap();
        let first = approval(&"1".repeat(32), 100);
        let previous = BTreeMap::from([(
            first.operation,
            Operation {
                expires_at: first.expires_at,
                containers: BTreeMap::from([(container('a'), RestartStep::Stop)]),
            },
        )]);
        std::fs::write(
            dir.path().join(FILE_NAME),
            serde_json::to_vec(&previous).unwrap(),
        )
        .unwrap();
        let log = ConsumptionLog::new(dir.path());
        assert!(replayed(log.consume(
            &first,
            &container('a'),
            RestartStep::Stop,
            at(50)
        )));
        log.consume(&first, &container('a'), RestartStep::Start, at(51))
            .unwrap();
        let reopened = ConsumptionLog::new(dir.path());
        assert!(replayed(reopened.consume(
            &first,
            &container('a'),
            RestartStep::Start,
            at(52)
        )));
    }

    #[test]
    fn a_reused_operation_keeps_consumption_until_its_latest_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let log = ConsumptionLog::new(dir.path());
        let short = approval(&"1".repeat(32), 100);
        log.consume(&short, &container('a'), RestartStep::Start, at(50))
            .unwrap();
        let extended = RestartApproval {
            expires_at: at(300_000),
            ..short.clone()
        };
        assert!(replayed(log.consume(
            &extended,
            &container('a'),
            RestartStep::Start,
            at(60)
        )));
        let later = approval(&"2".repeat(32), 400_000);
        log.consume(&later, &container('b'), RestartStep::Start, at(200_000))
            .unwrap();
        let reopened = ConsumptionLog::new(dir.path());
        assert!(replayed(reopened.consume(
            &extended,
            &container('a'),
            RestartStep::Start,
            at(200_001)
        )));
    }

    #[test]
    fn a_write_that_lands_then_errors_is_reloaded_before_the_next_consumption() {
        let dir = tempfile::tempdir().unwrap();
        let log = ConsumptionLog::new(dir.path());
        let first = approval(&"1".repeat(32), 100);
        assert!(matches!(
            log.consume_with(
                &first,
                &container('a'),
                RestartStep::Stop,
                at(1),
                |path, contents| {
                    crate::filesystem::atomic_write(path, contents, 0o600)?;
                    Err(io::Error::other("directory sync failed after rename"))
                }
            ),
            Err(Error::ApprovalRecord(_))
        ));
        let later = approval(&"2".repeat(32), 100);
        log.consume(&later, &container('a'), RestartStep::Stop, at(2))
            .unwrap();
        assert!(replayed(log.consume(
            &first,
            &container('a'),
            RestartStep::Stop,
            at(3)
        )));
        let reopened = ConsumptionLog::new(dir.path());
        assert!(replayed(reopened.consume(
            &first,
            &container('a'),
            RestartStep::Stop,
            at(3)
        )));
    }

    #[test]
    fn an_unreadable_record_refuses_rather_than_forgetting() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE_NAME), b"{").unwrap();
        let log = ConsumptionLog::new(dir.path());
        assert!(matches!(
            log.consume(
                &approval(&"1".repeat(32), 100),
                &container('a'),
                RestartStep::Start,
                at(1)
            ),
            Err(Error::ApprovalRecord(_))
        ));
    }
}
