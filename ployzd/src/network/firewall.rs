use std::process::Command;

use ployz_core::MachineSubnet;

use crate::dns;

use super::{
    CORROSION_GOSSIP_PORT, DOCKER_NETWORK_NAME, MACHINE_API_PORT, NetworkError, UNREGISTRY_PORT,
    WIREGUARD_INTERFACE_NAME, WIREGUARD_PORT, checked_command,
};

const INPUT_CHAIN: &str = "PLOYZ-INPUT";

pub fn apply_firewall_rules(subnet: MachineSubnet) -> Result<(), NetworkError> {
    for program in ["iptables", "ip6tables"] {
        ensure_chain(program, INPUT_CHAIN)?;
        replace_first_rule(program, "filter", "INPUT", &["-j", INPUT_CHAIN])?;
        ensure_rule(
            program,
            "filter",
            INPUT_CHAIN,
            &[
                "-p",
                "udp",
                "--dport",
                &WIREGUARD_PORT.to_string(),
                "-j",
                "ACCEPT",
            ],
        )?;
    }
    // TODO(UT-105): allow access only from the machine IPs (10.210.N.1) but not the containers running on them. Use ipset?
    for port in [UNREGISTRY_PORT, MACHINE_API_PORT] {
        ensure_rule(
            "iptables",
            "filter",
            INPUT_CHAIN,
            &[
                "-i",
                WIREGUARD_INTERFACE_NAME,
                "-d",
                &subnet.gateway().0.to_string(),
                "-p",
                "tcp",
                "--dport",
                &port.to_string(),
                "-j",
                "ACCEPT",
            ],
        )?;
    }
    for protocol in ["udp", "tcp"] {
        ensure_rule(
            "iptables",
            "filter",
            INPUT_CHAIN,
            &[
                "-i",
                DOCKER_NETWORK_NAME,
                "-d",
                &subnet.gateway().0.to_string(),
                "-p",
                protocol,
                "--dport",
                &dns::PORT.to_string(),
                "-j",
                "ACCEPT",
            ],
        )?;
    }
    for (protocol, port) in [("tcp", MACHINE_API_PORT), ("udp", CORROSION_GOSSIP_PORT)] {
        ensure_rule(
            "ip6tables",
            "filter",
            INPUT_CHAIN,
            &[
                "-i",
                WIREGUARD_INTERFACE_NAME,
                "-s",
                "fdcc::/16",
                "-p",
                protocol,
                "--dport",
                &port.to_string(),
                "-j",
                "ACCEPT",
            ],
        )?;
    }
    ensure_rule(
        "iptables",
        "filter",
        "DOCKER-USER",
        &[
            "-i",
            WIREGUARD_INTERFACE_NAME,
            "-o",
            DOCKER_NETWORK_NAME,
            "-j",
            "ACCEPT",
        ],
    )?;
    let nat_rule = [
        "-s",
        &subnet.to_string(),
        "-o",
        WIREGUARD_INTERFACE_NAME,
        "-j",
        "RETURN",
    ];
    delete_rule("iptables", "nat", "POSTROUTING", &nat_rule)?;
    let mut insert = vec!["-t", "nat", "-I", "POSTROUTING", "1"];
    insert.extend(nat_rule);
    checked_command("iptables", &insert)?;
    Ok(())
}

pub(super) fn remove_firewall_rules(subnet: MachineSubnet) -> Result<(), NetworkError> {
    let mut failures = Vec::new();
    let mut attempt = |result: Result<(), NetworkError>| {
        if let Err(error) = result {
            failures.push(error.to_string());
        }
    };
    attempt(delete_rule(
        "iptables",
        "filter",
        "DOCKER-USER",
        &[
            "-i",
            WIREGUARD_INTERFACE_NAME,
            "-o",
            DOCKER_NETWORK_NAME,
            "-j",
            "ACCEPT",
        ],
    ));
    attempt(delete_rule(
        "iptables",
        "nat",
        "POSTROUTING",
        &[
            "-s",
            &subnet.to_string(),
            "-o",
            WIREGUARD_INTERFACE_NAME,
            "-j",
            "RETURN",
        ],
    ));
    for program in ["iptables", "ip6tables"] {
        attempt(delete_rule(
            program,
            "filter",
            "INPUT",
            &["-j", INPUT_CHAIN],
        ));
        if command_succeeds(program, &["-t", "filter", "-L", INPUT_CHAIN]) {
            attempt(checked_command(program, &["-t", "filter", "-F", INPUT_CHAIN]).map(|_| ()));
            attempt(checked_command(program, &["-t", "filter", "-X", INPUT_CHAIN]).map(|_| ()));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(NetworkError::Io(std::io::Error::other(failures.join("; "))))
    }
}

fn ensure_chain(program: &str, chain: &str) -> Result<(), NetworkError> {
    if !command_succeeds(program, &["-t", "filter", "-L", chain]) {
        checked_command(program, &["-t", "filter", "-N", chain])?;
    }
    checked_command(program, &["-t", "filter", "-F", chain])?;
    Ok(())
}

fn ensure_rule(program: &str, table: &str, chain: &str, rule: &[&str]) -> Result<(), NetworkError> {
    let mut check = vec!["-t", table, "-C", chain];
    check.extend_from_slice(rule);
    if !command_succeeds(program, &check) {
        let mut insert = vec!["-t", table, "-I", chain, "1"];
        insert.extend_from_slice(rule);
        checked_command(program, &insert)?;
    }
    Ok(())
}

fn replace_first_rule(
    program: &str,
    table: &str,
    chain: &str,
    rule: &[&str],
) -> Result<(), NetworkError> {
    delete_rule(program, table, chain, rule)?;
    let mut insert = vec!["-t", table, "-I", chain, "1"];
    insert.extend_from_slice(rule);
    checked_command(program, &insert)?;
    Ok(())
}

fn delete_rule(program: &str, table: &str, chain: &str, rule: &[&str]) -> Result<(), NetworkError> {
    let mut check = vec!["-t", table, "-C", chain];
    check.extend_from_slice(rule);
    if command_succeeds(program, &check) {
        let mut delete = vec!["-t", table, "-D", chain];
        delete.extend_from_slice(rule);
        checked_command(program, &delete)?;
    }
    Ok(())
}

fn command_succeeds(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .output()
        .is_ok_and(|output| output.status.success())
}
