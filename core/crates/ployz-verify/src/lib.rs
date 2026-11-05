//! Model checks of Ployz protocols that tests rarely reach. Each module models one
//! protocol, calls the real code where it can, and its tests explore every
//! interleaving within the bounds the module states.

#[cfg(test)]
mod build_handoff;
#[cfg(test)]
mod caddy_load;
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
