use crate::cluster::Call;
use std::time::Duration;

use clap::ArgMatches;
use ployz_core::{
    Hint, InspectRequest, KeptVolume, LargePackets, ListKeptVolumesRequest, Machine,
    MachineDetails, MachineName, MachineObservation, MachineRuntime, MachineStorageObservation,
    MachineTarget, MachineUpgradeAttempt, MachineUpgradeOutcome, MembershipObservation,
    TelemetryObservation, op,
};
use serde_json::{Value, json};

use super::super::route::connect_or_none;
use crate::handlers::shared::disk::{full_disks, size};
use crate::{
    cluster::StorageGaps,
    connect::{ConnectError, TARGET_RPC_TIMEOUT},
    handlers::{Error, leaf_matches, runtime},
    ui::{self, Cell, Fields, Gaps, Table, Tone, printable},
};

pub(in crate::handlers) fn list(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let context = leaf.get_one::<String>("context").map(String::as_str);
    runtime()?.block_on(async {
        let (machines, storage, disks, entry) = match connect_or_none(leaf, context).await? {
            Some(mut client) => {
                let mut machines = client.machines().await?;
                let listed = machines.clone();
                let (storage, disks) = tokio::join!(
                    client.observe_machine_storage(&mut machines),
                    client.observe_docker_disks(&listed),
                );
                (machines, storage, disks, client.entry_machine())
            }
            None => (Vec::new(), StorageGaps::default(), Vec::new(), None),
        };
        if let Some(warning) = daemon_skew_warning(&machines, env!("CARGO_PKG_VERSION")) {
            ui::warn(warning);
        }
        if let Some(warning) = power_cut_warning(&machines) {
            ui::warn(warning);
        }
        for full in full_disks(&machines, &disks) {
            ui::warn(full.to_string());
        }
        for observed in machines
            .iter()
            .filter(|observed| drops_large_packets(observed))
        {
            ui::warn(dropped_path(&observed.machine.name));
        }
        for observed in machines.iter().filter(|observed| observed.unarrived()) {
            ui::warn(never_joined(observed));
        }
        for link in unasked_dead_links(&machines, entry) {
            ui::warn(link.explained());
        }
        let listed = machines.iter().map(observation_json).collect::<Vec<_>>();
        let mut gaps = Gaps::default()
            .named(
                machines
                    .iter()
                    .map(|observed| (observed.machine.id, &observed.machine.name)),
            )
            .explained(crate::deploy::dead_links(&machines, entry));
        gaps.extend(&storage.failures, &storage.omissions);
        let mut table = Table::new(
            [
                "SERVER",
                "REGION",
                "MEMBERSHIP",
                "STORAGE",
                "PUBLIC IP",
                "SUBNET",
                "DAEMON",
                "OS",
            ],
            "No Servers yet.",
        );
        for observed in &machines {
            let machine = &observed.machine;
            table.row([
                Cell::from(machine.name.to_string()),
                Cell::from(
                    machine
                        .labels
                        .iter()
                        .find(|(key, _)| key.as_str() == ployz_core::Region::LABEL)
                        .map_or("-", |(_, region)| region.as_str())
                        .to_owned(),
                ),
                Cell::status(
                    observed.membership.as_str(),
                    membership_tone(&observed.membership),
                ),
                Cell::from(format_storage(observed.storage)),
                Cell::from(
                    machine
                        .public_ip
                        .map(|ip| ip.to_string())
                        .unwrap_or_default(),
                ),
                Cell::from(machine.subnet.to_string()),
                Cell::from(machine.runtime.daemon_version.to_string()),
                Cell::from(format!(
                    "{} {}",
                    machine.runtime.os_pretty_name, machine.runtime.architecture
                )),
            ]);
        }
        crate::ui::finish_fanout("servers", &listed, &gaps, || ui::rows(&table))
    })
}

/// The dead links to Servers this listing didn't ask, because membership reads them down.
/// A Server it asked that didn't answer gets its link under that warning instead.
fn unasked_dead_links(
    machines: &[MachineObservation],
    entry: Option<ployz_core::MachineId>,
) -> Vec<crate::deploy::DeadLink> {
    crate::deploy::dead_links(machines, entry)
        .into_iter()
        .filter(|link| {
            machines
                .iter()
                .any(|observed| observed.machine.id == link.machine && !observed.invites_rpc())
        })
        .collect()
}

/// A listed Server that hasn't joined. One whose join is still under way looks the same, even
/// after its add stopped waiting, so the command that takes it out comes with that condition.
fn never_joined(observed: &MachineObservation) -> String {
    format!(
        "Server {} hasn't joined and may still be joining. Once that Server is deleted or won't \
         join, take it out with `{}`.",
        observed.machine.name,
        ui::pasteable(&crate::enrollment::output::forget_unarrived(
            observed.machine.id
        ))
    )
}

fn membership_tone(membership: &MembershipObservation) -> Tone {
    match membership {
        MembershipObservation::Up => Tone::Good,
        MembershipObservation::Suspect | MembershipObservation::Unknown => Tone::Change,
        MembershipObservation::Down | MembershipObservation::Unrecognized(_) => Tone::Bad,
    }
}

#[must_use]
fn daemon_skew_warning(machines: &[MachineObservation], cli_version: &str) -> Option<String> {
    let count = machines
        .iter()
        .filter(|observed| observed.machine.runtime.daemon_version != cli_version)
        .count();
    match count {
        0 => None,
        1 => Some(format!(
            "1 Server runs a daemon version different from CLI {cli_version}."
        )),
        count => Some(format!(
            "{count} Servers run daemon versions different from CLI {cli_version}."
        )),
    }
}

/// Filesystems that refuse, or don't pass on, the flag that made the store of a daemon
/// up to 0.3.11 write synchronously, by the name the daemon reports and the name a
/// person knows. A daemon that reports its store syncs each commit is not among them.
const POWER_CUT_STORES: [(&str, &str); 3] =
    [("zfs", "ZFS"), ("btrfs", "Btrfs"), ("tmpfs", "tmpfs")];

fn power_cut_store(runtime: &MachineRuntime) -> Option<&'static str> {
    if runtime.store_syncs_commits == Some(true) {
        return None;
    }
    let filesystem = runtime.store_filesystem.as_deref()?;
    POWER_CUT_STORES
        .iter()
        .find(|(reported, _)| *reported == filesystem)
        .map(|(_, shown)| *shown)
}

#[must_use]
fn power_cut_warning(machines: &[MachineObservation]) -> Option<String> {
    let lossy = machines
        .iter()
        .filter_map(|observed| {
            let machine = &observed.machine;
            Some((machine.name.to_string(), power_cut_store(&machine.runtime)?))
        })
        .collect::<Vec<_>>();
    match lossy.as_slice() {
        [] => None,
        [(name, filesystem)] => Some(format!(
            "{name} stores its state on {filesystem}, so it can lose its last changes in a \
             power cut. If it loses power, restart the services on it, and again if some \
             requests still reach its old replicas."
        )),
        several => {
            let mut filesystems = several.iter().map(|(_, shown)| *shown).collect::<Vec<_>>();
            filesystems.sort_unstable();
            filesystems.dedup();
            Some(format!(
                "{} store their state on {}, so they can lose their last changes in a power \
                 cut. If one loses power, restart the services on it, and again if some \
                 requests still reach its old replicas.",
                prose_list(several.iter().map(|(name, _)| name.as_str()), "and"),
                prose_list(filesystems, "or"),
            ))
        }
    }
}

fn prose_list<'item>(items: impl IntoIterator<Item = &'item str>, conjunction: &str) -> String {
    let items = items.into_iter().collect::<Vec<_>>();
    match items.split_last() {
        None => String::new(),
        Some((last, [])) => (*last).to_owned(),
        Some((last, rest)) => format!("{} {conjunction} {last}", rest.join(", ")),
    }
}

#[must_use]
fn format_storage(storage: Option<MachineStorageObservation>) -> String {
    match storage {
        None => "Volume support unknown".into(),
        Some(MachineStorageObservation::Stateless) => "Docker only".into(),
        Some(MachineStorageObservation::Ready) => "Managed volumes available".into(),
        Some(MachineStorageObservation::Pool {
            size_bytes,
            used_bytes,
            free_bytes,
        }) => format!(
            "Managed volumes available ({}, {} used, {} free)",
            size(size_bytes.get()),
            size(used_bytes),
            size(free_bytes)
        ),
    }
}

/// Print fresh targeted Machine telemetry, its round-trip times to peers, and its
/// latest upgrade attempt; list/watch request only storage evidence.
pub(in crate::handlers) fn show(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let selector = leaf
        .get_one::<String>("server")
        .ok_or_else(|| Error::usage("server is required"))?;
    let selector = MachineTarget::parse(selector)?;
    let context = leaf.get_one::<String>("context").map(String::as_str);
    runtime()?.block_on(async {
        let (mut client, held) = super::super::route::connect_held(leaf, context).await?;
        let direct = Direct {
            leaf,
            context,
            held: held.as_deref(),
        };
        inspect(&direct, &mut client, &selector).await
    })
}

async fn inspect(
    direct: &Direct<'_>,
    client: &mut crate::connect::Client,
    selector: &MachineTarget,
) -> Result<(), Error> {
    let (details, kept) = tokio::join!(
        read_details(direct, client, selector),
        kept_volumes(client, selector)
    );
    let details = match details {
        Ok(details) => details,
        Err(error) => {
            let message = if error.target_unanswered() {
                // One listing from the entry explains a silent target both ways.
                let machines =
                    tokio::time::timeout(crate::deploy::DIAGNOSIS_WAIT, client.machines())
                        .await
                        .ok()
                        .and_then(Result::ok)
                        .unwrap_or_default();
                let target =
                    match selector.resolve(machines.iter().map(|observed| &observed.machine)) {
                        ployz_core::NameMatches::One(target) => Some(target.id),
                        ployz_core::NameMatches::None
                        | ployz_core::NameMatches::Ambiguous { .. } => None,
                    };
                let observed = machines
                    .iter()
                    .find(|observed| Some(observed.machine.id) == target);
                if !error.large_packets_dropped()
                    && let Some(observed) =
                        observed.filter(|observed| drops_large_packets(observed))
                {
                    ui::warn(dropped_path(&observed.machine.name));
                }
                let link = crate::deploy::dead_links(&machines, client.entry_machine())
                    .into_iter()
                    .find(|link| Some(link.machine) == target);
                unshown(selector, link.as_ref())
            } else {
                unshown(selector, None)
            };
            return Err(Error::from(crate::connect::rpc_error(error)).context(message));
        }
    };
    let mut server = serde_json::to_value(&details)?;
    if let Value::Object(fields) = &mut server {
        fields.remove("upgrade");
        fields.insert("public_key".into(), json!(details.public_key.to_string()));
        if let Some(machine) = &details.machine {
            fields.insert(
                "machine".into(),
                crate::enrollment::output::machine_json(machine),
            );
        }
    }
    let mut record = record(&details);
    if let Some(kept) = &kept {
        record.push("kept volumes", kept_summary(kept));
    }
    let name = details.machine.as_ref().map_or_else(
        || details.id.to_string(),
        |machine| machine.name.to_string(),
    );
    crate::ui::finish(
        &json!({ "server": server, "upgrade": details.upgrade, "kept_volumes": kept }),
        || {
            crate::ui::record(&record);
            if kept.as_ref().is_some_and(|kept| !kept.is_empty()) {
                crate::ui::hint(&Hint::Next(crate::cli::rerun(
                    direct.leaf,
                    &["server", "volumes", &name],
                )));
            }
        },
    )
}

/// Why Server `selector` can't be shown: the link the entry sees down, when there is one, and
/// otherwise the plain failure, whose cause follows it.
fn unshown(selector: &MachineTarget, link: Option<&crate::deploy::DeadLink>) -> String {
    match link {
        Some(link) => format!(
            "Server {selector} didn't answer, so it can't be shown. {}",
            link.explained()
        ),
        None => format!("Could not show Server {}.", selector.as_str()),
    }
}

/// How long `server show` waits for the Server's kept volumes, which it measures, before
/// it shows the rest without them.
const KEPT_VOLUMES_WAIT: Duration = Duration::from_secs(5);

/// The volumes the Server kept from removed containers, or `None` when its daemon predates
/// them or didn't answer in time.
async fn kept_volumes(
    client: &crate::connect::Client,
    selector: &MachineTarget,
) -> Option<Vec<KeptVolume>> {
    tokio::time::timeout(
        KEPT_VOLUMES_WAIT,
        client.call_shared::<op::ListKeptVolumes>(
            ListKeptVolumesRequest {},
            Call::once().to(selector),
        ),
    )
    .await
    .ok()?
    .ok()
    .map(|kept| kept.volumes)
}

/// How many volumes the Server kept and the space they take, at least, when some
/// weren't measured.
fn kept_summary(kept: &[KeptVolume]) -> String {
    if kept.is_empty() {
        return "none".into();
    }
    let measured: u64 = kept.iter().filter_map(|volume| volume.size_bytes).sum();
    let unmeasured = kept
        .iter()
        .filter(|volume| volume.size_bytes.is_none())
        .count();
    let count = kept.len();
    if unmeasured == count {
        format!("{count}, size unknown")
    } else if unmeasured > 0 {
        format!("{count}, at least {}", size(measured))
    } else {
        format!("{count}, {}", size(measured))
    }
}

/// How long a read routed through the entry runs alone before the same read starts on the
/// Server itself. A mesh path that loses large replies stalls the routed read until its budget
/// runs out, while the Server still answers over its own connection.
const DIRECT_AFTER: Duration = Duration::from_secs(3);

fn inspect_request() -> InspectRequest {
    InspectRequest {
        telemetry: ployz_core::InspectTelemetry::Full,
        include_rtts: true,
        include_storage: true,
        ..Default::default()
    }
}

/// What a direct read dials from: the command's connection flags, and the connections Cloud
/// already handed over, if it was Cloud.
struct Direct<'a> {
    leaf: &'a ArgMatches,
    context: Option<&'a str>,
    held: Option<&'a [crate::context::Connection]>,
}

/// Inspect the Server `selector` names through the entry. Once that runs past
/// [`DIRECT_AFTER`], or the entry says large packets to the Server are dropped, read it once
/// more over a connection to the Server itself. Both share one [`TARGET_RPC_TIMEOUT`] and the
/// first answer wins; when neither answers, the routed error says why. Any other routed error
/// returns at once, as a direct dial to a Server that just reset or went down only waits.
/// Inspect only reads, so asking twice changes nothing.
async fn read_details(
    direct: &Direct<'_>,
    client: &crate::connect::Client,
    selector: &MachineTarget,
) -> Result<MachineDetails, ConnectError> {
    let deadline = tokio::time::Instant::now() + TARGET_RPC_TIMEOUT;
    let routed = tokio::time::timeout_at(
        deadline,
        client.call_shared::<op::Inspect>(inspect_request(), Call::once().to(selector)),
    );
    tokio::pin!(routed);
    let routed_error = tokio::select! {
        outcome = &mut routed => match budgeted(outcome) {
            Err(error) if error.large_packets_dropped() => error,
            outcome => return outcome,
        },
        () = tokio::time::sleep(DIRECT_AFTER) => {
            let read = tokio::time::timeout_at(deadline, read_direct(direct, client, selector));
            tokio::pin!(read);
            tokio::select! {
                outcome = &mut routed => match budgeted(outcome) {
                    Err(error) if error.large_packets_dropped() => {
                        return read_directly(read.await).ok_or(error);
                    }
                    outcome => return outcome,
                },
                outcome = &mut read => match read_directly(outcome) {
                    Some(details) => return Ok(details),
                    None => return budgeted(routed.await),
                },
            }
        }
    };
    read_directly(tokio::time::timeout_at(deadline, read_direct(direct, client, selector)).await)
        .ok_or(routed_error)
}

/// A routed outcome whose shared deadline ran out reads as the Server not answering in time.
fn budgeted(
    outcome: Result<Result<MachineDetails, ConnectError>, tokio::time::error::Elapsed>,
) -> Result<MachineDetails, ConnectError> {
    outcome.unwrap_or_else(|_| {
        Err(ConnectError::Rpc(
            crate::connect::TransportError::budget_expired("target Machine RPC timed out"),
        ))
    })
}

/// The Server's details read over a connection to it alone, or `None` when there is no such
/// connection, it is the entry already asked, or it doesn't answer.
async fn read_direct(
    direct: &Direct<'_>,
    client: &crate::connect::Client,
    selector: &MachineTarget,
) -> Option<(MachineDetails, MachineName)> {
    let machines = client.clone().machines().await.ok()?;
    let ployz_core::NameMatches::One(target) =
        selector.resolve(machines.iter().map(|observed| &observed.machine))
    else {
        return None;
    };
    let target_id = target.id;
    if client.entry_machine() == Some(target_id) {
        return None;
    }
    let name = target.name.clone();
    let server =
        super::super::route::connect_only(direct.leaf, direct.context, direct.held, &target_id)
            .await
            .ok()??;
    // Untargeted, so the Server answers for itself rather than resolving the selector again
    // against its own view, where the name may lead elsewhere.
    let details = server
        .call_shared::<op::Inspect>(inspect_request(), Call::once())
        .await
        .map_err(crate::connect::rpc_error)
        .ok()?;
    (details.id == target_id).then_some((details, name))
}

/// The details a direct read returned, saying where they came from.
fn read_directly(
    outcome: Result<Option<(MachineDetails, MachineName)>, tokio::time::error::Elapsed>,
) -> Option<MachineDetails> {
    let (details, name) = outcome.ok()??;
    ui::warn(format!(
        "{name} did not answer through the Server this command entered by, so it was read over its own connection."
    ));
    Some(details)
}

/// Whether the entry's path probes found that large packets to this Server are dropped.
fn drops_large_packets(observed: &MachineObservation) -> bool {
    observed
        .path
        .is_some_and(|path| path.large_packets == LargePackets::Dropped)
}

fn dropped_path(name: &MachineName) -> String {
    format!(
        "Large packets to {name} are dropped on its mesh path from the Server that answered, so calls with large replies to {name} fail."
    )
}

/// The human `server show`: who the Server is, what it runs, how full it is, and
/// how far its peers are; `--json` keeps everything, capabilities included.
fn record(details: &MachineDetails) -> Fields {
    let name = details.machine.as_ref().map_or_else(
        || details.id.to_string(),
        |machine| machine.name.to_string(),
    );
    let mut lines = vec![
        ("server", name),
        ("id", details.id.to_string()),
        ("phase", details.phase.as_str().to_owned()),
    ];
    if let Some(machine) = &details.machine {
        let runtime = &machine.runtime;
        lines.extend([
            (
                "os",
                format!("{} {}", runtime.os_pretty_name, runtime.architecture),
            ),
            ("kernel", runtime.kernel_version.clone()),
            ("hostname", runtime.hostname.clone()),
            ("daemon", runtime.daemon_version.clone()),
            ("docker", runtime.docker_version.clone()),
            (
                "public ip",
                machine
                    .public_ip
                    .map(|ip| ip.to_string())
                    .unwrap_or_default(),
            ),
            ("subnet", machine.subnet.to_string()),
            ("accepts", accepts(machine)),
            (
                "labels",
                joined(
                    machine
                        .labels
                        .iter()
                        .map(|(key, value)| format!("{key}={value}")),
                ),
            ),
        ]);
    }
    lines.push((
        "endpoints",
        joined(
            details
                .advertised_endpoints
                .iter()
                .map(|endpoint| endpoint.0),
        ),
    ));
    lines.push(("storage", format_storage(details.storage)));
    if let Some(machine) = &details.machine
        && let Some(filesystem) = &machine.runtime.store_filesystem
    {
        let store = match power_cut_store(&machine.runtime) {
            Some(shown) => format!(
                "{shown}. Can lose its last changes in a power cut; restart its services after one."
            ),
            None => filesystem.clone(),
        };
        lines.push(("store", store));
    }
    if let Some(TelemetryObservation::Full { host, .. }) = &details.telemetry {
        let (memory, disk) = (host.memory(), host.docker_root());
        lines.extend([
            (
                "cpu",
                format!(
                    "{} CPUs, load {}.{:02}",
                    host.cpu_count,
                    host.load_average_milli / 1000,
                    host.load_average_milli % 1000 / 10
                ),
            ),
            (
                "memory",
                format!(
                    "{} available of {}",
                    size(memory.available()),
                    size(memory.total())
                ),
            ),
            (
                "docker disk",
                format!("{} free of {}", size(disk.available()), size(disk.total())),
            ),
            ("containers", host.managed_containers.to_string()),
        ]);
    }
    lines.extend(details.rtts.iter().map(|rtt| {
        let peer = rtt
            .machine
            .as_ref()
            .map_or_else(|| rtt.address.to_string(), |peer| peer.name.to_string());
        // Corrosion can measure the round trip by way of another Server, so it says nothing here.
        if let Some(ployz_core::WireGuardObservation::Down {
            since_handshake_secs,
        }) = rtt.wireguard
        {
            let since = since_handshake_secs.map_or_else(
                || "ever".to_owned(),
                |secs| format!("for {}", crate::deploy::span(Duration::from_secs(secs))),
            );
            return (
                "peer",
                format!("{peer} down, no WireGuard handshake {since}"),
            );
        }
        let tenths = rtt.statistics.median_ns / 100_000;
        let path = match rtt.path {
            Some(path) if path.large_packets == LargePackets::Dropped => {
                ", large packets dropped".to_owned()
            }
            Some(path) if path.lowered => format!(", mtu {}", path.mtu),
            Some(_) => String::new(),
            None => String::new(),
        };
        (
            "peer",
            format!("{peer} {}.{} ms{path}", tenths / 10, tenths % 10),
        )
    }));
    lines.push(("management", joined(&details.management_clients)));
    if let Some(attempt) = &details.upgrade {
        lines.push(("upgrade", upgrade(attempt)));
    }
    let mut record = Fields::new();
    for (label, value) in lines {
        record.push(label, printable(&value));
    }
    record
}

fn accepts(machine: &Machine) -> String {
    let accepted = [
        (machine.accepts_services, "services"),
        (machine.accepts_builds, "builds"),
        (machine.accepts_ingress, "ingress"),
    ]
    .into_iter()
    .filter_map(|(accepts, work)| accepts.then_some(work));
    let accepted = joined(accepted);
    if accepted.is_empty() {
        "nothing".into()
    } else {
        accepted
    }
}

fn upgrade(attempt: &MachineUpgradeAttempt) -> String {
    let target = &attempt.target;
    match &attempt.outcome {
        MachineUpgradeOutcome::Accepted => format!("to {target} accepted"),
        MachineUpgradeOutcome::Running { stage } => {
            format!("to {target} running: {}", stage.as_str())
        }
        MachineUpgradeOutcome::Succeeded { version } => format!("to {version} succeeded"),
        MachineUpgradeOutcome::Failed { stage, error } => {
            format!("to {target} failed at {}: {error}", stage.as_str())
        }
        MachineUpgradeOutcome::Interrupted { stage } => {
            format!("to {target} interrupted at {}", stage.as_str())
        }
    }
}

fn joined(items: impl IntoIterator<Item = impl std::fmt::Display>) -> String {
    items
        .into_iter()
        .map(|item| item.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// One `server ls` row: the observation, its Machine as every `server` command prints
/// it, and the gateway of its subnet.
fn observation_json(observation: &MachineObservation) -> Value {
    let mut value = serde_json::to_value(observation).expect("a Machine observation serializes");
    if let Value::Object(fields) = &mut value {
        fields.insert(
            "machine".into(),
            crate::enrollment::output::machine_json(&observation.machine),
        );
        fields.insert(
            "gateway".into(),
            json!(observation.machine.subnet.gateway().0),
        );
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::MachineStorageObservation;

    #[test]
    fn a_server_that_has_not_joined_is_named_with_the_command_that_forgets_it_by_id() {
        let machine = crate::global_fixtures::machine('d', "web-3");
        let id = machine.id;
        let never = MachineObservation::new(machine, MembershipObservation::Down);
        assert!(never.unarrived());

        assert_eq!(
            never_joined(&never),
            format!(
                "Server web-3 hasn't joined and may still be joining. Once that Server is deleted \
                 or won't join, take it out with `ployz server forget {} --confirm dead`.",
                id.as_str()
            )
        );
    }

    #[test]
    fn storage_column_distinguishes_ready_without_a_pool() {
        assert_eq!(format_storage(None), "Volume support unknown");
        assert_eq!(
            format_storage(Some(MachineStorageObservation::Stateless)),
            "Docker only"
        );
        assert_eq!(
            format_storage(Some(MachineStorageObservation::Ready)),
            "Managed volumes available"
        );
        assert_eq!(
            format_storage(Some(MachineStorageObservation::Pool {
                size_bytes: std::num::NonZeroU64::new(4_294_967_296).unwrap(),
                used_bytes: 3_865_470_566,
                free_bytes: 429_496_730,
            })),
            "Managed volumes available (4.3 GB, 3.9 GB used, 429 MB free)"
        );
    }

    #[test]
    fn a_shown_server_is_a_record_not_json() {
        let key = json!(ployz_core::WireGuardPublicKey([7; 32]));
        let details: MachineDetails = serde_json::from_value(json!({
            "id": "8d996790baf04c20b81cdbf8c7897016",
            "phase": "participating",
            "machine": {
                "id": "8d996790baf04c20b81cdbf8c7897016",
                "name": "machine-1",
                "subnet": "10.210.0.0/24",
                "public_key": key,
                "public_ip": "5.39.66.176",
                "labels": { "zone": "a" },
                "accepts_builds": false,
                "accepts_ingress": true,
                "accepts_services": true,
                "runtime": {
                    "daemon_version": "0.3.0-beta.6",
                    "docker_version": "29.7.0",
                    "hostname": "box-1",
                    "architecture": "x86_64",
                    "os_pretty_name": "Ubuntu 24.04.4 LTS",
                    "kernel_version": "6.8.0-117-generic",
                    "store_filesystem": "zfs"
                }
            },
            "public_key": key,
            "advertised_endpoints": ["192.168.122.53:51820", "5.39.66.176:51820"],
            "rtts": [{
                "peer_id": "3f334967-fab6-43ff-8e11-39b7ecbcc9e5",
                "address": "[fdcc::1]:7570",
                "machine": { "id": "8bd99128eeb24bfeb80ae7489cd46211", "name": "machine-3" },
                "statistics": { "median_ns": 1_250_000, "population_stddev_ns": 300_000 }
            }],
            "management_clients": ["cli-5ec2b97c41404ba093fc78cf6489", "cloud"],
            "storage": { "state": "ready" },
            "telemetry": {
                "scope": "full",
                "host": {
                    "observed_at_unix_seconds": 1_792_333_856,
                    "managed_containers": 2,
                    "cpu_count": 2,
                    "load_average_milli": 20,
                    "memory_total_bytes": 4_106_260_480_u64,
                    "memory_available_bytes": 2_894_979_072_u64,
                    "docker_root_total_bytes": 73_635_184_640_u64,
                    "docker_root_free_bytes": 47_283_404_800_u64
                },
                "bridge": {
                    "bridge_usable_endpoints": 253,
                    "bridge_attached_endpoints": 2,
                    "bridge_free_endpoints": 251
                }
            },
            "upgrade": {
                "attempt_id": "7b4f0d2c9a8e4f6b8c1d2e3f4a5b6c7d",
                "target": "0.3.0-beta.7",
                "outcome": "failed",
                "stage": "activating",
                "error": "systemd refused the unit"
            }
        }))
        .unwrap();
        let mut out = anstream::StripStream::new(Vec::new());
        record(&details).write(&mut out, false).unwrap();
        assert_eq!(
            String::from_utf8(out.into_inner()).unwrap(),
            "server = machine-1\n\
             id = 8d996790baf04c20b81cdbf8c7897016\n\
             phase = participating\n\
             os = Ubuntu 24.04.4 LTS x86_64\n\
             kernel = 6.8.0-117-generic\n\
             hostname = box-1\n\
             daemon = 0.3.0-beta.6\n\
             docker = 29.7.0\n\
             public ip = 5.39.66.176\n\
             subnet = 10.210.0.0/24\n\
             accepts = services, ingress\n\
             labels = zone=a\n\
             endpoints = 192.168.122.53:51820, 5.39.66.176:51820\n\
             storage = Managed volumes available\n\
             store = ZFS. Can lose its last changes in a power cut; restart its services after one.\n\
             cpu = 2 CPUs, load 0.02\n\
             memory = 2.9 GB available of 4.1 GB\n\
             docker disk = 47.3 GB free of 73.6 GB\n\
             containers = 2\n\
             peer = machine-3 1.2 ms\n\
             management = cli-5ec2b97c41404ba093fc78cf6489, cloud\n\
             upgrade = to 0.3.0-beta.7 failed at activating: systemd refused the unit\n"
        );
    }

    /// A peer the shown Server's WireGuard sees down shows as down, not with a round trip
    /// that came another way.
    #[test]
    fn a_peer_whose_wireguard_is_down_shows_down_not_a_round_trip() {
        let rtt = |name: &str, wireguard: Value| {
            json!({
                "peer_id": name,
                "address": "[fdcc::1]:7570",
                "machine": { "id": "8bd99128eeb24bfeb80ae7489cd46211", "name": name },
                "statistics": { "median_ns": 400_000, "population_stddev_ns": 0 },
                "wireguard": wireguard,
            })
        };
        let details: MachineDetails = serde_json::from_value(json!({
            "id": "8d996790baf04c20b81cdbf8c7897016",
            "phase": "participating",
            "public_key": ployz_core::WireGuardPublicKey([7; 32]),
            "advertised_endpoints": [],
            "rtts": [
                rtt("machine-2", json!({ "state": "up" })),
                rtt("machine-3", json!({ "state": "down", "since_handshake_secs": 387 })),
                rtt("machine-4", json!({ "state": "down", "since_handshake_secs": null })),
                rtt("machine-5", Value::Null),
            ],
            "management_clients": [],
        }))
        .unwrap();
        let mut out = anstream::StripStream::new(Vec::new());
        record(&details).write(&mut out, false).unwrap();
        let peers: Vec<_> = String::from_utf8(out.into_inner())
            .unwrap()
            .lines()
            .filter(|line| line.starts_with("peer = "))
            .map(str::to_owned)
            .collect();
        assert_eq!(
            peers,
            [
                "peer = machine-2 0.4 ms",
                "peer = machine-3 down, no WireGuard handshake for 6m",
                "peer = machine-4 down, no WireGuard handshake ever",
                "peer = machine-5 0.4 ms",
            ]
        );
    }

    /// In a 2-Server partition membership reads the peer down, so `server ls` never asks it
    /// and has no failure to put the link under. The link is still named.
    #[test]
    fn a_server_membership_reads_down_still_has_its_dead_link_named() {
        let entry = on("machine-1", None);
        let mut peer = on("machine-2", None);
        peer.membership = MembershipObservation::Down;
        peer.wireguard = Some(ployz_core::WireGuardObservation::Down {
            since_handshake_secs: Some(315),
        });
        let named = |peer: &MachineObservation| {
            unasked_dead_links(&[entry.clone(), peer.clone()], Some(entry.machine.id))
                .iter()
                .map(crate::deploy::DeadLink::explained)
                .collect::<Vec<_>>()
        };
        let [line] = named(&peer).try_into().unwrap();
        assert!(
            line.contains("machine-1, the Server this command entered by, has had no WireGuard handshake with machine-2 for 5m"),
            "{line}"
        );
        assert!(line.contains("UDP 51820"), "{line}");

        peer.membership = MembershipObservation::Up;
        assert!(
            named(&peer).is_empty(),
            "a Server it asked gets the link under its own warning"
        );
    }

    /// A Server that didn't answer while the entry's WireGuard sees it up keeps the plain
    /// failure; only a link seen down changes the words.
    #[test]
    fn a_silent_server_the_entry_sees_up_keeps_the_plain_failure() {
        let selector = MachineTarget::parse("machine-2").unwrap();
        assert_eq!(unshown(&selector, None), "Could not show Server machine-2.");
        let entry = on("machine-1", None);
        let mut peer = on("machine-2", None);
        peer.wireguard = Some(ployz_core::WireGuardObservation::Down {
            since_handshake_secs: None,
        });
        let [link] = crate::deploy::dead_links(&[entry.clone(), peer], Some(entry.machine.id))
            .try_into()
            .unwrap();
        assert!(
            unshown(&selector, Some(&link)).starts_with(
                "Server machine-2 didn't answer, so it can't be shown. machine-1, the Server this command entered by, has never had"
            )
        );
    }

    fn on(name: &str, filesystem: Option<&str>) -> MachineObservation {
        MachineObservation::new(
            ployz_core::Machine {
                labels: Default::default(),
                accepts_builds: true,
                accepts_services: true,
                accepts_ingress: true,
                id: ployz_core::MachineId::random(),
                name: name.parse().unwrap(),
                subnet: "10.210.7.0/24".parse().unwrap(),
                public_key: ployz_core::WireGuardPublicKey([7; 32]),
                public_ip: None,
                advertised_endpoints: Vec::new(),
                runtime: MachineRuntime {
                    store_filesystem: filesystem.map(str::to_owned),
                    ..Default::default()
                },
                join_approval: None,
                build_concurrency: None,
            },
            MembershipObservation::Up,
        )
    }

    #[test]
    fn servers_on_synchronous_or_unreported_stores_are_not_warned_about() {
        assert_eq!(
            power_cut_warning(&[
                on("web-1", Some("ext4")),
                on("web-2", None),
                on("web-3", Some("0x1234abcd")),
                on("web-4", Some("overlay")),
            ]),
            None
        );
    }

    #[test]
    fn a_server_whose_store_syncs_each_commit_is_not_warned_about_on_any_filesystem() {
        let mut zfs = on("web-1", Some("zfs"));
        zfs.machine.runtime.store_syncs_commits = Some(true);
        assert_eq!(power_cut_warning(&[zfs]), None);
        let mut tmpfs = on("web-2", Some("tmpfs"));
        tmpfs.machine.runtime.store_syncs_commits = Some(false);
        assert!(power_cut_warning(&[tmpfs]).is_some());
    }

    #[test]
    fn one_server_that_can_lose_its_last_changes_is_named() {
        assert_eq!(
            power_cut_warning(&[on("web-1", Some("ext4")), on("web-2", Some("zfs"))]).as_deref(),
            Some(
                "web-2 stores its state on ZFS, so it can lose its last changes in a power cut. \
                 If it loses power, restart the services on it, and again if some requests \
                 still reach its old replicas."
            )
        );
        assert_eq!(
            power_cut_warning(&[on("web-3", Some("btrfs"))]).as_deref(),
            Some(
                "web-3 stores its state on Btrfs, so it can lose its last changes in a power cut. \
                 If it loses power, restart the services on it, and again if some requests \
                 still reach its old replicas."
            )
        );
    }

    #[test]
    fn several_servers_that_can_lose_their_last_changes_are_named_in_one_line() {
        assert_eq!(
            power_cut_warning(&[
                on("web-1", Some("zfs")),
                on("web-2", Some("ext4")),
                on("web-3", Some("btrfs")),
                on("db-1", Some("zfs")),
            ])
            .as_deref(),
            Some(
                "web-1, web-3 and db-1 store their state on Btrfs or ZFS, so they can lose their \
                 last changes in a power cut. If one loses power, restart the services on it, \
                 and again if some requests still reach its old replicas."
            )
        );
    }

    #[test]
    fn a_server_on_a_synchronous_store_shows_its_plain_filesystem() {
        let mut details: MachineDetails = serde_json::from_value(json!({
            "id": "8d996790baf04c20b81cdbf8c7897016",
            "phase": "participating",
            "public_key": ployz_core::WireGuardPublicKey([7; 32]),
        }))
        .unwrap();
        let mut machine = on("web-1", Some("ext4")).machine;
        machine.id = details.id;
        details.machine = Some(machine);
        let mut out = anstream::StripStream::new(Vec::new());
        record(&details).write(&mut out, false).unwrap();
        let shown = String::from_utf8(out.into_inner()).unwrap();
        assert!(shown.contains("\nstore = ext4\n"), "{shown}");
        details.machine.as_mut().unwrap().runtime.store_filesystem = None;
        let mut out = anstream::StripStream::new(Vec::new());
        record(&details).write(&mut out, false).unwrap();
        assert!(
            !String::from_utf8(out.into_inner())
                .unwrap()
                .contains("store =")
        );
    }

    #[test]
    fn machine_json_projection_includes_the_derived_gateway() {
        let observation = MachineObservation::new(
            ployz_core::Machine {
                labels: Default::default(),
                accepts_builds: true,
                accepts_services: true,
                accepts_ingress: true,
                id: "0".repeat(32).parse().unwrap(),
                name: "node-a".parse().unwrap(),
                subnet: "10.210.7.0/24".parse().unwrap(),
                public_key: ployz_core::WireGuardPublicKey([7; 32]),
                public_ip: None,
                advertised_endpoints: Vec::new(),
                runtime: Default::default(),
                join_approval: None,
                build_concurrency: None,
            },
            ployz_core::MembershipObservation::Up,
        );
        let output = observation_json(&observation);
        assert_eq!(output.pointer("/gateway"), Some(&json!("10.210.7.1")));
        assert_eq!(output.pointer("/machine/name"), Some(&json!("node-a")));
        assert_eq!(
            output.pointer("/machine/public_key"),
            Some(&json!("BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc="))
        );
        assert_eq!(
            output.get("machine"),
            Some(&crate::enrollment::output::machine_json(
                &observation.machine
            ))
        );
    }
}
