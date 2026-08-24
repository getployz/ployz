use std::{
    net::Ipv4Addr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use clap::ArgMatches;
use futures_util::future::join_all;
use ployz_core::{
    InspectRequest, InspectWireGuardRequest, MachineFailure, MachineObservation,
    MachineStorageObservation, MachineSuccess, MachineTarget, PartialResult, RpcError,
    RttObservation, RttStatistics, WireGuardPeer, op,
};
use serde::Serialize;

use super::{machine_list, with_client};
use crate::{
    connect::TARGET_RPC_TIMEOUT,
    handlers::{Error, leaf_matches},
};

pub(in crate::handlers) fn list(root: &ArgMatches) -> Result<(), Error> {
    let output = leaf_matches(root).get_one::<String>("output").cloned();
    with_client(root, |client| {
        Box::pin(async move {
            let mut machines = machine_list(client).await?;
            client.observe_machine_storage(&mut machines).await;
            let warning = daemon_skew_warning(&machines, env!("CARGO_PKG_VERSION"));
            if output.as_deref() == Some("json") {
                let machines = machines
                    .iter()
                    .map(|observation| MachineObservationOutput {
                        gateway: observation.machine.subnet.gateway().0,
                        observation,
                    })
                    .collect::<Vec<_>>();
                println!("{}", serde_json::to_string_pretty(&machines)?);
                if let Some(warning) = warning {
                    eprintln!("{warning}");
                }
                return Ok(());
            }
            println!(
                "ID\tNAME\tMEMBERSHIP\tSTORAGE\tSUBNET\tGATEWAY\tPUBLIC IP\tENDPOINTS\tHOSTNAME\tDAEMON\tDOCKER\tOS\tKERNEL\tARCH"
            );
            for observed in machines {
                let machine = observed.machine;
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    machine.id,
                    machine.name,
                    observed.membership.as_str(),
                    format_storage(observed.storage),
                    machine.subnet,
                    machine.subnet.gateway().0,
                    machine
                        .public_ip
                        .map_or_else(|| "-".into(), |ip| ip.to_string()),
                    machine
                        .advertised_endpoints
                        .iter()
                        .map(|endpoint| endpoint.0.to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                    machine.runtime.hostname,
                    machine.runtime.daemon_version,
                    machine.runtime.docker_version,
                    machine.runtime.os_pretty_name,
                    machine.runtime.kernel_version,
                    machine.runtime.architecture,
                );
            }
            if let Some(warning) = warning {
                eprintln!("{warning}");
            }
            Ok(())
        })
    })
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
            "WARNING: 1 Machine runs a daemon version different from CLI {cli_version}."
        )),
        count => Some(format!(
            "WARNING: {count} Machines run daemon versions different from CLI {cli_version}."
        )),
    }
}

#[must_use]
fn format_storage(storage: Option<MachineStorageObservation>) -> String {
    match storage {
        None => "-".into(),
        Some(MachineStorageObservation::Stateless) => "STATELESS".into(),
        Some(MachineStorageObservation::Ready) => "READY (NO POOL)".into(),
        Some(MachineStorageObservation::Pool {
            size_bytes,
            used_bytes,
            free_bytes,
        }) => format!("POOL ({size_bytes} BYTES, {used_bytes} USED, {free_bytes} FREE)"),
    }
}

/// Print fresh targeted Machine telemetry; list/watch request only storage evidence.
pub(in crate::handlers) fn inspect(root: &ArgMatches) -> Result<(), Error> {
    let selector = MachineTarget::parse(
        leaf_matches(root)
            .get_one::<String>("machine")
            .ok_or_else(|| Error::usage("machine is required"))?,
    )?;
    with_client(root, |client| {
        Box::pin(async move {
            let details = client
                .invoke::<op::Inspect>(
                    InspectRequest {
                        telemetry: ployz_core::InspectTelemetry::Full,
                        ..Default::default()
                    },
                    &selector,
                    Some(TARGET_RPC_TIMEOUT),
                )
                .await?;
            println!("{}", serde_json::to_string_pretty(&details)?);
            Ok(())
        })
    })
}

#[derive(Serialize)]
struct MachineObservationOutput<'a> {
    #[serde(flatten)]
    observation: &'a MachineObservation,
    gateway: Ipv4Addr,
}

pub(in crate::handlers) fn rtt(root: &ArgMatches) -> Result<(), Error> {
    with_client(root, |client| {
        Box::pin(async move {
            let machines = machine_list(client).await?;
            let mut requests = Vec::new();
            let mut result = PartialResult {
                successes: Vec::new(),
                failures: Vec::new(),
                omissions: Vec::new(),
            };
            for observed in machines {
                let id = observed.machine.id;
                if !observed.membership.invites_rpc() {
                    result.omissions.push(id);
                    continue;
                }
                let mut client = client.clone();
                requests.push(async move {
                    let request = InspectRequest {
                        include_rtts: true,
                        ..Default::default()
                    };
                    let outcome = client
                        .read::<op::Inspect>(request, &MachineTarget::from(&id))
                        .await;
                    (id, outcome)
                });
            }
            for (id, outcome) in join_all(requests).await {
                match outcome {
                    Ok(details) => result.successes.push(MachineSuccess {
                        machine_id: id,
                        value: details.rtts,
                    }),
                    Err(error) => result.failures.push(MachineFailure {
                        machine_id: id,
                        error,
                    }),
                }
            }
            print_rtts(&result);
            Ok(())
        })
    })
}

#[must_use]
fn format_measured_rtt(median_ns: u64) -> String {
    if median_ns == 0 {
        "<1ms".into()
    } else {
        format!("{:?}", Duration::from_nanos(median_ns))
    }
}

#[must_use]
fn wg_rtt_line(rtt: Option<&RttStatistics>) -> Option<String> {
    rtt.map(|statistics| format!("  rtt: {}", format_measured_rtt(statistics.median_ns)))
}

#[must_use]
fn format_rtt_table(result: &PartialResult<Vec<RttObservation>, RpcError>) -> String {
    let mut table = String::from("SOURCE\tTARGET\tMEDIAN\tSTDDEV\n");
    for success in &result.successes {
        for observation in &success.value {
            let target = observation
                .machine
                .as_ref()
                .map_or(observation.peer_id.as_str(), |machine| machine.id.as_str());
            table.push_str(&format!(
                "{}\t{target}\t{}\t{}\n",
                success.machine_id,
                format_measured_rtt(observation.statistics.median_ns),
                format_measured_rtt(observation.statistics.population_stddev_ns),
            ));
        }
    }
    table
}

fn print_rtts(result: &PartialResult<Vec<RttObservation>, RpcError>) {
    print!("{}", format_rtt_table(result));
    for failure in &result.failures {
        eprintln!(
            "WARNING: RTT inspection failed for {}: {}",
            failure.machine_id, failure.error
        );
    }
    for machine_id in &result.omissions {
        eprintln!("WARNING: RTT inspection omitted for {machine_id}");
    }
}

pub(in crate::handlers) fn wireguard_show(root: &ArgMatches) -> Result<(), Error> {
    let selector = leaf_matches(root).get_one::<String>("machine").cloned();
    with_client(root, |client| {
        Box::pin(async move {
            let target = selector.map(MachineTarget::parse).transpose()?;
            let device = match target.as_ref() {
                None => {
                    client
                        .call::<op::InspectWireguard>(InspectWireGuardRequest {}, None)
                        .await?
                        .device
                }
                Some(target) => {
                    client
                        .invoke::<op::InspectWireguard>(
                            InspectWireGuardRequest {},
                            target,
                            Some(TARGET_RPC_TIMEOUT),
                        )
                        .await?
                        .device
                }
            };
            println!("interface: {}", device.interface_name);
            println!("public key: {}", device.public_key);
            println!("listening port: {}", device.listen_port);
            let now_unix_seconds = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs());
            for peer in device.peers {
                println!();
                println!("peer: {}", peer.public_key);
                if let Some(machine) = &peer.machine {
                    println!("  machine: {} ({})", machine.name, machine.id);
                }
                if let Some(endpoint) = peer.endpoint {
                    println!("  endpoint: {endpoint}");
                }
                println!("{}", format_wg_show_peer_stats(&peer, now_unix_seconds));
                if let Some(line) = wg_rtt_line(peer.rtt.as_ref()) {
                    println!("{line}");
                }
            }
            Ok(())
        })
    })
}

#[must_use]
fn format_wg_ago(elapsed_seconds: u64) -> String {
    let mut remaining = elapsed_seconds;
    let mut parts = Vec::new();
    for (seconds_per_unit, name) in [
        (365 * 24 * 60 * 60, "year"),
        (24 * 60 * 60, "day"),
        (60 * 60, "hour"),
        (60, "minute"),
        (1, "second"),
    ] {
        let count = remaining / seconds_per_unit;
        remaining %= seconds_per_unit;
        if count == 0 {
            continue;
        }
        parts.push(format!(
            "{count} {name}{}",
            if count == 1 { "" } else { "s" }
        ));
    }
    if parts.is_empty() {
        parts.push("0 seconds".into());
    }
    format!("{} ago", parts.join(", "))
}

#[must_use]
fn format_wg_show_peer_stats(peer: &WireGuardPeer, now_unix_seconds: u64) -> String {
    let mut lines = Vec::new();
    if let Some(handshake) = peer.last_handshake_unix_seconds {
        lines.push(format!(
            "  latest handshake: {}",
            format_wg_ago(now_unix_seconds.saturating_sub(handshake))
        ));
    }
    lines.push(format!(
        "  transfer: {} received, {} sent",
        peer.received_bytes, peer.sent_bytes
    ));
    lines.push(format!(
        "  allowed ips: {}",
        peer.allowed_ips
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    ));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::{MachineId, MachineIdentity, MachineName, MachineStorageObservation};

    #[test]
    fn storage_column_distinguishes_ready_without_a_pool() {
        assert_eq!(format_storage(None), "-");
        assert_eq!(
            format_storage(Some(MachineStorageObservation::Stateless)),
            "STATELESS"
        );
        assert_eq!(
            format_storage(Some(MachineStorageObservation::Ready)),
            "READY (NO POOL)"
        );
        assert_eq!(
            format_storage(Some(MachineStorageObservation::Pool {
                size_bytes: std::num::NonZeroU64::new(4_294_967_296).unwrap(),
                used_bytes: 3_865_470_566,
                free_bytes: 429_496_730,
            })),
            "POOL (4294967296 BYTES, 3865470566 USED, 429496730 FREE)"
        );
    }

    #[test]
    fn missing_rtt_is_omitted_and_sub_millisecond_samples_are_not_printed_as_0ns() {
        assert_eq!(wg_rtt_line(None), None);
        assert_eq!(format_measured_rtt(0), "<1ms");
        assert_eq!(
            wg_rtt_line(Some(&RttStatistics {
                median_ns: 0,
                population_stddev_ns: 0,
            }))
            .as_deref(),
            Some("  rtt: <1ms")
        );

        let source = machine_id('1');
        let table = format_rtt_table(&PartialResult {
            successes: vec![MachineSuccess {
                machine_id: source,
                value: vec![rtt_observation("peer-zero", 0, 0)],
            }],
            failures: Vec::new(),
            omissions: Vec::new(),
        });
        assert_eq!(
            table,
            format!("SOURCE\tTARGET\tMEDIAN\tSTDDEV\n{source}\tpeer-zero\t<1ms\t<1ms\n")
        );
        assert!(!table.contains("0ns"));
    }

    #[test]
    fn measured_rtt_prints_the_same_human_units_for_machine_rtt_and_wg_show() {
        assert_eq!(format_measured_rtt(1_500_000), "1.5ms");
        let statistics = RttStatistics {
            median_ns: 1_500_000,
            population_stddev_ns: 200_000,
        };
        assert_eq!(
            wg_rtt_line(Some(&statistics)).as_deref(),
            Some("  rtt: 1.5ms")
        );

        let source = machine_id('1');
        let target = machine_id('2');
        let table = format_rtt_table(&PartialResult {
            successes: vec![MachineSuccess {
                machine_id: source,
                value: vec![RttObservation {
                    peer_id: "peer-live".into(),
                    address: "[fdcc::2]:51001".parse().unwrap(),
                    machine: Some(MachineIdentity {
                        id: target,
                        name: MachineName::parse("node-b").unwrap(),
                    }),
                    statistics,
                }],
            }],
            failures: Vec::new(),
            omissions: Vec::new(),
        });
        assert_eq!(
            table,
            format!("SOURCE\tTARGET\tMEDIAN\tSTDDEV\n{source}\t{target}\t1.5ms\t200µs\n")
        );
        assert!(!table.contains("1500000"));
        assert!(!table.contains("0ns"));
    }

    fn machine_id(digit: char) -> MachineId {
        digit.to_string().repeat(32).parse().unwrap()
    }

    fn rtt_observation(peer_id: &str, median_ns: u64, population_stddev_ns: u64) -> RttObservation {
        RttObservation {
            peer_id: peer_id.into(),
            address: "[fdcc::2]:51001".parse().unwrap(),
            machine: None,
            statistics: RttStatistics {
                median_ns,
                population_stddev_ns,
            },
        }
    }

    #[test]
    fn wg_ago_matches_wireguard_style_for_one_minute_twelve_seconds() {
        assert_eq!(format_wg_ago(72), "1 minute, 12 seconds ago");
    }

    #[test]
    fn wg_show_prints_a_known_handshake_as_relative_ago() {
        assert_eq!(
            format_wg_show_peer_stats(&sample_peer(Some(1_700_000_000 - 72)), 1_700_000_000),
            "  latest handshake: 1 minute, 12 seconds ago\n  transfer: 7 received, 8 sent\n  allowed ips: 10.0.0.2/32"
        );
    }

    #[test]
    fn wg_show_omits_handshake_and_keeps_transfer_and_allowed_ips() {
        assert_eq!(
            format_wg_show_peer_stats(&sample_peer(None), 1_700_000_000),
            "  transfer: 7 received, 8 sent\n  allowed ips: 10.0.0.2/32"
        );
    }

    fn sample_peer(last_handshake_unix_seconds: Option<u64>) -> WireGuardPeer {
        WireGuardPeer {
            public_key: ployz_core::WireGuardPublicKey([1; 32]),
            endpoint: None,
            last_handshake_unix_seconds,
            received_bytes: 7,
            sent_bytes: 8,
            allowed_ips: vec!["10.0.0.2/32".parse().unwrap()],
            machine: None,
            rtt: None,
        }
    }

    #[test]
    fn machine_json_projection_includes_the_derived_gateway() {
        let observation = MachineObservation {
            machine: ployz_core::Machine {
                id: "0".repeat(32).parse().unwrap(),
                name: "node-a".parse().unwrap(),
                subnet: "10.210.7.0/24".parse().unwrap(),
                management_address: ployz_core::ManagementAddress("fdcc::7".parse().unwrap()),
                public_key: ployz_core::WireGuardPublicKey([7; 32]),
                public_ip: None,
                advertised_endpoints: Vec::new(),
                runtime: Default::default(),
            },
            membership: ployz_core::MembershipObservation::Up,
            storage: None,
            selected_endpoint: None,
            rtt: None,
        };
        let output = serde_json::to_value(MachineObservationOutput {
            gateway: observation.machine.subnet.gateway().0,
            observation: &observation,
        })
        .unwrap();
        assert_eq!(output.get("gateway").unwrap(), "10.210.7.1");
    }
}
