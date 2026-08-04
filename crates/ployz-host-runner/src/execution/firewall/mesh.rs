use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use ipnet::Ipv6Net;
use ployz_core::operation::FailureMessage;

use super::{
    CORROSION_CHAIN, FirewallBackend, command_probe, failure_message, nft, require_success,
    unmanaged_firewall,
};
use crate::HostRunnerCommandRunner;

const FIREWALLD_ACCEPT_PRIORITY: i16 = -100;
const FIREWALLD_REJECT_PRIORITY: i16 = -90;
const FIREWALLD_ZONE: &str = "ployz";
const FIREWALLD_ZONE_DESCRIPTION: &str = "Managed by Ployz built-in WireGuard";
const UFW_CORROSION_MARKER: &str = "ployz-corrosion-gossip";
const UFW_API_MARKER: &str = "ployz-api-http";
static RESTORE_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl FirewallBackend {
    /// Allows traffic decapsulated by a managed mesh interface to reach
    /// machine-local control-plane listeners.
    pub fn allow_control_plane_http_with(
        &self,
        interface: &str,
        port: u16,
        runner: &mut impl HostRunnerCommandRunner,
    ) -> Result<(), FailureMessage> {
        match self {
            Self::Firewalld => allow_firewalld_api_http(interface, port, runner),
            Self::Ufw => allow_ufw_api_http(interface, port, runner),
            Self::None => Ok(()),
            Self::Unmanaged(name) => Err(unmanaged_firewall(name)),
        }
    }

    /// Restricts one UDP listener on the mesh interface to the supplied
    /// machine `/112`s. Other mesh traffic remains governed by the backend's
    /// ordinary interface rule, so roaming peers can still reach the HTTP API.
    pub fn restrict_udp_to_ipv6_sources_with(
        &self,
        interface: &str,
        port: u16,
        sources: &BTreeSet<Ipv6Net>,
        runner: &mut impl HostRunnerCommandRunner,
    ) -> Result<(), FailureMessage> {
        match self {
            Self::Firewalld => {
                converge_firewalld_machine_only_udp(interface, port, sources, runner)
            }
            Self::Ufw => {
                install_ufw_reload_backstop(interface, port, runner)?;
                converge_machine_only_udp(interface, port, sources, runner)
            }
            Self::None => converge_machine_only_udp(interface, port, sources, runner),
            Self::Unmanaged(name) => Err(unmanaged_firewall(name)),
        }
    }

    /// Allows the two routed container directions between the WireGuard
    /// interface and the Docker bridge through the detected firewall owner.
    pub fn allow_forwarding_between_with(
        &self,
        wireguard_interface: &str,
        bridge_interface: &str,
        runner: &mut impl HostRunnerCommandRunner,
    ) -> Result<(), FailureMessage> {
        match self {
            Self::Ufw => {
                allow_ufw_forward(wireguard_interface, bridge_interface, runner)?;
                allow_ufw_forward(bridge_interface, wireguard_interface, runner)
            }
            Self::Firewalld => Err(failure_message(format!(
                "firewalld cannot safely express forwarding only between {wireguard_interface} and {bridge_interface} without owning the bridge's zone"
            ))),
            Self::None => {
                allow_iptables_forward(wireguard_interface, bridge_interface, runner)?;
                allow_iptables_forward(bridge_interface, wireguard_interface, runner)
            }
            Self::Unmanaged(name) => Err(unmanaged_firewall(name)),
        }
    }
}

fn ensure_firewalld_zone(
    interface: &str,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    let zones = runner.command("firewall-cmd", &["--permanent", "--get-zones"])?;
    if !zones.success {
        return Err(failure_message(zones.failure));
    }
    if zones.stdout_truncated {
        return Err(failure_message("firewalld zone observation was truncated"));
    }
    if zones
        .stdout
        .split_ascii_whitespace()
        .any(|zone| zone == FIREWALLD_ZONE)
    {
        let description = runner.command(
            "firewall-cmd",
            &["--permanent", "--zone=ployz", "--get-description"],
        )?;
        if !description.success {
            return Err(failure_message(description.failure));
        }
        if description.stdout.trim() != FIREWALLD_ZONE_DESCRIPTION {
            return Err(failure_message(
                "firewalld zone ployz exists without the Ployz ownership marker",
            ));
        }
    } else {
        create_firewalld_zone(runner)?;
    }
    let change = format!("--change-interface={interface}");
    require_success(
        runner,
        "firewall-cmd",
        &["--permanent", "--zone=ployz", &change],
    )
}

fn create_firewalld_zone(runner: &mut impl HostRunnerCommandRunner) -> Result<(), FailureMessage> {
    let (path, mut file) = create_staged_file("ployz-firewalld-zone", "xml")?;
    let zone = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<zone target=\"DROP\">\n  <short>Ployz</short>\n  <description>{FIREWALLD_ZONE_DESCRIPTION}</description>\n</zone>\n"
    );
    let result = file
        .write_all(zone.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| failure_message(format!("write staged firewalld zone: {error}")))
        .and_then(|()| {
            let path = path.to_string_lossy();
            let create = format!("--new-zone-from-file={path}");
            require_success(
                runner,
                "firewall-cmd",
                &["--permanent", &create, "--name=ployz"],
            )
        });
    drop(file);
    let cleanup = fs::remove_file(&path)
        .map_err(|error| failure_message(format!("remove staged firewalld zone: {error}")));
    result.and(cleanup)
}

fn allow_firewalld_api_http(
    interface: &str,
    port: u16,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    ensure_firewalld_zone(interface, runner)?;
    let output = runner.command(
        "firewall-cmd",
        &["--permanent", "--zone=ployz", "--list-rich-rules"],
    )?;
    if !output.success {
        return Err(failure_message(output.failure));
    }
    if output.stdout_truncated {
        return Err(failure_message(
            "firewalld permanent rich-rule observation was truncated",
        ));
    }
    let rule = format!("rule family=\"ipv6\" port port=\"{port}\" protocol=\"tcp\" accept");
    let mut current_present = false;
    for line in output.stdout.lines() {
        let Some(existing_port) = parse_firewalld_api_rule(line) else {
            continue;
        };
        if existing_port == port && !current_present {
            current_present = true;
        } else {
            remove_permanent_rich_rule(line, runner)?;
        }
    }
    if !current_present {
        add_permanent_rich_rule(&rule, runner)?;
    }
    require_success(runner, "firewall-cmd", &["--reload"])
}

fn parse_firewalld_api_rule(line: &str) -> Option<u16> {
    let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
    let [
        "rule",
        "family=\"ipv6\"",
        "port",
        port,
        "protocol=\"tcp\"",
        "accept",
    ] = fields.as_slice()
    else {
        return None;
    };
    quoted_attribute(port, "port")?.parse().ok()
}

fn allow_ufw_api_http(
    interface: &str,
    port: u16,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    let port = port.to_string();
    let output = runner.command("ufw", &["status", "numbered"])?;
    if !output.success {
        return Err(failure_message(output.failure));
    }
    if output.stdout_truncated {
        return Err(failure_message("UFW numbered status was truncated"));
    }
    let plan = plan_ufw_owned_rule(&output.stdout, UFW_API_MARKER, |line| {
        ufw_has_port_protocol(line, &port, "tcp")
            && line.contains("ALLOW IN")
            && line.contains("(v6)")
            && line
                .split_ascii_whitespace()
                .any(|column| column == interface)
    })?;
    delete_ufw_rules(&plan.deletions, runner)?;
    if plan.current_present {
        return Ok(());
    }
    require_success(
        runner,
        "ufw",
        &[
            "allow",
            "in",
            "on",
            interface,
            "from",
            "::/0",
            "to",
            "any",
            "port",
            &port,
            "proto",
            "tcp",
            "comment",
            UFW_API_MARKER,
        ],
    )
}

fn install_ufw_reload_backstop(
    interface: &str,
    port: u16,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    let port = port.to_string();
    let output = runner.command("ufw", &["status", "numbered"])?;
    if !output.success {
        return Err(failure_message(output.failure));
    }
    if output.stdout_truncated {
        return Err(failure_message("UFW numbered status was truncated"));
    }
    let plan = plan_ufw_owned_rule(&output.stdout, UFW_CORROSION_MARKER, |line| {
        ufw_has_port_protocol(line, &port, "udp")
            && line.contains("DENY IN")
            && line.contains("(v6)")
            && line
                .split_ascii_whitespace()
                .any(|column| column == interface)
    })?;
    delete_ufw_rules(&plan.deletions, runner)?;
    if plan.current_present {
        return Ok(());
    }
    require_success(
        runner,
        "ufw",
        &[
            "insert",
            "1",
            "deny",
            "in",
            "on",
            interface,
            "from",
            "::/0",
            "to",
            "any",
            "port",
            &port,
            "proto",
            "udp",
            "comment",
            UFW_CORROSION_MARKER,
        ],
    )
}

struct UfwRulePlan {
    current_present: bool,
    deletions: Vec<u32>,
}

fn plan_ufw_owned_rule(
    status: &str,
    marker: &str,
    is_current: impl Fn(&str) -> bool,
) -> Result<UfwRulePlan, FailureMessage> {
    let mut current_present = false;
    let mut deletions = Vec::new();
    for line in status
        .lines()
        .filter(|line| ufw_comment(line) == Some(marker))
    {
        let Some(number) = ufw_rule_number(line) else {
            return Err(failure_message(format!(
                "UFW rule marked {marker} had no numbered status index"
            )));
        };
        if is_current(line) && !current_present {
            current_present = true;
        } else {
            deletions.push(number);
        }
    }
    deletions.sort_unstable_by(|left, right| right.cmp(left));
    Ok(UfwRulePlan {
        current_present,
        deletions,
    })
}

fn delete_ufw_rules(
    rule_numbers: &[u32],
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    for number in rule_numbers {
        let number = number.to_string();
        require_success(runner, "ufw", &["--force", "delete", &number])?;
    }
    Ok(())
}

fn ufw_comment(line: &str) -> Option<&str> {
    line.split_once(" # ").map(|(_, comment)| comment.trim())
}

fn ufw_has_port_protocol(line: &str, port: &str, protocol: &str) -> bool {
    let expected = format!("{port}/{protocol}");
    line.split_ascii_whitespace().any(|field| field == expected)
}

fn ufw_rule_number(line: &str) -> Option<u32> {
    line.trim_start()
        .strip_prefix('[')?
        .split_once(']')?
        .0
        .trim()
        .parse()
        .ok()
}

fn converge_firewalld_machine_only_udp(
    interface: &str,
    port: u16,
    sources: &BTreeSet<Ipv6Net>,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    ensure_firewalld_zone(interface, runner)?;
    let output = runner.command(
        "firewall-cmd",
        &["--permanent", "--zone=ployz", "--list-rich-rules"],
    )?;
    if !output.success {
        return Err(failure_message(output.failure));
    }
    if output.stdout_truncated {
        return Err(failure_message(
            "firewalld permanent rich-rule observation was truncated",
        ));
    }

    let existing = output
        .stdout
        .lines()
        .filter_map(|line| parse_owned_firewalld_rule(line).map(|rule| (line, rule)))
        .collect::<Vec<_>>();
    let reject = render_firewalld_reject(port);
    if !existing
        .iter()
        .any(|(_, rule)| matches!(rule, OwnedFirewalldRule::Reject { port: rule_port } if *rule_port == port))
    {
        add_permanent_rich_rule(&reject, runner)?;
    }

    for (line, rule) in &existing {
        let stale = match rule {
            OwnedFirewalldRule::Accept {
                port: rule_port,
                source,
            } => *rule_port != port || !sources.contains(source),
            OwnedFirewalldRule::Reject { .. } => false,
        };
        if stale {
            remove_permanent_rich_rule(line, runner)?;
        }
    }

    for source in sources {
        let present = existing.iter().any(|(_, rule)| {
            matches!(rule, OwnedFirewalldRule::Accept { port: rule_port, source: rule_source }
                if *rule_port == port && rule_source == source)
        });
        if !present {
            add_permanent_rich_rule(&render_firewalld_accept(port, *source), runner)?;
        }
    }

    for (line, rule) in &existing {
        if matches!(rule, OwnedFirewalldRule::Reject { port: rule_port } if *rule_port != port) {
            remove_permanent_rich_rule(line, runner)?;
        }
    }

    require_success(runner, "firewall-cmd", &["--reload"])
}

#[derive(Debug, PartialEq, Eq)]
enum OwnedFirewalldRule {
    Accept { port: u16, source: Ipv6Net },
    Reject { port: u16 },
}

fn parse_owned_firewalld_rule(line: &str) -> Option<OwnedFirewalldRule> {
    let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
    let ["rule", first, second, rest @ ..] = fields.as_slice() else {
        return None;
    };
    let attributes = [*first, *second];
    let family = attributes.contains(&"family=\"ipv6\"");
    if !family {
        return None;
    }
    let priority = attributes.iter().find_map(|field| {
        field
            .strip_prefix("priority=\"")
            .and_then(|value| value.strip_suffix('"'))
            .and_then(|value| value.parse::<i16>().ok())
    })?;
    match (priority, rest) {
        (
            FIREWALLD_ACCEPT_PRIORITY,
            [
                "source",
                address,
                "port",
                port,
                "protocol=\"udp\"",
                "accept",
            ],
        ) => {
            let source = quoted_attribute(address, "address")?
                .parse::<Ipv6Net>()
                .ok()?;
            if source.prefix_len() != 112 {
                return None;
            }
            Some(OwnedFirewalldRule::Accept {
                port: quoted_attribute(port, "port")?.parse().ok()?,
                source,
            })
        }
        (FIREWALLD_REJECT_PRIORITY, ["port", port, "protocol=\"udp\"", "reject"]) => {
            Some(OwnedFirewalldRule::Reject {
                port: quoted_attribute(port, "port")?.parse().ok()?,
            })
        }
        _ => None,
    }
}

fn quoted_attribute<'a>(value: &'a str, name: &str) -> Option<&'a str> {
    value
        .strip_prefix(&format!("{name}=\""))
        .and_then(|value| value.strip_suffix('"'))
}

fn render_firewalld_accept(port: u16, source: Ipv6Net) -> String {
    format!(
        "rule family=\"ipv6\" priority=\"{FIREWALLD_ACCEPT_PRIORITY}\" source address=\"{source}\" port port=\"{port}\" protocol=\"udp\" accept"
    )
}

fn render_firewalld_reject(port: u16) -> String {
    format!(
        "rule family=\"ipv6\" priority=\"{FIREWALLD_REJECT_PRIORITY}\" port port=\"{port}\" protocol=\"udp\" reject"
    )
}

fn add_permanent_rich_rule(
    rule: &str,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    let add = format!("--add-rich-rule={rule}");
    require_success(
        runner,
        "firewall-cmd",
        &["--permanent", "--zone=ployz", &add],
    )
}

fn remove_permanent_rich_rule(
    rule: &str,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    let remove = format!("--remove-rich-rule={rule}");
    require_success(
        runner,
        "firewall-cmd",
        &["--permanent", "--zone=ployz", &remove],
    )
}

fn converge_machine_only_udp(
    interface: &str,
    port: u16,
    sources: &BTreeSet<Ipv6Net>,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    let output = runner.command("ip6tables", &["-S", "INPUT"])?;
    if !output.success {
        return Err(failure_message(output.failure));
    }
    if output.stdout_truncated {
        return Err(failure_message("ip6tables INPUT observation was truncated"));
    }
    let rules = render_atomic_filter_rules(&output.stdout, interface, port, sources);
    restore_ipv6_filter(&rules, runner)
}

fn render_atomic_filter_rules(
    input_rules: &str,
    interface: &str,
    port: u16,
    sources: &BTreeSet<Ipv6Net>,
) -> String {
    let mut lines = vec!["*filter".to_owned(), format!(":{CORROSION_CHAIN} - [0:0]")];
    lines.extend(owned_corrosion_jump_deletions(input_rules));
    lines.push(format!(
        "-I INPUT 1 -i {interface} -p udp -m udp --dport {port} -j {CORROSION_CHAIN}"
    ));
    for source in sources {
        lines.push(format!("-A {CORROSION_CHAIN} -s {source} -j ACCEPT"));
    }
    lines.push(format!("-A {CORROSION_CHAIN} -j REJECT"));
    lines.push("COMMIT".to_owned());
    format!("{}\n", lines.join("\n"))
}

fn owned_corrosion_jump_deletions(input_rules: &str) -> Vec<String> {
    input_rules
        .lines()
        .filter_map(|line| {
            let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
            let ["-A", "INPUT", rest @ ..] = fields.as_slice() else {
                return None;
            };
            rest.windows(2)
                .any(|fields| {
                    matches!(fields, ["-j", target] if nft::identifier_eq(target, CORROSION_CHAIN))
                })
                .then(|| line.replacen("-A INPUT", "-D INPUT", 1))
        })
        .collect()
}

fn restore_ipv6_filter(
    rules: &str,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    let (path, mut file) = create_restore_file()?;
    let result = file
        .write_all(rules.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| failure_message(format!("write staged ip6tables rules: {error}")))
        .and_then(|()| {
            let path = path.to_string_lossy();
            require_success(
                runner,
                "ip6tables-restore",
                &["--noflush", "--wait", "5", &path],
            )
        });
    drop(file);
    let cleanup = fs::remove_file(&path)
        .map_err(|error| failure_message(format!("remove staged ip6tables rules: {error}")));
    result.and(cleanup)
}

fn create_restore_file() -> Result<(PathBuf, fs::File), FailureMessage> {
    create_staged_file("ployz-ip6tables", "rules")
}

fn create_staged_file(
    prefix: &str,
    extension: &str,
) -> Result<(PathBuf, fs::File), FailureMessage> {
    for _ in 0..16 {
        let sequence = RESTORE_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}-{sequence}.{extension}",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(failure_message(format!(
                    "create staged firewall file {}: {error}",
                    path.display()
                )));
            }
        }
    }
    Err(failure_message(
        "create staged firewall file: temporary name collision limit reached",
    ))
}

fn allow_ufw_forward(
    input: &str,
    output: &str,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    require_success(
        runner,
        "ufw",
        &["route", "allow", "in", "on", input, "out", "on", output],
    )
}

fn allow_iptables_forward(
    input: &str,
    output: &str,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    let rule = ["FORWARD", "-i", input, "-o", output, "-j", "ACCEPT"];
    let mut check = vec!["-C"];
    check.extend(rule);
    if command_probe(runner, "iptables", &check, &[1])? {
        return Ok(());
    }
    let mut insert = vec!["-I"];
    insert.extend(rule);
    require_success(runner, "iptables", &insert)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::super::tests::{RecordingRunner, active, command_failure};
    use super::*;

    #[test]
    fn atomic_filter_render_replaces_only_exact_owned_rules() {
        let input = "-P INPUT ACCEPT\n-A INPUT -j ployz-corrosion\n-A INPUT -i old0 -p udp --dport 7777 -j PLOYZ-CORROSION\n-A INPUT -p tcp --dport 22 -j ACCEPT\n";
        let sources = BTreeSet::from([
            "fd12:3456:789a:1::/112".parse().expect("source"),
            "fd12:3456:789a:2::/112".parse().expect("source"),
        ]);

        let rendered = render_atomic_filter_rules(input, "ployz0", 8_787, &sources);

        assert!(rendered.starts_with("*filter\n:PLOYZ-CORROSION - [0:0]\n"));
        assert!(rendered.contains("-D INPUT -i old0 -p udp --dport 7777 -j PLOYZ-CORROSION\n"));
        assert!(
            rendered
                .contains("-I INPUT 1 -i ployz0 -p udp -m udp --dport 8787 -j PLOYZ-CORROSION\n")
        );
        assert!(!rendered.contains("-D INPUT -j ployz-corrosion"));
        assert!(!rendered.contains("--dport 22"));
        let owned = rendered
            .lines()
            .filter(|line| line.starts_with("-A PLOYZ-CORROSION "))
            .collect::<Vec<_>>();
        let mut expected = sources
            .iter()
            .map(|source| format!("-A PLOYZ-CORROSION -s {source} -j ACCEPT"))
            .collect::<Vec<_>>();
        expected.push("-A PLOYZ-CORROSION -j REJECT".to_owned());
        assert_eq!(owned, expected);
    }

    #[test]
    fn firewalld_owned_rule_parser_is_exact_and_case_sensitive() {
        assert_eq!(
            parse_owned_firewalld_rule(
                "rule family=\"ipv6\" priority=\"-100\" source address=\"fd00::/112\" port port=\"8787\" protocol=\"udp\" accept"
            ),
            Some(OwnedFirewalldRule::Accept {
                port: 8_787,
                source: "fd00::/112".parse().expect("source"),
            })
        );
        assert!(parse_owned_firewalld_rule(
            "rule family=\"ipv6\" priority=\"-100\" source address=\"fd00::/112\" port port=\"8787\" protocol=\"udp\" ACCEPT"
        )
        .is_none());
    }

    #[test]
    fn raw_restore_is_the_only_mutation_and_failure_leaves_the_snapshot_live() {
        let mut runner = RecordingRunner::with_outputs([
            active("-P INPUT ACCEPT\n"),
            command_failure("restore rejected"),
        ]);

        let error = FirewallBackend::None
            .restrict_udp_to_ipv6_sources_with("ployz0", 8_787, &BTreeSet::new(), &mut runner)
            .expect_err("restore failure");

        assert_eq!(error.as_str(), "restore rejected");
        let [observe, restore] = runner.calls.as_slice() else {
            panic!(
                "expected observation and atomic restore calls: {:?}",
                runner.calls
            );
        };
        assert_eq!(observe, "ip6tables -S INPUT");
        assert!(restore.starts_with("ip6tables-restore --noflush --wait 5 "));
        let path = restore
            .split_ascii_whitespace()
            .next_back()
            .expect("restore path");
        assert!(!Path::new(path).exists(), "staged restore file was removed");
    }

    #[test]
    fn firewalld_converges_permanent_fail_closed_rules_then_reloads() {
        let existing = concat!(
            "rule family=\"ipv6\" priority=\"-100\" source address=\"fd00::/112\" port port=\"8787\" protocol=\"udp\" accept\n",
            "rule family=\"ipv6\" priority=\"-100\" source address=\"fd99::/112\" port port=\"8787\" protocol=\"udp\" accept\n",
            "rule family=\"ipv6\" priority=\"-90\" port port=\"7777\" protocol=\"udp\" reject\n",
            "rule family=\"ipv6\" service name=\"ssh\" accept\n",
        );
        let mut runner = RecordingRunner::with_outputs([
            active("public ployz\n"),
            active("Managed by Ployz built-in WireGuard\n"),
            active(""),
            active(existing),
            active(""),
            active(""),
            active(""),
            active(""),
            active(""),
        ]);
        let sources = BTreeSet::from([
            "fd00::/112".parse().expect("source"),
            "fd01::/112".parse().expect("source"),
        ]);

        FirewallBackend::Firewalld
            .restrict_udp_to_ipv6_sources_with("ployz0", 8_787, &sources, &mut runner)
            .expect("firewalld convergence");

        assert_eq!(
            runner.calls,
            [
                "firewall-cmd --permanent --get-zones",
                "firewall-cmd --permanent --zone=ployz --get-description",
                "firewall-cmd --permanent --zone=ployz --change-interface=ployz0",
                "firewall-cmd --permanent --zone=ployz --list-rich-rules",
                "firewall-cmd --permanent --zone=ployz --add-rich-rule=rule family=\"ipv6\" priority=\"-90\" port port=\"8787\" protocol=\"udp\" reject",
                "firewall-cmd --permanent --zone=ployz --remove-rich-rule=rule family=\"ipv6\" priority=\"-100\" source address=\"fd99::/112\" port port=\"8787\" protocol=\"udp\" accept",
                "firewall-cmd --permanent --zone=ployz --add-rich-rule=rule family=\"ipv6\" priority=\"-100\" source address=\"fd01::/112\" port port=\"8787\" protocol=\"udp\" accept",
                "firewall-cmd --permanent --zone=ployz --remove-rich-rule=rule family=\"ipv6\" priority=\"-90\" port port=\"7777\" protocol=\"udp\" reject",
                "firewall-cmd --reload",
            ]
        );
    }

    #[test]
    fn firewalld_refuses_a_same_named_foreign_zone_without_mutation() {
        let mut runner =
            RecordingRunner::with_outputs([active("ployz public\n"), active("operator owned\n")]);

        let error = FirewallBackend::Firewalld
            .restrict_udp_to_ipv6_sources_with("ployz0", 8_787, &BTreeSet::new(), &mut runner)
            .expect_err("foreign zone");

        assert!(
            error
                .as_str()
                .contains("without the Ployz ownership marker")
        );
        assert_eq!(
            runner.calls,
            [
                "firewall-cmd --permanent --get-zones",
                "firewall-cmd --permanent --zone=ployz --get-description",
            ]
        );
    }

    #[test]
    fn firewalld_api_convergence_removes_stale_owned_ports_only() {
        let existing = concat!(
            "rule family=\"ipv6\" port port=\"2020\" protocol=\"tcp\" accept\n",
            "rule family=\"ipv6\" port port=\"3030\" protocol=\"tcp\" accept\n",
            "rule family=\"ipv6\" source address=\"fd00::/112\" port port=\"4040\" protocol=\"tcp\" accept\n",
        );
        let mut runner = RecordingRunner::with_outputs([
            active("public ployz\n"),
            active("Managed by Ployz built-in WireGuard\n"),
            active(""),
            active(existing),
            active(""),
            active(""),
        ]);

        FirewallBackend::Firewalld
            .allow_control_plane_http_with("ployz0", 3_030, &mut runner)
            .expect("API port convergence");

        assert_eq!(
            runner.calls,
            [
                "firewall-cmd --permanent --get-zones",
                "firewall-cmd --permanent --zone=ployz --get-description",
                "firewall-cmd --permanent --zone=ployz --change-interface=ployz0",
                "firewall-cmd --permanent --zone=ployz --list-rich-rules",
                "firewall-cmd --permanent --zone=ployz --remove-rich-rule=rule family=\"ipv6\" port port=\"2020\" protocol=\"tcp\" accept",
                "firewall-cmd --reload",
            ]
        );
    }

    #[test]
    fn firewalld_forwarding_refuses_false_success_without_mutation() {
        let mut runner = RecordingRunner::default();

        let error = FirewallBackend::Firewalld
            .allow_forwarding_between_with("ployz0", "br-ployz", &mut runner)
            .expect_err("firewalld needs a native scoped policy");

        assert!(error.as_str().contains("cannot safely express forwarding"));
        assert!(runner.calls.is_empty());
    }

    #[test]
    fn ufw_prunes_all_stale_and_duplicate_exact_markers_before_atomic_convergence() {
        let status = concat!(
            "[ 1] 8787/udp (v6) on ployz0 DENY IN Anywhere (v6) # ployz-corrosion-gossip\n",
            "[ 2] 8787/udp (v6) on ployz0 DENY IN Anywhere (v6) # operator-rule\n",
            "[ 3] 8787/udp (v6) on ployz0 DENY IN Anywhere (v6) # ployz-corrosion-gossip\n",
            "[ 4] 7777/udp on old0 DENY IN Anywhere # ployz-corrosion-gossip\n",
            "[ 5] 8787/udp (v6) on ployz0 DENY IN Anywhere (v6) # foreign-ployz-corrosion-gossip\n",
        );
        let mut runner = RecordingRunner::with_outputs([
            active(status),
            active(""),
            active(""),
            active("-P INPUT ACCEPT\n"),
            active(""),
        ]);

        FirewallBackend::Ufw
            .restrict_udp_to_ipv6_sources_with("ployz0", 8_787, &BTreeSet::new(), &mut runner)
            .expect("idempotent UFW convergence");

        let [status, delete_stale, delete_duplicate, observe, restore] = runner.calls.as_slice()
        else {
            panic!("unexpected calls: {:?}", runner.calls);
        };
        assert_eq!(status, "ufw status numbered");
        assert_eq!(delete_stale, "ufw --force delete 4");
        assert_eq!(delete_duplicate, "ufw --force delete 3");
        assert_eq!(observe, "ip6tables -S INPUT");
        assert!(restore.starts_with("ip6tables-restore --noflush --wait 5 "));
    }

    #[test]
    fn ufw_api_convergence_deletes_marked_port_drift_in_descending_order() {
        let status = concat!(
            "[ 1] 12020/tcp (v6) on ployz0 ALLOW IN Anywhere (v6) # ployz-api-http\n",
            "[ 2] 2020/tcp (v6) on ployz0 ALLOW IN Anywhere (v6) # foreign-ployz-api-http\n",
            "[ 3] 2020/tcp (v6) on ployz0 ALLOW IN Anywhere (v6) # ployz-api-http\n",
            "[ 4] 2020/tcp (v6) on ployz0 ALLOW IN Anywhere (v6) # ployz-api-http\n",
        );
        let mut runner = RecordingRunner::with_outputs([active(status), active(""), active("")]);

        FirewallBackend::Ufw
            .allow_control_plane_http_with("ployz0", 2_020, &mut runner)
            .expect("API port convergence");

        assert_eq!(
            runner.calls,
            [
                "ufw status numbered",
                "ufw --force delete 4",
                "ufw --force delete 1",
            ]
        );
    }
}
