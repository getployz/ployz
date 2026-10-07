//! `ployz server drain`: the CLI's face on [`crate::drain`]. It reads the owned Namespaces
//! from its Config Store, streams a line per step, and prints the report as `--json`.

use std::collections::BTreeMap;

use clap::ArgMatches;
use ployz_core::{MachineName, MachineTarget, RpcError};
use ployz_store::NamespacesQuery;
use serde_json::Value;

use super::target;
use crate::drain::{
    DrainError, DrainOutcome, DrainReport, DrainScope, DrainStep, Remaining, ServiceDrain,
    ServicesRole,
};
use crate::handlers::{Error, leaf_matches, store, with_client};

const NOTHING_MOVES_BACK: &str = "Turning the services role back on does not move anything back.";

pub(in crate::handlers) fn drain(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let target = MachineTarget::parse(target(matches, "server")?)?;
    // Store reads block on their own runtime, so they run before the Cluster's.
    let scope = scope(root)?;
    with_client(root, |client| {
        Box::pin(async move {
            let cancellation = crate::cancellation::on_ctrl_c();
            let report = client
                .drain(&target, &scope, &cancellation, &mut |step| {
                    if went_wrong(step) {
                        crate::ui::warn(step_line(step));
                    } else {
                        crate::ui::stream(format_args!("{}", step_line(step)));
                    }
                })
                .await
                .map_err(refusal)?;
            if let Some(stop) = &report.stopped {
                crate::ui::warn(format!("Drain stopped: {stop}"));
            }
            let remaining = remaining(&report);
            if let Err(warning) = &remaining {
                crate::ui::warn(warning.clone());
            }
            let mut json = serde_json::to_value(&report).expect("a Drain report serializes");
            if let Value::Object(fields) = &mut json {
                fields.insert("server".into(), super::machine_json(&report.server));
                fields.insert("note".into(), NOTHING_MOVES_BACK.into());
            }
            crate::ui::finish(&json, || {
                if let Ok(line) = &remaining {
                    crate::ui::stream(format_args!("{line}"));
                }
                crate::ui::note(NOTHING_MOVES_BACK);
            })?;
            if !report.complete() {
                return Err(Error::partial());
            }
            Ok(())
        })
    })
}

/// Namespaces some Project owns, when a Config Store is reachable. Standalone Clusters
/// have none, so every user Namespace counts there.
fn scope(root: &ArgMatches) -> Result<DrainScope, Error> {
    let matches = leaf_matches(root);
    let Some(store) = store::reachable(root)? else {
        return Ok(DrainScope::EveryNamespace);
    };
    // A Cloud Store owns the signed-in Organization's Namespaces; another Cluster
    // reached through --context or --connect is not that Organization's.
    if matches!(store.backend(), store::Backend::Cloud(..))
        && (matches.get_one::<String>("context").is_some()
            || matches.get_one::<String>("connect").is_some())
    {
        return Ok(DrainScope::EveryNamespace);
    }
    Ok(DrainScope::Owned {
        namespaces: store
            .read(&NamespacesQuery {})?
            .namespaces
            .into_iter()
            .map(|owned| owned.namespace)
            .collect(),
    })
}

fn refusal(error: DrainError) -> Error {
    match error {
        // The CLI reports a connection failure with its own words.
        DrainError::Connect(error) => error.into(),
        error @ (DrainError::Refused(_)
        | DrainError::RoleNotObserved
        | DrainError::Unobservable { .. }
        | DrainError::Cancelled) => RpcError::from(error).into(),
    }
}

fn step_line(step: DrainStep<'_>) -> String {
    match step {
        DrainStep::ServicesOff {
            server,
            role: ServicesRole::TurnedOff,
        } => format!("Server {} no longer accepts Services.", server.name),
        DrainStep::ServicesOff {
            server,
            role: ServicesRole::AlreadyOff,
        } => format!("Server {} already accepts no Services.", server.name),
        DrainStep::Service { server, service } => line(service, &server.name),
    }
}

const fn went_wrong(step: DrainStep<'_>) -> bool {
    match step {
        DrainStep::ServicesOff { .. } => false,
        DrainStep::Service { service, .. } => match service.outcome {
            DrainOutcome::NotRetired { .. }
            | DrainOutcome::Failed { .. }
            | DrainOutcome::Interrupted { .. } => true,
            DrainOutcome::Retired
            | DrainOutcome::Stays { .. }
            | DrainOutcome::NothingToMove
            | DrainOutcome::NotAttempted
            | DrainOutcome::Moved { .. } => false,
        },
    }
}

fn line(entry: &ServiceDrain, server: &MachineName) -> String {
    let service = &entry.service;
    let (moves, ending) = match &entry.outcome {
        DrainOutcome::Retired => return format!("{service}: global, retired on {server}"),
        DrainOutcome::NotRetired { error } => {
            return format!("{service}: global, failed to retire on {server}: {error}");
        }
        DrainOutcome::Stays { reason } => return format!("{service}: stays: {reason}"),
        DrainOutcome::NothingToMove => return format!("{service}: nothing to move"),
        DrainOutcome::NotAttempted => return format!("{service}: not attempted"),
        DrainOutcome::Moved { moves } => (moves, None),
        DrainOutcome::Failed { moves, failure } => (moves, Some(format!("failed: {failure}"))),
        DrainOutcome::Interrupted { moves } => (moves, Some("interrupted".to_owned())),
    };
    let mut counts = BTreeMap::<(&MachineName, &MachineName), usize>::new();
    for step in moves {
        *counts.entry((&step.from.name, &step.to.name)).or_default() += 1;
    }
    let mut parts = Vec::new();
    if !counts.is_empty() {
        let routes = counts
            .iter()
            .map(|((from, to), count)| format!("{count} from {from} to {to}"))
            .collect::<Vec<_>>();
        parts.push(format!("moved {}", routes.join(", ")));
    }
    parts.extend(ending);
    format!("{service}: {}", parts.join("; "))
}

/// What still runs on the Server, or the warning that nobody could look.
fn remaining(report: &DrainReport) -> Result<String, String> {
    let server = &report.server.name;
    match &report.remaining {
        Remaining::Observed { services, .. } if services.is_empty() => {
            Ok(format!("Nothing runs on {server} now."))
        }
        Remaining::Observed { services, .. } => Ok(format!(
            "Still on {server}: {}",
            services
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )),
        Remaining::Unobserved { error } => Err(format!(
            "Cannot observe Services on Server {server}: {error}"
        )),
    }
}

#[cfg(test)]
#[path = "drain_tests.rs"]
mod tests;
