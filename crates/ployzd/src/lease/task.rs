use crate::intent::lease_intent::{LeaseIntentStore, LeaseIntentStoreError, StoreLeaseOutcome};
use crate::lease::LeaseClient;
use crate::operations::log::{
    ManagedLeaseOperationSubmission, OperationRepository, RecordManagedLeaseTransitionError,
};
use crate::tasks::TaskRegistry;
use ployz_core::cert::{ManagedLeaseName, PublicUrlMode};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::ops::{
    FailureMessage, MANAGED_LEASE_ACQUISITION_SUBJECT, ManagedLeaseOperationFailure,
    ManagedLeaseTransition, OperationStatus,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const MANAGED_LEASE_TICK_INTERVAL: Duration = Duration::from_secs(60);
const MANAGED_LEASE_CONFIGURATION_POLL_INTERVAL: Duration = Duration::from_millis(250);

pub fn start_managed_lease_task(
    registry: &TaskRegistry,
    lease_intent: LeaseIntentStore,
    repository: OperationRepository,
    client: LeaseClient,
    core_machine_id: MachineId,
) {
    registry.spawn(run_loop(lease_intent, repository, client, core_machine_id));
}

async fn run_loop(
    lease_intent: LeaseIntentStore,
    repository: OperationRepository,
    client: LeaseClient,
    core_machine_id: MachineId,
) {
    if let Err(error) = recover_accepted_operations(&repository).await {
        eprintln!("ployzd managed lease recovery warning: {error}");
    }
    loop {
        let delay = match run_once(&lease_intent, &repository, &client, &core_machine_id).await {
            Ok(ManagedLeaseTaskOutcome::AwaitingConfiguration) => {
                MANAGED_LEASE_CONFIGURATION_POLL_INTERVAL
            }
            Ok(
                ManagedLeaseTaskOutcome::NoAction
                | ManagedLeaseTaskOutcome::Acquired { .. }
                | ManagedLeaseTaskOutcome::BundleDownloaded { .. }
                | ManagedLeaseTaskOutcome::Renewed { .. },
            ) => MANAGED_LEASE_TICK_INTERVAL,
            Ok(ManagedLeaseTaskOutcome::Failed { operation_id }) => {
                eprintln!(
                    "ployzd managed lease warning: operation {} failed",
                    operation_id.as_str()
                );
                MANAGED_LEASE_TICK_INTERVAL
            }
            Err(error) => {
                eprintln!("ployzd managed lease warning: {error}");
                if let Err(recovery_error) = recover_accepted_operations(&repository).await {
                    eprintln!("ployzd managed lease recovery warning: {recovery_error}");
                }
                MANAGED_LEASE_TICK_INTERVAL
            }
        };
        tokio::time::sleep(delay).await;
    }
}

pub async fn run_once(
    lease_intent: &LeaseIntentStore,
    repository: &OperationRepository,
    client: &LeaseClient,
    core_machine_id: &MachineId,
) -> Result<ManagedLeaseTaskOutcome, ManagedLeaseTaskError> {
    let Some(intent) = lease_intent.load_if_configured().await? else {
        return Ok(ManagedLeaseTaskOutcome::AwaitingConfiguration);
    };
    if !matches!(intent.mode, PublicUrlMode::Auto) {
        return Ok(ManagedLeaseTaskOutcome::NoAction);
    }

    if intent.needs_acquisition() {
        let lease_name = ManagedLeaseName::try_new(MANAGED_LEASE_ACQUISITION_SUBJECT)
            .map_err(|error| ManagedLeaseTaskError::OperationId(error.to_string()))?;
        let operation = submit_operation(repository, lease_name.clone()).await?;
        // There is no separate cluster-id record; the founder core machine id is
        // the stable bootstrap identity available before the first lease exists.
        match client.acquire(core_machine_id.as_str().to_owned()).await {
            Ok(acquired) => {
                if !store_worker_result(
                    lease_intent,
                    repository,
                    &operation,
                    &lease_name,
                    acquired.lease,
                    acquired.bundle,
                )
                .await?
                {
                    return Ok(ManagedLeaseTaskOutcome::Failed {
                        operation_id: operation,
                    });
                }
                record_completed(repository, &operation, &lease_name).await?;
                return Ok(ManagedLeaseTaskOutcome::Acquired {
                    operation_id: operation,
                });
            }
            Err(error) => {
                record_failed(repository, &operation, &lease_name, &error).await?;
                return Ok(ManagedLeaseTaskOutcome::Failed {
                    operation_id: operation,
                });
            }
        }
    }

    if intent.bundle.is_none() {
        let Some(lease) = intent.lease.clone() else {
            return Ok(ManagedLeaseTaskOutcome::NoAction);
        };
        let lease_name = lease.name.clone();
        let operation = submit_operation(repository, lease_name.clone()).await?;
        match client
            .download_bundle(lease.name.clone(), lease.token.clone())
            .await
        {
            Ok(bundle) => {
                if !store_worker_result(
                    lease_intent,
                    repository,
                    &operation,
                    &lease_name,
                    lease,
                    bundle,
                )
                .await?
                {
                    return Ok(ManagedLeaseTaskOutcome::Failed {
                        operation_id: operation,
                    });
                }
                record_completed(repository, &operation, &lease_name).await?;
                return Ok(ManagedLeaseTaskOutcome::BundleDownloaded {
                    operation_id: operation,
                });
            }
            Err(error) => {
                record_failed(repository, &operation, &lease_name, &error).await?;
                return Ok(ManagedLeaseTaskOutcome::Failed {
                    operation_id: operation,
                });
            }
        }
    }

    if intent.needs_renewal(now_seconds()?) {
        let Some(lease) = intent.lease else {
            return Ok(ManagedLeaseTaskOutcome::NoAction);
        };
        let lease_name = lease.name.clone();
        let operation = submit_operation(repository, lease_name.clone()).await?;
        match client.renew(lease.name, lease.token).await {
            Ok(renewed) => {
                if !store_worker_result(
                    lease_intent,
                    repository,
                    &operation,
                    &lease_name,
                    renewed.lease,
                    renewed.bundle,
                )
                .await?
                {
                    return Ok(ManagedLeaseTaskOutcome::Failed {
                        operation_id: operation,
                    });
                }
                record_completed(repository, &operation, &lease_name).await?;
                return Ok(ManagedLeaseTaskOutcome::Renewed {
                    operation_id: operation,
                });
            }
            Err(error) => {
                record_failed(repository, &operation, &lease_name, &error).await?;
                return Ok(ManagedLeaseTaskOutcome::Failed {
                    operation_id: operation,
                });
            }
        }
    }

    Ok(ManagedLeaseTaskOutcome::NoAction)
}

pub async fn recover_accepted_operations(
    repository: &OperationRepository,
) -> Result<(), ManagedLeaseTaskError> {
    for status in repository.accepted_managed_lease_operations().await? {
        let OperationStatus::ManagedLease { id, lease_name, .. } = status else {
            continue;
        };
        record_failed(
            repository,
            &id,
            &lease_name,
            &"managed lease task resumed without terminal evidence",
        )
        .await?;
    }
    Ok(())
}

async fn submit_operation(
    repository: &OperationRepository,
    lease_name: ManagedLeaseName,
) -> Result<OperationId, ManagedLeaseTaskError> {
    let operation_id = OperationId::try_new(format!("op_managed_lease_{}", nuid::next()))
        .map_err(|error| ManagedLeaseTaskError::OperationId(error.to_string()))?;
    let accepted = repository
        .submit_managed_lease(ManagedLeaseOperationSubmission {
            operation_id,
            lease_name,
        })
        .await
        .map_err(|error| ManagedLeaseTaskError::Submit(format!("{error:?}")))?;
    Ok(accepted.operation_id)
}

async fn store_worker_result(
    lease_intent: &LeaseIntentStore,
    repository: &OperationRepository,
    operation_id: &OperationId,
    lease_name: &ManagedLeaseName,
    record: ployz_core::cert::ManagedLeaseRecord,
    bundle: ployz_core::cert::ManagedCertBundle,
) -> Result<bool, ManagedLeaseTaskError> {
    match lease_intent.store_lease(record, bundle).await {
        Ok(StoreLeaseOutcome::Stored) => Ok(true),
        Ok(StoreLeaseOutcome::Superseded) => {
            record_failed(
                repository,
                operation_id,
                lease_name,
                &"managed lease result was superseded by a public URL mode change",
            )
            .await?;
            Ok(false)
        }
        Err(error) => {
            record_failed(repository, operation_id, lease_name, &error).await?;
            Ok(false)
        }
    }
}

async fn record_completed(
    repository: &OperationRepository,
    operation_id: &OperationId,
    lease_name: &ManagedLeaseName,
) -> Result<(), ManagedLeaseTaskError> {
    repository
        .record_managed_lease_transition(
            operation_id,
            lease_name,
            ManagedLeaseTransition::Completed,
        )
        .await?;
    Ok(())
}

async fn record_failed(
    repository: &OperationRepository,
    operation_id: &OperationId,
    lease_name: &ManagedLeaseName,
    error: &impl std::fmt::Display,
) -> Result<(), ManagedLeaseTaskError> {
    let message = match FailureMessage::try_new(error.to_string()) {
        Ok(message) => message,
        Err(_) => FailureMessage::try_new("managed lease request failed")
            .expect("static managed lease failure message is non-empty"),
    };
    repository
        .record_managed_lease_transition(
            operation_id,
            lease_name,
            ManagedLeaseTransition::Failed {
                failure: ManagedLeaseOperationFailure { message },
            },
        )
        .await?;
    Ok(())
}

fn now_seconds() -> Result<u64, ManagedLeaseTaskError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| ManagedLeaseTaskError::ClockBeforeUnixEpoch)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedLeaseTaskOutcome {
    AwaitingConfiguration,
    NoAction,
    Acquired { operation_id: OperationId },
    BundleDownloaded { operation_id: OperationId },
    Renewed { operation_id: OperationId },
    Failed { operation_id: OperationId },
}

#[derive(Debug, thiserror::Error)]
pub enum ManagedLeaseTaskError {
    #[error("{0}")]
    Intent(#[from] LeaseIntentStoreError),
    #[error("managed lease operation id: {0}")]
    OperationId(String),
    #[error("managed lease operation submission failed: {0}")]
    Submit(String),
    #[error("managed lease operation record failed: {0}")]
    Record(#[from] RecordManagedLeaseTransitionError),
    #[error("managed lease operation recovery failed: {0}")]
    Recovery(#[from] crate::operations::log::OperationStatusStoreError),
    #[error("system clock is before Unix epoch")]
    ClockBeforeUnixEpoch,
}
