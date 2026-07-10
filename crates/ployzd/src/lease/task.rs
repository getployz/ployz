use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ployz_core::cert::{
    AutoLeaseState, ManagedCertBundle, ManagedLeaseAcquireRequest, ManagedLeaseIntent,
    ManagedLeaseRecord,
};
use ployz_core::ids::OperationId;
use ployz_core::ops::{
    FailureMessage, ManagedLeaseFailureClass, ManagedLeaseOperationFailure, ManagedLeaseSubject,
    ManagedLeaseTransition, OperationStatus,
};

use crate::fact_cache::FactCache;
use crate::intent::lease_intent::{LeaseIntentStore, LeaseIntentStoreError, StoreLeaseOutcome};
use crate::intent::machine_roster::{MachineRosterStore, MachineRosterStoreError};
use crate::lease::{LeaseClient, LeaseClientError};
use crate::operations::log::{
    ManagedLeaseOperationSubmission, OperationRepository, RecordManagedLeaseTransitionError,
};
use crate::tasks::TaskRegistry;

use super::client::BundleDownloadOutcome;

pub const MANAGED_LEASE_TICK_INTERVAL: Duration = Duration::from_secs(60);
const MANAGED_LEASE_CONFIGURATION_POLL_INTERVAL: Duration = Duration::from_millis(250);
const MANAGED_LEASE_FAILURE_BACKOFF_CAP: Duration = Duration::from_secs(60 * 60);

pub fn start_managed_lease_task(
    registry: &TaskRegistry,
    lease_intent: LeaseIntentStore,
    repository: OperationRepository,
    client: LeaseClient,
    facts: FactCache,
    roster: MachineRosterStore,
) {
    registry.spawn(run_loop(lease_intent, repository, client, facts, roster));
}

async fn run_loop(
    lease_intent: LeaseIntentStore,
    repository: OperationRepository,
    client: LeaseClient,
    facts: FactCache,
    roster: MachineRosterStore,
) {
    if let Err(error) = recover_accepted_operations(&repository).await {
        eprintln!("ployzd managed lease recovery warning: {error}");
    }
    let mut consecutive_failures = 0;
    let mut acquisition_attempted = false;
    loop {
        let acquiring = matches!(
            lease_intent.load_if_configured().await,
            Ok(Some(ManagedLeaseIntent::Auto { state }))
                if matches!(state.as_ref(), AutoLeaseState::Unacquired)
        );
        if !acquisition_allowed(acquisition_attempted, acquiring) {
            tokio::time::sleep(MANAGED_LEASE_TICK_INTERVAL).await;
            continue;
        }
        if !acquiring {
            acquisition_attempted = false;
        }
        let outcome =
            run_once_with_roster(&lease_intent, &repository, &client, &facts, &roster).await;
        let posted_acquisition = acquiring
            && !matches!(
                &outcome,
                Ok(ManagedLeaseTaskOutcome::AwaitingGatewayTestimony { .. })
                    | Err(ManagedLeaseTaskError::Roster(_))
            );
        let delay = match outcome {
            Ok(ManagedLeaseTaskOutcome::AwaitingConfiguration) => {
                consecutive_failures = 0;
                MANAGED_LEASE_CONFIGURATION_POLL_INTERVAL
            }
            Ok(ManagedLeaseTaskOutcome::AwaitingGatewayTestimony { missing }) => {
                eprintln!(
                    "ployzd managed lease waiting for gateway endpoint testimony: {}",
                    missing
                        .iter()
                        .map(ployz_core::ids::MachineId::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                consecutive_failures = 0;
                MANAGED_LEASE_CONFIGURATION_POLL_INTERVAL
            }
            Ok(
                ManagedLeaseTaskOutcome::NoAction
                | ManagedLeaseTaskOutcome::Acquired { .. }
                | ManagedLeaseTaskOutcome::BundleDownloaded { .. }
                | ManagedLeaseTaskOutcome::Renewed { .. },
            ) => {
                consecutive_failures = 0;
                MANAGED_LEASE_TICK_INTERVAL
            }
            Ok(ManagedLeaseTaskOutcome::Failed { operation_id }) => {
                eprintln!(
                    "ployzd managed lease warning: operation {} failed",
                    operation_id.as_str()
                );
                consecutive_failures += 1;
                failure_delay(consecutive_failures)
            }
            Err(error) => {
                eprintln!("ployzd managed lease warning: {error}");
                if let Err(recovery_error) = recover_accepted_operations(&repository).await {
                    eprintln!("ployzd managed lease recovery warning: {recovery_error}");
                }
                consecutive_failures += 1;
                failure_delay(consecutive_failures)
            }
        };
        acquisition_attempted |= posted_acquisition;
        tokio::time::sleep(delay).await;
    }
}

const fn acquisition_allowed(attempted: bool, acquiring: bool) -> bool {
    !attempted || !acquiring
}

fn failure_delay(consecutive_failures: u32) -> Duration {
    MANAGED_LEASE_TICK_INTERVAL
        .saturating_mul(2_u32.saturating_pow(consecutive_failures))
        .min(MANAGED_LEASE_FAILURE_BACKOFF_CAP)
}

pub async fn run_once(
    lease_intent: &LeaseIntentStore,
    repository: &OperationRepository,
    client: &LeaseClient,
    facts: &FactCache,
) -> Result<ManagedLeaseTaskOutcome, ManagedLeaseTaskError> {
    let acquisition = known_gateway_addresses_from_ids(
        &std::collections::BTreeSet::new(),
        facts.machine_endpoint_observations(),
    );
    run_once_with_addresses(
        lease_intent,
        repository,
        client,
        acquisition.request,
        acquisition.missing,
    )
    .await
}

async fn run_once_with_roster(
    lease_intent: &LeaseIntentStore,
    repository: &OperationRepository,
    client: &LeaseClient,
    facts: &FactCache,
    roster: &MachineRosterStore,
) -> Result<ManagedLeaseTaskOutcome, ManagedLeaseTaskError> {
    let gateway_ids = roster
        .active_machines()
        .await?
        .into_iter()
        .filter(|machine| {
            matches!(
                machine.roles.gateway,
                ployz_core::roles::GatewayRole::Install
            )
        })
        .map(|machine| machine.machine_id)
        .collect();
    let acquisition =
        known_gateway_addresses_from_ids(&gateway_ids, facts.machine_endpoint_observations());
    if !acquisition.missing.is_empty() {
        return Ok(ManagedLeaseTaskOutcome::AwaitingGatewayTestimony {
            missing: acquisition.missing,
        });
    }
    run_once_with_addresses(
        lease_intent,
        repository,
        client,
        acquisition.request,
        Vec::new(),
    )
    .await
}

async fn run_once_with_addresses(
    lease_intent: &LeaseIntentStore,
    repository: &OperationRepository,
    client: &LeaseClient,
    acquisition: ManagedLeaseAcquireRequest,
    missing_gateways: Vec<ployz_core::ids::MachineId>,
) -> Result<ManagedLeaseTaskOutcome, ManagedLeaseTaskError> {
    let Some(intent) = lease_intent.load_if_configured().await? else {
        return Ok(ManagedLeaseTaskOutcome::AwaitingConfiguration);
    };
    let (needs_lease_renewal, needs_certificate_refresh) = match &intent {
        ManagedLeaseIntent::Auto { state }
            if matches!(state.as_ref(), AutoLeaseState::Ready { .. }) =>
        {
            let now = now_seconds()?;
            (
                intent.needs_lease_renewal(now),
                intent.needs_certificate_refresh(now),
            )
        }
        ManagedLeaseIntent::Auto { .. }
        | ManagedLeaseIntent::BringYourOwn
        | ManagedLeaseIntent::None => (false, false),
    };

    let ManagedLeaseIntent::Auto { state } = intent else {
        return Ok(ManagedLeaseTaskOutcome::NoAction);
    };

    match *state {
        AutoLeaseState::Unacquired => {
            run_step(
                lease_intent,
                repository,
                ManagedLeaseSubject::Acquire,
                || async {
                    if !missing_gateways.is_empty() {
                        return Err(LeaseClientError::Transport {
                            message: format!(
                                "gateway endpoint testimony unavailable for {}",
                                missing_gateways
                                    .iter()
                                    .map(ployz_core::ids::MachineId::as_str)
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        });
                    }
                    client
                        .acquire(acquisition)
                        .await
                        .map(|acquired| (acquired.lease, acquired.bundle))
                },
                |operation_id| ManagedLeaseTaskOutcome::Acquired { operation_id },
            )
            .await
        }
        AutoLeaseState::RecordOnly { lease } => {
            run_download_step(lease_intent, repository, client, lease).await
        }
        AutoLeaseState::Ready { lease, bundle: _ } => {
            if needs_certificate_refresh && !needs_lease_renewal {
                return run_download_step(lease_intent, repository, client, lease).await;
            }
            if !needs_lease_renewal {
                return Ok(ManagedLeaseTaskOutcome::NoAction);
            }
            let subject = ManagedLeaseSubject::Renew {
                lease: lease.name.clone(),
            };
            run_step(
                lease_intent,
                repository,
                subject,
                || async {
                    client
                        .renew(lease.name, lease.token)
                        .await
                        .map(|renewed| (renewed.lease, renewed.bundle))
                },
                |operation_id| ManagedLeaseTaskOutcome::Renewed { operation_id },
            )
            .await
        }
    }
}

async fn run_download_step(
    lease_intent: &LeaseIntentStore,
    repository: &OperationRepository,
    client: &LeaseClient,
    lease: ManagedLeaseRecord,
) -> Result<ManagedLeaseTaskOutcome, ManagedLeaseTaskError> {
    let subject = ManagedLeaseSubject::DownloadBundle {
        lease: lease.name.clone(),
    };
    let result = match client
        .download_bundle(lease.name.clone(), lease.token.clone())
        .await
    {
        Ok(BundleDownloadOutcome::Pending(_)) => {
            return Ok(ManagedLeaseTaskOutcome::NoAction);
        }
        Ok(BundleDownloadOutcome::Ready(bundle)) => Ok((lease, Some(bundle))),
        Err(error) => Err(error),
    };
    run_step(
        lease_intent,
        repository,
        subject,
        || async { result },
        |operation_id| ManagedLeaseTaskOutcome::BundleDownloaded { operation_id },
    )
    .await
}

struct GatewayAddressCandidates {
    request: ManagedLeaseAcquireRequest,
    missing: Vec<ployz_core::ids::MachineId>,
}

fn known_gateway_addresses_from_ids(
    gateways: &std::collections::BTreeSet<ployz_core::ids::MachineId>,
    endpoints: Vec<ployz_core::state::MachineEndpointObservation>,
) -> GatewayAddressCandidates {
    let answering = endpoints
        .iter()
        .map(|endpoint| endpoint.machine_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let missing = gateways.difference(&answering).cloned().collect();
    let mut ipv4 = Vec::new();
    let mut ipv6 = Vec::new();
    for endpoint in endpoints {
        if !gateways.contains(&endpoint.machine_id) {
            continue;
        }
        for address in endpoint.control_endpoints {
            match address {
                std::net::IpAddr::V4(address) => ipv4.push(address),
                std::net::IpAddr::V6(address) => ipv6.push(address),
            }
        }
    }
    ipv4.sort_unstable();
    ipv4.dedup();
    ipv6.sort_unstable();
    ipv6.dedup();
    GatewayAddressCandidates {
        request: ManagedLeaseAcquireRequest { ipv4, ipv6 },
        missing,
    }
}

async fn run_step<Worker, WorkerFuture, Success>(
    lease_intent: &LeaseIntentStore,
    repository: &OperationRepository,
    subject: ManagedLeaseSubject,
    worker: Worker,
    success: Success,
) -> Result<ManagedLeaseTaskOutcome, ManagedLeaseTaskError>
where
    Worker: FnOnce() -> WorkerFuture,
    WorkerFuture:
        Future<Output = Result<(ManagedLeaseRecord, Option<ManagedCertBundle>), LeaseClientError>>,
    Success: FnOnce(OperationId) -> ManagedLeaseTaskOutcome,
{
    let operation_id = submit_operation(repository, subject.clone()).await?;
    let (record, bundle) = match worker().await {
        Ok(result) => result,
        Err(error) => {
            record_failed(
                repository,
                &operation_id,
                &subject,
                lease_client_failure_class(&error),
                &error,
            )
            .await?;
            return Ok(ManagedLeaseTaskOutcome::Failed { operation_id });
        }
    };
    match lease_intent.store_lease(record, bundle).await {
        Ok(StoreLeaseOutcome::Stored) => {
            record_completed(repository, &operation_id, &subject).await?;
            Ok(success(operation_id))
        }
        Ok(StoreLeaseOutcome::Superseded) => {
            record_failed(
                repository,
                &operation_id,
                &subject,
                ManagedLeaseFailureClass::Superseded,
                &"managed lease result was superseded by a public URL mode change",
            )
            .await?;
            Ok(ManagedLeaseTaskOutcome::Failed { operation_id })
        }
        Err(error) => {
            record_failed(
                repository,
                &operation_id,
                &subject,
                ManagedLeaseFailureClass::Storage,
                &error,
            )
            .await?;
            Ok(ManagedLeaseTaskOutcome::Failed { operation_id })
        }
    }
}

pub async fn recover_accepted_operations(
    repository: &OperationRepository,
) -> Result<(), ManagedLeaseTaskError> {
    for status in repository.accepted_managed_lease_operations().await? {
        let OperationStatus::ManagedLease { id, subject, .. } = status else {
            continue;
        };
        record_failed(
            repository,
            &id,
            &subject,
            ManagedLeaseFailureClass::Interrupted,
            &"managed lease task resumed without terminal evidence",
        )
        .await?;
    }
    Ok(())
}

async fn submit_operation(
    repository: &OperationRepository,
    subject: ManagedLeaseSubject,
) -> Result<OperationId, ManagedLeaseTaskError> {
    let operation_id = OperationId::try_new(format!("op_managed_lease_{}", nuid::next()))
        .map_err(|error| ManagedLeaseTaskError::OperationId(error.to_string()))?;
    let accepted = repository
        .submit_managed_lease(ManagedLeaseOperationSubmission {
            operation_id,
            subject,
        })
        .await
        .map_err(|error| ManagedLeaseTaskError::Submit(format!("{error:?}")))?;
    Ok(accepted.operation_id)
}

async fn record_completed(
    repository: &OperationRepository,
    operation_id: &OperationId,
    subject: &ManagedLeaseSubject,
) -> Result<(), ManagedLeaseTaskError> {
    repository
        .record_managed_lease_transition(operation_id, subject, ManagedLeaseTransition::Completed)
        .await?;
    Ok(())
}

async fn record_failed(
    repository: &OperationRepository,
    operation_id: &OperationId,
    subject: &ManagedLeaseSubject,
    class: ManagedLeaseFailureClass,
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
            subject,
            ManagedLeaseTransition::Failed {
                failure: ManagedLeaseOperationFailure { class, message },
            },
        )
        .await?;
    Ok(())
}

const fn lease_client_failure_class(error: &LeaseClientError) -> ManagedLeaseFailureClass {
    match error {
        LeaseClientError::Unauthorized => ManagedLeaseFailureClass::WorkerUnauthorized,
        LeaseClientError::LeaseNotFound => ManagedLeaseFailureClass::LeaseNotFound,
        LeaseClientError::Http { .. } => ManagedLeaseFailureClass::WorkerHttp,
        LeaseClientError::Transport { .. } => ManagedLeaseFailureClass::Transport,
        LeaseClientError::Decode { .. } => ManagedLeaseFailureClass::Decode,
    }
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
    AwaitingGatewayTestimony {
        missing: Vec<ployz_core::ids::MachineId>,
    },
    NoAction,
    Acquired {
        operation_id: OperationId,
    },
    BundleDownloaded {
        operation_id: OperationId,
    },
    Renewed {
        operation_id: OperationId,
    },
    Failed {
        operation_id: OperationId,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ManagedLeaseTaskError {
    #[error("{0}")]
    Intent(#[from] LeaseIntentStoreError),
    #[error("managed lease roster read: {0}")]
    Roster(#[from] MachineRosterStoreError),
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

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::state::MachineEndpointObservation;
    use ployz_test_support::ids::machine_id;

    #[test]
    fn failure_backoff_doubles_and_caps() {
        assert_eq!(failure_delay(1), Duration::from_secs(120));
        assert_eq!(failure_delay(10), MANAGED_LEASE_FAILURE_BACKOFF_CAP);
    }

    #[test]
    fn acquisition_addresses_include_only_known_gateway_endpoints() {
        let gateway = machine_id("gateway");
        let result = known_gateway_addresses_from_ids(
            &std::collections::BTreeSet::from([gateway.clone(), machine_id("silent")]),
            vec![
                MachineEndpointObservation {
                    machine_id: gateway,
                    control_endpoints: vec![
                        "203.0.113.8".parse().expect("IPv4"),
                        "2001:db8::8".parse().expect("IPv6"),
                    ],
                    mesh_endpoints: Vec::new(),
                },
                MachineEndpointObservation {
                    machine_id: machine_id("worker"),
                    control_endpoints: vec!["203.0.113.9".parse().expect("IPv4")],
                    mesh_endpoints: Vec::new(),
                },
            ],
        );

        assert_eq!(
            result.request.ipv4,
            ["203.0.113.8".parse::<std::net::Ipv4Addr>().expect("IPv4")]
        );
        assert_eq!(
            result.request.ipv6,
            ["2001:db8::8".parse::<std::net::Ipv6Addr>().expect("IPv6")]
        );
        assert_eq!(result.missing, [machine_id("silent")]);
    }

    #[test]
    fn failed_non_idempotent_acquisition_is_not_repeated_in_same_run() {
        assert!(acquisition_allowed(false, true));
        assert!(!acquisition_allowed(true, true));
        assert!(acquisition_allowed(true, false));
    }

    #[test]
    fn delayed_gateway_testimony_does_not_consume_acquisition_attempt() {
        let gateway = machine_id("gateway");
        let gateways = std::collections::BTreeSet::from([gateway.clone()]);
        let waiting = known_gateway_addresses_from_ids(&gateways, Vec::new());
        let mut attempted = false;
        attempted |= waiting.missing.is_empty();
        assert!(!attempted);

        let ready = known_gateway_addresses_from_ids(
            &gateways,
            vec![MachineEndpointObservation {
                machine_id: gateway,
                control_endpoints: vec!["203.0.113.8".parse().expect("IPv4")],
                mesh_endpoints: Vec::new(),
            }],
        );

        assert!(ready.missing.is_empty());
        assert!(acquisition_allowed(attempted, true));
    }
}
