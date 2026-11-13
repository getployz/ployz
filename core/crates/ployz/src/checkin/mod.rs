//! `ployz server checkin`: the loop a waiting host runs until Cloud enrolls,
//! rejects or forgets it.
//!
//! Each pass probes the host, reads how far preparation has got, posts one signed
//! check-in and acts on the answer. The host keeps no Cluster state: Cloud steers,
//! and `status.json` only remembers the last answer for `ployz status`.

mod client;
mod enroll;
mod host;
pub(crate) mod install;
mod paths;
mod plan;
mod probe;
mod run;
mod status;
mod token_file;
pub(crate) mod unit;
mod wire;

pub(crate) use client::{CheckinClient, CheckinKey, Hold, KeyError, key_failure};
pub(crate) use enroll::Committed;
pub(crate) use host::{Host, System, local_machine, process_is_root, systemctl};
pub(crate) use paths::{Paths, hold_loop, loop_stopped};
pub(crate) use plan::{InstallPass, Next, install_pass, joined, restart_preparation};
pub(crate) use run::{CheckinEnd, run};
pub(crate) use status::{
    Headline, LocalCheckin, NO_CLUSTER, NO_CLUSTER_WHY, Seen, ago, no_ployzd, record,
};
pub(crate) use token_file::{FleetToken, ensure_readable as ensure_token_readable, loop_token};
pub(crate) use wire::{
    Attempt, DOCKER_INSTALL_FAILED, DockerPrep, EnrollInstruction, Issue, Prep, StopReason, ZfsPrep,
};

#[cfg(test)]
use {
    host::{keeps_locks_local, marked_on, preparation_marked},
    plan::{
        Liveness, PREPARE_RETRY, PREPARE_TRIES, Preparation, REFUSED_ONCE, REVOKED,
        observe_liveness, pass_from, preparation, read_prep_state,
    },
    ployz_core::{MachineLayout, host_preparation::PrepSteps, probe_document::ProbeDocument},
    probe::Probes,
    run::{Said, stop},
    status::Refusal,
    std::{
        io,
        path::{Path, PathBuf},
    },
    wire::PrepRecord,
};

#[cfg(test)]
pub(crate) use {plan::pass, token_file::replaces as replaces_token};

#[cfg(test)]
#[path = "checkin_tests.rs"]
mod tests;
