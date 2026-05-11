use std::collections::HashSet;
use std::time::Duration;

use ipnet::Ipv4Net;
use ployz_orchestrator::coordination::{ClaimError, SubnetClaim};
use ployz_orchestrator::ipam::pick_candidate_subnet;
use ployz_store_api::{InviteStore, MachineMembershipStore, StoreDriver};
use ployz_types::model::MachineId;
use ployz_types::time::now_unix_secs;

use crate::daemon::DaemonState;

use super::super::types::MachineAddContext;

const SUBNET_RESERVATION_TTL: Duration = Duration::from_secs(30);
const MAX_SUBNET_ATTEMPTS: usize = 64;

pub(in super::super) struct BootstrapSubnetClaim {
    claim: SubnetClaim,
    pub(super) subnet: Ipv4Net,
}

impl BootstrapSubnetClaim {
    #[must_use]
    pub(in crate::daemon::handlers::machine) fn subnet(&self) -> Ipv4Net {
        self.subnet
    }
}

impl std::fmt::Debug for BootstrapSubnetClaim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BootstrapSubnetClaim")
            .field("subnet", &self.subnet)
            .finish_non_exhaustive()
    }
}

impl DaemonState {
    pub(in crate::daemon::handlers::machine) async fn reserve_machine_subnet(
        &self,
        owner: &MachineId,
    ) -> Result<BootstrapSubnetClaim, String> {
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "no running network".to_string())?;
        let cluster: Ipv4Net = self
            .cluster_cidr
            .parse()
            .map_err(|err| format!("invalid cluster CIDR '{}': {err}", self.cluster_cidr))?;
        let machines = active
            .mesh
            .store
            .list_machines()
            .await
            .map_err(|err| format!("list machines for subnet reservation: {err}"))?;
        let bias_seed = machine_bias_seed(owner);

        let mut taken: HashSet<Ipv4Net> = machines
            .iter()
            .filter_map(|machine| machine.subnet)
            .collect();

        for _ in 0..MAX_SUBNET_ATTEMPTS {
            let Some(candidate) =
                pick_candidate_subnet(cluster, self.subnet_prefix_len, &taken, bias_seed)
            else {
                return Err("no available subnets".into());
            };

            match self
                .subnet_coord
                .try_claim(candidate, owner, SUBNET_RESERVATION_TTL)
                .await
            {
                Ok(claim) => {
                    return Ok(BootstrapSubnetClaim {
                        claim,
                        subnet: candidate,
                    });
                }
                Err(ClaimError::AlreadyHeld) => {
                    taken.insert(candidate);
                    continue;
                }
                Err(ClaimError::Backend(message)) => {
                    return Err(format!("subnet lock backend: {message}"));
                }
            }
        }

        Err("no available subnets".into())
    }
}

pub(in crate::daemon::handlers::machine) async fn release_reserved_subnet(
    subnet_claim: BootstrapSubnetClaim,
) -> Result<(), String> {
    let subnet = subnet_claim.subnet;
    if let Err(err) = subnet_claim.claim.release().await {
        tracing::warn!(
            subnet = %subnet,
            error = %err,
            "subnet reservation release failed; lease will expire by TTL"
        );
    }
    Ok(())
}

pub(super) async fn consume_invite(
    context: &MachineAddContext,
    invite_id: &str,
    machine_id: &MachineId,
) -> Result<(), String> {
    context
        .store
        .redeem_invite(invite_id, machine_id, now_unix_secs())
        .await
        .map(|_| ())
        .map_err(|err| format!("consume invite: {err}"))
}

fn machine_bias_seed(machine_id: &MachineId) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    machine_id.hash(&mut hasher);
    hasher.finish()
}

pub(super) async fn assert_subnet_unique(
    store: &StoreDriver,
    machine_id: &MachineId,
    claimed_subnet: Ipv4Net,
) -> Result<(), String> {
    let machines = store
        .list_machines()
        .await
        .map_err(|err| format!("list machines for subnet assertion: {err}"))?;
    let conflicting_machine_ids = machines
        .into_iter()
        .filter(|machine| machine.id != *machine_id && machine.subnet == Some(claimed_subnet))
        .map(|machine| machine.id.into_string())
        .collect::<Vec<_>>();
    if conflicting_machine_ids.is_empty() {
        return Ok(());
    }

    Err(format!(
        "subnet uniqueness invariant violated for machine '{}' subnet '{}'; conflicting machines: {}",
        machine_id,
        claimed_subnet,
        conflicting_machine_ids.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::assert_subnet_unique;
    use ipnet::Ipv4Net;
    use ployz_store_api::{MachineMembershipStore, StoreDriver};
    use ployz_types::model::{
        MachineId, MachineLifecycle, MachineMembership, OverlayIp, PublicKey,
    };

    #[tokio::test]
    async fn subnet_assertion_rejects_duplicate_claims() {
        let store = StoreDriver::memory();
        let subnet: Ipv4Net = "10.210.1.0/24".parse().expect("valid subnet");
        store
            .upsert_self_machine(&machine_record("alpha", "::1", Some(subnet)))
            .await
            .expect("upsert alpha");
        store
            .upsert_self_machine(&machine_record("beta", "::2", Some(subnet)))
            .await
            .expect("upsert beta");

        let err = assert_subnet_unique(&store, &MachineId::new("alpha"), subnet)
            .await
            .expect_err("duplicate subnet should fail");

        assert!(err.contains("beta"));
    }

    #[tokio::test]
    async fn subnet_assertion_accepts_unique_claim() {
        let store = StoreDriver::memory();
        let claimed_subnet: Ipv4Net = "10.210.1.0/24".parse().expect("valid subnet");
        let other_subnet: Ipv4Net = "10.210.2.0/24".parse().expect("valid subnet");
        store
            .upsert_self_machine(&machine_record("alpha", "::1", Some(claimed_subnet)))
            .await
            .expect("upsert alpha");
        store
            .upsert_self_machine(&machine_record("beta", "::2", Some(other_subnet)))
            .await
            .expect("upsert beta");

        let result = assert_subnet_unique(&store, &MachineId::new("alpha"), claimed_subnet).await;

        assert!(result.is_ok());
    }

    fn machine_record(
        machine_id: &str,
        overlay_ip: &str,
        subnet: Option<Ipv4Net>,
    ) -> MachineMembership {
        let record = MachineMembership::seed(
            MachineId::new(machine_id),
            PublicKey([1; 32]),
            overlay_ip.parse().map(OverlayIp).expect("valid overlay"),
            subnet,
            vec![],
        );
        MachineMembership {
            lifecycle: MachineLifecycle::Active,
            ..record
        }
    }
}
