use std::{io, time::Duration};

use ployz_core::MachineSubnet;
use tokio_util::sync::CancellationToken;

use crate::network::{
    NetworkError,
    firewall::{Installed, remove_firewall_rules},
};

/// How long another tool's firewall change may shadow the mesh before the
/// rules are applied again.
pub(super) const FIREWALL_INTERVAL: Duration = Duration::from_secs(60);

/// Why the bridge stays IPv4-only when Docker doesn't filter IPv6, told before the
/// fence would need it, since without the fence no later check runs (#3377). It
/// names the cause Ployz saw (bet 8). A host without IPv6 never had it to lose, so
/// says nothing.
/// Apply `firewall` again every [`FIREWALL_INTERVAL`] until `stop`, or until a
/// teardown retires it. Each apply runs off the runtime, so a slow iptables never
/// holds up the mesh.
pub(super) async fn keep_firewall(firewall: Installed, stop: CancellationToken) {
    let mut ticks = tokio::time::interval_at(
        tokio::time::Instant::now() + FIREWALL_INTERVAL,
        FIREWALL_INTERVAL,
    );
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            () = stop.cancelled() => return,
            _ = ticks.tick() => {}
        }
        match tokio::task::spawn_blocking(move || firewall.reapply()).await {
            Ok(Ok(None)) => return,
            Ok(Ok(Some(restored))) => {
                if !restored.displaced.is_empty() {
                    eprintln!(
                        "restored firewall rules that were missing or no longer first: {}",
                        restored.displaced.join("; "),
                    );
                }
                if !restored.behind_docker_nat.is_empty() {
                    tracing::debug!(
                        rules = restored.behind_docker_nat.join("; "),
                        "moved the mesh's nat rule back ahead of Docker's",
                    );
                }
            }
            Ok(Err(error)) => eprintln!(
                "failed to reapply firewall rules: {error}",
                error = ployz_core::error_chain::inline(&error),
            ),
            Err(error) => eprintln!("failed to reapply firewall rules: {error}"),
        }
    }
}

/// Off the runtime: removal waits for the firewall writer and the xtables lock.
pub(super) async fn remove_firewall(subnet: MachineSubnet) -> Result<(), NetworkError> {
    tokio::task::spawn_blocking(move || remove_firewall_rules(subnet))
        .await
        .map_err(|error| NetworkError::Io(io::Error::other(error)))?
}
