//! Bounded enrollment after the caller has durably saved its allocation.
pub mod local;

use crate::cluster::{Call, Deadline};
use crate::connect::{Client, ConnectError};
use ployz_core::{
    EnrollmentAssignment, EnrollmentSnapshot, ListMachinesRequest, RegisterRequest, Registered,
    RpcError, op,
};

/// Observe the Entry Machine's enrollment facts before taking an operator lock.
///
/// # Errors
/// Returns transport errors or missing enrollment facts.
pub async fn observe_enrollment(entry: &mut Client) -> Result<EnrollmentSnapshot, RpcError> {
    entry
        .call::<op::ListMachines>(ListMachinesRequest {}, Call::read())
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
                let mut error = RpcError::from(error);
                error.message = format!(
                    "Register response lost; outcome may be uncertain: {}",
                    error.message
                );
                error
            }
        })
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
}
