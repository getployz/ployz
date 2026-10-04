//! `ployz server drain`: the CLI's face on [`crate::drain`]. It reads the owned Namespaces
//! from its Config Store, streams a line per step, and prints the report as `--json`.

use std::collections::BTreeMap;

use clap::ArgMatches;
use ployz_core::{MachineName, MachineTarget, QualifiedService};
use ployz_store::NamespacesQuery;
use serde_json::{Map, Value, json};

use super::{server_json, target};
use crate::drain::{
    DrainError, DrainOutcome, DrainReport, DrainScope, DrainStep, Move, Remaining, ServiceDrain,
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
            output::emit(&report_json(&report))?;
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
        DrainError::Connect(error) => error.into(),
        DrainError::Cordon(error) => error.into(),
        error @ DrainError::NotFound(_) => Error::not_found(error.to_string()),
        error @ DrainError::Ambiguous { .. } => Error::ambiguous(error.to_string()),
        error @ (DrainError::RoleNotObserved
        | DrainError::Unobservable { .. }
        | DrainError::Cancelled) => Error::unavailable(error.to_string()),
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
    let (moves, failure) = match &entry.outcome {
        DrainOutcome::Retired => return format!("{service}: global, retired on {server}"),
        DrainOutcome::NotRetired { error } => {
            return format!("{service}: global, failed to retire on {server}: {error}");
        }
        DrainOutcome::Stays { reason } => return format!("{service}: stays: {reason}"),
        DrainOutcome::NothingToMove => return format!("{service}: nothing to move"),
        DrainOutcome::NotAttempted => return format!("{service}: not attempted"),
        DrainOutcome::Moved { moves } => (moves, None),
        DrainOutcome::Failed { moves, failure } => (moves, Some(failure)),
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
    if let Some(failure) = failure {
        parts.push(format!("failed: {failure}"));
    }
    format!("{service}: {}", parts.join("; "))
}

fn closing_lines(report: &DrainReport) -> Vec<String> {
    let server = &report.server.name;
    let mut lines = Vec::new();
    if let Some(stop) = &report.stopped {
        lines.push(format!("Drain stopped: {stop}"));
    }
    lines.push(match &report.remaining {
        Remaining::Observed { services } if services.is_empty() => {
            format!("Nothing runs on {server} now.")
        }
        Remaining::Observed { services } => format!(
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

/// The CLI's `--json`, a projection of the report kept as it was before the report was
/// typed. `stopped` and `remaining_error` appear only where the Drain used to exit
/// without a result.
fn report_json(report: &DrainReport) -> Value {
    let mut out = Map::new();
    out.insert("server".into(), server_json(&report.server));
    out.insert(
        "services".into(),
        report.services.iter().map(service_json).collect(),
    );
    if let Some(stop) = &report.stopped {
        out.insert("stopped".into(), Value::String(stop.to_string()));
    }
    match &report.remaining {
        Remaining::Observed { services } => {
            out.insert("remaining".into(), json!(services));
        }
        Remaining::Unobserved { error } => {
            out.insert("remaining".into(), Value::Null);
            out.insert("remaining_error".into(), Value::String(error.clone()));
        }
    }
    out.insert("note".into(), Value::String(NOTHING_MOVES_BACK.into()));
    Value::Object(out)
}

fn service_json(entry: &ServiceDrain) -> Value {
    let service = &entry.service;
    match &entry.outcome {
        DrainOutcome::Moved { moves } => moved_json(service, moves, None),
        DrainOutcome::NothingToMove => moved_json(service, &[], None),
        DrainOutcome::Failed { moves, failure } => {
            moved_json(service, moves, Some(failure.to_string()))
        }
        DrainOutcome::Stays { reason } => {
            json!({ "service": service, "result": "stays", "reason": reason.to_string() })
        }
        DrainOutcome::Retired => json!({ "service": service, "result": "retired" }),
        DrainOutcome::NotRetired { error } => {
            json!({ "service": service, "result": "failed", "error": error })
        }
        DrainOutcome::NotAttempted => json!({ "service": service, "result": "not_attempted" }),
    }
}

fn moved_json(service: &QualifiedService, moves: &[Move], failed: Option<String>) -> Value {
    json!({
        "service": service,
        "result": "moved",
        "moved": moves
            .iter()
            .map(|step| json!({ "from": step.from.name, "to": step.to.name }))
            .collect::<Vec<_>>(),
        "failed": failed,
    })
}

#[cfg(test)]
#[path = "drain_tests.rs"]
mod tests;
