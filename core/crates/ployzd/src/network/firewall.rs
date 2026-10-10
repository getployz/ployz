use std::{
    io::Write,
    process::{Command, Stdio},
};

use ployz_core::{MANAGEMENT_PORT, MachineSubnet, ManagementAddress, VOLUME_SEND_PORT};

use crate::dns;

use super::{
    CORROSION_GOSSIP_PORT, DOCKER_NETWORK_NAME, MACHINE_API_PORT, NetworkError, UNREGISTRY_PORT,
    WIREGUARD_INTERFACE_NAME, WIREGUARD_PORT, checked_command,
};

const INPUT_CHAIN: &str = "PLOYZ-INPUT";

/// Apply the Machine's ingress and forwarding policy, including the private
/// Machine API, Direct Image Transfer and Volume send endpoints on `management_address`.
///
/// Each address family's policy lands in one restore transaction, so a running
/// Machine never passes through a state that admits less or more than either the
/// old policy or the new one.
///
/// # Errors
///
/// Returns an error when an iptables command cannot be executed or rejects a
/// required rule.
pub fn apply_firewall_rules(
    subnet: MachineSubnet,
    management_address: ManagementAddress,
) -> Result<(), NetworkError> {
    let gateway = subnet.gateway().0;
    let dns_port = dns::PORT;
    let mut ipv4 = mesh_ports();
    for protocol in ["udp", "tcp"] {
        ipv4.push(format!(
            "-i {DOCKER_NETWORK_NAME} -d {gateway} -p {protocol} --dport {dns_port} -j ACCEPT"
        ));
    }
    let mut ipv4_document = filter_document("iptables", &ipv4)?;
    let nat_moves = move_to_top(
        "iptables",
        "nat",
        "POSTROUTING",
        &format!("-s {subnet} -o {WIREGUARD_INTERFACE_NAME} -j RETURN"),
    )?;
    if !nat_moves.is_empty() {
        ipv4_document.push_str(&format!("*nat\n{nat_moves}COMMIT\n"));
    }
    restore("iptables", &ipv4_document)?;
    restore(
        "ip6tables",
        &filter_document("ip6tables", &management_policy(management_address))?,
    )?;
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
    )
}

fn mesh_ports() -> Vec<String> {
    [WIREGUARD_PORT, MANAGEMENT_PORT]
        .into_iter()
        .map(|port| format!("-p udp --dport {port} -j ACCEPT"))
        .collect()
}

/// Accepts come first: the drops close the management ports to everything the
/// accepts don't name.
fn management_policy(management_address: ManagementAddress) -> Vec<String> {
    let management = format!("{}/128", management_address.0);
    let mut rules = mesh_ports();
    for (protocol, port) in [("tcp", MACHINE_API_PORT), ("udp", CORROSION_GOSSIP_PORT)] {
        rules.push(format!(
            "-i {WIREGUARD_INTERFACE_NAME} -s fdcc::/16 -p {protocol} --dport {port} -j ACCEPT"
        ));
    }
    for port in [UNREGISTRY_PORT, VOLUME_SEND_PORT] {
        rules.push(format!(
            "-i {WIREGUARD_INTERFACE_NAME} -s fdcc::/16 -d {management} -p tcp --dport {port} -j ACCEPT"
        ));
    }
    rules.push(format!(
        "-i lo -s {management} -d {management} -p tcp --sport {UNREGISTRY_PORT} -j ACCEPT"
    ));
    rules.push(format!(
        "-i lo -d {management} -p tcp --dport {UNREGISTRY_PORT} -j ACCEPT"
    ));
    for port in [MACHINE_API_PORT, UNREGISTRY_PORT, VOLUME_SEND_PORT] {
        rules.push(format!("-d {management} -p tcp --dport {port} -j DROP"));
    }
    rules
}

/// Declaring the chain in a `--noflush` restore replaces its rules in the same
/// commit that fills it again.
fn filter_document(program: &str, rules: &[String]) -> Result<String, NetworkError> {
    let mut document = format!("*filter\n:{INPUT_CHAIN} - [0:0]\n");
    for rule in rules {
        document.push_str(&format!("-A {INPUT_CHAIN} {rule}\n"));
    }
    document.push_str(&move_to_top(
        program,
        "filter",
        "INPUT",
        &format!("-j {INPUT_CHAIN}"),
    )?);
    document.push_str("COMMIT\n");
    Ok(document)
}

/// Restore lines that make `rule` the first in `chain`; none when it already is.
fn move_to_top(
    program: &str,
    table: &str,
    chain: &str,
    rule: &str,
) -> Result<String, NetworkError> {
    let listing = checked_command(program, &["-t", table, "-S", chain])?;
    let expected = format!("-A {chain} {rule}");
    let first = String::from_utf8_lossy(&listing.stdout)
        .lines()
        .find(|line| line.starts_with("-A "))
        .map(str::to_owned);
    if first.as_deref() == Some(expected.as_str()) {
        return Ok(String::new());
    }
    let mut check = vec!["-t", table, "-C", chain];
    check.extend(rule.split(' '));
    let mut moves = String::new();
    if command_succeeds(program, &check) {
        moves.push_str(&format!("-D {chain} {rule}\n"));
    }
    moves.push_str(&format!("-I {chain} 1 {rule}\n"));
    Ok(moves)
}

fn restore(program: &str, document: &str) -> Result<(), NetworkError> {
    let program = format!("{program}-restore");
    let mut child = Command::new(&program)
        .arg("--noflush")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(document.as_bytes())?;
    let output = child.wait_with_output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(NetworkError::Command {
            program,
            stderr: String::from_utf8_lossy(&output.stderr).trim().into(),
        })
    }
}

pub(super) fn remove_firewall_rules(subnet: MachineSubnet) -> Result<(), NetworkError> {
    let mut failures = Vec::new();
    let mut attempt = |result: Result<(), NetworkError>| {
        if let Err(error) = result {
            failures.push(ployz_core::error_chain::inline(&error));
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
        if command_succeeds(program, &["-t", "filter", "-n", "-L", INPUT_CHAIN]) {
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
