//! Model checks of Ployz protocols that tests rarely reach. Each module models one
//! protocol and its tests explore every interleaving within the bounds the module states.
//!
//! A module calls the `ployz-core` and `ployz-decide` code its protocol runs where that code is
//! pure: `ployz_decide::issuance` and `issuance_gate` in `acme_issuance`,
//! `ployz_decide::bound_work` in `bound_work`,
//! `ployz_decide::bridge` and `RestartPolicy` in `bridge_notes`, `ployz_decide::github_build` in
//! `build_handoff`, `ployz_decide::caddy_load` in `caddy_load`, `ployz_decide::fate` in
//! `mcp_login`, `ployz_decide::standing` in `runner_standing`, `ployz_decide::volume_root`
//! and `volume_floor` in `volume_marks`, `plan`, `ployz_decide::volume_size` and
//! `volume_floor` in `volume_runs`, `MachineUpgradeOutcome::is_terminal` in
//! `upgrade_attempt`, the `ployz_decide::service_floor` decisions in `service_floor`, the
//! Deploy planner itself (`ployz::deploy::preview_deploy`) in `region_placement` and
//! `first_placement`, and `ployz_core::admit_plain_mount` in `first_placement`. The other modules re-state their protocol's decisions by hand, and a
//! decision that moves into `ployz-decide` is called, never re-stated.

#[cfg(test)]
mod acme_issuance;
#[cfg(test)]
mod approval_binding;
#[cfg(test)]
mod bound_work;
#[cfg(test)]
mod bridge_notes;
#[cfg(test)]
mod build_handoff;
#[cfg(test)]
mod caddy_load;
#[cfg(test)]
mod drain_copy;
#[cfg(test)]
mod first_placement;
#[cfg(test)]
mod harness;
#[cfg(test)]
mod mcp_login;
#[cfg(test)]
mod member_removal;
#[cfg(test)]
mod pool_ownership;
#[cfg(test)]
mod region_placement;
#[cfg(test)]
mod removal_marks;
#[cfg(test)]
mod runner_standing;
#[cfg(test)]
mod service_floor;
#[cfg(test)]
mod sign_in_store;
#[cfg(test)]
mod trash_restore;
#[cfg(test)]
mod upgrade_attempt;
#[cfg(test)]
mod volume_marks;
#[cfg(test)]
mod volume_runs;
