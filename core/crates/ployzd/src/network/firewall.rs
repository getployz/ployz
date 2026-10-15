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
/// One chain per build slot, hooked into OUTPUT, so slots never rewrite each
/// other's fence.
const BUILD_CHAIN: &str = "PLOYZ-BUILD";
/// Hooked first in mangle FORWARD for traffic off any build bridge.
const BUILD_FORWARD_CHAIN: &str = "PLOYZ-BUILD-FORWARD";

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
    ipv4_document.push_str(&build_forward_document(
        "iptables",
        &ipv4_build_forwarding(),
    )?);
    restore("iptables", &ipv4_document)?;
    let mut ipv6_document = filter_document("ip6tables", &management_policy(management_address))?;
    ipv6_document.push_str(&build_forward_document(
        "ip6tables",
        &ipv6_build_forwarding(),
    )?);
    restore("ip6tables", &ipv6_document)?;
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
    Ok(())
}

/// Build steps route to the internet, never into the Machine's services, across
/// the mesh, to a cloud metadata service or to a private network. Mangle runs
/// before BuildKit's CNI accepts its own bridge's traffic in filter. A
/// published port is public, so DNAT to it still passes, and so does DNS to any
/// resolver, since the host's may sit on the LAN or on link-local.
fn ipv4_build_forwarding() -> Vec<String> {
    build_forwarding(
        vec![
            format!("-o {WIREGUARD_INTERFACE_NAME} -j DROP"),
            format!("-o {DOCKER_NETWORK_NAME} -m conntrack ! --ctstate DNAT -j DROP"),
            "-m conntrack --ctstate DNAT -j RETURN".to_owned(),
        ],
        &[
            LINK_LOCAL,
            "10.0.0.0/8",
            "172.16.0.0/12",
            "192.168.0.0/16",
            "100.64.0.0/10",
        ],
    )
}

/// Build steps have no IPv6 route today; this holds if they ever get one.
fn ipv6_build_forwarding() -> Vec<String> {
    build_forwarding(
        vec![format!("-o {WIREGUARD_INTERFACE_NAME} -j DROP")],
        &["fc00::/7", "fe80::/10"],
    )
}

fn build_forwarding(mut rules: Vec<String>, private: &[&str]) -> Vec<String> {
    rules.extend(
        ["udp", "tcp"]
            .iter()
            .map(|protocol| format!("-p {protocol} --dport {DNS_PORT} -j RETURN")),
    );
    rules.extend(
        private
            .iter()
            .map(|destination| format!("-d {destination} -j DROP")),
    );
    rules
}

fn build_forward_jump() -> String {
    format!(
        "-i {}+ -j {BUILD_FORWARD_CHAIN}",
        ployz_build::BUILD_BRIDGE_PREFIX
    )
}

fn build_forward_document(program: &str, rules: &[String]) -> Result<String, NetworkError> {
    chain_document(
        program,
        "mangle",
        "FORWARD",
        &build_forward_jump(),
        BUILD_FORWARD_CHAIN,
        rules,
    )
}

const LINK_LOCAL: &str = "169.254.0.0/16";
const DNS_PORT: u16 = 53;

/// Fence one build slot's BuildKit worker. BuildKit runs on the host network,
/// so a Dockerfile `ADD` of a URL would otherwise reach everything the host
/// reaches. Fenced, it gets the internet, DNS, its own image contexts and the
/// mesh image servers, and nothing else on the host or the mesh.
///
/// Each family's chain and its OUTPUT hook land in one restore transaction.
/// The hook goes first in OUTPUT, ahead of any loopback accept.
///
/// # Errors
///
/// Returns why the fence could not be installed. The Build must not solve.
pub fn fence_builder(fence: &ployz_build::Fence) -> Result<(), String> {
    let chain = format!("{BUILD_CHAIN}{}", fence.slot);
    for (program, rules) in [
        ("iptables", ipv4_fence(fence)),
        ("ip6tables", ipv6_fence(fence)),
    ] {
        chain_document(
            program,
            "filter",
            "OUTPUT",
            &format!("-j {chain}"),
            &chain,
            &rules,
        )
        .and_then(|document| restore(program, &document))
        .map_err(|error| ployz_core::error_chain::inline(&error))?;
    }
    Ok(())
}

/// Loopback DNS passes, then `drops` fall on the worker's sockets alone. The
/// cgroup sits on each drop, not on a leading RETURN: a SYN-ACK or reset the
/// kernel sends without a full socket never matches a cgroup, so a negated
/// match would fence every host listener's replies.
fn fenced(
    fence: &ployz_build::Fence,
    loopback: &str,
    allowed: Vec<String>,
    drops: &[String],
) -> Vec<String> {
    let mut rules: Vec<String> = ["udp", "tcp"]
        .iter()
        .map(|protocol| format!("-d {loopback} -p {protocol} --dport {DNS_PORT} -j RETURN"))
        .collect();
    rules.extend(allowed.into_iter().map(|rule| format!("{rule} -j RETURN")));
    let cgroup = fence.cgroup.as_str();
    rules.push(format!(
        "-m cgroup --path {cgroup} -m addrtype --dst-type LOCAL -j DROP"
    ));
    rules.extend(
        drops
            .iter()
            .map(|rule| format!("-m cgroup --path {cgroup} {rule} -j DROP")),
    );
    rules
}

fn ipv4_fence(fence: &ployz_build::Fence) -> Vec<String> {
    let allowed = fence
        .image_ports
        .iter()
        .map(|port| format!("-d 127.0.0.1/32 -p tcp --dport {port}"))
        .collect();
    let mut drops: Vec<String> = [
        &format!("{}+", ployz_build::BUILD_BRIDGE_PREFIX),
        DOCKER_NETWORK_NAME,
        WIREGUARD_INTERFACE_NAME,
        "docker0",
    ]
    .iter()
    .map(|output| format!("-o {output}"))
    .collect();
    for destination in [
        super::default_cluster_network().to_string(),
        LINK_LOCAL.into(),
    ] {
        drops.push(format!("-d {destination}"));
    }
    fenced(fence, "127.0.0.0/8", allowed, &drops)
}

fn ipv6_fence(fence: &ployz_build::Fence) -> Vec<String> {
    let allowed = vec![format!("-d fdcc::/16 -p tcp --dport {UNREGISTRY_PORT}")];
    let drops = ["-d fdcc::/16".to_owned(), "-d fe80::/10".to_owned()];
    fenced(fence, "::1/128", allowed, &drops)
}

/// Build steps run on their slot's bridge and get only the internet: nothing
/// on the host answers them, on any address.
fn mesh_ports() -> Vec<String> {
    let mut rules = vec![format!("-i {}+ -j DROP", ployz_build::BUILD_BRIDGE_PREFIX)];
    rules.extend(
        [WIREGUARD_PORT, MANAGEMENT_PORT]
            .into_iter()
            .map(|port| format!("-p udp --dport {port} -j ACCEPT")),
    );
    rules
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
    rules.push(format!(
        "-d {management} -p udp --dport {CORROSION_GOSSIP_PORT} -j DROP"
    ));
    rules
}

fn filter_document(program: &str, rules: &[String]) -> Result<String, NetworkError> {
    chain_document(
        program,
        "filter",
        "INPUT",
        &format!("-j {INPUT_CHAIN}"),
        INPUT_CHAIN,
        rules,
    )
}

/// Declaring the chain in a `--noflush` restore replaces its rules in the same
/// commit that fills it again. `jump` enters `chain` from `hook`, first.
fn chain_document(
    program: &str,
    table: &str,
    hook: &str,
    jump: &str,
    chain: &str,
    rules: &[String],
) -> Result<String, NetworkError> {
    let mut document = format!("*{table}\n:{chain} - [0:0]\n");
    for rule in rules {
        document.push_str(&format!("-A {chain} {rule}\n"));
    }
    document.push_str(&move_to_top(program, table, hook, jump)?);
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
        .args(["--noflush", "--wait"])
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
        attempt(remove_chain(
            program,
            "filter",
            "INPUT",
            &format!("-j {INPUT_CHAIN}"),
            INPUT_CHAIN,
        ));
        attempt(remove_chain(
            program,
            "mangle",
            "FORWARD",
            &build_forward_jump(),
            BUILD_FORWARD_CHAIN,
        ));
        attempt(remove_build_fences(program));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(NetworkError::Io(std::io::Error::other(failures.join("; "))))
    }
}

fn remove_chain(
    program: &str,
    table: &str,
    hook: &str,
    jump: &str,
    chain: &str,
) -> Result<(), NetworkError> {
    let jump: Vec<&str> = jump.split(' ').collect();
    delete_rule(program, table, hook, &jump)?;
    if command_succeeds(program, &["-t", table, "-n", "-L", chain]) {
        checked_command(program, &["-t", table, "-F", chain])?;
        checked_command(program, &["-t", table, "-X", chain])?;
    }
    Ok(())
}

/// Every slot's fence, whichever slots this Machine ever ran.
fn remove_build_fences(program: &str) -> Result<(), NetworkError> {
    let listing = checked_command(program, &["-t", "filter", "-S"])?;
    let listing = String::from_utf8_lossy(&listing.stdout);
    let hooks = build_fence_hooks(&listing);
    for chain in &hooks.hooked {
        delete_rule(program, "filter", "OUTPUT", &["-j", chain])?;
    }
    for chain in &hooks.chains {
        checked_command(program, &["-t", "filter", "-F", chain])?;
        checked_command(program, &["-t", "filter", "-X", chain])?;
    }
    Ok(())
}

#[derive(Debug, Default, Eq, PartialEq)]
struct BuildFenceHooks {
    hooked: Vec<String>,
    chains: Vec<String>,
}

fn build_fence_hooks(listing: &str) -> BuildFenceHooks {
    let slot_chain = |name: &str| {
        name.strip_prefix(BUILD_CHAIN)
            .is_some_and(|slot| !slot.is_empty() && slot.bytes().all(|b| b.is_ascii_digit()))
    };
    let mut hooks = BuildFenceHooks::default();
    for line in listing.lines() {
        if let Some(chain) = line.strip_prefix("-A OUTPUT -j ")
            && slot_chain(chain)
        {
            hooks.hooked.push(chain.to_owned());
        } else if let Some(chain) = line.strip_prefix("-N ")
            && slot_chain(chain)
        {
            hooks.chains.push(chain.to_owned());
        }
    }
    hooks
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fence(image_ports: Vec<u16>) -> ployz_build::Fence {
        ployz_build::Fence {
            slot: 2,
            cgroup: ployz_build::CgroupPath::new("/system.slice/docker-abc.scope").unwrap(),
            image_ports,
        }
    }

    #[test]
    fn the_worker_reaches_dns_and_its_image_contexts_but_nothing_else_on_the_host() {
        assert_eq!(
            ipv4_fence(&fence(vec![41_234])),
            [
                "-d 127.0.0.0/8 -p udp --dport 53 -j RETURN",
                "-d 127.0.0.0/8 -p tcp --dport 53 -j RETURN",
                "-d 127.0.0.1/32 -p tcp --dport 41234 -j RETURN",
                "-m cgroup --path /system.slice/docker-abc.scope -m addrtype --dst-type LOCAL -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -o ployz-build+ -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -o ployz -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -o ployz-wg -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -o docker0 -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -d 10.210.0.0/16 -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -d 169.254.0.0/16 -j DROP",
            ]
        );
    }

    #[test]
    fn the_worker_reaches_only_the_image_servers_across_the_mesh() {
        assert_eq!(
            ipv6_fence(&fence(vec![41_234])),
            [
                "-d ::1/128 -p udp --dport 53 -j RETURN",
                "-d ::1/128 -p tcp --dport 53 -j RETURN",
                "-d fdcc::/16 -p tcp --dport 7572 -j RETURN",
                "-m cgroup --path /system.slice/docker-abc.scope -m addrtype --dst-type LOCAL -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -d fdcc::/16 -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -d fe80::/10 -j DROP",
            ]
        );
    }

    #[test]
    fn build_steps_reach_published_ports_and_dns_but_not_the_mesh_metadata_or_a_private_network() {
        assert_eq!(
            ipv4_build_forwarding(),
            [
                "-o ployz-wg -j DROP",
                "-o ployz -m conntrack ! --ctstate DNAT -j DROP",
                "-m conntrack --ctstate DNAT -j RETURN",
                "-p udp --dport 53 -j RETURN",
                "-p tcp --dport 53 -j RETURN",
                "-d 169.254.0.0/16 -j DROP",
                "-d 10.0.0.0/8 -j DROP",
                "-d 172.16.0.0/12 -j DROP",
                "-d 192.168.0.0/16 -j DROP",
                "-d 100.64.0.0/10 -j DROP",
            ]
        );
    }

    #[test]
    fn build_steps_never_reach_the_mesh_or_a_private_network_over_ipv6() {
        assert_eq!(
            ipv6_build_forwarding(),
            [
                "-o ployz-wg -j DROP",
                "-p udp --dport 53 -j RETURN",
                "-p tcp --dport 53 -j RETURN",
                "-d fc00::/7 -j DROP",
                "-d fe80::/10 -j DROP",
            ]
        );
    }

    #[test]
    fn teardown_finds_every_slots_fence_and_nothing_else() {
        let listing = "-P OUTPUT ACCEPT\n-N PLOYZ-BUILD0\n-N PLOYZ-BUILD12\n-N PLOYZ-BUILDX\n-N PLOYZ-BUILD-FORWARD\n-N PLOYZ-INPUT\n-A OUTPUT -j PLOYZ-BUILD12\n-A OUTPUT -j PLOYZ-BUILD0\n-A OUTPUT -j ufw-before-output\n-A FORWARD -i ployz-build+ -j PLOYZ-BUILD-FORWARD\n-A INPUT -j PLOYZ-INPUT\n";
        assert_eq!(
            build_fence_hooks(listing),
            BuildFenceHooks {
                hooked: vec!["PLOYZ-BUILD12".into(), "PLOYZ-BUILD0".into()],
                chains: vec!["PLOYZ-BUILD0".into(), "PLOYZ-BUILD12".into()],
            }
        );
    }
}
