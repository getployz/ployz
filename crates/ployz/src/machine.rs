//! Machine membership product ports.

use std::net::IpAddr;

use crate::error::MachineFailure;
use crate::operation::MutationContext;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MachineId(String);

impl MachineId {
    pub fn parse(value: impl Into<String>) -> Result<Self, MachineFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MachineEpoch(u64);

impl MachineEpoch {
    pub fn new(value: u64) -> Result<Self, MachineFailure> {
        if value == 0 {
            return Err(MachineFailure::InvalidPayload);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OverlayIp(IpAddr);

impl OverlayIp {
    pub fn parse(value: impl Into<String>) -> Result<Self, MachineFailure> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(MachineFailure::InvalidPayload);
        }
        let address = value
            .parse::<IpAddr>()
            .map_err(|_| MachineFailure::InvalidPayload)?;
        Ok(Self(address))
    }

    #[must_use]
    pub fn address(self) -> IpAddr {
        self.0
    }

    #[must_use]
    pub fn canonical(self) -> String {
        self.0.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IrohEndpointId(String);

impl IrohEndpointId {
    pub fn parse(value: impl Into<String>) -> Result<Self, MachineFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WireGuardPublicKey(String);

impl WireGuardPublicKey {
    pub fn parse(value: impl Into<String>) -> Result<Self, MachineFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineNetworkIdentity {
    pub overlay_ip: OverlayIp,
    pub iroh_endpoint_id: IrohEndpointId,
    pub wireguard_public_key: WireGuardPublicKey,
}

impl MachineNetworkIdentity {
    #[must_use]
    pub fn new(
        overlay_ip: OverlayIp,
        iroh_endpoint_id: IrohEndpointId,
        wireguard_public_key: WireGuardPublicKey,
    ) -> Self {
        Self {
            overlay_ip,
            iroh_endpoint_id,
            wireguard_public_key,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineMembership {
    pub machine: MachineId,
    pub epoch: MachineEpoch,
    pub network: MachineNetworkIdentity,
}

impl MachineMembership {
    #[must_use]
    pub fn new(machine: MachineId, epoch: MachineEpoch, network: MachineNetworkIdentity) -> Self {
        Self {
            machine,
            epoch,
            network,
        }
    }

    #[must_use]
    fn same_machine_identity(&self, other: &Self) -> bool {
        self.machine == other.machine && self.network == other.network
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineTombstone {
    pub machine: MachineId,
    pub epoch: MachineEpoch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineRemoval {
    pub machine: MachineId,
    pub epoch: MachineEpoch,
    pub reason: MachineRemovalReason,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MachineRemovalReason(String);

impl MachineRemovalReason {
    pub fn parse(value: impl Into<String>) -> Result<Self, MachineFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineStatus {
    Absent,
    Joined(MachineMembership),
    Removing(MachineRemoval),
    Tombstoned(MachineTombstone),
    Conflicted {
        machine: MachineId,
        epoch: MachineEpoch,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineAddRequest {
    pub machine: MachineId,
    pub epoch: MachineEpoch,
    pub network: MachineNetworkIdentity,
}

impl MachineAddRequest {
    #[must_use]
    fn desired_membership(&self) -> MachineMembership {
        MachineMembership::new(self.machine.clone(), self.epoch, self.network.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineAddOutcome {
    Joined(MachineMembership),
    AlreadyPresent(MachineMembership),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MachineAddPlan {
    Join(MachineMembership),
    AlreadyPresent(MachineMembership),
}

impl MachineAddPlan {
    fn diff(observed: MachineStatus, desired: MachineMembership) -> Result<Self, MachineFailure> {
        match observed {
            MachineStatus::Absent => Ok(Self::Join(desired)),
            MachineStatus::Joined(current) if current.same_machine_identity(&desired) => {
                Ok(Self::AlreadyPresent(current))
            }
            MachineStatus::Joined(current) => Err(MachineFailure::MembershipConflict {
                machine: current.machine,
                epoch: Some(current.epoch),
            }),
            MachineStatus::Removing(removal) => Err(MachineFailure::MachineRemoving {
                machine: removal.machine,
                epoch: removal.epoch,
            }),
            MachineStatus::Tombstoned(tombstone) => Err(MachineFailure::MachineTombstoned {
                machine: tombstone.machine,
                epoch: tombstone.epoch,
            }),
            MachineStatus::Conflicted { machine, epoch } => {
                Err(MachineFailure::MembershipConflict {
                    machine,
                    epoch: Some(epoch),
                })
            }
        }
    }
}

pub trait MachineMembershipPort {
    fn observe(
        &self,
        context: &MutationContext,
        machine: &MachineId,
    ) -> Result<MachineStatus, MachineFailure>;

    fn join(
        &self,
        context: &MutationContext,
        membership: &MachineMembership,
    ) -> Result<MachineMembership, MachineFailure>;
}

pub struct MachineMembershipService<M> {
    membership: M,
}

impl<M> MachineMembershipService<M> {
    #[must_use]
    pub fn new(membership: M) -> Self {
        Self { membership }
    }
}

impl<M> MachineMembershipService<M>
where
    M: MachineMembershipPort,
{
    pub fn add_machine(
        &self,
        context: &MutationContext,
        request: MachineAddRequest,
    ) -> Result<MachineAddOutcome, MachineFailure> {
        let observed = self.membership.observe(context, &request.machine)?;
        let desired = request.desired_membership();
        match MachineAddPlan::diff(observed, desired)? {
            MachineAddPlan::AlreadyPresent(current) => {
                Ok(MachineAddOutcome::AlreadyPresent(current))
            }
            MachineAddPlan::Join(desired) => {
                let joined = self.membership.join(context, &desired)?;
                if !joined.same_machine_identity(&desired) {
                    return Err(MachineFailure::MutationMismatch {
                        machine: desired.machine,
                    });
                }
                Ok(MachineAddOutcome::Joined(joined))
            }
        }
    }
}

fn parse_non_empty<T>(
    value: impl Into<String>,
    build: impl FnOnce(String) -> T,
) -> Result<T, MachineFailure> {
    let value = value.into();
    if value.trim().is_empty() {
        return Err(MachineFailure::InvalidPayload);
    }
    Ok(build(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_machine_id_is_invalid_payload() {
        assert_eq!(MachineId::parse(""), Err(MachineFailure::InvalidPayload));
    }

    #[test]
    fn zero_epoch_is_invalid_payload() {
        assert_eq!(MachineEpoch::new(0), Err(MachineFailure::InvalidPayload));
    }

    #[test]
    fn overlay_ip_must_be_an_ip_address() {
        assert_eq!(
            OverlayIp::parse("not-an-ip"),
            Err(MachineFailure::InvalidPayload)
        );
    }

    #[test]
    fn overlay_ip_identity_is_canonical() {
        assert_eq!(
            OverlayIp::parse("fd00:0:0:0:0:0:0:1").expect("long ip"),
            OverlayIp::parse("fd00::1").expect("canonical ip")
        );
        assert_eq!(
            OverlayIp::parse("fd00:0:0:0:0:0:0:1")
                .expect("long ip")
                .canonical(),
            "fd00::1"
        );
    }

    #[test]
    fn absent_machine_plans_join() {
        let desired = membership("node-a", 1, "fd00::1");
        let plan = MachineAddPlan::diff(MachineStatus::Absent, desired.clone()).expect("plan");

        assert!(matches!(plan, MachineAddPlan::Join(join) if join == desired));
    }

    #[test]
    fn same_identity_is_already_present_even_with_newer_observed_epoch() {
        let desired = membership("node-a", 1, "fd00::1");
        let current = membership("node-a", 2, "fd00::1");
        let plan =
            MachineAddPlan::diff(MachineStatus::Joined(current.clone()), desired).expect("plan");

        assert!(matches!(plan, MachineAddPlan::AlreadyPresent(present) if present == current));
    }

    #[test]
    fn different_identity_is_conflict() {
        let desired = membership("node-a", 1, "fd00::1");
        let current = membership("node-a", 1, "fd00::2");
        let error =
            MachineAddPlan::diff(MachineStatus::Joined(current), desired).expect_err("conflict");

        assert_eq!(
            error,
            MachineFailure::MembershipConflict {
                machine: MachineId::parse("node-a").expect("machine"),
                epoch: Some(MachineEpoch::new(1).expect("epoch")),
            }
        );
    }

    #[test]
    fn removing_machine_is_not_already_present() {
        let desired = membership("node-a", 1, "fd00::1");
        let removal = MachineRemoval {
            machine: MachineId::parse("node-a").expect("machine"),
            epoch: MachineEpoch::new(3).expect("epoch"),
            reason: MachineRemovalReason::parse("operator requested").expect("reason"),
        };
        let error =
            MachineAddPlan::diff(MachineStatus::Removing(removal), desired).expect_err("removing");

        assert_eq!(
            error,
            MachineFailure::MachineRemoving {
                machine: MachineId::parse("node-a").expect("machine"),
                epoch: MachineEpoch::new(3).expect("epoch"),
            }
        );
    }

    #[test]
    fn fact_backed_first_add_projects_joined_membership() {
        let fact_backed_membership = crate::composition::in_memory_machine_membership();
        let service = MachineMembershipService::new(fact_backed_membership);

        let outcome = service
            .add_machine(&context(), request("node-a", 1, "fd00::1"))
            .expect("machine add");

        assert!(
            matches!(outcome, MachineAddOutcome::Joined(joined) if joined == membership("node-a", 1, "fd00::1"))
        );
    }

    #[test]
    fn fact_backed_already_present_reads_projection_without_fresh_join() {
        let fact_backed_membership = crate::composition::in_memory_machine_membership();
        let service = MachineMembershipService::new(fact_backed_membership);
        service
            .add_machine(&context(), request("node-a", 1, "fd00::1"))
            .expect("first add");

        let outcome = service
            .add_machine(&context(), request("node-a", 2, "fd00::1"))
            .expect("second add");

        assert!(
            matches!(outcome, MachineAddOutcome::AlreadyPresent(joined) if joined == membership("node-a", 1, "fd00::1"))
        );
    }

    fn context() -> MutationContext {
        use std::time::{Duration, UNIX_EPOCH};

        use crate::operation::{
            AuthorityContext, AuthorityEpoch, IdempotencyKey, MutationContext, OperationId,
            PrincipalId, ScopeId,
        };

        MutationContext::new(
            OperationId::parse("machine-add-1").expect("operation"),
            IdempotencyKey::parse("machine-add-1").expect("idempotency"),
            AuthorityContext::new(
                PrincipalId::parse("node-a").expect("principal"),
                ScopeId::parse("cluster").expect("scope"),
                AuthorityEpoch::new(7),
            ),
            None,
            UNIX_EPOCH + Duration::from_secs(60),
        )
    }

    fn request(machine: &str, epoch: u64, overlay_ip: &str) -> MachineAddRequest {
        MachineAddRequest {
            machine: MachineId::parse(machine).expect("machine"),
            epoch: MachineEpoch::new(epoch).expect("epoch"),
            network: MachineNetworkIdentity::new(
                OverlayIp::parse(overlay_ip).expect("overlay ip"),
                IrohEndpointId::parse("iroh-node-a").expect("iroh endpoint"),
                WireGuardPublicKey::parse("wg-node-a").expect("wireguard key"),
            ),
        }
    }

    fn membership(machine: &str, epoch: u64, overlay_ip: &str) -> MachineMembership {
        MachineMembership::new(
            MachineId::parse(machine).expect("machine"),
            MachineEpoch::new(epoch).expect("epoch"),
            MachineNetworkIdentity::new(
                OverlayIp::parse(overlay_ip).expect("overlay ip"),
                IrohEndpointId::parse("iroh-node-a").expect("iroh endpoint"),
                WireGuardPublicKey::parse("wg-node-a").expect("wireguard key"),
            ),
        )
    }
}
