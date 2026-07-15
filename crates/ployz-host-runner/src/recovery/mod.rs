//! Local Control-Plane Core recovery capability.
//!
//! Promotion, demotion, wrapped authority material, identity resurrection, and
//! recovery-secret environment handling share this boundary. Recovery remains
//! an explicit operator-triggered local mutation; this module does not own
//! cluster truth or automatic failover policy.

mod demote;
mod demote_command;
pub(crate) mod environment;
mod identity;
mod promote;
mod secret;

pub use demote::{CoreDemoteTarget, demote_local_core, repoint_non_core_roles};
pub use identity::{
    ClusterNatsIdentity, CoreSeeds, CoreSeedsError, NatsCaKeyPem, NatsIdentityError,
    NatsServerCertificate, NatsServerKeyPem, ServerCertificateSans, generate_cluster_nats_identity,
    issue_server_certificate, resurrect_core_identity, unwrap_core_seeds, wrap_core_seeds,
};
pub use secret::{RecoverySecretError, generate_recovery_secret, unwrap, wrap};

pub(crate) use demote_command::run_core_demote_command;
pub(crate) use promote::run_core_promote_command;
