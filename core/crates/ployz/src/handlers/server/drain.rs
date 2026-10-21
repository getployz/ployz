//! `ployz server drain`: the CLI's face on [`crate::drain`]. It reads the owned Namespaces
//! from its Config Store, streams a line per step, and prints the report as `--json`.

use std::collections::BTreeMap;

use clap::ArgMatches;
use ployz_core::{Machine, MachineName, MachineTarget, QualifiedService, RpcError};
use ployz_store::NamespacesQuery;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};

use super::target;
use crate::approval::{self, Verb};
use crate::cloud_account::{self, Credential};
use crate::drain::{
    DrainError, DrainOutcome, DrainReport, DrainScope, DrainStep, Move, MoveFailure, OldCopyEnd,
    Remaining, ServiceDrain, ServicesRole, StayReason, Unconfirmed,
};
use crate::handlers::{Error, leaf_matches, runtime, store, with_client};

const NOTHING_MOVES_BACK: &str = "Turning the services role back on does not move anything back.";

pub(in crate::handlers) fn drain(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let selector = target(matches, "server")?;
    let target = MachineTarget::parse(selector)?;
    let runtime = runtime()?;
    if let Some(credential) = super::cloud_runs(&runtime, matches)? {
        let machine = super::cloud_machine(&runtime, matches, selector)?;
        let report = through_cloud(&runtime, matches, &credential, &machine, selector)?;
        let server = &report.server;
        show(DrainStep::ServicesOff {
            server,
            role: report.services_role,
        });
        for service in &report.services {
            show(DrainStep::Service { server, service });
        }
        return render(&report);
    }
    drop(runtime);
    // Store reads block on their own runtime, so they run before the Cluster's.
    let scope = scope(root)?;
    with_client(root, |client| {
        Box::pin(async move {
            let cancellation = crate::cancellation::on_ctrl_c();
            let report = client
                .drain(&target, &scope, &cancellation, &mut show)
                .await
                .map_err(refusal)?;
            render(&report)
        })
    })
}

fn through_cloud(
    runtime: &tokio::runtime::Runtime,
    matches: &ArgMatches,
    credential: &Credential,
    machine: &Machine,
    selector: &str,
) -> Result<DrainReport, Error> {
    let id = approval::approved(
        runtime,
        credential,
        Verb::Drain,
        matches.get_one::<String>("approval").cloned(),
        |id| super::approved_rerun(matches, &["server", "drain", selector], id),
        async |approval| {
            cloud_account::start_run(
                credential,
                Method::POST,
                &format!("servers/{}/drain", machine.id),
                &json!({ "server_name": machine.name }),
                approval,
            )
            .await
        },
    )?;
    #[derive(Deserialize)]
    struct Finished {
        report: DrainReport,
    }
    let settled = runtime.block_on(cloud_account::follow_operation::<Finished>(
        credential,
        &format!("server-drains/{id}"),
        &format!("Draining Server {}", machine.name),
    ))?;
    Ok(settled.finished()?.report)
}

fn show(step: DrainStep<'_>) {
    if went_wrong(step) {
        crate::ui::warn(step_line(step));
    } else {
        crate::ui::stream(format_args!("{}", step_line(step)));
    }
}

fn render(report: &DrainReport) -> Result<(), Error> {
    if let Some(stop) = &report.stopped {
        crate::ui::warn(format!("Drain stopped: {stop}"));
    }
    let remaining = remaining(report);
    if let Err(warning) = &remaining {
        crate::ui::warn(warning.clone());
    }
    let mut json = serde_json::to_value(report).expect("a Drain report serializes");
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
    if report.complete() {
        return Ok(());
    }
    Err(incomplete(report))
}

fn incomplete(report: &DrainReport) -> Error {
    let server = &report.server.name;
    let mut stays = Vec::new();
    let mut left = Vec::new();
    for entry in &report.services {
        match &entry.outcome {
            DrainOutcome::Stays {
                reason: StayReason::LastIn,
            }
            | DrainOutcome::Failed {
                failure:
                    MoveFailure::Refused {
                        reason: StayReason::LastIn,
                    },
                ..
            }
            | DrainOutcome::Held => stays.push(entry.service.to_string()),
            DrainOutcome::Moved { moves }
                if moves.iter().any(|step| step.old != OldCopyEnd::Stopped) =>
            {
                left.push(entry.service.to_string());
            }
            DrainOutcome::OldCopies { ends }
                if ends.iter().any(|end| *end != OldCopyEnd::Stopped) =>
            {
                left.push(entry.service.to_string());
            }
            DrainOutcome::Stays { .. }
            | DrainOutcome::Moved { .. }
            | DrainOutcome::OldCopies { .. }
            | DrainOutcome::NothingToMove
            | DrainOutcome::Retired
            | DrainOutcome::NotRetired { .. }
            | DrainOutcome::Failed { .. }
            | DrainOutcome::Interrupted { .. }
            | DrainOutcome::NotAttempted => {}
        }
    }
    if !stays.is_empty() {
        return Error::conflict(format!("{} stays on {server}.", stays.join(", ")));
    }
    if !left.is_empty() {
        return Error::unavailable(format!(
            "an old copy of {} is left on {server}. The next drain cleans it up.",
            left.join(", ")
        ));
    }
    Error::partial()
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

fn went_wrong(step: DrainStep<'_>) -> bool {
    match step {
        DrainStep::ServicesOff { .. } => false,
        DrainStep::Service { service, .. } => match &service.outcome {
            DrainOutcome::NotRetired { .. }
            | DrainOutcome::Failed { .. }
            | DrainOutcome::Interrupted { .. } => true,
            DrainOutcome::Moved { moves } => {
                moves.iter().any(|step| step.old != OldCopyEnd::Stopped)
            }
            DrainOutcome::OldCopies { ends } => ends.iter().any(|end| *end != OldCopyEnd::Stopped),
            DrainOutcome::Retired
            | DrainOutcome::Stays { .. }
            | DrainOutcome::Held
            | DrainOutcome::NothingToMove
            | DrainOutcome::NotAttempted => false,
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
        DrainOutcome::Stays { reason } => {
            return format!("{service}  stays on {server} ({})", stay(service, reason));
        }
        DrainOutcome::Held => {
            return format!(
                "{service}  stays on {server} (stopped by you; start or remove it first)"
            );
        }
        DrainOutcome::NothingToMove => return format!("{service}: nothing to move"),
        DrainOutcome::NotAttempted => return format!("{service}: not attempted"),
        DrainOutcome::Moved { moves } => {
            return moves
                .iter()
                .map(|step| moved(service, server, step))
                .collect::<Vec<_>>()
                .join("\n");
        }
        DrainOutcome::OldCopies { ends } => {
            return ends
                .iter()
                .map(|end| format!("{service}  {}", old_copy(server, end)))
                .collect::<Vec<_>>()
                .join("\n");
        }
        DrainOutcome::Failed { moves, failure } => (moves, format!("failed: {failure}")),
        DrainOutcome::Interrupted { moves } => (moves, "interrupted".to_owned()),
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
    parts.push(ending);
    format!("{service}: {}", parts.join("; "))
}

fn stay(service: &QualifiedService, reason: &StayReason) -> String {
    match reason {
        StayReason::LastIn => {
            format!("last copy in rotation; deploy {service} elsewhere first")
        }
        other @ (StayReason::Unobserved { .. }
        | StayReason::MidRollout
        | StayReason::Global
        | StayReason::BindMount { .. }
        | StayReason::Volume { .. }
        | StayReason::NoDestination { .. }) => other.to_string(),
    }
}

fn moved(service: &QualifiedService, server: &MachineName, step: &Move) -> String {
    format!(
        "{service}  moved to {}, {}",
        step.to.name,
        old_copy(server, &step.old)
    )
}

fn old_copy(server: &MachineName, end: &OldCopyEnd) -> String {
    match end {
        OldCopyEnd::Stopped => "old copy stopped  done".to_owned(),
        OldCopyEnd::Running { unconfirmed } => format!(
            "old copy left running on {server} ({}). The next drain cleans it up.",
            not_confirmed(unconfirmed)
        ),
        OldCopyEnd::In { unconfirmed } => format!(
            "old copy still in rotation on {server} ({}). Both copies are serving. The next drain cleans it up.",
            not_confirmed(unconfirmed)
        ),
    }
}

fn not_confirmed(unconfirmed: &Unconfirmed) -> String {
    match unconfirmed {
        Unconfirmed::Proxies { servers } => {
            let names = servers
                .iter()
                .map(|server| server.name.to_string())
                .collect::<Vec<_>>();
            if let [one] = names.as_slice() {
                format!("proxy on {one} did not confirm")
            } else {
                format!("proxies on {} did not confirm", names.join(", "))
            }
        }
        Unconfirmed::Cancelled => "the drain was cancelled before it confirmed".to_owned(),
        Unconfirmed::Failed { detail } => detail.clone(),
    }
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
