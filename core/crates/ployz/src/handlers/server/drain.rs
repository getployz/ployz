//! `ployz server drain`: the CLI's face on [`crate::drain`]. It reads the owned Namespaces
//! from its Config Store, streams a line per step, and prints the report as `--json`.

use std::collections::BTreeMap;

use clap::ArgMatches;
use ployz_core::{MachineName, MachineTarget, RpcError};
use ployz_store::NamespacesQuery;
use serde_json::Value;

use super::{server_json, target};
use crate::drain::{
    DrainError, DrainOutcome, DrainReport, DrainScope, DrainStep, Remaining, ServiceDrain,
    ServicesRole,
};
use crate::handlers::{Error, leaf_matches, store, with_client};
use crate::output::{self, say};

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
                    say!("{}", step_line(step));
                })
                .await
                .map_err(refusal)?;
            for line in closing_lines(&report) {
                say!("{line}");
            }
            let mut json = serde_json::to_value(&report).expect("a Drain report serializes");
            if let Value::Object(fields) = &mut json {
                fields.insert("server".into(), server_json(&report.server));
                fields.insert("note".into(), NOTHING_MOVES_BACK.into());
            }
            output::emit(&json)?;
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

fn closing_lines(report: &DrainReport) -> Vec<String> {
    let server = &report.server.name;
    let mut lines = Vec::new();
    if let Some(stop) = &report.stopped {
        lines.push(format!("Drain stopped: {stop}"));
    }
    lines.push(match &report.remaining {
        Remaining::Observed { services, .. } if services.is_empty() => {
            format!("Nothing runs on {server} now.")
        }
        Remaining::Observed { services, .. } => format!(
            "Still on {server}: {}",
            services
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Remaining::Unobserved { error } => {
            format!("Cannot observe Services on Server {server}: {error}")
        }
    });
    lines.push(NOTHING_MOVES_BACK.to_owned());
    lines
}

#[cfg(test)]
#[path = "drain_tests.rs"]
mod tests;
