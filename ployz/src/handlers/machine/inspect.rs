use std::{
    net::Ipv4Addr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use clap::ArgMatches;
use ployz_core::{
    InspectRequest, InspectWireGuardRequest, MachineFailure, MachineObservation, MachineSelector,
    MachineSubnet, MachineSuccess, PartialResult, RttObservation, WireGuardPeer, op,
};
use serde::Serialize;

use super::{ConnectionOptions, machine_list, runtime};
use crate::handlers::{Error, leaf_matches};

pub(in crate::handlers) fn list(root: &ArgMatches) -> Result<(), Error> {
    let options = ConnectionOptions::from_matches(root)?;
    let output = leaf_matches(root)
        .get_one::<String>("output")
        .map(String::as_str);
    let machines = runtime()?.block_on(async {
        let mut client = options.connect().await?;
        machine_list(&mut client).await
    })?;
    if output == Some("json") {
        let machines = machines
            .iter()
            .map(|observation| MachineObservationOutput {
                gateway: gateway(observation.machine.subnet),
                observation,
            })
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string_pretty(&machines).map_err(|error| error.to_string())?
        );
        return Ok(());
    }
    println!(
        "ID\tNAME\tMEMBERSHIP\tSUBNET\tGATEWAY\tPUBLIC IP\tENDPOINTS\tHOSTNAME\tDAEMON\tDOCKER\tOS\tKERNEL\tARCH"
    );
    for observed in machines {
        let machine = observed.machine;
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            machine.id,
            machine.name,
            observed.membership.as_str(),
            machine.subnet.0,
            gateway(machine.subnet),
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
    Ok(())
}

#[derive(Serialize)]
struct MachineObservationOutput<'a> {
    #[serde(flatten)]
    observation: &'a MachineObservation,
    gateway: Ipv4Addr,
}

fn gateway(network: MachineSubnet) -> Ipv4Addr {
    Ipv4Addr::from(u32::from(network.0.network()).saturating_add(1))
}

pub(in crate::handlers) fn rtt(root: &ArgMatches) -> Result<(), Error> {
    let options = ConnectionOptions::from_matches(root)?;
    let result = runtime()?.block_on(async {
        let mut client = options.connect().await?;
        let machines = machine_list(&mut client).await?;
        let mut result = PartialResult {
            successes: Vec::new(),
            failures: Vec::new(),
            omissions: Vec::new(),
        };
        for observed in machines {
            let id = observed.machine.id;
            let selector = MachineSelector::from(&id);
            let request = InspectRequest {
                include_rtts: true,
                ..Default::default()
            };
            match client.call::<op::Inspect>(request, Some(&selector)).await {
                Ok(details) => result.successes.push(MachineSuccess {
                    machine_id: id,
                    value: details.rtts,
                }),
                Err(error) => result.failures.push(MachineFailure {
                    machine_id: id,
                    error: error.to_string(),
                }),
            }
        }
        Ok::<_, Error>(result)
    })?;
    print_rtts(&result);
    Ok(())
}

fn print_rtts(result: &PartialResult<Vec<RttObservation>, String>) {
    println!("SOURCE\tTARGET\tMEDIAN\tSTDDEV");
    for success in &result.successes {
        for observation in &success.value {
            let target = observation
                .machine
                .as_ref()
                .map_or(observation.peer_id.as_str(), |machine| machine.id.as_str());
            println!(
                "{}\t{}\t{:?}\t{:?}",
                success.machine_id,
                target,
                Duration::from_nanos(observation.statistics.median_ns),
                Duration::from_nanos(observation.statistics.population_stddev_ns),
            );
        }
    }
    for failure in &result.failures {
        eprintln!(
            "WARNING: RTT inspection failed for {}: {}",
            failure.machine_id, failure.error
        );
    }
}

pub(in crate::handlers) fn wireguard_show(root: &ArgMatches) -> Result<(), Error> {
    let options = ConnectionOptions::from_matches(root)?;
    let selector = leaf_matches(root).get_one::<String>("machine").cloned();
    let device = runtime()?
        .block_on(async {
            let mut client = options.connect().await?;
            let target = selector
                .map(MachineSelector::parse)
                .transpose()
                .map_err(|error| error.to_string())?;
            client
                .call::<op::InspectWireguard>(InspectWireGuardRequest {}, target.as_ref())
                .await
                .map_err(|error| Error::from(error.to_string()))
        })?
        .device;
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
        if let Some(rtt) = peer.rtt {
            println!("  rtt: {:?}", Duration::from_nanos(rtt.median_ns));
        }
    }
    Ok(())
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
                subnet: MachineSubnet("10.210.7.0/24".parse().unwrap()),
                management_address: ployz_core::ManagementAddress("fdcc::7".parse().unwrap()),
                public_key: ployz_core::WireGuardPublicKey([7; 32]),
                public_ip: None,
                advertised_endpoints: Vec::new(),
                runtime: Default::default(),
            },
            membership: ployz_core::MembershipObservation::Up,
            selected_endpoint: None,
        };
        let output = serde_json::to_value(MachineObservationOutput {
            gateway: gateway(observation.machine.subnet),
            observation: &observation,
        })
        .unwrap();
        assert_eq!(output.get("gateway").unwrap(), "10.210.7.1");
    }
}
