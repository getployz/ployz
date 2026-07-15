//! Optional Ployz DNS Target projection consumer.

use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{FutureExt, StreamExt};
use ployz_core::cert::{ManagedLeaseAcquireRequest, ManagedLeaseRecord, ManagedLeaseRenewRequest};
use ployz_core::ids::OperationId;
use ployz_core::ingress::{
    IngressEndpointProjection, IngressEndpointProjectionIdentity, IngressEndpointProjectionState,
    IngressEndpointSet, PloyzDnsTargetIntent,
};
use ployz_core::ops::{
    FailureMessage, ManagedDnsReconcileFailure, ManagedDnsReconcileFailureClass,
    ManagedDnsReconcileSubject, ManagedDnsReconcileTransition, ManagedDnsWithdrawAuthorization,
    OperationStatus,
};
use ployz_core::subjects::{INGRESS_ENDPOINT_CHANGED, INGRESS_ENDPOINT_GET};
use ployz_nats::service_runtime::{NatsJsonServiceRequestError, request_json};
use serde::Serialize;

use crate::intent::ingress_intent::{
    IngressIntentStore, ManagedDnsCheckpoint, ManagedDnsEndpointSet, PloyzDnsTargetAllocation,
    PloyzDnsTargetStore, PloyzDnsTargetWrite,
};
use crate::lease::{LeaseClient, LeaseClientError};
use crate::operations::log::{
    ManagedDnsReconcileOperationSubmission, OperationRepository,
    RecordManagedDnsReconcileTransitionError,
};
use crate::tasks::TaskRegistry;

pub const MANAGED_DNS_TICK_INTERVAL: Duration = Duration::from_secs(60);
const PROJECTION_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const SUCCESS_FOLLOW_UP_INTERVAL: Duration = Duration::from_millis(250);
const FAILURE_BACKOFF_BASE: Duration = Duration::from_secs(5);
const FAILURE_BACKOFF_CAP: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(deny_unknown_fields)]
struct IngressEndpointGetRequest {}

pub fn start_managed_dns_task(
    registry: &TaskRegistry,
    client: async_nats::Client,
    ingress_intent: IngressIntentStore,
    target: PloyzDnsTargetStore,
    repository: OperationRepository,
    worker: LeaseClient,
    certificate_wake: tokio::sync::mpsc::Sender<()>,
) {
    registry.spawn(run_loop(
        client,
        ingress_intent,
        target,
        repository,
        worker,
        certificate_wake,
    ));
}

async fn run_loop(
    client: async_nats::Client,
    ingress_intent: IngressIntentStore,
    target: PloyzDnsTargetStore,
    repository: OperationRepository,
    worker: LeaseClient,
    certificate_wake: tokio::sync::mpsc::Sender<()>,
) {
    if let Err(error) = recover_accepted_operations(&repository).await {
        eprintln!("ployzd managed DNS recovery warning: {error}");
    }
    let mut consecutive_failures = 0;
    loop {
        let mut changed = match client.subscribe(INGRESS_ENDPOINT_CHANGED).await {
            Ok(changed) => changed,
            Err(error) => {
                consecutive_failures += 1;
                eprintln!("ployzd managed DNS subscribe warning: {error}");
                tokio::time::sleep(failure_delay(consecutive_failures)).await;
                continue;
            }
        };

        loop {
            let projection = match projection(&client).await {
                Ok(projection) => Some(projection),
                Err(error) => {
                    eprintln!("ployzd managed DNS projection warning: {error}");
                    None
                }
            };
            let outcome = reconcile_once(
                &ingress_intent,
                &target,
                &repository,
                &worker,
                projection.as_ref(),
                match now_seconds() {
                    Ok(now) => now,
                    Err(error) => {
                        eprintln!("ployzd managed DNS clock warning: {error}");
                        tokio::time::sleep(failure_delay(consecutive_failures + 1)).await;
                        continue;
                    }
                },
            )
            .await;

            let delay = match outcome {
                Ok(ManagedDnsTaskOutcome::Failed { operation_id }) => {
                    consecutive_failures += 1;
                    eprintln!(
                        "ployzd managed DNS warning: operation {} failed",
                        operation_id.as_str()
                    );
                    failure_delay(consecutive_failures)
                }
                Err(error) => {
                    consecutive_failures += 1;
                    eprintln!("ployzd managed DNS warning: {error}");
                    if let Err(recovery_error) = recover_accepted_operations(&repository).await {
                        eprintln!("ployzd managed DNS recovery warning: {recovery_error}");
                    }
                    failure_delay(consecutive_failures)
                }
                Ok(ManagedDnsTaskOutcome::Reconciled { .. }) => {
                    let _ = certificate_wake.try_send(());
                    consecutive_failures = 0;
                    SUCCESS_FOLLOW_UP_INTERVAL
                }
                Ok(_) => {
                    consecutive_failures = 0;
                    MANAGED_DNS_TICK_INTERVAL
                }
            };

            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                message = changed.next() => {
                    if message.is_none() {
                        break;
                    }
                    while changed.next().now_or_never().flatten().is_some() {}
                }
            }
        }
    }
}

async fn projection(
    client: &async_nats::Client,
) -> Result<IngressEndpointProjection, NatsJsonServiceRequestError> {
    request_json(
        client,
        INGRESS_ENDPOINT_GET.to_owned(),
        &IngressEndpointGetRequest {},
        PROJECTION_REQUEST_TIMEOUT,
    )
    .await
}

pub async fn reconcile_once(
    ingress_intent: &IngressIntentStore,
    target: &PloyzDnsTargetStore,
    repository: &OperationRepository,
    worker: &LeaseClient,
    projection: Option<&IngressEndpointProjection>,
    now_seconds: u64,
) -> Result<ManagedDnsTaskOutcome, ManagedDnsTaskError> {
    let Some(configuration) = ingress_intent.load().await? else {
        return Ok(ManagedDnsTaskOutcome::AwaitingConfiguration);
    };
    match configuration.ployz_dns_target() {
        PloyzDnsTargetIntent::Disabled => reconcile_disabled(target, repository, worker).await,
        PloyzDnsTargetIntent::Enabled => {
            reconcile_enabled(target, repository, worker, projection, now_seconds).await
        }
    }
}

async fn reconcile_enabled(
    target: &PloyzDnsTargetStore,
    repository: &OperationRepository,
    worker: &LeaseClient,
    projection: Option<&IngressEndpointProjection>,
    now_seconds: u64,
) -> Result<ManagedDnsTaskOutcome, ManagedDnsTaskError> {
    let Some(allocation) = target.ensure_acquisition().await? else {
        return Ok(ManagedDnsTaskOutcome::NoAction);
    };
    match allocation {
        PloyzDnsTargetAllocation::Unacquired {
            acquisition_id,
            token,
        } => {
            let subject = ManagedDnsReconcileSubject::Acquire;
            run_worker_step(repository, subject, || async move {
                let acquired = worker
                    .acquire(ManagedLeaseAcquireRequest {
                        acquisition_id: acquisition_id.clone(),
                        token,
                        ipv4: Vec::new(),
                        ipv6: Vec::new(),
                    })
                    .await?;
                match target
                    .store_acquired(acquisition_id, acquired.lease)
                    .await?
                {
                    PloyzDnsTargetWrite::Stored => Ok(()),
                    PloyzDnsTargetWrite::Superseded => Err(StepError::Superseded),
                }
            })
            .await
        }
        PloyzDnsTargetAllocation::Allocated { lease } => {
            let checkpoint = target.load_checkpoint().await?;
            let Some(action) = next_action(projection, checkpoint.as_ref(), &lease, now_seconds)
            else {
                return Ok(if projection.is_some() {
                    ManagedDnsTaskOutcome::NoAction
                } else {
                    ManagedDnsTaskOutcome::AwaitingProjection
                });
            };
            reconcile_allocation(target, repository, worker, lease, action).await
        }
    }
}

async fn reconcile_disabled(
    target: &PloyzDnsTargetStore,
    repository: &OperationRepository,
    worker: &LeaseClient,
) -> Result<ManagedDnsTaskOutcome, ManagedDnsTaskError> {
    let Some(PloyzDnsTargetAllocation::Allocated { lease }) = target.load_allocation().await?
    else {
        return Ok(ManagedDnsTaskOutcome::NoAction);
    };
    if matches!(
        target.load_checkpoint().await?,
        Some(ManagedDnsCheckpoint::Withdrawn)
    ) {
        return Ok(ManagedDnsTaskOutcome::NoAction);
    }
    let subject = ManagedDnsReconcileSubject::AuthorizedWithdraw {
        lease: lease.name.clone(),
        authorization: ManagedDnsWithdrawAuthorization::TargetDisabled,
    };
    run_worker_step(repository, subject, || async move {
        let renewed = worker
            .renew(
                lease.name,
                lease.token,
                ManagedLeaseRenewRequest {
                    ipv4: Vec::new(),
                    ipv6: Vec::new(),
                },
            )
            .await?;
        match target.store_successful_withdraw(renewed.lease).await? {
            PloyzDnsTargetWrite::Stored => Ok(()),
            PloyzDnsTargetWrite::Superseded => Err(StepError::Superseded),
        }
    })
    .await
}

async fn reconcile_allocation(
    target: &PloyzDnsTargetStore,
    repository: &OperationRepository,
    worker: &LeaseClient,
    lease: ManagedLeaseRecord,
    action: ReconcileAction,
) -> Result<ManagedDnsTaskOutcome, ManagedDnsTaskError> {
    let (subject, identity, endpoints) = match action {
        ReconcileAction::ProjectionApply {
            identity,
            endpoints,
        } => (
            ManagedDnsReconcileSubject::ProjectionApply {
                lease: lease.name.clone(),
                projection: identity,
            },
            Some(identity),
            endpoints,
        ),
        ReconcileAction::ProjectionWithdraw { identity } => (
            ManagedDnsReconcileSubject::AuthorizedWithdraw {
                lease: lease.name.clone(),
                authorization: ManagedDnsWithdrawAuthorization::ProjectionUnavailable {
                    projection: identity,
                },
            },
            Some(identity),
            empty_managed_dns_endpoint_set(),
        ),
        ReconcileAction::LeaseRenewal {
            identity,
            endpoints,
        } => (
            ManagedDnsReconcileSubject::LeaseRenewal {
                lease: lease.name.clone(),
                projection: identity,
            },
            identity,
            endpoints,
        ),
    };
    run_worker_step(repository, subject, || async move {
        let renewed = worker
            .renew(
                lease.name,
                lease.token,
                ManagedLeaseRenewRequest {
                    ipv4: endpoints.ipv4().iter().copied().collect(),
                    ipv6: endpoints.ipv6().iter().copied().collect(),
                },
            )
            .await?;
        let checkpoint = ManagedDnsCheckpoint::Applied {
            last_applied_identity: identity,
            last_applied_endpoints: endpoints,
        };
        match target
            .store_successful_reconcile(renewed.lease, checkpoint)
            .await?
        {
            PloyzDnsTargetWrite::Stored => Ok(()),
            PloyzDnsTargetWrite::Superseded => Err(StepError::Superseded),
        }
    })
    .await
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReconcileAction {
    ProjectionApply {
        identity: IngressEndpointProjectionIdentity,
        endpoints: ManagedDnsEndpointSet,
    },
    ProjectionWithdraw {
        identity: IngressEndpointProjectionIdentity,
    },
    LeaseRenewal {
        identity: Option<IngressEndpointProjectionIdentity>,
        endpoints: ManagedDnsEndpointSet,
    },
}

fn next_action(
    projection: Option<&IngressEndpointProjection>,
    checkpoint: Option<&ManagedDnsCheckpoint>,
    lease: &ManagedLeaseRecord,
    now_seconds: u64,
) -> Option<ReconcileAction> {
    if let Some(projection) = projection {
        match &projection.state {
            IngressEndpointProjectionState::Current { endpoints }
            | IngressEndpointProjectionState::Retained { endpoints } => {
                let endpoints = managed_dns_endpoint_set(endpoints);
                if !checkpoint_matches(checkpoint, Some(projection.identity()), &endpoints) {
                    return Some(ReconcileAction::ProjectionApply {
                        identity: projection.identity(),
                        endpoints,
                    });
                }
            }
            IngressEndpointProjectionState::Unavailable { .. } => {
                let endpoints = empty_managed_dns_endpoint_set();
                if !checkpoint_matches(checkpoint, Some(projection.identity()), &endpoints) {
                    return Some(ReconcileAction::ProjectionWithdraw {
                        identity: projection.identity(),
                    });
                }
            }
            IngressEndpointProjectionState::Pending => {}
        }
    }

    if renewal_due(lease, now_seconds)
        && let Some(ManagedDnsCheckpoint::Applied {
            last_applied_identity,
            last_applied_endpoints,
        }) = checkpoint
    {
        return Some(ReconcileAction::LeaseRenewal {
            identity: *last_applied_identity,
            endpoints: last_applied_endpoints.clone(),
        });
    }
    if renewal_due(lease, now_seconds)
        && matches!(checkpoint, Some(ManagedDnsCheckpoint::Withdrawn))
    {
        return Some(ReconcileAction::LeaseRenewal {
            identity: None,
            endpoints: empty_managed_dns_endpoint_set(),
        });
    }
    None
}

fn managed_dns_endpoint_set(endpoints: &IngressEndpointSet) -> ManagedDnsEndpointSet {
    ManagedDnsEndpointSet::new(
        endpoints.ipv4().iter().copied().collect(),
        endpoints.ipv6().iter().copied().collect(),
    )
}

fn empty_managed_dns_endpoint_set() -> ManagedDnsEndpointSet {
    ManagedDnsEndpointSet::new(Vec::new(), Vec::new())
}

fn checkpoint_matches(
    checkpoint: Option<&ManagedDnsCheckpoint>,
    identity: Option<IngressEndpointProjectionIdentity>,
    endpoints: &ManagedDnsEndpointSet,
) -> bool {
    matches!(
        checkpoint,
        Some(ManagedDnsCheckpoint::Applied {
            last_applied_identity,
            last_applied_endpoints,
        }) if *last_applied_identity == identity && last_applied_endpoints == endpoints
    )
}

fn renewal_due(lease: &ManagedLeaseRecord, now_seconds: u64) -> bool {
    let issued_at = lease.issued_at.unix_seconds();
    let lifetime = lease.expires_at.unix_seconds().saturating_sub(issued_at);
    now_seconds >= issued_at.saturating_add(lifetime.saturating_mul(2) / 3)
}

async fn run_worker_step<Run, Fut>(
    repository: &OperationRepository,
    subject: ManagedDnsReconcileSubject,
    run: Run,
) -> Result<ManagedDnsTaskOutcome, ManagedDnsTaskError>
where
    Run: FnOnce() -> Fut,
    Fut: Future<Output = Result<(), StepError>>,
{
    let operation_id = OperationId::try_new(format!("op_managed_dns_{}", nuid::next()))
        .expect("NUID operation id is valid");
    repository
        .submit_managed_dns_reconcile(ManagedDnsReconcileOperationSubmission {
            operation_id: operation_id.clone(),
            subject: subject.clone(),
        })
        .await?;
    let result = run().await;
    let transition = match &result {
        Ok(()) => ManagedDnsReconcileTransition::Completed,
        Err(error) => ManagedDnsReconcileTransition::Failed {
            failure: error.operation_failure(),
        },
    };
    repository
        .record_managed_dns_reconcile_transition(&operation_id, &subject, transition)
        .await?;
    Ok(match result {
        Ok(()) => ManagedDnsTaskOutcome::Reconciled { operation_id },
        Err(_) => ManagedDnsTaskOutcome::Failed { operation_id },
    })
}

async fn recover_accepted_operations(
    repository: &OperationRepository,
) -> Result<(), ManagedDnsTaskError> {
    for status in repository
        .accepted_managed_dns_reconcile_operations()
        .await?
    {
        let OperationStatus::ManagedDnsReconcile {
            id,
            subject,
            state: _,
            last_event_sequence: _,
        } = status
        else {
            continue;
        };
        repository
            .record_managed_dns_reconcile_transition(
                &id,
                &subject,
                ManagedDnsReconcileTransition::Failed {
                    failure: ManagedDnsReconcileFailure {
                        class: ManagedDnsReconcileFailureClass::Interrupted,
                        message: failure_message(
                            "core stopped before the managed DNS call completed",
                        ),
                    },
                },
            )
            .await?;
    }
    Ok(())
}

fn failure_delay(consecutive_failures: u32) -> Duration {
    FAILURE_BACKOFF_BASE
        .saturating_mul(2_u32.saturating_pow(consecutive_failures.saturating_sub(1)))
        .min(FAILURE_BACKOFF_CAP)
}

fn now_seconds() -> Result<u64, ManagedDnsTaskError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| ManagedDnsTaskError::Clock {
            message: error.to_string(),
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedDnsTaskOutcome {
    AwaitingConfiguration,
    AwaitingProjection,
    NoAction,
    Reconciled { operation_id: OperationId },
    Failed { operation_id: OperationId },
}

#[derive(Debug, thiserror::Error)]
enum StepError {
    #[error(transparent)]
    Worker(#[from] LeaseClientError),
    #[error(transparent)]
    Store(#[from] crate::core_store::CoreStoreError),
    #[error("managed DNS result was superseded by current intent")]
    Superseded,
}

impl StepError {
    fn operation_failure(&self) -> ManagedDnsReconcileFailure {
        let class = match self {
            Self::Worker(LeaseClientError::Unauthorized) => {
                ManagedDnsReconcileFailureClass::WorkerUnauthorized
            }
            Self::Worker(LeaseClientError::LeaseNotFound) => {
                ManagedDnsReconcileFailureClass::LeaseNotFound
            }
            Self::Worker(LeaseClientError::Http { .. }) => {
                ManagedDnsReconcileFailureClass::WorkerHttp
            }
            Self::Worker(LeaseClientError::Transport { .. }) => {
                ManagedDnsReconcileFailureClass::Transport
            }
            Self::Worker(LeaseClientError::Decode { .. }) => {
                ManagedDnsReconcileFailureClass::Decode
            }
            Self::Store(_) => ManagedDnsReconcileFailureClass::Storage,
            Self::Superseded => ManagedDnsReconcileFailureClass::Superseded,
        };
        ManagedDnsReconcileFailure {
            class,
            message: failure_message(&self.to_string()),
        }
    }
}

fn failure_message(message: &str) -> FailureMessage {
    FailureMessage::try_new(message).expect("managed DNS failures are non-empty")
}

#[derive(Debug, thiserror::Error)]
pub enum ManagedDnsTaskError {
    #[error(transparent)]
    Intent(#[from] crate::intent::ingress_intent::IngressIntentStoreError),
    #[error(transparent)]
    Store(#[from] crate::core_store::CoreStoreError),
    #[error("submit managed DNS operation: {message}")]
    Submit { message: String },
    #[error(transparent)]
    Record(#[from] RecordManagedDnsReconcileTransitionError),
    #[error(transparent)]
    Status(#[from] crate::operations::log::OperationStatusStoreError),
    #[error("system clock: {message}")]
    Clock { message: String },
}

impl From<crate::operations::log::SubmitOperationError> for ManagedDnsTaskError {
    fn from(error: crate::operations::log::SubmitOperationError) -> Self {
        Self::Submit {
            message: format!("{error:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::cert::{LeaseBearerToken, LeaseExpiresAt, LeaseIssuedAt, ManagedLeaseName};
    use ployz_core::ingress::IngressEndpointUnavailableReason;
    use ployz_core::state::ControlPlaneEpoch;

    fn lease(issued_at: u64, expires_at: u64) -> ManagedLeaseRecord {
        ManagedLeaseRecord::try_new(
            ManagedLeaseName::try_new("cluster-one").expect("lease name"),
            LeaseBearerToken::try_new("lease-token").expect("token"),
            LeaseIssuedAt::try_new(issued_at).expect("issued at"),
            LeaseExpiresAt::try_new(expires_at).expect("expires at"),
        )
        .expect("lease window")
    }

    fn projection(
        state: IngressEndpointProjectionState,
        revision: u64,
    ) -> IngressEndpointProjection {
        IngressEndpointProjection {
            control_plane_epoch: ControlPlaneEpoch::initial().next(),
            revision,
            state,
        }
    }

    fn endpoints() -> IngressEndpointSet {
        IngressEndpointSet::try_new(
            ["8.8.8.8".parse().expect("IPv4")],
            ["2001:4860:4860::8888".parse().expect("IPv6")],
        )
        .expect("public endpoints")
    }

    #[test]
    fn pending_and_get_error_make_no_projection_call_before_renewal() {
        let lease = lease(100, 190);
        let checkpoint = ManagedDnsCheckpoint::Applied {
            last_applied_identity: None,
            last_applied_endpoints: empty_managed_dns_endpoint_set(),
        };
        let pending = projection(IngressEndpointProjectionState::Pending, 0);

        assert_eq!(
            next_action(Some(&pending), Some(&checkpoint), &lease, 159),
            None
        );
        assert_eq!(next_action(None, Some(&checkpoint), &lease, 159), None);
    }

    #[test]
    fn current_applies_once_and_retained_with_same_identity_is_stable() {
        let lease = lease(100, 190);
        let current = projection(
            IngressEndpointProjectionState::Current {
                endpoints: endpoints(),
            },
            4,
        );
        let IngressEndpointProjectionState::Current {
            endpoints: current_endpoints,
        } = &current.state
        else {
            panic!("expected current endpoint projection");
        };
        let expected = managed_dns_endpoint_set(current_endpoints);
        assert!(matches!(
            next_action(Some(&current), None, &lease, 120),
            Some(ReconcileAction::ProjectionApply { .. })
        ));
        let checkpoint = ManagedDnsCheckpoint::Applied {
            last_applied_identity: Some(current.identity()),
            last_applied_endpoints: expected,
        };
        let retained = projection(
            IngressEndpointProjectionState::Retained {
                endpoints: endpoints(),
            },
            4,
        );
        assert_eq!(
            next_action(Some(&retained), Some(&checkpoint), &lease, 120),
            None
        );
    }

    #[test]
    fn unavailable_authorizes_empty_withdrawal() {
        let lease = lease(100, 190);
        let unavailable = projection(
            IngressEndpointProjectionState::Unavailable {
                reason: IngressEndpointUnavailableReason::NoDeclaredGateways,
            },
            5,
        );
        assert!(matches!(
            next_action(Some(&unavailable), None, &lease, 120),
            Some(ReconcileAction::ProjectionWithdraw { .. })
        ));
    }

    #[test]
    fn renewal_is_due_at_two_thirds_and_replays_last_known_good_during_uncertainty() {
        let lease = lease(100, 190);
        let identity = IngressEndpointProjectionIdentity {
            control_plane_epoch: ControlPlaneEpoch::initial().next(),
            revision: 4,
        };
        let checkpoint = ManagedDnsCheckpoint::Applied {
            last_applied_identity: Some(identity),
            last_applied_endpoints: managed_dns_endpoint_set(&endpoints()),
        };
        assert_eq!(next_action(None, Some(&checkpoint), &lease, 159), None);
        assert!(matches!(
            next_action(None, Some(&checkpoint), &lease, 160),
            Some(ReconcileAction::LeaseRenewal {
                identity: Some(current),
                ..
            }) if current == identity
        ));
    }

    #[test]
    fn failure_backoff_is_bounded() {
        assert_eq!(failure_delay(1), Duration::from_secs(5));
        assert_eq!(failure_delay(2), Duration::from_secs(10));
        assert_eq!(failure_delay(99), Duration::from_secs(60 * 60));
    }
}
