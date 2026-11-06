use std::{
    io::Write,
    process::{Command, Output, Stdio},
    sync::{Mutex, MutexGuard, PoisonError},
};

use ployz_core::{MANAGEMENT_PORT, MachineSubnet, ManagementAddress, VOLUME_SEND_PORT};

use crate::dns;

use super::{
    CORROSION_GOSSIP_PORT, DOCKER_NETWORK_NAME, MACHINE_API_PORT, NetworkError, UNREGISTRY_PORT,
    WIREGUARD_INTERFACE_NAME, WIREGUARD_PORT, docker_firewall::Ipv6Off,
};

const INPUT_CHAIN: &str = "PLOYZ-INPUT";
/// One chain per build slot, hooked into OUTPUT, so slots never rewrite each
/// other's fence.
const BUILD_CHAIN: &str = "PLOYZ-BUILD";
/// Hooked first in mangle FORWARD for traffic off any build bridge.
const BUILD_FORWARD_CHAIN: &str = "PLOYZ-BUILD-FORWARD";

/// Each writer here moves hooks from a listing it read first, and a Build's
/// fence runs beside the network's periodic re-apply. One writer at a time
/// keeps that listing true when the restore lands. The count names the current
/// [`Installed`] firewall: every install and every removal moves it on.
static WRITER: Mutex<u64> = Mutex::new(0);

fn writer() -> MutexGuard<'static, u64> {
    WRITER.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A firewall [`apply_firewall_rules`] installed. It applies again only while it
/// is still the current one, so a re-apply queued before a teardown or a later
/// install never puts back rules for addresses this Machine no longer holds.
#[derive(Clone, Copy, Debug)]
pub struct Installed {
    generation: u64,
    subnet: MachineSubnet,
    management_address: ManagementAddress,
}

/// What a re-apply had to put back.
#[derive(Debug, Default)]
pub struct Restored {
    /// Each hook or ordered rule that was missing or no longer first, as
    /// `<table> <chain>: <rule>`, and the TCPMSS clamp when it was missing.
    pub displaced: Vec<String>,
    /// The mesh's nat POSTROUTING RETURN, which Docker displaces with its own
    /// MASQUERADE each time it creates a network or restarts.
    pub behind_docker_nat: Vec<String>,
}

impl Installed {
    /// Apply this firewall again, as [`apply_firewall_rules`] did. `None` when it
    /// was removed or replaced since, and nothing was written.
    ///
    /// # Errors
    ///
    /// Returns an error when an iptables command fails, as [`apply_firewall_rules`]
    /// does.
    pub fn reapply(&self) -> Result<Option<Restored>, NetworkError> {
        let generation = writer();
        if *generation != self.generation {
            return Ok(None);
        }
        install(self.subnet, self.management_address).map(Some)
    }
}

/// Apply the Machine's ingress and forwarding policy, including the private
/// Machine API, Direct Image Transfer and Volume send endpoints on `management_address`.
///
/// Each address family's policy lands in one restore transaction, so a running
/// Machine never passes through a state that admits less or more than either the
/// old policy or the new one. Applying again converges: the Ployz chains are
/// rewritten whole, and each hook and ordered rule goes back to the top of its
/// chain when another tool has displaced or deleted it.
///
/// Returns the firewall now installed, which [`Installed::reapply`] applies
/// again until [`remove_firewall_rules`] or the next install.
///
/// # Errors
///
/// Returns an error when an iptables command cannot be executed or rejects a
/// required rule.
pub fn apply_firewall_rules(
    subnet: MachineSubnet,
    management_address: ManagementAddress,
) -> Result<Installed, NetworkError> {
    let mut generation = writer();
    *generation += 1;
    install(subnet, management_address)?;
    Ok(Installed {
        generation: *generation,
        subnet,
        management_address,
    })
}

/// Called with [`writer`] held.
fn install(
    subnet: MachineSubnet,
    management_address: ManagementAddress,
) -> Result<Restored, NetworkError> {
    let mut restored = Restored::default();
    let moved = &mut restored.displaced;
    let gateway = subnet.gateway().0;
    let dns_port = dns::PORT;
    let mut ipv4 = mesh_ports();
    for protocol in ["udp", "tcp"] {
        ipv4.push(format!(
            "-i {DOCKER_NETWORK_NAME} -d {gateway} -p {protocol} --dport {dns_port} -j ACCEPT"
        ));
    }
    let mut ipv4_document = filter_document("iptables", &ipv4, moved)?;
    let nat_moves = move_to_top(
        "iptables",
        "nat",
        "POSTROUTING",
        &format!("-s {subnet} -o {WIREGUARD_INTERFACE_NAME} -j RETURN"),
        &mut restored.behind_docker_nat,
    )?;
    let moved = &mut restored.displaced;
    if !nat_moves.is_empty() {
        ipv4_document.push_str(&format!("*nat\n{nat_moves}COMMIT\n"));
    }
    let forward_moves = move_to_top(
        "iptables",
        "filter",
        "DOCKER-USER",
        &format!("-i {WIREGUARD_INTERFACE_NAME} -o {DOCKER_NETWORK_NAME} -j ACCEPT"),
        moved,
    )?;
    if !forward_moves.is_empty() {
        ipv4_document.push_str(&format!("*filter\n{forward_moves}COMMIT\n"));
    }
    ipv4_document.push_str(&build_forward_document(
        "iptables",
        &ipv4_build_forwarding(),
        moved,
    )?);
    restore("iptables", &ipv4_document)?;
    add_mss_clamp(moved);
    for rule in LOOSE_BUILD_FORWARDING {
        delete_rule("iptables", "mangle", "FORWARD", rule)?;
    }
    let mut ipv6_document =
        filter_document("ip6tables", &management_policy(management_address), moved)?;
    if docker_ipv6_firewall().is_ok() {
        ipv6_document.push_str(&container_mesh_fence_document(moved)?);
    }
    ipv6_document.push_str(&build_forward_document(
        "ip6tables",
        &ipv6_build_forwarding(),
        moved,
    )?);
    restore("ip6tables", &ipv6_document)?;
    Ok(restored)
}

/// A Service Container's TCP to a peer's Service crosses this Machine's router. Its SYN
/// and SYN-ACK leave over WireGuard clamped to the MTU of the peer's route, which the
/// path probe lowers on a path that loses large packets, so the far end sends this
/// container segments that cross. The container's own segments are bounded by the
/// route MTU as they're forwarded. On a full route it changes nothing. Container IPv6 never enters the mesh
/// (see [`container_mesh_fence`]), so IPv4 is the whole of it.
fn mss_clamp() -> String {
    format!(
        "-o {WIREGUARD_INTERFACE_NAME} -p tcp -m tcp --tcp-flags SYN,RST SYN -j TCPMSS --clamp-mss-to-pmtu"
    )
}

/// Add [`mss_clamp`] outside the policy's restore: a kernel without the TCPMSS target
/// loses only the clamp, and the route MTU still covers the Machine's own TCP. An
/// added clamp is recorded in `moved`.
fn add_mss_clamp(moved: &mut Vec<String>) {
    let rule = mss_clamp();
    let words: Vec<&str> = rule.split(' ').collect();
    let added = rule_exists("iptables", "mangle", "FORWARD", &words).and_then(|exists| {
        if exists {
            return Ok(false);
        }
        let mut append = vec!["-t", "mangle", "-A", "FORWARD"];
        append.extend_from_slice(&words);
        xtables("iptables", &append).map(|_| true)
    });
    match added {
        Ok(true) => moved.push(format!("mangle FORWARD: {rule}")),
        Ok(false) => {}
        Err(error) => eprintln!(
            "failed to clamp container TCP to the mesh path MTU: {error}",
            error = ployz_core::error_chain::inline(&error),
        ),
    }
}

/// Docker's NAT66 would rewrite a container's IPv6 into the mesh to this Machine's
/// management address, which every peer trusts. Nothing in the mesh dials a
/// container over IPv6, so even replies drop: a container can forge the state of a
/// DNAT'd connection through hairpin NAT. Container IPv4 into the mesh keeps its own
/// source and reaches only containers, so it stays open.
fn container_mesh_fence() -> String {
    format!("-i {DOCKER_NETWORK_NAME} -o {WIREGUARD_INTERFACE_NAME} -j DROP")
}

fn container_mesh_fence_document(moved: &mut Vec<String>) -> Result<String, NetworkError> {
    let moves = move_to_top(
        "ip6tables",
        "filter",
        "DOCKER-USER",
        &container_mesh_fence(),
        moved,
    )?;
    Ok(if moves.is_empty() {
        moves
    } else {
        format!("*filter\n{moves}COMMIT\n")
    })
}

/// Docker filters IPv6 itself, so DOCKER-USER exists for the mesh fence. Without it
/// the bridge gets no IPv6. The chain outlives a Docker restart that turns
/// ip6tables off, so dockerd's own settings decide, and name the cause first.
pub(super) fn docker_ipv6_firewall() -> Result<(), Ipv6Off> {
    super::docker_firewall::docker_filters_ipv6()?;
    if docker_user_chain() {
        Ok(())
    } else {
        Err(Ipv6Off::NoDockerUserChain)
    }
}

fn docker_user_chain() -> bool {
    command_succeeds("ip6tables", &["-t", "filter", "-S", "DOCKER-USER"])
}

/// Install the mesh fence on its own, ahead of the rest of the firewall.
///
/// # Errors
///
/// Returns an error when ip6tables rejects the rule.
pub(super) fn fence_container_ipv6() -> Result<(), NetworkError> {
    let _writer = writer();
    let document = container_mesh_fence_document(&mut Vec::new())?;
    if document.is_empty() {
        Ok(())
    } else {
        restore("ip6tables", &document)
    }
}

/// Build steps route to the internet, never into the Machine's services, across
/// the mesh, to a cloud metadata service or to a private network. Mangle runs
/// before BuildKit's CNI accepts its own bridge's traffic in filter. DNS passes
/// to any resolver, since the host's may sit on the LAN or on link-local. A port
/// published on a public address passes, DNAT'd to its container; one the step
/// reached through a private address drops on that original destination.
/// Untracked traffic has no original destination, so the closing drops match
/// its destination instead.
fn ipv4_build_forwarding() -> Vec<String> {
    let mut rules = vec![
        format!("-o {WIREGUARD_INTERFACE_NAME} -j DROP"),
        format!("-o {DOCKER_NETWORK_NAME} -m conntrack ! --ctstate DNAT -j DROP"),
    ];
    rules.extend(dns_returns());
    rules.extend(
        PRIVATE_V4
            .iter()
            .map(|destination| format!("-m conntrack --ctorigdst {destination} -j DROP")),
    );
    rules.push("-m conntrack --ctstate DNAT -j RETURN".to_owned());
    rules.extend(
        PRIVATE_V4
            .iter()
            .map(|destination| format!("-d {destination} -j DROP")),
    );
    rules
}

/// Link-local, RFC1918, CGNAT, and Azure's WireServer, which hands out VM
/// configuration from a public address.
const PRIVATE_V4: [&str; 6] = [
    LINK_LOCAL,
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "100.64.0.0/10",
    "168.63.129.16/32",
];

/// Unique local and link-local addresses.
const PRIVATE_V6: [&str; 2] = ["fc00::/7", "fe80::/10"];

/// Build steps have no IPv6 route today; this holds if they ever get one.
fn ipv6_build_forwarding() -> Vec<String> {
    let mut rules = vec![format!("-o {WIREGUARD_INTERFACE_NAME} -j DROP")];
    rules.extend(dns_returns());
    rules.extend(
        PRIVATE_V6
            .iter()
            .map(|destination| format!("-d {destination} -j DROP")),
    );
    rules
}

fn dns_returns() -> impl Iterator<Item = String> {
    ["udp", "tcp"]
        .into_iter()
        .map(|protocol| format!("-p {protocol} --dport {DNS_PORT} -j RETURN"))
}

/// Loose rules main hooked into mangle FORWARD before this chain. A RETURN
/// from the chain resumes FORWARD, where their link-local drop would still
/// catch DNS to a cloud's link-local resolver.
const LOOSE_BUILD_FORWARDING: [&[&str]; 3] = [
    &[
        "-i",
        "ployz-build+",
        "-o",
        "ployz",
        "-m",
        "conntrack",
        "!",
        "--ctstate",
        "DNAT",
        "-j",
        "DROP",
    ],
    &["-i", "ployz-build+", "-o", "ployz-wg", "-j", "DROP"],
    &["-i", "ployz-build+", "-d", "169.254.0.0/16", "-j", "DROP"],
];

fn build_forward_jump() -> String {
    format!(
        "-i {}+ -j {BUILD_FORWARD_CHAIN}",
        ployz_build::BUILD_BRIDGE_PREFIX
    )
}

fn build_forward_document(
    program: &str,
    rules: &[String],
    moved: &mut Vec<String>,
) -> Result<String, NetworkError> {
    chain_document(
        program,
        "mangle",
        "FORWARD",
        &build_forward_jump(),
        BUILD_FORWARD_CHAIN,
        rules,
        moved,
    )
}

const LINK_LOCAL: &str = "169.254.0.0/16";
const DNS_PORT: u16 = 53;

/// Fence one build slot's BuildKit worker. BuildKit runs on the host network,
/// so a Dockerfile `ADD` of a URL would otherwise reach everything the host
/// reaches. Fenced, it gets the internet, DNS, its own image contexts and the
/// mesh image servers, and nothing else on the host, the mesh or a private
/// network, as build steps get. Its `FROM` pulls go through the same fence, so a
/// registry or mirror at a private address is out of reach too.
///
/// Each family's chain and its OUTPUT hook land in one restore transaction.
/// The hook goes first in OUTPUT, ahead of any loopback accept.
///
/// # Errors
///
/// Returns why the fence could not be installed. The Build must not solve.
pub fn fence_builder(fence: &ployz_build::Fence) -> Result<(), String> {
    let _writer = writer();
    let mut moved = Vec::new();
    let chain = format!("{BUILD_CHAIN}{}", fence.slot);
    for (program, rules, forwarding) in [
        ("iptables", ipv4_fence(fence), ipv4_build_forwarding()),
        ("ip6tables", ipv6_fence(fence), ipv6_build_forwarding()),
    ] {
        chain_document(
            program,
            "filter",
            "OUTPUT",
            &format!("-j {chain}"),
            &chain,
            &rules,
            &mut moved,
        )
        .and_then(|fence| Ok(fence + &build_forward_document(program, &forwarding, &mut moved)?))
        .and_then(|document| restore(program, &document))
        .map_err(|error| ployz_core::error_chain::inline(&error))?;
    }
    Ok(())
}

/// Loopback DNS passes, then `drops` fall on the worker's sockets alone, then
/// DNS to any other address passes, since the host's resolver may sit on the
/// LAN or on link-local, then `private` drops. The cgroup sits on each drop,
/// not on a leading RETURN: a SYN-ACK or reset the kernel sends without a full
/// socket never matches a cgroup, so a negated match would fence every host
/// listener's replies.
fn fenced(
    fence: &ployz_build::Fence,
    loopback: &str,
    allowed: Vec<String>,
    drops: &[String],
    private: &[&str],
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
    rules.extend(dns_returns());
    rules.extend(
        private
            .iter()
            .map(|destination| format!("-m cgroup --path {cgroup} -d {destination} -j DROP")),
    );
    rules
}

/// Docker's embedded DNS, which a host on a user-defined Docker network
/// resolves through, DNATs port 53 to a random port on the same address.
/// Filter OUTPUT sees the new port, so only that address may keep its DNS
/// through a redirect: one anywhere else could open any port to the worker.
const DOCKER_EMBEDDED_DNS: &str = "127.0.0.11/32";

fn ipv4_fence(fence: &ployz_build::Fence) -> Vec<String> {
    let allowed = ["udp", "tcp"]
        .iter()
        .map(|protocol| {
            format!(
                "-d {DOCKER_EMBEDDED_DNS} -p {protocol} -m conntrack --ctorigdst {DOCKER_EMBEDDED_DNS} --ctorigdstport {DNS_PORT}"
            )
        })
        .chain(
            fence
                .image_ports
                .iter()
                .map(|port| format!("-d 127.0.0.1/32 -p tcp --dport {port}")),
        )
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
    drops.push(format!("-d {}", super::default_cluster_network()));
    fenced(fence, "127.0.0.0/8", allowed, &drops, &PRIVATE_V4)
}

fn ipv6_fence(fence: &ployz_build::Fence) -> Vec<String> {
    let allowed = vec![format!("-d fdcc::/16 -p tcp --dport {UNREGISTRY_PORT}")];
    let drops = ["-d fdcc::/16".to_owned()];
    fenced(fence, "::1/128", allowed, &drops, &PRIVATE_V6)
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

fn filter_document(
    program: &str,
    rules: &[String],
    moved: &mut Vec<String>,
) -> Result<String, NetworkError> {
    chain_document(
        program,
        "filter",
        "INPUT",
        &format!("-j {INPUT_CHAIN}"),
        INPUT_CHAIN,
        rules,
        moved,
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
    moved: &mut Vec<String>,
) -> Result<String, NetworkError> {
    let mut document = format!("*{table}\n:{chain} - [0:0]\n");
    for rule in rules {
        document.push_str(&format!("-A {chain} {rule}\n"));
    }
    document.push_str(&move_to_top(program, table, hook, jump, moved)?);
    document.push_str("COMMIT\n");
    Ok(document)
}

/// Restore lines that make `rule` the first in `chain`; none when it already is.
/// A rule that needs them is recorded in `moved`.
fn move_to_top(
    program: &str,
    table: &str,
    chain: &str,
    rule: &str,
    moved: &mut Vec<String>,
) -> Result<String, NetworkError> {
    let listing = xtables(program, &["-t", table, "-S", chain])?;
    let expected = format!("-A {chain} {rule}");
    let first = String::from_utf8_lossy(&listing.stdout)
        .lines()
        .find(|line| line.starts_with("-A "))
        .map(str::to_owned);
    if first.as_deref() == Some(expected.as_str()) {
        return Ok(String::new());
    }
    moved.push(format!("{table} {chain}: {rule}"));
    let words: Vec<&str> = rule.split(' ').collect();
    let mut moves = String::new();
    if rule_exists(program, table, chain, &words)? {
        moves.push_str(&format!("-D {chain} {rule}\n"));
    }
    moves.push_str(&format!("-I {chain} 1 {rule}\n"));
    Ok(moves)
}

fn restore(program: &str, document: &str) -> Result<(), NetworkError> {
    let program = format!("{program}-restore");
    let mut child = Command::new(&program)
        .args(["--noflush", &format!("--wait={LOCK_WAIT}")])
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

/// Remove the firewall, and retire the [`Installed`] one so no re-apply puts it
/// back.
pub(super) fn remove_firewall_rules(subnet: MachineSubnet) -> Result<(), NetworkError> {
    let mut generation = writer();
    *generation += 1;
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
    for rule in LOOSE_BUILD_FORWARDING {
        attempt(delete_rule("iptables", "mangle", "FORWARD", rule));
    }
    let clamp = mss_clamp();
    let clamp = clamp.split(' ').collect::<Vec<_>>();
    attempt(delete_rule("iptables", "mangle", "FORWARD", &clamp));
    // A fence left in a chain Docker abandoned is still ours to remove.
    if docker_user_chain() {
        let fence = container_mesh_fence();
        let fence = fence.split(' ').collect::<Vec<_>>();
        attempt(delete_rule("ip6tables", "filter", "DOCKER-USER", &fence));
    }
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
        xtables(program, &["-t", table, "-F", chain])?;
        xtables(program, &["-t", table, "-X", chain])?;
    }
    Ok(())
}

/// Every slot's fence, whichever slots this Machine ever ran.
fn remove_build_fences(program: &str) -> Result<(), NetworkError> {
    let listing = xtables(program, &["-t", "filter", "-S"])?;
    let listing = String::from_utf8_lossy(&listing.stdout);
    let hooks = build_fence_hooks(&listing);
    for chain in &hooks.hooked {
        delete_rule(program, "filter", "OUTPUT", &["-j", chain])?;
    }
    for chain in &hooks.chains {
        xtables(program, &["-t", "filter", "-F", chain])?;
        xtables(program, &["-t", "filter", "-X", chain])?;
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

fn delete_rule(program: &str, table: &str, chain: &str, rule: &[&str]) -> Result<(), NetworkError> {
    while rule_exists(program, table, chain, rule)? {
        let mut delete = vec!["-t", table, "-D", chain];
        delete.extend_from_slice(rule);
        xtables(program, &delete)?;
    }
    Ok(())
}

/// Seconds an iptables command waits for the xtables lock before it fails. A
/// held lock fails the step, and the next re-apply tries again.
const LOCK_WAIT: &str = "10";

fn xtables(program: &str, args: &[&str]) -> Result<Output, NetworkError> {
    super::checked_command(program, &waiting(args))
}

fn command_succeeds(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(waiting(args))
        .output()
        .is_ok_and(|output| output.status.success())
}

fn waiting<'arg>(args: &[&'arg str]) -> Vec<&'arg str> {
    let mut waiting = vec!["-w", LOCK_WAIT];
    waiting.extend_from_slice(args);
    waiting
}

/// Whether `rule` is in `chain`. iptables exits 1 when the rule or the chain is
/// missing, and 2 when the rule jumps to a chain that doesn't exist yet, so
/// neither can hold it. Any other failure, such as a held lock, is an error
/// rather than a missing rule.
fn rule_exists(
    program: &str,
    table: &str,
    chain: &str,
    rule: &[&str],
) -> Result<bool, NetworkError> {
    let mut args = waiting(&["-t", table, "-C", chain]);
    args.extend_from_slice(rule);
    let output = Command::new(program).args(&args).output()?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1 | 2) => Ok(false),
        _ => Err(NetworkError::Command {
            program: program.into(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().into(),
        }),
    }
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
    fn the_worker_reaches_dns_and_its_image_contexts_but_nothing_else_on_the_host_or_a_private_network()
     {
        assert_eq!(
            ipv4_fence(&fence(vec![41_234])),
            [
                "-d 127.0.0.0/8 -p udp --dport 53 -j RETURN",
                "-d 127.0.0.0/8 -p tcp --dport 53 -j RETURN",
                "-d 127.0.0.11/32 -p udp -m conntrack --ctorigdst 127.0.0.11/32 --ctorigdstport 53 -j RETURN",
                "-d 127.0.0.11/32 -p tcp -m conntrack --ctorigdst 127.0.0.11/32 --ctorigdstport 53 -j RETURN",
                "-d 127.0.0.1/32 -p tcp --dport 41234 -j RETURN",
                "-m cgroup --path /system.slice/docker-abc.scope -m addrtype --dst-type LOCAL -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -o ployz-build+ -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -o ployz -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -o ployz-wg -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -o docker0 -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -d 10.210.0.0/16 -j DROP",
                "-p udp --dport 53 -j RETURN",
                "-p tcp --dport 53 -j RETURN",
                "-m cgroup --path /system.slice/docker-abc.scope -d 169.254.0.0/16 -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -d 10.0.0.0/8 -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -d 172.16.0.0/12 -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -d 192.168.0.0/16 -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -d 100.64.0.0/10 -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -d 168.63.129.16/32 -j DROP",
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
                "-p udp --dport 53 -j RETURN",
                "-p tcp --dport 53 -j RETURN",
                "-m cgroup --path /system.slice/docker-abc.scope -d fc00::/7 -j DROP",
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
                "-p udp --dport 53 -j RETURN",
                "-p tcp --dport 53 -j RETURN",
                "-m conntrack --ctorigdst 169.254.0.0/16 -j DROP",
                "-m conntrack --ctorigdst 10.0.0.0/8 -j DROP",
                "-m conntrack --ctorigdst 172.16.0.0/12 -j DROP",
                "-m conntrack --ctorigdst 192.168.0.0/16 -j DROP",
                "-m conntrack --ctorigdst 100.64.0.0/10 -j DROP",
                "-m conntrack --ctorigdst 168.63.129.16/32 -j DROP",
                "-m conntrack --ctstate DNAT -j RETURN",
                "-d 169.254.0.0/16 -j DROP",
                "-d 10.0.0.0/8 -j DROP",
                "-d 172.16.0.0/12 -j DROP",
                "-d 192.168.0.0/16 -j DROP",
                "-d 100.64.0.0/10 -j DROP",
                "-d 168.63.129.16/32 -j DROP",
            ]
        );
    }

    #[test]
    fn containers_never_reach_the_mesh_over_ipv6() {
        assert_eq!(container_mesh_fence(), "-i ployz -o ployz-wg -j DROP");
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

    /// Runs `test` as root in a fresh network namespace with
    /// `PLOYZ_TEARDOWN_CHILD` set, then lists each of `tables` there for both
    /// families.
    fn in_namespace(test: &str, tables: &[&str]) -> Vec<String> {
        let short = test.rsplit("::").next().unwrap().get(..8).unwrap();
        let namespace = format!("ployz-{short}-{}", std::process::id());
        let sudo = |args: &[&str]| Command::new("sudo").arg("-n").args(args).output().unwrap();
        assert!(sudo(&["ip", "netns", "add", &namespace]).status.success());
        let exe = std::env::current_exe().unwrap();
        let child = sudo(&[
            "ip",
            "netns",
            "exec",
            &namespace,
            "env",
            "PLOYZ_TEARDOWN_CHILD=1",
            exe.to_str().unwrap(),
            "--ignored",
            "--exact",
            test,
        ]);
        let left: Vec<_> = ["iptables", "ip6tables"]
            .into_iter()
            .flat_map(|program| tables.iter().map(move |table| (program, *table)))
            .map(|(program, table)| {
                sudo(&[
                    "ip", "netns", "exec", &namespace, program, "-t", table, "-S",
                ])
            })
            .collect();
        sudo(&["ip", "netns", "del", &namespace]);
        assert!(
            child.status.success(),
            "{}",
            String::from_utf8_lossy(&child.stdout)
        );
        left.into_iter()
            .map(|listing| {
                assert!(
                    listing.status.success(),
                    "{}",
                    String::from_utf8_lossy(&listing.stderr)
                );
                String::from_utf8(listing.stdout).unwrap()
            })
            .collect()
    }

    fn in_child() -> bool {
        std::env::var_os("PLOYZ_TEARDOWN_CHILD").is_some()
    }

    /// An earlier daemon's loose rules sit alongside this daemon's chain.
    #[test]
    #[ignore = "needs root and ip netns"]
    fn teardown_removes_the_chain_and_loose_rules_from_earlier_daemons() {
        if in_child() {
            for rule in LOOSE_BUILD_FORWARDING {
                let mut insert = vec!["-t", "mangle", "-I", "FORWARD", "1"];
                insert.extend_from_slice(rule);
                xtables("iptables", &insert).unwrap();
            }
            restore(
                "iptables",
                &build_forward_document("iptables", &ipv4_build_forwarding(), &mut Vec::new())
                    .unwrap(),
            )
            .unwrap();
            restore(
                "ip6tables",
                &build_forward_document("ip6tables", &ipv6_build_forwarding(), &mut Vec::new())
                    .unwrap(),
            )
            .unwrap();
            remove_firewall_rules("10.210.1.0/24".parse().unwrap()).unwrap();
            return;
        }
        for rules in in_namespace(
            "network::firewall::tests::teardown_removes_the_chain_and_loose_rules_from_earlier_daemons",
            &["mangle"],
        ) {
            assert!(
                rules.lines().all(|line| line.starts_with("-P ")),
                "a rule survived teardown:\n{rules}"
            );
        }
    }

    /// A re-apply that was queued while reset removed the firewall runs last.
    #[test]
    #[ignore = "needs root and ip netns"]
    fn a_reapply_that_runs_after_teardown_puts_nothing_back() {
        if in_child() {
            xtables("iptables", &["-N", "DOCKER-USER"]).unwrap();
            let subnet = "10.210.1.0/24".parse().unwrap();
            let installed =
                apply_firewall_rules(subnet, ManagementAddress("fdcc::1".parse().unwrap()))
                    .unwrap();
            remove_firewall_rules(subnet).unwrap();
            assert!(installed.reapply().unwrap().is_none());
            return;
        }
        let listings = in_namespace(
            "network::firewall::tests::a_reapply_that_runs_after_teardown_puts_nothing_back",
            &["filter", "nat"],
        );
        for rules in listings {
            for ployz in [
                "PLOYZ-INPUT",
                "-i ployz-wg -o ployz -j ACCEPT",
                "-o ployz-wg -j RETURN",
            ] {
                assert!(!rules.contains(ployz), "teardown left {ployz}:\n{rules}");
            }
        }
    }
}
