//! Model checks of Ployz protocols that tests rarely reach. Each module models one
//! protocol and its tests explore every interleaving within the bounds the module states.
//!
//! A module calls the `ployz-core` code its protocol runs where that code is pure:
//! `issuance_gate`, `issuance_backoff` and `issuance_failure_clock` in `acme_issuance`,
//! `RestartPolicy` in `bridge_notes`, `MachineUpgradeAttempt` in `upgrade_attempt` and
//! `plan` in `volume_runs`. The other modules re-state their protocol's decisions by hand,
//! and a decision that moves into `ployz-decide` is called, never re-stated.

#[cfg(test)]
mod acme_issuance;
#[cfg(test)]
mod bridge_notes;
#[cfg(test)]
mod build_handoff;
#[cfg(test)]
mod caddy_load;
#[cfg(test)]
mod harness;
#[cfg(test)]
mod mcp_login;
#[cfg(test)]
mod pool_ownership;
#[cfg(test)]
mod removal_marks;
#[cfg(test)]
mod runner_standing;
#[cfg(test)]
mod sign_in_store;
#[cfg(test)]
mod upgrade_attempt;
#[cfg(test)]
mod volume_marks;
#[cfg(test)]
mod volume_runs;
