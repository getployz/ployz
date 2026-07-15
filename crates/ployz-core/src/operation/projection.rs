//! Status projection spine: the projection result and error types shared by
//! every operation kind, and the single dispatcher that routes a classified
//! event to its kind's projection. Each kind's projection logic lives in
//! that kind's module.

use super::cert::{self, CertOperationState};
use super::core_replace::{self, CoreReplaceOperationState};
use super::credential_grant::{self, CredentialGrantOperationState};
use super::deploy::{self, DeployOperationState};
use super::events::{ClassifiedOperationEvent, OperationSubjectRef};
use super::ingress_configure::{self, IngressConfigureOperationState};
use super::machine_add::{self, MachineAddFields, MachineAddOperationState};
use super::machine_lifecycle::{self, MachineLifecycleOperationState};
use super::machine_update::{self, MachineUpdateOperationState};
use super::managed_dns_reconcile::{self, ManagedDnsReconcileOperationState};
use super::namespace_remove::{self, NamespaceRemoveOperationState};
use super::network_repair::{self, NetworkRepairOperationState};
use super::service_restart::{self, ServiceRestartOperationState};
use super::volume_remove::{self, VolumeRemoveOperationState};
use super::{EventSequence, OperationEvent, OperationId, OperationKind, OperationStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationProjection {
    StatusChanged { status: Box<OperationStatus> },
    AlreadySatisfied,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StatusProjectionError {
    #[error("missing operation {}", .operation_id.as_str())]
    MissingOperation { operation_id: OperationId },
    #[error(
        "operation {} kind mismatch: expected {}, found {}",
        .operation_id.as_str(),
        operation_kind_name(*.expected),
        operation_kind_name(*.actual)
    )]
    OperationKindMismatch {
        operation_id: OperationId,
        expected: OperationKind,
        actual: OperationKind,
    },
    #[error(
        "operation {} subject mismatch: expected {}, found {}",
        .operation_id.as_str(),
        subject_ref_text(.expected),
        subject_ref_text(.actual)
    )]
    OperationSubjectMismatch {
        operation_id: OperationId,
        expected: Box<OperationSubjectRef>,
        actual: Box<OperationSubjectRef>,
    },
    #[error(
        "operation event mismatch: expected {}, found {}",
        .expected_operation_id.as_str(),
        .actual_operation_id.as_str()
    )]
    OperationEventMismatch {
        expected_operation_id: OperationId,
        actual_operation_id: OperationId,
    },
    #[error("credential grant operation {} action does not match its submitted action", .operation_id.as_str())]
    CredentialGrantActionMismatch { operation_id: OperationId },
    #[error("ingress configuration operation {} does not match its submitted configuration", .operation_id.as_str())]
    IngressConfigurationMismatch { operation_id: OperationId },
    #[error(
        "operation {} is terminal in its {} state; a {} transition was attempted",
        .operation_id.as_str(),
        operation_kind_name(.current.kind()),
        operation_kind_name(.attempted.kind())
    )]
    TerminalState {
        operation_id: OperationId,
        current: Box<ProjectionOperationState>,
        attempted: Box<ProjectionOperationState>,
    },
    #[error(
        "operation {} cannot transition from its {} state to the attempted {} state",
        .operation_id.as_str(),
        operation_kind_name(.current.kind()),
        operation_kind_name(.attempted.kind())
    )]
    InvalidTransition {
        operation_id: OperationId,
        current: Box<ProjectionOperationState>,
        attempted: Box<ProjectionOperationState>,
    },
    #[error("managed lease operation {} does not support cancellation", .operation_id.as_str())]
    ManagedDnsReconcileCancellationUnsupported { operation_id: OperationId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionOperationState {
    Deploy(DeployOperationState),
    Cert(CertOperationState),
    MachineAdd(MachineAddOperationState),
    MachineUpdate(MachineUpdateOperationState),
    MachineLifecycle(MachineLifecycleOperationState),
    CoreReplace(CoreReplaceOperationState),
    CredentialGrant(CredentialGrantOperationState),
    NetworkRepair(NetworkRepairOperationState),
    ServiceRestart(ServiceRestartOperationState),
    ManagedDnsReconcile(ManagedDnsReconcileOperationState),
    IngressConfigure(IngressConfigureOperationState),
    NamespaceRemove(NamespaceRemoveOperationState),
    VolumeRemove(VolumeRemoveOperationState),
}

impl ProjectionOperationState {
    #[must_use]
    pub const fn kind(&self) -> OperationKind {
        match self {
            Self::Deploy(_) => OperationKind::Deploy,
            Self::Cert(_) => OperationKind::Cert,
            Self::MachineAdd(_) => OperationKind::MachineAdd,
            Self::MachineUpdate(_) => OperationKind::MachineUpdate,
            Self::MachineLifecycle(_) => OperationKind::MachineLifecycle,
            Self::CoreReplace(_) => OperationKind::CoreReplace,
            Self::CredentialGrant(_) => OperationKind::CredentialGrant,
            Self::NetworkRepair(_) => OperationKind::NetworkRepair,
            Self::ServiceRestart(_) => OperationKind::ServiceRestart,
            Self::ManagedDnsReconcile(_) => OperationKind::ManagedDnsReconcile,
            Self::IngressConfigure(_) => OperationKind::IngressConfigure,
            Self::NamespaceRemove(_) => OperationKind::NamespaceRemove,
            Self::VolumeRemove(_) => OperationKind::VolumeRemove,
        }
    }
}

pub(crate) const fn operation_kind_name(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::Deploy => "deploy",
        OperationKind::Cert => "cert",
        OperationKind::MachineAdd => "machine-add",
        OperationKind::MachineUpdate => "machine-update",
        OperationKind::MachineLifecycle => "machine-lifecycle",
        OperationKind::CoreReplace => "core-replace",
        OperationKind::CredentialGrant => "credential-grant",
        OperationKind::NetworkRepair => "network-repair",
        OperationKind::ServiceRestart => "service-restart",
        OperationKind::ManagedDnsReconcile => "managed-dns-reconcile",
        OperationKind::IngressConfigure => "ingress-configure",
        OperationKind::NamespaceRemove => "namespace-remove",
        OperationKind::VolumeRemove => "volume-remove",
    }
}

fn subject_ref_text(subject: &OperationSubjectRef) -> String {
    match subject {
        OperationSubjectRef::Cert(cert_id) => format!("cert {}", cert_id.as_str()),
        OperationSubjectRef::MachineAdd(machine_id) => {
            format!("machine-add {}", machine_id.as_str())
        }
        OperationSubjectRef::MachineUpdate(machine_id) => {
            format!("machine-update {}", machine_id.as_str())
        }
        OperationSubjectRef::MachineLifecycle(machine_id) => {
            format!("machine-lifecycle {}", machine_id.as_str())
        }
        OperationSubjectRef::CoreReplace(machine_id) => {
            format!("core-replace {}", machine_id.as_str())
        }
        OperationSubjectRef::CredentialGrant => "credential-grant".to_owned(),
        OperationSubjectRef::ManagedDnsReconcile(subject) => {
            format!("managed-dns-reconcile {subject:?}")
        }
        OperationSubjectRef::IngressConfigure => "ingress-configure".to_owned(),
    }
}

pub(super) fn kind_mismatch(
    current: &OperationStatus,
    actual: OperationKind,
) -> StatusProjectionError {
    StatusProjectionError::OperationKindMismatch {
        operation_id: current.id().clone(),
        expected: current.kind(),
        actual,
    }
}

/// Checks the subject an event claims against the status record's subject,
/// so a misrouted event surfaces as typed evidence instead of silently
/// mutating the wrong operation.
pub(super) fn verify_subject<Subject: Clone + PartialEq>(
    operation_id: &OperationId,
    expected: &Subject,
    actual: &Subject,
    subject_ref: fn(Subject) -> OperationSubjectRef,
) -> Result<(), StatusProjectionError> {
    if expected == actual {
        return Ok(());
    }

    Err(StatusProjectionError::OperationSubjectMismatch {
        operation_id: operation_id.clone(),
        expected: Box::new(subject_ref(expected.clone())),
        actual: Box::new(subject_ref(actual.clone())),
    })
}

/// The transition skeleton every kind shares: an idempotent re-record is
/// `AlreadySatisfied`, a transition out of a terminal state or across a
/// disallowed edge is a typed error, and an allowed edge advances the status.
/// A kind supplies only its terminal check, its adjacency table, the wrapper
/// that boxes its state into `ProjectionOperationState`, and the status it
/// becomes — everything else lives here once.
pub(super) fn project_transition<S: Clone + PartialEq>(
    operation_id: &OperationId,
    current: &S,
    attempted: S,
    is_terminal: impl Fn(&S) -> bool,
    allowed: impl Fn(&S, &S) -> bool,
    wrap: impl Fn(S) -> ProjectionOperationState,
    build_status: impl FnOnce(S) -> OperationStatus,
) -> Result<OperationProjection, StatusProjectionError> {
    if current == &attempted {
        return Ok(OperationProjection::AlreadySatisfied);
    }
    if is_terminal(current) {
        return Err(StatusProjectionError::TerminalState {
            operation_id: operation_id.clone(),
            current: Box::new(wrap(current.clone())),
            attempted: Box::new(wrap(attempted)),
        });
    }
    if !allowed(current, &attempted) {
        return Err(StatusProjectionError::InvalidTransition {
            operation_id: operation_id.clone(),
            current: Box::new(wrap(current.clone())),
            attempted: Box::new(wrap(attempted)),
        });
    }
    Ok(OperationProjection::StatusChanged {
        status: Box::new(build_status(attempted)),
    })
}

pub fn project_operation_event(
    current: &OperationStatus,
    event: OperationEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    let event = ClassifiedOperationEvent::from(event);
    let event_operation_id = event.operation_id();
    let current_operation_id = current.id();
    if event_operation_id != current_operation_id {
        return Err(StatusProjectionError::OperationEventMismatch {
            expected_operation_id: current_operation_id.clone(),
            actual_operation_id: event_operation_id.clone(),
        });
    }

    let last_event_sequence = current.last_event_sequence();
    if event_sequence <= last_event_sequence {
        return Ok(OperationProjection::AlreadySatisfied);
    }

    match event {
        ClassifiedOperationEvent::Deploy { event, .. } => {
            let OperationStatus::Deploy {
                id,
                namespace_id,
                service_id,
                origin,
                state,
                ..
            } = current
            else {
                return Err(kind_mismatch(current, OperationKind::Deploy));
            };
            deploy::project_event(
                id,
                namespace_id,
                service_id,
                origin,
                state,
                event,
                event_sequence,
            )
        }
        ClassifiedOperationEvent::Cert { event, .. } => {
            let OperationStatus::Cert {
                id, cert_id, state, ..
            } = current
            else {
                return Err(kind_mismatch(current, OperationKind::Cert));
            };
            cert::project_event(id, cert_id, state, event, event_sequence)
        }
        ClassifiedOperationEvent::MachineAdd { event, .. } => {
            let OperationStatus::MachineAdd {
                id,
                machine_id,
                name,
                roles,
                host_port_assurance,
                state,
                ..
            } = current
            else {
                return Err(kind_mismatch(current, OperationKind::MachineAdd));
            };
            let fields = MachineAddFields {
                id,
                machine_id,
                name,
                roles: *roles,
                host_port_assurance: *host_port_assurance,
                state,
            };
            machine_add::project_event(fields, event, event_sequence)
        }
        ClassifiedOperationEvent::MachineUpdate { event, .. } => {
            let OperationStatus::MachineUpdate {
                id,
                machine_id,
                target_version,
                state,
                ..
            } = current
            else {
                return Err(kind_mismatch(current, OperationKind::MachineUpdate));
            };
            machine_update::project_event(
                id,
                machine_id,
                target_version,
                state,
                event,
                event_sequence,
            )
        }
        ClassifiedOperationEvent::MachineLifecycle { event, .. } => {
            let OperationStatus::MachineLifecycle {
                id,
                machine_id,
                target,
                state,
                ..
            } = current
            else {
                return Err(kind_mismatch(current, OperationKind::MachineLifecycle));
            };
            machine_lifecycle::project_event(id, machine_id, *target, state, event, event_sequence)
        }
        ClassifiedOperationEvent::CoreReplace { event, .. } => {
            let OperationStatus::CoreReplace {
                id,
                machine_id,
                successor_nats_url,
                state,
                ..
            } = current
            else {
                return Err(kind_mismatch(current, OperationKind::CoreReplace));
            };
            core_replace::project_event(
                id,
                machine_id,
                successor_nats_url,
                state,
                event,
                event_sequence,
            )
        }
        ClassifiedOperationEvent::CredentialGrant { event, .. } => {
            let OperationStatus::CredentialGrant {
                id, action, state, ..
            } = current
            else {
                return Err(kind_mismatch(current, OperationKind::CredentialGrant));
            };
            credential_grant::project_event(id, action, state, event, event_sequence)
        }
        ClassifiedOperationEvent::NetworkRepair { event, .. } => {
            let OperationStatus::NetworkRepair {
                id,
                target_machine_id,
                state,
                ..
            } = current
            else {
                return Err(kind_mismatch(current, OperationKind::NetworkRepair));
            };
            network_repair::project_event(id, target_machine_id, state, event, event_sequence)
        }
        ClassifiedOperationEvent::ServiceRestart { event, .. } => {
            let OperationStatus::ServiceRestart {
                id,
                namespace_id,
                service_id,
                state,
                ..
            } = current
            else {
                return Err(kind_mismatch(current, OperationKind::ServiceRestart));
            };
            service_restart::project_event(
                id,
                namespace_id,
                service_id,
                state,
                event,
                event_sequence,
            )
        }
        ClassifiedOperationEvent::ManagedDnsReconcile { event, .. } => {
            let OperationStatus::ManagedDnsReconcile {
                id, subject, state, ..
            } = current
            else {
                return Err(kind_mismatch(current, OperationKind::ManagedDnsReconcile));
            };
            managed_dns_reconcile::project_event(id, subject, state, event, event_sequence)
        }
        ClassifiedOperationEvent::IngressConfigure { event, .. } => {
            let OperationStatus::IngressConfigure {
                id,
                configuration,
                state,
                ..
            } = current
            else {
                return Err(kind_mismatch(current, OperationKind::IngressConfigure));
            };
            ingress_configure::project_event(id, configuration, state, event, event_sequence)
        }
        ClassifiedOperationEvent::NamespaceRemove { event, .. } => {
            let OperationStatus::NamespaceRemove {
                id,
                namespace_id,
                state,
                ..
            } = current
            else {
                return Err(kind_mismatch(current, OperationKind::NamespaceRemove));
            };
            namespace_remove::project_event(id, namespace_id, state, event, event_sequence)
        }
        ClassifiedOperationEvent::VolumeRemove { event, .. } => {
            let OperationStatus::VolumeRemove {
                id,
                namespace_id,
                volume_name,
                state,
                ..
            } = current
            else {
                return Err(kind_mismatch(current, OperationKind::VolumeRemove));
            };
            volume_remove::project_event(
                id,
                namespace_id,
                volume_name,
                state,
                event,
                event_sequence,
            )
        }
    }
}
