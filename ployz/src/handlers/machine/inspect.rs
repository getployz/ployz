use std::{net::Ipv4Addr, time::Duration};

use clap::ArgMatches;
use ployz_core::{
    InspectRequest, MachineFailure, MachineObservation, MachineSelector, MachineSubnet,
    MachineSuccess, PartialResult, RpcRequest, RttObservation,
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
            let request = InspectRequest { include_rtts: true };
            match client
                .request(RpcRequest::inspect(request), Some(&selector))
                .await
            {
                Ok(response) => match response.decode_machine_details() {
                    Ok(details) => result.successes.push(MachineSuccess {
                        machine_id: id,
                        value: details.rtts.clone(),
                    }),
                    Err(error) => result.failures.push(MachineFailure {
                        machine_id: id,
                        error: error.to_string(),
                    }),
                },
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
    let device = runtime()?.block_on(async {
        let mut client = options.connect().await?;
        let target = selector
            .map(MachineSelector::parse)
            .transpose()
            .map_err(|error| error.to_string())?;
        client
            .request(RpcRequest::inspect_wireguard(), target.as_ref())
            .await
            .map_err(|error| error.to_string())?
            .decode_wireguard_inspected()
            .cloned()
            .map_err(|error| error.to_string())
    })?;
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
        if let Some(rtt) = peer.rtt {
            println!("  rtt: {:?}", Duration::from_nanos(rtt.median_ns));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
