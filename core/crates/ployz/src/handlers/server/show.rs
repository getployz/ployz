use std::fmt;

use clap::ArgMatches;
use ployz_core::{
    ByteCapacity, InspectRequest, LargePackets, Machine, MachineDetails, MachineId, MachineName,
    MachineObservation, MachineRuntime, MachineStorageObservation, MachineTarget,
    MachineUpgradeAttempt, MachineUpgradeOutcome, MembershipObservation, RpcErrorCode,
    TelemetryObservation, op,
};
use serde_json::{Value, json};

use super::{connect_or_none, has_no_servers, signed_in, waiting};
use crate::{
    cluster::StorageGaps,
    connect::{ConnectError, TARGET_RPC_TIMEOUT},
    context::ContextError,
    handlers::{Error, leaf_matches, runtime},
    ui::{self, Cell, Fields, Gaps, Table, Tone, printable},
};

pub(in crate::handlers) fn list(root: &ArgMatches) -> Result<(), Error> {
    if leaf_matches(root).get_flag("waiting") {
        return waiting::list(leaf_matches(root));
    }
    let leaf = leaf_matches(root);
    let context = leaf.get_one::<String>("context").map(String::as_str);
    runtime()?.block_on(async {
        let (machines, storage, disks) = match connect_or_none(leaf, context).await? {
            Some(mut client) => {
                let mut machines = client.machines().await?;
                let listed = machines.clone();
                let (storage, disks) = tokio::join!(
                    client.observe_machine_storage(&mut machines),
                    client.observe_docker_disks(&listed),
                );
                (machines, storage, disks)
            }
            None => (Vec::new(), StorageGaps::default(), Vec::new()),
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
        let listed = machines.iter().map(observation_json).collect::<Vec<_>>();
        let mut gaps = Gaps::default().named(
            machines
                .iter()
                .map(|observed| (observed.machine.id, &observed.machine.name)),
        );
        gaps.extend(&storage.failures, &storage.omissions);
        let mut table = Table::new(
            [
                "SERVER",
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

/// Filesystems that refuse, or don't pass on, the flag that makes the store write
/// synchronously, by the name the daemon reports and the name a person knows.
const POWER_CUT_STORES: [(&str, &str); 3] =
    [("zfs", "ZFS"), ("btrfs", "Btrfs"), ("tmpfs", "tmpfs")];

fn power_cut_store(runtime: &MachineRuntime) -> Option<&'static str> {
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

/// Bytes in the decimal GB storage limits are written in (MB under one GB); --json keeps the bytes.
fn size(bytes: u64) -> String {
    #[expect(clippy::cast_precision_loss, reason = "display rounding only")]
    let bytes = bytes as f64;
    if bytes >= 1e9 {
        format!("{:.1} GB", bytes / 1e9)
    } else {
        format!("{:.0} MB", bytes / 1e6)
    }
}

/// Under this much free space a pull fails or soon will, as ployzd judges a full disk.
const NEARLY_FULL_BYTES: u64 = 1_000_000_000;

/// A Server with little space left under its Docker root.
pub(in crate::handlers) struct FullDisk<'a> {
    pub(in crate::handlers) machine: &'a MachineName,
    pub(in crate::handlers) free_bytes: u64,
}

impl FullDisk<'_> {
    /// What is wrong, said of the Server named beside it.
    pub(in crate::handlers) fn reason(&self) -> String {
        format!("Its {self:#}")
    }
}

impl fmt::Display for FullDisk<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !f.alternate() {
            write!(f, "Server {}'s ", self.machine)?;
        }
        write!(
            f,
            "disk is nearly full: {} free for Docker. Pulling an image onto it can fail until you free space.",
            size(self.free_bytes)
        )
    }
}

/// Each listed Server whose Docker root has under [`NEARLY_FULL_BYTES`] free.
pub(in crate::handlers) fn full_disks<'a>(
    machines: &'a [MachineObservation],
    disks: &[(MachineId, ByteCapacity)],
) -> Vec<FullDisk<'a>> {
    machines
        .iter()
        .filter_map(|observed| {
            let (_, disk) = disks
                .iter()
                .find(|(machine, _)| *machine == observed.machine.id)?;
            (disk.available() < NEARLY_FULL_BYTES).then_some(FullDisk {
                machine: &observed.machine.name,
                free_bytes: disk.available(),
            })
        })
        .collect()
}

/// Show a Server, or else a server waiting to join under that name or id.
pub(in crate::handlers) fn show(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let selector = leaf
        .get_one::<String>("server")
        .ok_or_else(|| Error::usage("server is required"))?;
    let (error, unconnected) = match server(root, selector) {
        Ok(()) => return Ok(()),
        Err(Unshown::Unconnected(error)) => (error, true),
        Err(Unshown::Failed(error)) => (error, false),
    };
    let report = error.report();
    let missing = report.code == RpcErrorCode::NotFound || has_no_servers(&report);
    let unreachable = super::servers_unanswered(&report);
    // An inherited PLOYZ_CONNECT or PLOYZ_CONTEXT that reached no daemon was never asked.
    let unasked = unconnected && waiting::inherits_a_connection(leaf);
    if !(missing || unreachable || unasked) || waiting::points_elsewhere(leaf) {
        return Err(error);
    }
    let runtime = runtime()?;
    let Ok(Some(credential)) = signed_in(&runtime, leaf) else {
        return Err(error);
    };
    let listed = match runtime.block_on(waiting::fetch(&credential)) {
        Ok(listed) => listed,
        Err(fetch) => {
            ui::warn_cause("The servers waiting to join could not be read", &fetch);
            return Err(error);
        }
    };
    match waiting::named(&listed, selector)?.as_slice() {
        [] => Err(error.hint(waiting::copied_hint(selector))),
        [host] => waiting::show(&runtime.block_on(waiting::detail(&credential, &host.row.id))?),
        hosts => Err(waiting::ambiguous(
            format!(
                "{} waiting servers are named {}. Show one by its id",
                hosts.len(),
                ui::printable(selector)
            ),
            hosts,
        )),
    }
}

/// Print fresh targeted Machine telemetry, its round-trip times to peers, and its
/// latest upgrade attempt; list/watch request only storage evidence.
fn server(root: &ArgMatches, selector: &str) -> Result<(), Unshown> {
    let selector = MachineTarget::parse(selector).map_err(|error| Unshown::Failed(error.into()))?;
    let leaf = leaf_matches(root);
    let context = leaf.get_one::<String>("context").map(String::as_str);
    runtime().map_err(Unshown::Failed)?.block_on(async {
        let mut client = super::connect(leaf, context).await.map_err(Unshown::of)?;
        inspect(&mut client, &selector)
            .await
            .map_err(Unshown::Failed)
    })
}

/// Why a Server was not shown: no connection reached a daemon, or a daemon answered.
enum Unshown {
    Unconnected(Error),
    Failed(Error),
}

impl Unshown {
    /// A connection that a daemon refused, or whose identity or protocol it contradicted,
    /// was answered: only selection, dial and transport failures leave it unconnected.
    fn of(error: Error) -> Self {
        let source = std::error::Error::source(&error);
        let unreached = source
            .and_then(|source| source.downcast_ref::<ConnectError>())
            .map_or_else(
                || source.is_some_and(|source| source.is::<ContextError>()),
                ConnectError::unreached,
            );
        if unreached {
            Self::Unconnected(error)
        } else {
            Self::Failed(error)
        }
    }
}

async fn inspect(
    client: &mut crate::connect::Client,
    selector: &MachineTarget,
) -> Result<(), Error> {
    let details = client
        .invoke_answered::<op::Inspect>(
            InspectRequest {
                telemetry: ployz_core::InspectTelemetry::Full,
                include_rtts: true,
                include_storage: true,
                ..Default::default()
            },
            selector,
            Some(TARGET_RPC_TIMEOUT),
        )
        .await;
    let details = match details {
        Ok(details) => details,
        Err(error) => {
            if error.target_unanswered()
                && !error.large_packets_dropped()
                && let Some(name) = dropped_target(client, selector).await
            {
                ui::warn(dropped_path(&name));
            }
            return Err(Error::from(crate::connect::rpc_error(error))
                .context(format!("Could not show Server {}.", selector.as_str())));
        }
    };
    let mut server = serde_json::to_value(&details)?;
    if let Value::Object(fields) = &mut server {
        fields.remove("upgrade");
        fields.insert("public_key".into(), json!(details.public_key.to_string()));
        if let Some(machine) = &details.machine {
            fields.insert("machine".into(), super::machine_json(machine));
        }
    }
    crate::ui::fields(
        &json!({ "server": server, "upgrade": details.upgrade }),
        &record(&details),
    )
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

/// The Server `selector` names, when the entry lists its path as dropping large packets.
/// A target that didn't answer may be silent for that reason alone.
async fn dropped_target(
    client: &mut crate::connect::Client,
    selector: &MachineTarget,
) -> Option<MachineName> {
    let machines = client.machines().await.ok()?;
    let ployz_core::NameMatches::One(target) =
        selector.resolve(machines.iter().map(|observed| &observed.machine))
    else {
        return None;
    };
    machines
        .iter()
        .find(|observed| observed.machine.id == target.id && drops_large_packets(observed))
        .map(|observed| observed.machine.name.clone())
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
        let tenths = rtt.statistics.median_ns / 100_000;
        let path = match rtt.path {
            Some(path) if path.large_packets == LargePackets::Dropped => {
                ", large packets dropped".to_owned()
            }
            Some(path) => format!(", mtu {}", path.mtu),
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
        fields.insert("machine".into(), super::machine_json(&observation.machine));
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
    fn a_server_whose_docker_disk_is_nearly_full_is_named() {
        let [full, roomy, unread] = [
            crate::global_fixtures::machine('a', "machine-1"),
            crate::global_fixtures::machine('b', "machine-2"),
            crate::global_fixtures::machine('c', "machine-3"),
        ];
        let disks = [
            (
                full.id,
                ployz_core::ByteCapacity::parse(24_000_000_000, 30_000_000).unwrap(),
            ),
            (
                roomy.id,
                ployz_core::ByteCapacity::parse(24_000_000_000, 1_000_000_000).unwrap(),
            ),
        ];
        let machines = [full, roomy, unread]
            .map(|machine| MachineObservation::new(machine, MembershipObservation::Up));
        let named: Vec<String> = full_disks(&machines, &disks)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            named,
            [
                "Server machine-1's disk is nearly full: 30 MB free for Docker. \
              Pulling an image onto it can fail until you free space."
            ]
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
            Some(&super::super::machine_json(&observation.machine))
        );
    }
}
