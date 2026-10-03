//! `ployz server drain`: turn a Server's services role off, then converge every
//! replicated user Service with a Container on it.

use std::collections::{BTreeMap, BTreeSet};

use clap::ArgMatches;
use ployz_core::{
    EnvironmentValues, MachineName, MachineTarget, MachineUpdate, Namespace, QualifiedService,
    UpdateMachineRequest, op,
};
use ployz_store::NamespacesQuery;
use serde::Serialize;
use serde_json::json;

use super::remove::{replicated_services_on, select_machine, services_on};
use super::{server_json, target, wait_for_role};
use crate::connect::{Client, TARGET_RPC_TIMEOUT};
use crate::deploy::{Convergence, converge};
use crate::handlers::{Error, leaf_matches, store, with_client};
use crate::output::{self, say};

const NOTHING_MOVES_BACK: &str = "Turning the services role back on does not move anything back.";

#[derive(Serialize)]
struct ServiceReport {
    service: QualifiedService,
    #[serde(flatten)]
    convergence: Convergence,
}

pub(in crate::handlers) fn drain(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let selector = target(matches, "server")?.to_owned();
    // Store reads block on their own runtime, so they run before the Cluster's.
    let owned = owned_namespaces(root)?;
    with_client(root, |client| {
        Box::pin(async move {
            let machines = client.machines().await?;
            let selected = select_machine(&machines, &selector)?;
            if selected.accepts_services {
                cordon(client, &selected.id).await?;
                say!("Server {} no longer accepts Services.", selected.name);
            } else {
                say!("Server {} already accepts no Services.", selected.name);
            }
            let services = movable(client, &selected, owned.as_ref()).await?;
            let cancellation = crate::cancellation::on_ctrl_c();
            let mut reports = Vec::new();
            for service in services {
                let convergence = converge(client, &service, &cancellation).await?;
                say!("{}", line(&service, &convergence));
                reports.push(ServiceReport {
                    service,
                    convergence,
                });
            }
            let remaining = remaining(client, &selected).await?;
            say!("{}", remaining_line(&selected.name, &remaining));
            say!("{NOTHING_MOVES_BACK}");
            output::emit(&json!({
                "server": server_json(&selected),
                "services": reports,
                "remaining": remaining,
                "note": NOTHING_MOVES_BACK,
            }))?;
            if reports.iter().any(|report| {
                matches!(
                    report.convergence,
                    Convergence::Moved {
                        failed: Some(_),
                        ..
                    }
                )
            }) {
                return Err(Error::partial());
            }
            Ok(())
        })
    })
}

/// Namespaces some Project owns, when a Config Store is reachable. Standalone Clusters
/// have none, so every user Namespace counts there.
fn owned_namespaces(root: &ArgMatches) -> Result<Option<BTreeSet<Namespace>>, Error> {
    let matches = leaf_matches(root);
    let Some(store) = store::reachable(root)? else {
        return Ok(None);
    };
    // A Cloud Store owns the signed-in Organization's Namespaces; another Cluster
    // reached through --context or --connect is not that Organization's.
    if matches!(store.backend(), store::Backend::Cloud(..))
        && (matches.get_one::<String>("context").is_some()
            || matches.get_one::<String>("connect").is_some())
    {
        return Ok(None);
    }
    Ok(Some(
        store
            .read(&NamespacesQuery {})?
            .namespaces
            .into_iter()
            .map(|owned| owned.namespace)
            .collect(),
    ))
}

async fn cordon(client: &mut Client, id: &ployz_core::MachineId) -> Result<(), Error> {
    client
        .invoke::<op::UpdateMachine>(
            UpdateMachineRequest {
                update: MachineUpdate {
                    accepts_services: Some(false),
                    ..MachineUpdate::default()
                },
            },
            &MachineTarget::from(id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await?;
    wait_for_role(client, id, "services", |machine| !machine.accepts_services).await
}

/// Replicated user Services with a Container on the Server.
async fn movable(
    client: &mut Client,
    selected: &ployz_core::Machine,
    owned: Option<&BTreeSet<Namespace>>,
) -> Result<Vec<QualifiedService>, Error> {
    let machines = client.machines().await?;
    let live = client
        .live_services_from(&machines, EnvironmentValues::Redacted)
        .await?;
    observed(&live, selected)?;
    Ok(replicated_services_on(&selected.id, &live)
        .into_iter()
        .filter(|service| {
            !service.namespace.is_reserved()
                && owned.is_none_or(|owned| owned.contains(&service.namespace))
        })
        .collect())
}

async fn remaining(
    client: &mut Client,
    selected: &ployz_core::Machine,
) -> Result<Vec<QualifiedService>, Error> {
    let machines = client.machines().await?;
    let live = client
        .live_services_from(&machines, EnvironmentValues::Redacted)
        .await?;
    observed(&live, selected)?;
    Ok(services_on(&selected.id, &live))
}

fn observed(
    live: &ployz_core::LiveServices<ployz_core::RpcError>,
    selected: &ployz_core::Machine,
) -> Result<(), Error> {
    if let Some(failure) = live
        .containers
        .failures
        .iter()
        .find(|failure| failure.machine_id == selected.id)
    {
        return Err(Error::unavailable(format!(
            "Cannot observe Services on Server {}: {}",
            selected.name, failure.error.message
        )));
    }
    if live.containers.omissions.contains(&selected.id) {
        return Err(Error::unavailable(format!(
            "Cannot observe Services on Server {}: no terminal response",
            selected.name
        )));
    }
    Ok(())
}

fn line(service: &QualifiedService, convergence: &Convergence) -> String {
    match convergence {
        Convergence::Stays { reason } => format!("{service}: stays: {reason}"),
        Convergence::Moved {
            moved,
            failed: None,
        } if moved.is_empty() => format!("{service}: nothing to move"),
        Convergence::Moved { moved, failed } => {
            let mut counts = BTreeMap::<(&MachineName, &MachineName), usize>::new();
            for step in moved {
                *counts.entry((&step.from, &step.to)).or_default() += 1;
            }
            let moves = counts
                .iter()
                .map(|((from, to), count)| format!("{count} from {from} to {to}"))
                .collect::<Vec<_>>();
            let mut parts = Vec::new();
            if !moves.is_empty() {
                parts.push(format!("moved {}", moves.join(", ")));
            }
            if let Some(error) = failed {
                parts.push(format!("failed: {error}"));
            }
            format!("{service}: {}", parts.join("; "))
        }
    }
}

fn remaining_line(server: &MachineName, remaining: &[QualifiedService]) -> String {
    if remaining.is_empty() {
        return format!("Nothing runs on {server} now.");
    }
    format!(
        "Still on {server}: {}",
        remaining
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

#[cfg(test)]
mod tests {
    use ployz_core::{MachineName, QualifiedService};

    use super::{Convergence, line, remaining_line};
    use crate::deploy::Move;

    #[test]
    fn the_report_counts_moves_by_route_then_names_what_remains() {
        let name = |value: &str| MachineName::parse(value).unwrap();
        let service = QualifiedService::parse("app/web").unwrap();
        let step = |to: &str| Move {
            from: name("web-2"),
            to: name(to),
        };
        let moved = Convergence::Moved {
            moved: vec![step("web-1"), step("web-3"), step("web-1")],
            failed: Some("boom".into()),
        };
        assert_eq!(
            line(&service, &moved),
            "app/web: moved 2 from web-2 to web-1, 1 from web-2 to web-3; failed: boom"
        );
        let idle = Convergence::Moved {
            moved: Vec::new(),
            failed: None,
        };
        assert_eq!(line(&service, &idle), "app/web: nothing to move");
        assert_eq!(
            remaining_line(&name("web-2"), &[service]),
            "Still on web-2: app/web"
        );
        assert_eq!(
            remaining_line(&name("web-2"), &[]),
            "Nothing runs on web-2 now."
        );
    }
}
