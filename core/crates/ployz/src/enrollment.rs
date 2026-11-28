//! Enrolling a Server: what `server add` asks for, the calls to its daemon, and its Cloud
//! enrollment. Bounded enrollment after the caller has durably saved its allocation sits here
//! as well.
pub(crate) mod asked_policy;
pub(crate) mod catch_up;
pub(crate) mod cloud;
pub(crate) mod founding;
pub mod local;
pub(crate) mod machine;
pub(crate) mod output;
pub(crate) mod plan;
pub(crate) mod recovery;

pub(crate) use asked_policy::AskedPolicy;
pub(crate) use plan::EnrollPlan;

use crate::cluster::{Call, Deadline};
use crate::connect::{Client, ConnectError};
use ployz_core::{
    EnrollmentAssignment, EnrollmentSnapshot, MachineId, RegisterRequest, Registered, RpcError, op,
};

/// Observe the Entry Machine's enrollment facts before taking an operator lock.
///
/// # Errors
/// Returns transport errors or missing enrollment facts.
pub async fn observe_enrollment(entry: &mut Client) -> Result<EnrollmentSnapshot, RpcError> {
    entry
        .machine_list_uncapped()
        .await?
        .enrollment
        .ok_or_else(|| RpcError::unavailable("Entry Machine returned no enrollment snapshot"))
}

/// What a CLI that runs a joiner's catch-up claims in its Register: it follows the
/// Cluster's declared Doors, and the joiner opens the floors sealed beside the Sealing Key
/// when `joiner`, its daemon's own runtime, says so. A Machine sealing floors to an older
/// daemon would break its join.
#[must_use]
pub fn claims(joiner: &ployz_core::MachineRuntime) -> Vec<&'static str> {
    let mut claimed = vec![ployz_core::SET_CLUSTER_DOORS_CAPABILITY];
    for capability in [
        ployz_core::FORGET_VERSIONS_CAPABILITY,
        ployz_core::JOIN_TRUST_CAPABILITY,
    ] {
        if joiner.claim(capability) == ployz_core::Claim::Advertises {
            claimed.push(capability);
        }
    }
    claimed
}

/// Hand a joiner that claimed `join-trust.v1` the restrictions its admitting Machine
/// answered the confirmation with, so it may participate. It refuses to without them, so a
/// reply that lacks them fails here, naming the upgrade it needs.
///
/// The joiner's daemon restarts right after it enrolls, which closes the connection that
/// enrolled it, so each attempt dials afresh with `dial` until one lands or the start wait
/// ends. Taking the same restrictions twice changes nothing on the joiner.
///
/// # Errors
/// Returns a missing set as `unsupported`, and the joiner's own refusal, or the last
/// transport error once the start wait ends.
pub(crate) async fn confirm_join(
    mut dial: impl AsyncFnMut() -> Result<Client, ConnectError>,
    claimed: bool,
    registration: &Registered,
) -> Result<(), crate::setup_retry::Error<ConnectError>> {
    if !claimed {
        return Ok(());
    }
    crate::setup_retry::run_expecting(
        &mut (),
        &crate::ui::WaitNotices,
        "Confirming the join",
        Some(crate::setup_retry::STARTING),
        ployz_core::MACHINE_START_WAIT,
        ConnectError::is_start_retryable,
        async |()| confirm_once(&mut dial().await?, registration).await,
    )
    .await
}

async fn confirm_once(joiner: &mut Client, registration: &Registered) -> Result<(), ConnectError> {
    let Some(restrictions) = registration.restrictions.clone() else {
        // A joiner that already participates took its restrictions on an earlier run.
        let details = joiner
            .call_shared::<op::Inspect>(ployz_core::InspectRequest::default(), Call::once())
            .await?;
        if details.phase == ployz_core::LocalMachinePhase::Participating {
            return Ok(());
        }
        return Err(ConnectError::Remote(
            RpcError::unsupported(format!(
                "The admitting Server didn't answer with the restrictions this Server needs before it serves ({}). Upgrade the admitting Server, then run this again.",
                ployz_core::JOIN_TRUST_CAPABILITY
            ))
            .with_details(serde_json::json!({ "capability": ployz_core::JOIN_TRUST_CAPABILITY })),
        ));
    };
    joiner
        .call_shared::<op::ConfirmJoin>(
            ployz_core::ConfirmJoinRequest { restrictions },
            Call::once(),
        )
        .await?;
    Ok(())
}

/// The Register request for a saved assignment: what the joiner asked, with the subnet
/// and runtime the Cluster assigned. The capabilities are the joiner's own claim, which
/// the caller that runs its catch-up sets and Cloud relays; nothing here adds one.
fn registration(assignment: &EnrollmentAssignment) -> RegisterRequest {
    let mut request = assignment.request.clone();
    request.assigned_subnet = Some(assignment.machine.subnet);
    request.runtime.clone_from(&assignment.machine.runtime);
    request
}

/// The Register request for an assignment, once the Cluster's snapshot still agrees with the
/// identity it saved. The saved assignment never holds a claim or a First Key hint, so the
/// check compares without them.
fn checked_registration(
    assignment: &EnrollmentAssignment,
    snapshot: &ployz_core::EnrollmentSnapshot,
) -> Result<RegisterRequest, RpcError> {
    let mut saved = assignment.clone();
    saved.request.capabilities = Vec::new();
    saved.request.first_key = None;
    ployz_core::allocate_enrollment(&saved.request, snapshot, std::slice::from_ref(&saved))
        .map_err(|error| {
            let conflict = RpcError::conflict(error.to_string());
            if error == ployz_core::EnrollmentError::Conflict {
                // Cloud tells the person to remove the Server the Cluster lists, by this reason.
                conflict.with_details(serde_json::json!({ "reason": "identity_conflict" }))
            } else {
                conflict
            }
        })?;
    Ok(registration(assignment))
}

/// Publish a durably saved assignment before bootstrapping the joining Machine.
///
/// # Errors
/// Returns identity conflicts and Entry Machine publication/transport errors.
pub async fn publish_enrollment(
    entry: &mut Client,
    assignment: &EnrollmentAssignment,
) -> Result<Registered, RpcError> {
    let snapshot = observe_enrollment(entry).await?;
    let request = checked_registration(assignment, &snapshot)?;
    let entry_machine = entry.entry_machine();
    entry
        .call_shared::<op::Register>(
            request,
            Call::once().within(Deadline::MutationReply(
                crate::cluster::LIVE_MUTATION_REPLY_TIMEOUT,
            )),
        )
        .await
        .map_err(|error| {
            if let ConnectError::Remote(error) = error {
                error
            } else {
                let mut error = lost_reply(entry_machine, error);
                error.message = format!(
                    "Register response lost; outcome may be uncertain: {}",
                    error.message
                );
                error
            }
        })
}

/// `error` from a call sent once to the entry Machine, marked when it went out and no
/// answer came back, so the entry may have done the work. A call that never went out, or
/// that the entry answered, is not.
pub(crate) fn lost_reply(entry: Option<MachineId>, error: ConnectError) -> RpcError {
    let lost = error.unreplied();
    let error = RpcError::from(error);
    if lost {
        error.mark(&ployz_core::ReplyLost { machine_id: entry })
    } else {
        error
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::{
        EnrollmentAssignment, Machine, MachineId, MachineName, RegisterRequest,
        SET_CLUSTER_DOORS_CAPABILITY, WireGuardPublicKey,
    };

    fn assignment(capabilities: Vec<String>) -> EnrollmentAssignment {
        let id = MachineId::parse("a".repeat(32)).unwrap();
        let name = MachineName::parse("machine-2").unwrap();
        let public_key = WireGuardPublicKey([7; 32]);
        EnrollmentAssignment {
            request: RegisterRequest {
                join_approval: None,
                join_confirmation: None,
                machine_id: id,
                assigned_subnet: None,
                initial_policy: Default::default(),
                name: name.clone(),
                storage: ployz_core::StorageChoice::None,
                public_key,
                public_ip: None,
                advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
                    "192.0.2.2:51820".parse().unwrap(),
                )],
                runtime: Default::default(),
                capabilities,
                first_key: None,
            },
            network: "10.210.0.0/16".parse().unwrap(),
            machine: Machine {
                labels: Default::default(),
                accepts_builds: true,
                accepts_services: true,
                accepts_ingress: true,
                id,
                name,
                subnet: "10.210.1.0/24".parse().unwrap(),
                public_key,
                public_ip: None,
                advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
                    "192.0.2.2:51820".parse().unwrap(),
                )],
                runtime: Default::default(),
                join_approval: None,
                build_concurrency: None,
            },
        }
    }

    #[test]
    fn a_register_carries_the_joiners_own_capability_claim_and_never_adds_one() {
        let claimed =
            super::registration(&assignment(vec![SET_CLUSTER_DOORS_CAPABILITY.to_owned()]));
        assert_eq!(claimed.capabilities, [SET_CLUSTER_DOORS_CAPABILITY]);
        let silent = super::registration(&assignment(Vec::new()));
        assert!(silent.capabilities.is_empty(), "{silent:?}");
    }

    #[test]
    fn the_claim_survives_allocation_and_the_identity_check() {
        let snapshot = ployz_core::EnrollmentSnapshot {
            network: "10.210.0.0/16".parse().unwrap(),
            machines: Vec::new(),
            target_versions: Default::default(),
            doors: None,
        };
        let request = assignment(Vec::new()).request;
        let mut saved = ployz_core::allocate_enrollment(&request, &snapshot, &[]).unwrap();
        assert!(
            saved.request.capabilities.is_empty(),
            "allocation saves no claim"
        );
        saved.request.capabilities = vec![SET_CLUSTER_DOORS_CAPABILITY.to_owned()];
        let registered = super::checked_registration(&saved, &snapshot).unwrap();
        assert_eq!(registered.capabilities, [SET_CLUSTER_DOORS_CAPABILITY]);
        assert_eq!(registered.assigned_subnet, Some(saved.machine.subnet));
    }

    /// A `server add` stopped after the joiner enrolled leaves its row published with the
    /// approval it joined under. Running the add again must reach the admitting Server, which
    /// replays the join; only another identity under the same Machine ID conflicts.
    #[test]
    fn an_interrupted_add_resumes_once_its_row_is_published_with_its_approval() {
        use ployz_core::authority::{JoinApproval, Signed, Signer, Timestamp};
        let empty = ployz_core::EnrollmentSnapshot {
            network: "10.210.0.0/16".parse().unwrap(),
            machines: Vec::new(),
            target_versions: Default::default(),
            doors: None,
        };
        let request = assignment(Vec::new()).request;
        let saved = ployz_core::allocate_enrollment(&request, &empty, &[]).unwrap();
        let signer = Signer::from_seed(&[1; 32]);
        let issued_at = Timestamp::from_millis(1_700_000_000_000).unwrap();
        let mut published = saved.machine.clone();
        published.join_approval = Some(
            Signed::sign(
                &JoinApproval {
                    cluster: ployz_core::SealingPublicKey::from_bytes([5; 32]),
                    machine: (&saved.machine).into(),
                    network: saved.network,
                    sealing_recipient: saved.machine.public_key,
                    devices: vec![signer.public().into()],
                    passkeys: Vec::new(),
                    grants: Vec::new(),
                    admitting_machines: vec![MachineId::parse("b".repeat(32)).unwrap()],
                    bootstrap: Vec::new(),
                    issued_at,
                    expires_at: issued_at.plus(60_000),
                    signer: signer.public().into(),
                    proposal: None,
                },
                &signer,
            )
            .unwrap(),
        );
        let joined = ployz_core::EnrollmentSnapshot {
            machines: vec![published],
            ..empty
        };

        let resumed = super::checked_registration(&saved, &joined).unwrap();
        assert_eq!(resumed.assigned_subnet, Some(saved.machine.subnet));

        let mut impostor = saved;
        impostor.request.public_key = WireGuardPublicKey([8; 32]);
        impostor.machine.public_key = WireGuardPublicKey([8; 32]);
        let refused = super::checked_registration(&impostor, &joined).unwrap_err();
        assert_eq!(refused.code, ployz_core::RpcErrorCode::from("conflict"));
        assert_eq!(
            refused.details,
            serde_json::json!({ "reason": "identity_conflict" })
        );
    }

    /// A Server added through Cloud registers with no runtime, so only this claim tells
    /// the admitting Machine to seal floors to it, and only a daemon that opens them may
    /// claim it.
    #[test]
    fn a_joiner_claims_the_floors_only_when_its_daemon_opens_them() {
        let mut older = ployz_core::MachineRuntime::default();
        older
            .capabilities
            .insert(SET_CLUSTER_DOORS_CAPABILITY.to_owned());
        assert_eq!(super::claims(&older), [SET_CLUSTER_DOORS_CAPABILITY]);
        let mut current = older;
        current
            .capabilities
            .insert(ployz_core::FORGET_VERSIONS_CAPABILITY.to_owned());
        assert_eq!(
            super::claims(&current),
            [
                SET_CLUSTER_DOORS_CAPABILITY,
                ployz_core::FORGET_VERSIONS_CAPABILITY
            ]
        );
    }

    fn dropped() -> crate::connect::ConnectError {
        tonic::Status::from_error(Box::new(std::io::Error::new(
            std::io::ErrorKind::NotConnected,
            "connection lost",
        )))
        .into()
    }

    #[test]
    fn a_reply_that_never_came_back_is_marked_with_the_entry_that_may_have_acted() {
        let entry = MachineId::parse("a".repeat(32)).unwrap();
        let named = super::lost_reply(Some(entry), dropped());
        assert!(named.reply_lost(), "{named:?}");
        assert_eq!(
            named.details,
            serde_json::json!({"machine_id": entry.to_string(), "reply_lost": true})
        );
        let unnamed = super::lost_reply(None, dropped());
        assert!(unnamed.reply_lost(), "{unnamed:?}");
        assert_eq!(unnamed.details, serde_json::json!({"reply_lost": true}));
    }

    #[test]
    fn an_answer_or_a_call_that_never_went_out_is_not_marked_lost() {
        let entry = MachineId::parse("a".repeat(32)).unwrap();
        let refusal = ployz_core::RpcError::unavailable("Cluster is not available");
        let answered = super::lost_reply(
            Some(entry),
            crate::connect::ConnectError::Remote(refusal.clone()),
        );
        assert_eq!(answered, refusal);
        let unsent = super::lost_reply(Some(entry), crate::connect::ConnectError::ClientRefused);
        assert!(!unsent.reply_lost(), "{unsent:?}");
    }
}
