use std::{net::Ipv4Addr, time::Duration};

use clap::ArgMatches;
use ployz_core::{
    InspectRequest, InspectWireGuardRequest, MachineFailure, MachineObservation, MachineSelector,
    MachineSubnet, MachineSuccess, PartialResult, RttObservation, RttStatistics, op,
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

#[must_use]
fn format_measured_rtt(median_ns: u64) -> Option<String> {
    (median_ns > 0).then(|| format!("{:?}", Duration::from_nanos(median_ns)))
}

#[must_use]
fn wg_rtt_line(rtt: Option<&RttStatistics>) -> Option<String> {
    rtt.and_then(|statistics| format_measured_rtt(statistics.median_ns))
        .map(|value| format!("  rtt: {value}"))
}

#[must_use]
fn format_rtt_table(result: &PartialResult<Vec<RttObservation>, String>) -> String {
    let mut table = String::from("SOURCE\tTARGET\tMEDIAN\tSTDDEV\n");
    for success in &result.successes {
        for observation in &success.value {
            let Some(median) = format_measured_rtt(observation.statistics.median_ns) else {
                continue;
            };
            let target = observation
                .machine
                .as_ref()
                .map_or(observation.peer_id.as_str(), |machine| machine.id.as_str());
            let stddev = format_measured_rtt(observation.statistics.population_stddev_ns)
                .unwrap_or_default();
            table.push_str(&format!(
                "{}\t{target}\t{median}\t{stddev}\n",
                success.machine_id
            ));
        }
    }
    table
}

fn print_rtts(result: &PartialResult<Vec<RttObservation>, String>) {
    print!("{}", format_rtt_table(result));
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
    for peer in device.peers {
        println!();
        println!("peer: {}", peer.public_key);
        if let Some(machine) = peer.machine {
            println!("  machine: {} ({})", machine.name, machine.id);
        }
        if let Some(endpoint) = peer.endpoint {
            println!("  endpoint: {endpoint}");
        }
        if let Some(handshake) = peer.last_handshake_unix_seconds {
            println!("  latest handshake: {handshake}");
        }
        println!(
            "  transfer: {} received, {} sent",
            peer.received_bytes, peer.sent_bytes
        );
        println!(
            "  allowed ips: {}",
            peer.allowed_ips
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
        if let Some(line) = wg_rtt_line(peer.rtt.as_ref()) {
            println!("{line}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::{MachineId, MachineIdentity, MachineName};

    #[test]
    fn missing_or_zero_rtt_is_omitted_instead_of_printing_0ns() {
        assert_eq!(format_measured_rtt(0), None);
        assert_eq!(wg_rtt_line(None), None);
        assert_eq!(
            wg_rtt_line(Some(&RttStatistics {
                median_ns: 0,
                population_stddev_ns: 0,
            })),
            None
        );

        let table = format_rtt_table(&PartialResult {
            successes: vec![MachineSuccess {
                machine_id: machine_id('1'),
                value: vec![rtt_observation("peer-zero", 0, 0)],
            }],
            failures: Vec::new(),
            omissions: Vec::new(),
        });
        assert_eq!(table, "SOURCE\tTARGET\tMEDIAN\tSTDDEV\n");
        assert!(!table.contains("0ns"));
    }

    #[test]
    fn measured_rtt_prints_the_same_human_units_for_machine_rtt_and_wg_show() {
        assert_eq!(format_measured_rtt(1_500_000).as_deref(), Some("1.5ms"));
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
                machine_id: source.clone(),
                value: vec![RttObservation {
                    peer_id: "peer-live".into(),
                    address: "[fdcc::2]:51001".parse().unwrap(),
                    machine: Some(MachineIdentity {
                        id: target.clone(),
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
