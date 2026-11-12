//! Bounded enrollment after the caller has durably saved its allocation.
pub mod local;

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
/// identity it saved. The saved assignment never holds a claim, so the check compares without it.
fn checked_registration(
    assignment: &EnrollmentAssignment,
    snapshot: &ployz_core::EnrollmentSnapshot,
) -> Result<RegisterRequest, RpcError> {
    let mut saved = assignment.clone();
    saved.request.capabilities = Vec::new();
    ployz_core::allocate_enrollment(&saved.request, snapshot, std::slice::from_ref(&saved))
        .map_err(|error| RpcError::conflict(error.to_string()))?;
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
