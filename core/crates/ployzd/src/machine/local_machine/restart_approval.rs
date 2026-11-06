//! Device-signed restart approvals: the switch that requires them, the gate an Operator
//! caller's restart step passes under it, and the record of steps already taken.

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use ployz_core::{
    ContainerId, ContainerNow, ContainerWant, DeviceEnforcementSet, DeviceKey, LocalMachinePhase,
    ManagementScope, OperationId, Sealing, SetDeviceEnforcementRequest,
    authority::{
        AuthorityRefusal, Purpose, RestartApproval, RestartFacts, RestartStep, Signed, Timestamp,
        restart,
    },
};
use serde::{Deserialize, Serialize};

use super::{Error, LocalMachine};

const FILE_NAME: &str = "restart-approvals.json";

impl LocalMachine {
    /// Turn Device enforcement on or off under mutation admission, at Full scope only.
    ///
    /// # Errors
    /// Returns [`Error::NeedsFullScope`] for an Operator caller, [`Error::NotParticipating`]
    /// when this Machine is not participating, admission failures, [`Error::RecordOwner`]
    /// when the record owner has stopped, and [`Error::Store`] when the record cannot be
    /// written.
    pub async fn set_device_enforcement(
        &self,
        request: SetDeviceEnforcementRequest,
    ) -> Result<DeviceEnforcementSet, Error> {
        let local = self.clone();
        let client = self.management_client;
        self.finish_mutation(async move {
            local
                .owner
                .mutate(move |store| {
                    if store.record().phase() != LocalMachinePhase::Participating {
                        return Err(Error::NotParticipating);
                    }
                    if client.is_some_and(|remote| {
                        store.record().management_scope(&remote) != Some(ManagementScope::Full)
                    }) {
                        return Err(Error::NeedsFullScope);
                    }
                    store.set_device_enforcement(request.enforced)?;
                    Ok(DeviceEnforcementSet {
                        enforced: request.enforced,
                    })
                })
                .await?
        })
        .await
    }

    /// Under Device enforcement, let an Operator caller's restart step through only with
    /// an approved Device's approval of exactly this restart, and record the step so the
    /// same approval never runs it again. A want that is no restart step, a Machine not
    /// enforcing, and a Container that is not there pass untouched.
    ///
    /// # Errors
    /// Returns [`Error::RestartRefused`] naming what the approval lacks,
    /// [`Error::ApprovalRecord`] when the record of taken steps cannot be read or written,
    /// and [`Error::DockerUnavailable`] or [`Error::Docker`] when the Container cannot be
    /// read.
    pub async fn admit_operator_restart(
        &self,
        want: &ContainerWant,
        approval: Option<&Signed<RestartApproval>>,
    ) -> Result<(), Error> {
        let Some((step, locator)) = RestartStep::of(want) else {
            return Ok(());
        };
        let record = self.record();
        if !record.device_enforcement() {
            return Ok(());
        }
        let approval = approval.ok_or(AuthorityRefusal::Unsigned {
            purpose: Purpose::Workflow,
        })?;
        let ContainerNow::Present(observed) = self.container_now(locator).await? else {
            return Ok(());
        };
        let devices: Vec<DeviceKey> = record.devices().collect();
        let identity = observed.identity();
        let machine = record.id();
        let now = Timestamp::now();
        let cluster = match self.held_sealing(record.phase()) {
            Some(Sealing::Held { public_key }) => Some(public_key),
            _ => None,
        };
        let body = restart::verify(
            approval,
            &RestartFacts {
                cluster,
                machine: &machine,
                devices: &devices,
                namespace: &identity.namespace,
                service: &identity.name,
                now,
            },
        )?;
        self.approvals
            .consume(&body, &observed.container_id, step, now)?;
        Ok(())
    }
}

/// The furthest restart step each approved operation reached on each Container, kept in
/// `<data_dir>/restart-approvals.json` so a replay is refused across daemon restarts. An
/// operation is forgotten once its approval expires, since the approval can no longer
/// pass verification.
pub(crate) struct ConsumptionLog {
    path: PathBuf,
    operations: Mutex<Option<BTreeMap<OperationId, Operation>>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Operation {
    expires_at: Timestamp,
    containers: BTreeMap<ContainerId, RestartStep>,
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
        let mut guard = self
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = Some(self.load().map_err(Error::ApprovalRecord)?);
        }
        let operations = guard.as_mut().expect("loaded above");
        let mut updated: BTreeMap<_, _> = operations
            .iter()
            .filter(|(_, operation)| operation.expires_at >= now)
            .map(|(operation, taken)| (*operation, taken.clone()))
            .collect();
        let operation = updated
            .entry(approval.operation)
            .or_insert_with(|| Operation {
                expires_at: approval.expires_at,
                containers: BTreeMap::new(),
            });
        if operation
            .containers
            .get(container)
            .is_some_and(|reached| *reached >= step)
        {
            return Err(AuthorityRefusal::Replayed {
                operation: approval.operation,
            }
            .into());
        }
        operation.containers.insert(*container, step);
        let contents = serde_json::to_vec(&updated).map_err(io::Error::other);
        contents
            .and_then(|contents| crate::filesystem::atomic_write(&self.path, &contents, 0o600))
            .map_err(Error::ApprovalRecord)?;
        *operations = updated;
        Ok(())
    }

    fn load(&self) -> io::Result<BTreeMap<OperationId, Operation>> {
        match std::fs::read(&self.path) {
            Ok(contents) => serde_json::from_slice(&contents).map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
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
            expires_at: Timestamp::from_millis(expires_at).unwrap(),
            signer: DeviceKey::from_bytes([2; 32]),
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
    fn an_expired_operation_is_forgotten() {
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
        log.consume(&short, &container('a'), RestartStep::Start, at(101))
            .unwrap();
        let stored = std::fs::read_to_string(dir.path().join(FILE_NAME)).unwrap();
        assert_eq!(stored.matches(&"1".repeat(32)).count(), 1, "{stored}");
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
