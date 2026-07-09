//! Shared plumbing for the gated Docker-in-Docker cluster scenarios:
//! core-cluster formation (the `scripts/local-dataplane-proof.sh` recipe,
//! host-driven), the edge join flow, evidence capture, and the
//! assertion/polling helpers the scenario bodies share.

pub mod assert;
pub mod formation;
pub mod join;

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::time::Duration;

use futures_util::FutureExt as _;
use ployz_e2e::dind::DindCluster;

/// Where Host Runner install leaves the cluster CA and the seeds on the core.
pub const NATS_MATERIAL_DIR: &str = "/var/lib/ployz/nats";
/// The ployzd-control-owned authority file (recovery evidence).
pub const AUTHORIZED_USERS_FILE: &str = "/etc/nats/authorized-users.conf";
/// Where the Host Runner join commit leaves the redeemed per-machine seed
/// (Host Runner state dir `/var/lib/ployz` + `join-material.d`).
pub const EDGE_NATS_CREDS_FILE: &str = "/var/lib/ployz/join-material.d/nats.creds";
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Evidence capture
// ---------------------------------------------------------------------------

/// Runs a scenario body and, when any assertion inside it panics, captures
/// whole-cluster evidence before resuming the panic. This makes every plain
/// `assert!`/`panic!`/`.expect` in the body an evidence-capturing failure.
pub async fn with_evidence<T>(cluster: &DindCluster, scenario: impl Future<Output = T>) -> T {
    match AssertUnwindSafe(scenario).catch_unwind().await {
        Ok(value) => value,
        Err(panic) => {
            match cluster.capture_evidence().await {
                Ok(dir) => eprintln!("scenario failed; evidence: {}", dir.display()),
                Err(error) => eprintln!("scenario failed; evidence capture also failed: {error}"),
            }
            std::panic::resume_unwind(panic)
        }
    }
}
