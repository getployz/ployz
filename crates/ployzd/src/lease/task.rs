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
use crate::lease::{LeaseClient, LeaseClientError};
use crate::operations::log::{
    ManagedLeaseOperationSubmission, OperationRepository, RecordManagedLeaseTransitionError,
};
use crate::tasks::TaskRegistry;

pub const MANAGED_LEASE_TICK_INTERVAL: Duration = Duration::from_secs(60);
const MANAGED_LEASE_CONFIGURATION_POLL_INTERVAL: Duration = Duration::from_millis(250);
const MANAGED_LEASE_FAILURE_BACKOFF_CAP: Duration = Duration::from_secs(60 * 60);

pub fn start_managed_lease_task(
    registry: &TaskRegistry,
    lease_intent: LeaseIntentStore,
    repository: OperationRepository,
    client: LeaseClient,
    facts: FactCache,
) {
    registry.spawn(run_loop(lease_intent, repository, client, facts));
}

async fn run_loop(
    lease_intent: LeaseIntentStore,
    repository: OperationRepository,
    client: LeaseClient,
    facts: FactCache,
) {
    if let Err(error) = recover_accepted_operations(&repository).await {
        eprintln!("ployzd managed lease recovery warning: {error}");
    }
    let mut consecutive_failures = 0;
    loop {
        let delay = match run_once(&lease_intent, &repository, &client, &facts).await {
            Ok(ManagedLeaseTaskOutcome::AwaitingConfiguration) => {
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
        tokio::time::sleep(delay).await;
    }
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
    let Some(intent) = lease_intent.load_if_configured().await? else {
        return Ok(ManagedLeaseTaskOutcome::AwaitingConfiguration);
    };
    let needs_renewal = match &intent {
        ManagedLeaseIntent::Auto { state }
            if matches!(state.as_ref(), AutoLeaseState::Ready { .. }) =>
        {
            intent.needs_renewal(now_seconds()?)
        }
        ManagedLeaseIntent::Auto { .. }
        | ManagedLeaseIntent::BringYourOwn
        | ManagedLeaseIntent::None => false,
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
                    client
                        .acquire(known_gateway_addresses(facts))
                        .await
                        .map(|acquired| (acquired.lease, acquired.bundle))
                },
                |operation_id| ManagedLeaseTaskOutcome::Acquired { operation_id },
            )
            .await
        }
        AutoLeaseState::RecordOnly { lease } => {
            let subject = ManagedLeaseSubject::DownloadBundle {
                lease: lease.name.clone(),
            };
            run_step(
                lease_intent,
                repository,
                subject,
                || async {
                    let bundle = client
                        .download_bundle(lease.name.clone(), lease.token.clone())
                        .await?;
                    Ok((lease, bundle))
                },
                |operation_id| ManagedLeaseTaskOutcome::BundleDownloaded { operation_id },
            )
            .await
        }
        AutoLeaseState::Ready { lease, bundle: _ } => {
            if !needs_renewal {
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

fn known_gateway_addresses(facts: &FactCache) -> ManagedLeaseAcquireRequest {
    gateway_addresses(
        facts.gateway_statuses(),
        facts.machine_endpoint_observations(),
    )
}

fn gateway_addresses(
    statuses: Vec<ployz_core::state::GatewayStatusObservation>,
    endpoints: Vec<ployz_core::state::MachineEndpointObservation>,
) -> ManagedLeaseAcquireRequest {
    let gateways = statuses
        .into_iter()
        .map(|status| status.machine_id)
        .collect::<std::collections::BTreeSet<_>>();
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
    ManagedLeaseAcquireRequest { ipv4, ipv6 }
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
        Future<Output = Result<(ManagedLeaseRecord, ManagedCertBundle), LeaseClientError>>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::state::{
        GatewayServingStatus, GatewayStatusObservation, MachineEndpointObservation,
    };
    use ployz_test_support::ids::machine_id;

    #[test]
    fn failure_backoff_doubles_and_caps() {
        assert_eq!(failure_delay(1), Duration::from_secs(120));
        assert_eq!(failure_delay(10), MANAGED_LEASE_FAILURE_BACKOFF_CAP);
    }

    #[test]
    fn acquisition_addresses_include_only_known_gateway_endpoints() {
        let gateway = machine_id("gateway");
        let result = gateway_addresses(
            vec![GatewayStatusObservation {
                machine_id: gateway.clone(),
                listen_addr: "0.0.0.0:80".parse().expect("listen address"),
                serving: GatewayServingStatus::Current,
                route_count: 0,
            }],
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
            result.ipv4,
            ["203.0.113.8".parse::<std::net::Ipv4Addr>().expect("IPv4")]
        );
        assert_eq!(
            result.ipv6,
            ["2001:db8::8".parse::<std::net::Ipv6Addr>().expect("IPv6")]
        );
    }
}
