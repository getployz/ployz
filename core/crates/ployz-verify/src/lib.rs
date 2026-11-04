//! Model checks of Ployz protocols that tests rarely reach. Each module models one
//! protocol, calls the real code where it can, and its tests explore every
//! interleaving within the bounds the module states.

#[cfg(test)]
mod caddy_load;
#[cfg(test)]
mod removal_marks;
#[cfg(test)]
mod volume_marks;
#[cfg(test)]
mod volume_runs;
