//! Operation-owned admission of a joining machine into dataplane projection.

use std::time::Duration;

use futures_util::future::try_join_all;
use ployz_core::dataplane::{
    DataplaneProjection, DataplaneProjectionMember, DataplaneProjectionTestimony,
    EbpfAttachmentStatus, EndpointBridgeStatus, MAX_HEALTHY_WIREGUARD_HANDSHAKE_AGE_SECONDS,
    MachineDataplaneStatus, WireGuardHandshakeStatus, WireGuardInterfaceMtu,
    WireGuardPeerEndpointSubnet,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::machine::{
    DataplaneAdmissionPeer, DataplaneProjectionAdmissionEvidence,
    DataplaneProjectionAdmissionFailure, RawJoinToken,
};
use ployz_core::ops::{FailureMessage, MachineAddOperationState, OperationStatus};
use ployz_core::state::StagedMachineDataplaneState;
use ployz_sdk_types::MachineJoinReportError;

use crate::roles::machine::client::{NatsMachineDataplaneReader, NatsMachineFactsReader};

use super::OperationApiHandlers;
use super::error_map::{corrupt, machine_join_report_error};

const DATAPLANE_ADMISSION_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const DATAPLANE_ADMISSION_DEADLINE: Duration = Duration::from_secs(45);
const DATAPLANE_ADMISSION_RETRY_DELAY: Duration = Duration::from_millis(250);

// ponytail: join report owns one bounded gather; move it to an operation worker if admission becomes asynchronous.
pub(super) async fn prove_completed_join(
    handlers: &OperationApiHandlers,
    raw_token: &RawJoinToken,
) -> Result<Option<DataplaneProjectionAdmissionEvidence>, MachineJoinReportError> {
    let target = handlers
        .controllers
        .repository()
        .machine_join_report_target(raw_token)
        .await
        .map_err(machine_join_report_error)?;
    let status = handlers
        .controllers
        .repository()
        .get(&target.operation_id)
        .await
        .map_err(unavailable)?
        .ok_or_else(|| MachineJoinReportError::Unavailable {
            message: corrupt("missing machine-add operation before dataplane admission"),
        })?;
    let OperationStatus::MachineAdd {
        id: operation_id,
        machine_id,
        state,
        ..
    } = status
    else {
        return Err(MachineJoinReportError::Unavailable {
            message: corrupt("joined operation is not machine-add"),
        });
    };
    match state {
        MachineAddOperationState::Joining { .. } => {
            admit_projection(handlers, operation_id, machine_id, target.endpoint_subnet).await
        }
        MachineAddOperationState::Pending { .. }
        | MachineAddOperationState::Completed
        | MachineAddOperationState::Failed { .. }
        | MachineAddOperationState::Cancelled { .. } => Ok(None),
    }
}

async fn admit_projection(
    handlers: &OperationApiHandlers,
    operation_id: OperationId,
    joining_machine_id: MachineId,
    joining_subnet: ployz_core::dataplane::MachineEndpointSubnet,
) -> Result<Option<DataplaneProjectionAdmissionEvidence>, MachineJoinReportError> {
    let mesh_lock = handlers.controllers.mesh_lock();
    let _mesh_guard = mesh_lock.lock().await;
    let active = handlers
        .machine_roster
        .active_machines()
        .await
        .map_err(unavailable)?;
    let mut machine_ids = active
        .iter()
        .map(|machine| machine.machine_id.clone())
        .chain(std::iter::once(joining_machine_id.clone()))
        .collect::<Vec<_>>();
    machine_ids.sort();

    let preparer = NatsMachineDataplaneReader::new(handlers.intent_change_client.clone())
        .with_request_timeout(DATAPLANE_ADMISSION_REQUEST_TIMEOUT);
    let public_key = match preparer
        .read_projection_public_key(
            operation_id.clone(),
            &joining_machine_id,
            machine_ids.clone(),
        )
        .await
    {
        Ok(public_key) => public_key,
        Err(error) => {
            return Ok(Some(no_answer(
                joining_machine_id,
                format!("WireGuard public key unavailable: {error:?}"),
            )));
        }
    };
    let facts = NatsMachineFactsReader::new(handlers.intent_change_client.clone())
        .with_request_timeout(DATAPLANE_ADMISSION_REQUEST_TIMEOUT)
        .machine_facts(&joining_machine_id)
        .await;
    let mut mesh_endpoints = match facts {
        Ok(facts) => facts
            .endpoints()
            .map(|endpoints| endpoints.mesh_endpoints.clone())
            .unwrap_or_default(),
        Err(error) => {
            return Ok(Some(no_answer(joining_machine_id, error.to_string())));
        }
    };
    if mesh_endpoints.is_empty() {
        return Ok(Some(no_answer(
            joining_machine_id,
            "machine facts contain no mesh endpoint".to_owned(),
        )));
    }
    mesh_endpoints.sort();
    mesh_endpoints.dedup();

    let staged = StagedMachineDataplaneState {
        operation_id: operation_id.clone(),
        machine_id: joining_machine_id.clone(),
        endpoint_subnet: joining_subnet,
        mesh_endpoints,
        wireguard_public_key: public_key,
    };
    handlers
        .controllers
        .repository()
        .stage_machine_dataplane(staged.clone())
        .await
        .map_err(unavailable)?;

    if let Err(error) = publish_invalidation(handlers).await {
        let evidence = no_answer(joining_machine_id, error);
        clear_staging(handlers, &operation_id).await?;
        return Ok(Some(evidence));
    }

    let projection = match handlers.intent_reader.dataplane_projection().await {
        Ok(projection) => projection,
        Err(error) => {
            let evidence = no_answer(joining_machine_id, error.to_string());
            clear_staging(handlers, &operation_id).await?;
            return Ok(Some(evidence));
        }
    };
    if let Err(message) = validate_staged_projection(&projection, &staged) {
        let evidence = invalid_staged_projection(joining_machine_id, message);
        clear_staging(handlers, &operation_id).await?;
        return Ok(Some(evidence));
    }

    let evidence = gather_admission(&preparer, &projection).await;
    if let Some(evidence) = evidence {
        clear_staging(handlers, &operation_id).await?;
        return Ok(Some(evidence));
    }

    Ok(None)
}

async fn gather_admission(
    preparer: &NatsMachineDataplaneReader,
    projection: &DataplaneProjection,
) -> Option<DataplaneProjectionAdmissionEvidence> {
    let deadline = tokio::time::Instant::now() + DATAPLANE_ADMISSION_DEADLINE;
    loop {
        let statuses = try_join_all(projection.target_members.iter().map(|member| async {
            preparer
                .read_dataplane_status(&member.machine_id)
                .await
                .map(|status| (member.machine_id.clone(), status))
                .map_err(|message| DataplaneProjectionAdmissionEvidence {
                    machine_id: member.machine_id.clone(),
                    reason: DataplaneProjectionAdmissionFailure::NoAnswer { message },
                })
        }))
        .await;
        let evidence = match statuses {
            Ok(statuses) => validate_admission(projection, &statuses),
            Err(evidence) => Some(evidence),
        };
        let evidence = evidence?;
        if tokio::time::Instant::now() >= deadline || !retryable_admission(&evidence.reason) {
            return Some(evidence);
        }
        tokio::time::sleep(DATAPLANE_ADMISSION_RETRY_DELAY).await;
    }
}

fn retryable_admission(reason: &DataplaneProjectionAdmissionFailure) -> bool {
    matches!(
        reason,
        DataplaneProjectionAdmissionFailure::NoAnswer { .. }
            | DataplaneProjectionAdmissionFailure::AwaitingTargetRevision { .. }
            | DataplaneProjectionAdmissionFailure::UnusableProjection {
                failure: ployz_core::dataplane::DataplaneProjectionFailure::FetchFailed { .. }
            }
    )
}

fn validate_admission(
    projection: &DataplaneProjection,
    statuses: &[(MachineId, MachineDataplaneStatus)],
) -> Option<DataplaneProjectionAdmissionEvidence> {
    for member in &projection.target_members {
        let Some((_, status)) = statuses
            .iter()
            .find(|(machine_id, _)| machine_id == &member.machine_id)
        else {
            return Some(no_answer(
                member.machine_id.clone(),
                "machine did not answer the dataplane admission gather".to_owned(),
            ));
        };
        if let Some(reason) = validate_machine(projection, member, status) {
            return Some(DataplaneProjectionAdmissionEvidence {
                machine_id: member.machine_id.clone(),
                reason,
            });
        }
    }
    None
}

fn validate_staged_projection(
    projection: &DataplaneProjection,
    staged: &StagedMachineDataplaneState,
) -> Result<(), &'static str> {
    let Some(projected_joiner) = projection
        .target_members
        .iter()
        .find(|member| member.machine_id == staged.machine_id)
    else {
        return Err("core-stamped projection is missing the staged machine");
    };
    if projection
        .declared_members
        .iter()
        .any(|member| member.machine_id == staged.machine_id)
        || projected_joiner.endpoint_subnet != staged.endpoint_subnet
        || projected_joiner.wireguard_public_key != staged.wireguard_public_key
        || projected_joiner.mesh_endpoints != staged.mesh_endpoints
    {
        return Err("core-stamped projection does not match the staged machine identity");
    }
    Ok(())
}

pub(crate) fn validate_machine(
    projection: &DataplaneProjection,
    member: &DataplaneProjectionMember,
    status: &MachineDataplaneStatus,
) -> Option<DataplaneProjectionAdmissionFailure> {
    validate_machine_scope(projection, member, status, AdmissionScope::Target)
}

pub(crate) fn validate_declared_machine(
    projection: &DataplaneProjection,
    member: &DataplaneProjectionMember,
    status: &MachineDataplaneStatus,
) -> Option<DataplaneProjectionAdmissionFailure> {
    validate_machine_scope(projection, member, status, AdmissionScope::Declared)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdmissionScope {
    Target,
    Declared,
}

impl AdmissionScope {
    fn expected_revision(
        self,
        projection: &DataplaneProjection,
    ) -> &ployz_core::dataplane::DataplaneProjectionRevision {
        match self {
            Self::Target => &projection.target_revision,
            Self::Declared => &projection.declared_revision,
        }
    }

    fn observed_revision(
        self,
        revisions: &ployz_core::dataplane::DataplaneProjectionRevisions,
    ) -> ployz_core::dataplane::DataplaneProjectionRevision {
        match self {
            Self::Target => revisions.target_revision.clone(),
            Self::Declared => revisions.declared_revision.clone(),
        }
    }

    fn required_members(self, projection: &DataplaneProjection) -> &[DataplaneProjectionMember] {
        match self {
            Self::Target => &projection.target_members,
            Self::Declared => &projection.declared_members,
        }
    }
}

fn validate_machine_scope(
    projection: &DataplaneProjection,
    member: &DataplaneProjectionMember,
    status: &MachineDataplaneStatus,
    scope: AdmissionScope,
) -> Option<DataplaneProjectionAdmissionFailure> {
    let expected_revision = scope.expected_revision(projection);
    match &status.projection.testimony {
        DataplaneProjectionTestimony::Applied { revisions }
            if scope.observed_revision(revisions) == *expected_revision => {}
        DataplaneProjectionTestimony::Applied { revisions } => {
            return Some(
                DataplaneProjectionAdmissionFailure::AwaitingTargetRevision {
                    expected: expected_revision.clone(),
                    observed: Some(scope.observed_revision(revisions)),
                },
            );
        }
        DataplaneProjectionTestimony::Unusable {
            attempted_revisions,
            failure,
            ..
        } if attempted_revisions
            .as_ref()
            .is_none_or(|revisions| scope.observed_revision(revisions) != *expected_revision) =>
        {
            return Some(
                DataplaneProjectionAdmissionFailure::AwaitingTargetRevision {
                    expected: expected_revision.clone(),
                    observed: attempted_revisions
                        .as_ref()
                        .map(|revisions| scope.observed_revision(revisions)),
                },
            );
        }
        DataplaneProjectionTestimony::Unusable { failure, .. } => {
            return Some(DataplaneProjectionAdmissionFailure::UnusableProjection {
                failure: failure.clone(),
            });
        }
    }
    if status.projection.endpoint_bridge
        != (EndpointBridgeStatus::Ready {
            subnet: member.endpoint_subnet.clone(),
        })
    {
        return Some(
            DataplaneProjectionAdmissionFailure::EndpointBridgeNotReady {
                status: status.projection.endpoint_bridge.clone(),
            },
        );
    }
    if status.wireguard.interface.is_empty() {
        return Some(DataplaneProjectionAdmissionFailure::WireGuardNotReady {
            failure: ployz_core::machine::WireGuardReadinessFailure::InterfaceMissing,
        });
    }
    if let WireGuardInterfaceMtu::Unavailable { .. } = &status.wireguard.interface_mtu {
        return Some(DataplaneProjectionAdmissionFailure::WireGuardNotReady {
            failure: ployz_core::machine::WireGuardReadinessFailure::InterfaceMtuUnavailable {
                observed: status.wireguard.interface_mtu.clone(),
            },
        });
    }
    if status.ebpf_attachment != EbpfAttachmentStatus::Attached {
        return Some(DataplaneProjectionAdmissionFailure::EbpfNotReady {
            status: status.ebpf_attachment.clone(),
        });
    }

    let required_members = scope.required_members(projection);
    let mut expected = required_members
        .iter()
        .filter(|peer| peer.machine_id != member.machine_id)
        .map(|peer| DataplaneAdmissionPeer {
            public_key: peer.wireguard_public_key.clone(),
            endpoint_subnet: WireGuardPeerEndpointSubnet::Valid {
                subnet: peer.endpoint_subnet.clone(),
            },
        })
        .collect::<Vec<_>>();
    let allowed_keys = projection
        .target_members
        .iter()
        .map(|peer| &peer.wireguard_public_key)
        .collect::<std::collections::BTreeSet<_>>();
    let has_unknown_peer = status
        .wireguard
        .peers
        .iter()
        .any(|peer| !allowed_keys.contains(&peer.public_key));
    let required_keys = required_members
        .iter()
        .map(|peer| &peer.wireguard_public_key)
        .collect::<std::collections::BTreeSet<_>>();
    let mut observed = status
        .wireguard
        .peers
        .iter()
        .filter(|peer| required_keys.contains(&peer.public_key))
        .map(|peer| DataplaneAdmissionPeer {
            public_key: peer.public_key.clone(),
            endpoint_subnet: peer.endpoint_subnet.clone(),
        })
        .collect::<Vec<_>>();
    expected.sort_by(|left, right| left.public_key.cmp(&right.public_key));
    observed.sort_by(|left, right| left.public_key.cmp(&right.public_key));
    if has_unknown_peer || expected != observed {
        return Some(DataplaneProjectionAdmissionFailure::PeerSetMismatch { expected, observed });
    }

    for peer in required_members
        .iter()
        .filter(|peer| peer.machine_id != member.machine_id)
    {
        let observed = status
            .wireguard
            .peers
            .iter()
            .find(|observed| observed.public_key == peer.wireguard_public_key)
            .expect("the exact peer-set check established every expected peer");
        match observed.handshake {
            WireGuardHandshakeStatus::Ago { seconds }
                if seconds <= MAX_HEALTHY_WIREGUARD_HANDSHAKE_AGE_SECONDS => {}
            WireGuardHandshakeStatus::Ago { seconds } => {
                return Some(DataplaneProjectionAdmissionFailure::PeerHandshakeStale {
                    peer_machine_id: peer.machine_id.clone(),
                    observed_age_seconds: seconds,
                });
            }
            WireGuardHandshakeStatus::Never => {
                return Some(DataplaneProjectionAdmissionFailure::PeerHandshakeNever {
                    peer_machine_id: peer.machine_id.clone(),
                });
            }
        }
    }
    None
}

async fn clear_staging(
    handlers: &OperationApiHandlers,
    operation_id: &OperationId,
) -> Result<(), MachineJoinReportError> {
    handlers
        .controllers
        .repository()
        .clear_staged_machine_dataplane(operation_id)
        .await
        .map_err(unavailable)?;
    let _ = publish_invalidation(handlers).await;
    Ok(())
}

async fn publish_invalidation(handlers: &OperationApiHandlers) -> Result<(), String> {
    handlers
        .intent_change_client
        .publish(ployz_core::subjects::INTENT_CHANGED, Vec::new().into())
        .await
        .map_err(|error| error.to_string())?;
    handlers
        .intent_change_client
        .flush()
        .await
        .map_err(|error| error.to_string())
}

fn no_answer(machine_id: MachineId, message: String) -> DataplaneProjectionAdmissionEvidence {
    DataplaneProjectionAdmissionEvidence {
        machine_id,
        reason: DataplaneProjectionAdmissionFailure::NoAnswer {
            message: failure_message(message),
        },
    }
}

fn invalid_staged_projection(
    machine_id: MachineId,
    message: impl AsRef<str>,
) -> DataplaneProjectionAdmissionEvidence {
    DataplaneProjectionAdmissionEvidence {
        machine_id,
        reason: DataplaneProjectionAdmissionFailure::UnusableProjection {
            failure: ployz_core::dataplane::DataplaneProjectionFailure::InvalidView {
                message: failure_message(message),
            },
        },
    }
}

fn failure_message(message: impl AsRef<str>) -> FailureMessage {
    FailureMessage::try_new(message.as_ref()).unwrap_or_else(|_| {
        FailureMessage::try_new("dataplane admission failed")
            .expect("static failure message is valid")
    })
}

fn unavailable(error: impl std::fmt::Display) -> MachineJoinReportError {
    MachineJoinReportError::Unavailable {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::dataplane::{
        DataplaneProjectionRevisions, NativeDataplaneProjectionStatus, WireGuardConfiguredMtu,
        WireGuardDetectedMtu, WireGuardInterfaceMtu, WireGuardMtuProbe, WireGuardPeerStatus,
        WireGuardPublicKey, WireGuardRttStatus, WireGuardStatus,
    };

    #[test]
    fn founder_is_admitted_with_local_readiness_and_no_peers() {
        let projection = projection(&["founder"]);
        let status = ready_status(&projection, "founder", &[]);

        assert_eq!(
            validate_admission(&projection, &[(machine_id("founder"), status)]),
            None
        );
    }

    #[test]
    fn exact_two_machine_projection_is_admitted_without_rtt() {
        let projection = projection(&["core", "edge"]);
        let statuses = vec![
            (
                machine_id("core"),
                ready_status(&projection, "core", &[("edge", handshake(7))]),
            ),
            (
                machine_id("edge"),
                ready_status(&projection, "edge", &[("core", handshake(8))]),
            ),
        ];

        assert_eq!(validate_admission(&projection, &statuses), None);
    }

    #[test]
    fn handshake_at_275_seconds_is_admitted() {
        let projection = projection(&["core", "edge"]);
        let statuses = statuses_with_handshake(&projection, handshake(275));

        assert_eq!(validate_admission(&projection, &statuses), None);
    }

    #[test]
    fn stale_and_never_handshakes_are_rejected() {
        let projection = projection(&["core", "edge"]);
        let stale = validate_admission(
            &projection,
            &statuses_with_handshake(&projection, handshake(276)),
        )
        .expect("stale handshake is rejected");
        assert!(matches!(
            stale.reason,
            DataplaneProjectionAdmissionFailure::PeerHandshakeStale {
                observed_age_seconds: 276,
                ..
            }
        ));

        let never = validate_admission(
            &projection,
            &statuses_with_handshake(&projection, WireGuardHandshakeStatus::Never),
        )
        .expect("never handshake is rejected");
        assert!(matches!(
            never.reason,
            DataplaneProjectionAdmissionFailure::PeerHandshakeNever { .. }
        ));
    }

    #[test]
    fn wrong_target_revision_is_rejected() {
        let target = projection(&["core", "edge"]);
        let other = projection(&["other"]);
        let mut status = ready_status(&target, "core", &[("edge", handshake(1))]);
        let DataplaneProjectionTestimony::Applied { revisions } = &mut status.projection.testimony
        else {
            panic!("ready status is applied");
        };
        revisions.target_revision = other.target_revision;

        let evidence = validate_admission(&target, &[(machine_id("core"), status)])
            .expect("wrong revision is rejected");
        assert!(matches!(
            evidence.reason,
            DataplaneProjectionAdmissionFailure::AwaitingTargetRevision { .. }
        ));
    }

    #[test]
    fn old_local_member_missing_testimony_remains_retryable() {
        let target = projection(&["core", "edge"]);
        let old = projection(&["core"]);
        let mut status = ready_status(&target, "edge", &[("core", handshake(1))]);
        status.projection.testimony = DataplaneProjectionTestimony::Unusable {
            attempted_revisions: Some(DataplaneProjectionRevisions {
                declared_revision: old.declared_revision,
                target_revision: old.target_revision.clone(),
            }),
            last_applied_revisions: None,
            failure: ployz_core::dataplane::DataplaneProjectionFailure::LocalMemberMissing,
        };

        let reason = validate_machine(&target, member(&target, "edge"), &status)
            .expect("old testimony awaits the staged target");
        assert!(matches!(
            &reason,
            DataplaneProjectionAdmissionFailure::AwaitingTargetRevision {
                observed: Some(observed),
                ..
            } if observed == &old.target_revision
        ));
        assert!(retryable_admission(&reason));
    }

    #[test]
    fn core_stamped_projection_must_match_staged_identity() {
        let target = projection(&["core", "edge"]);
        let edge = member(&target, "edge");
        let mut staged = StagedMachineDataplaneState {
            operation_id: OperationId::try_new("op_edge").expect("operation id"),
            machine_id: edge.machine_id.clone(),
            endpoint_subnet: edge.endpoint_subnet.clone(),
            mesh_endpoints: edge.mesh_endpoints.clone(),
            wireguard_public_key: edge.wireguard_public_key.clone(),
        };
        assert_eq!(validate_staged_projection(&target, &staged), Ok(()));

        staged.wireguard_public_key = public_key("wrong");
        assert_eq!(
            validate_staged_projection(&target, &staged),
            Err("core-stamped projection does not match the staged machine identity")
        );

        staged.wireguard_public_key = edge.wireguard_public_key.clone();
        staged.mesh_endpoints = vec!["192.0.2.99:51820".parse().expect("endpoint")];
        assert_eq!(
            validate_staged_projection(&target, &staged),
            Err("core-stamped projection does not match the staged machine identity")
        );
    }

    #[test]
    fn missing_and_extra_peers_are_rejected() {
        let target = projection(&["core", "edge"]);
        let missing = ready_status(&target, "core", &[]);
        assert!(matches!(
            validate_machine(&target, member(&target, "core"), &missing),
            Some(DataplaneProjectionAdmissionFailure::PeerSetMismatch { .. })
        ));

        let mut extra = ready_status(&target, "core", &[("edge", handshake(1))]);
        extra
            .wireguard
            .peers
            .push(peer_status(&projection(&["extra"]), "extra", handshake(1)));
        assert!(matches!(
            validate_machine(&target, member(&target, "core"), &extra),
            Some(DataplaneProjectionAdmissionFailure::PeerSetMismatch { .. })
        ));
    }

    #[test]
    fn declared_scope_ignores_known_staged_peer_but_rejects_unknown_peers() {
        let view = projection(&["core", "other", "edge"]);
        let status = ready_status(
            &view,
            "core",
            &[
                ("other", handshake(1)),
                ("edge", WireGuardHandshakeStatus::Never),
            ],
        );
        assert_eq!(
            validate_declared_machine(&view, member(&view, "core"), &status),
            None
        );

        let mut unknown = status;
        unknown
            .wireguard
            .peers
            .push(peer_status(&projection(&["extra"]), "extra", handshake(1)));
        assert!(matches!(
            validate_declared_machine(&view, member(&view, "core"), &unknown),
            Some(DataplaneProjectionAdmissionFailure::PeerSetMismatch { .. })
        ));
    }

    #[test]
    fn local_component_failure_is_rejected() {
        let projection = projection(&["founder"]);
        let mut status = ready_status(&projection, "founder", &[]);
        status.ebpf_attachment = EbpfAttachmentStatus::Detached {
            message: "not attached".to_owned(),
        };

        assert!(matches!(
            validate_machine(&projection, member(&projection, "founder"), &status),
            Some(DataplaneProjectionAdmissionFailure::EbpfNotReady { .. })
        ));
    }

    #[test]
    fn silent_target_is_rejected() {
        let projection = projection(&["founder"]);

        let evidence = validate_admission(&projection, &[]).expect("silence is rejected");
        assert!(matches!(
            evidence.reason,
            DataplaneProjectionAdmissionFailure::NoAnswer { .. }
        ));
    }

    fn statuses_with_handshake(
        projection: &DataplaneProjection,
        observed: WireGuardHandshakeStatus,
    ) -> Vec<(MachineId, MachineDataplaneStatus)> {
        vec![
            (
                machine_id("core"),
                ready_status(projection, "core", &[("edge", observed)]),
            ),
            (
                machine_id("edge"),
                ready_status(projection, "edge", &[("core", observed)]),
            ),
        ]
    }

    fn projection(machines: &[&str]) -> DataplaneProjection {
        let staged = machines.last().map(|machine| projection_member(machine));
        let declared = machines
            .iter()
            .take(machines.len().saturating_sub(1))
            .map(|machine| projection_member(machine))
            .collect();
        DataplaneProjection::try_new(declared, staged).expect("valid projection")
    }

    fn projection_member(machine: &str) -> DataplaneProjectionMember {
        let octet = match machine {
            "core" | "founder" => 1,
            "edge" => 2,
            "other" => 3,
            "extra" => 4,
            _ => 5,
        };
        DataplaneProjectionMember {
            machine_id: machine_id(machine),
            endpoint_subnet: ployz_core::dataplane::MachineEndpointSubnet::try_new(format!(
                "10.198.{octet}.0/24"
            ))
            .expect("valid subnet"),
            mesh_endpoints: vec![
                format!("192.0.2.{octet}:51820")
                    .parse()
                    .expect("valid endpoint"),
            ],
            wireguard_public_key: public_key(machine),
        }
    }

    fn ready_status(
        projection: &DataplaneProjection,
        local: &str,
        peers: &[(&str, WireGuardHandshakeStatus)],
    ) -> MachineDataplaneStatus {
        let local = member(projection, local);
        MachineDataplaneStatus {
            projection: NativeDataplaneProjectionStatus {
                endpoint_bridge: EndpointBridgeStatus::Ready {
                    subnet: local.endpoint_subnet.clone(),
                },
                testimony: DataplaneProjectionTestimony::Applied {
                    revisions: DataplaneProjectionRevisions {
                        declared_revision: projection.declared_revision.clone(),
                        target_revision: projection.target_revision.clone(),
                    },
                },
            },
            wireguard: WireGuardStatus {
                interface: "ployz-wg0".to_owned(),
                configured_mtu: WireGuardConfiguredMtu::Auto,
                detected_mtu: WireGuardDetectedMtu::Detected { mtu: 1420 },
                interface_mtu: WireGuardInterfaceMtu::Detected { mtu: 1420 },
                peers: peers
                    .iter()
                    .map(|(peer, handshake)| peer_status(projection, peer, *handshake))
                    .collect(),
            },
            ebpf_attachment: EbpfAttachmentStatus::Attached,
        }
    }

    fn peer_status(
        projection: &DataplaneProjection,
        peer: &str,
        handshake: WireGuardHandshakeStatus,
    ) -> WireGuardPeerStatus {
        let peer = member(projection, peer);
        WireGuardPeerStatus {
            public_key: peer.wireguard_public_key.clone(),
            endpoint_subnet: WireGuardPeerEndpointSubnet::Valid {
                subnet: peer.endpoint_subnet.clone(),
            },
            endpoint: peer.mesh_endpoints.first().copied(),
            handshake,
            rtt: WireGuardRttStatus::Unavailable {
                message: "not measured".to_owned(),
            },
            rx_bytes: 0,
            tx_bytes: 0,
            mtu_probe: WireGuardMtuProbe::NotRequested,
        }
    }

    fn member<'a>(
        projection: &'a DataplaneProjection,
        machine: &str,
    ) -> &'a DataplaneProjectionMember {
        projection
            .target_members
            .iter()
            .find(|member| member.machine_id == machine_id(machine))
            .expect("projection member exists")
    }

    const fn handshake(seconds: u64) -> WireGuardHandshakeStatus {
        WireGuardHandshakeStatus::Ago { seconds }
    }

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("valid machine id")
    }

    fn public_key(value: &str) -> WireGuardPublicKey {
        WireGuardPublicKey::try_new(format!("public-key-{value}"))
            .expect("valid WireGuard public key")
    }
}
