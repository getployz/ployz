//! Idempotent privileged effects for founding machine one.

use ployz_core::founding::FoundingDriverEnrollment;
use ployz_core::operation::FailureMessage;

/// Idempotent privileged effects performed by the feature-owned founding workflow.
///
/// Every method is an `ensure`: a crash after an effect but before its durable
/// milestone may repeat the call safely. Gateway remains disabled and inactive;
/// DNS activation is a separate step after the endpoint network exists.
pub trait FoundingHostEffects {
    fn stage_exact_ployz_and_corrosion(&mut self) -> Result<(), FailureMessage>;
    fn ensure_docker(&mut self) -> Result<(), FailureMessage>;
    fn ensure_machine_identity_and_wireguard(&mut self) -> Result<(), FailureMessage>;
    fn ensure_cluster_door_material(&mut self) -> Result<(), FailureMessage>;
    fn prepare_selected_storage(&mut self) -> Result<(), FailureMessage>;
    fn write_configuration_with_bootstrap(&mut self) -> Result<(), FailureMessage>;
    fn persist_validated_founding_request(&mut self) -> Result<(), FailureMessage>;
    fn restart_and_verify_docker_configuration(&mut self) -> Result<(), FailureMessage>;
    fn install_units_and_enable_ready_roles(&mut self) -> Result<(), FailureMessage>;
    fn start_keeper(&mut self) -> Result<(), FailureMessage>;
    fn start_corrosion(&mut self) -> Result<(), FailureMessage>;
    fn start_api_with_bootstrap(&mut self) -> Result<(), FailureMessage>;
    fn enable_and_start_dns(&mut self) -> Result<(), FailureMessage>;
    fn await_dns_readiness(&mut self) -> Result<(), FailureMessage>;
    /// Observes Keeper's convergence of an enrolled driver; it never writes WG state.
    fn await_driver_peer_convergence(
        &mut self,
        driver: &FoundingDriverEnrollment,
    ) -> Result<(), FailureMessage>;
    fn remove_bootstrap_credential(&mut self) -> Result<(), FailureMessage>;
    fn restart_api_without_bootstrap(&mut self) -> Result<(), FailureMessage>;
}
