//! Host firewall detection and port operations.

#[cfg(test)]
use std::collections::BTreeSet;

use ployz_core::operation::FailureMessage;

use super::command::HostRunnerCommandRunner;
#[cfg(test)]
use super::supervisor::SupervisorBackend;

mod detection;
mod mesh;
mod nft;
mod port;

pub use detection::detect_firewall_backend;
pub(crate) use mesh::converge_container_nat_with;
pub use port::{AssignedHostPort, HostPortProtocol};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirewallBackend {
    Firewalld,
    Ufw,
    Unmanaged(String),
    None,
}

impl FirewallBackend {
    pub fn open_with(
        &self,
        port: AssignedHostPort,
        runner: &mut impl HostRunnerCommandRunner,
    ) -> Result<(), FailureMessage> {
        match self {
            Self::Firewalld => change_firewalld(port, runner, FirewallChange::Open),
            Self::Ufw => open_ufw(port, runner),
            Self::None => Ok(()),
            Self::Unmanaged(name) => Err(unmanaged_firewall(name)),
        }
    }

    #[cfg(test)]
    pub fn close_with(
        &self,
        port: AssignedHostPort,
        runner: &mut impl HostRunnerCommandRunner,
    ) -> Result<(), FailureMessage> {
        match self {
            Self::Firewalld => change_firewalld(port, runner, FirewallChange::Close),
            Self::Ufw if !query_ufw(port, runner)? => Ok(()),
            Self::Ufw => require_success(runner, "ufw", &["delete", "allow", &render_port(port)]),
            Self::None => Ok(()),
            Self::Unmanaged(name) => Err(unmanaged_firewall(name)),
        }
    }
}

const CORROSION_CHAIN: &str = "PLOYZ-CORROSION";

fn open_ufw(
    port: AssignedHostPort,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FailureMessage> {
    if query_ufw(port, runner)? {
        return Ok(());
    }

    let port_name = render_port(port);
    let numbered = runner.command("ufw", &["status", "numbered"])?;
    if !numbered.success {
        return Err(failure_message(numbered.failure));
    }
    if numbered.stdout_truncated {
        return Err(failure_message("UFW numbered status was truncated"));
    }
    if numbered.stdout.lines().any(ufw_rule_has_number) {
        require_success(runner, "ufw", &["insert", "1", "allow", &port_name])?;
    } else {
        require_success(runner, "ufw", &["allow", &port_name])?;
    }
    if query_ufw(port, runner)? {
        return Ok(());
    }

    Err(failure_message(format!(
        "UFW did not assure inbound reachability for {port_name}"
    )))
}

fn ufw_rule_has_number(line: &str) -> bool {
    line.trim_start()
        .strip_prefix('[')
        .and_then(|rule| rule.split_once(']'))
        .is_some_and(|(number, _)| number.trim().parse::<u32>().is_ok())
}

#[derive(Clone, Copy)]
enum FirewallChange {
    Open,
    #[cfg(test)]
    Close,
}

fn change_firewalld(
    port: AssignedHostPort,
    runner: &mut impl HostRunnerCommandRunner,
    change: FirewallChange,
) -> Result<(), FailureMessage> {
    let port = render_port(port);
    let query = format!("--query-port={port}");
    let action = match change {
        FirewallChange::Open => format!("--add-port={port}"),
        #[cfg(test)]
        FirewallChange::Close => format!("--remove-port={port}"),
    };
    let closing = match change {
        FirewallChange::Open => false,
        #[cfg(test)]
        FirewallChange::Close => true,
    };
    let runtime_has_port = command_probe(runner, "firewall-cmd", &["--quiet", &query], &[1])?;
    let permanent_has_port = command_probe(
        runner,
        "firewall-cmd",
        &["--permanent", "--quiet", &query],
        &[1],
    )?;
    if runtime_has_port == closing {
        require_success(runner, "firewall-cmd", &[&action])?;
    }
    if permanent_has_port == closing {
        require_success(runner, "firewall-cmd", &["--permanent", &action])?;
    }
    Ok(())
}

fn query_ufw(
    port: AssignedHostPort,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<bool, FailureMessage> {
    let output = runner.command("ufw", &["status", "verbose"])?;
    if !output.success {
        return Err(failure_message(output.failure));
    }
    let port = render_port(port);
    for line in output.stdout.lines() {
        let mut columns = line.split_whitespace();
        if columns.next() != Some(port.as_str()) {
            continue;
        }
        let action = columns.next();
        if action == Some("(v6)") {
            continue;
        }
        return Ok(action == Some("ALLOW")
            && columns.next() == Some("IN")
            && columns.next() == Some("Anywhere"));
    }
    Ok(false)
}

fn command_probe(
    runner: &mut impl HostRunnerCommandRunner,
    program: &str,
    args: &[&str],
    absent_exit_codes: &[i32],
) -> Result<bool, FailureMessage> {
    let output = runner.command(program, args)?;
    if output.success {
        return Ok(true);
    }
    if output
        .exit_code
        .is_some_and(|code| absent_exit_codes.contains(&code))
    {
        return Ok(false);
    }
    Err(failure_message(output.failure))
}

fn require_success(
    runner: &mut impl HostRunnerCommandRunner,
    program: &str,
    args: &[&str],
) -> Result<(), FailureMessage> {
    let output = runner.command(program, args)?;
    if output.success {
        Ok(())
    } else {
        Err(failure_message(output.failure))
    }
}

fn render_port(port: AssignedHostPort) -> String {
    let protocol = match port.protocol {
        HostPortProtocol::Tcp => "tcp",
        HostPortProtocol::Udp => "udp",
    };
    format!("{}/{protocol}", port.port)
}

fn unmanaged_firewall(name: &str) -> FailureMessage {
    failure_message(format!(
        "active host firewall {name} is not managed by Ployz"
    ))
}

fn failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message).expect("firewall failure message is non-empty")
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::VecDeque;
    use std::path::Path;

    use super::super::command::HostRunnerCommandOutput;

    use super::*;

    const TCP_443: AssignedHostPort = AssignedHostPort::tcp(443);

    #[derive(Default)]
    pub(crate) struct RecordingRunner {
        pub(crate) calls: Vec<String>,
        outputs: VecDeque<Result<HostRunnerCommandOutput, FailureMessage>>,
    }

    impl RecordingRunner {
        pub(crate) fn with_outputs(
            outputs: impl IntoIterator<Item = Result<HostRunnerCommandOutput, FailureMessage>>,
        ) -> Self {
            Self {
                calls: Vec::new(),
                outputs: outputs.into_iter().collect(),
            }
        }
    }

    impl HostRunnerCommandRunner for RecordingRunner {
        fn command(
            &mut self,
            program: &str,
            args: &[&str],
        ) -> Result<HostRunnerCommandOutput, FailureMessage> {
            self.calls.push(format!("{program} {}", args.join(" ")));
            self.outputs.pop_front().unwrap_or_else(|| Ok(inactive()))
        }
        fn is_linux(&mut self) -> bool {
            unreachable!()
        }
        fn current_uid(&mut self) -> Result<u32, FailureMessage> {
            unreachable!()
        }
        fn download(&mut self, _: &str, _: &Path) -> Result<(), FailureMessage> {
            unreachable!()
        }
        fn docker_info(&mut self) -> Result<(), FailureMessage> {
            unreachable!()
        }
        fn docker_is_installed(&mut self) -> bool {
            unreachable!()
        }
        fn docker_uses_containerd_snapshotter(&mut self) -> Result<bool, FailureMessage> {
            unreachable!()
        }
        fn docker_has_insecure_registry(&mut self, _: &str) -> Result<bool, FailureMessage> {
            unreachable!()
        }
    }

    pub(crate) fn active(stdout: &str) -> Result<HostRunnerCommandOutput, FailureMessage> {
        Ok(HostRunnerCommandOutput {
            success: true,
            exit_code: Some(0),
            stdout: stdout.to_owned(),
            stdout_truncated: false,
            failure: String::new(),
        })
    }

    pub(crate) fn inactive() -> HostRunnerCommandOutput {
        HostRunnerCommandOutput {
            success: false,
            exit_code: Some(3),
            stdout: String::new(),
            stdout_truncated: false,
            failure: "inactive".to_owned(),
        }
    }

    fn absent() -> HostRunnerCommandOutput {
        HostRunnerCommandOutput {
            success: false,
            exit_code: Some(1),
            stdout: String::new(),
            stdout_truncated: false,
            failure: "absent".to_owned(),
        }
    }

    fn dash_command_absent() -> HostRunnerCommandOutput {
        HostRunnerCommandOutput {
            success: false,
            exit_code: Some(127),
            stdout: String::new(),
            stdout_truncated: false,
            failure: "absent".to_owned(),
        }
    }

    pub(super) fn command_failure(
        message: &str,
    ) -> Result<HostRunnerCommandOutput, FailureMessage> {
        Ok(HostRunnerCommandOutput {
            success: false,
            exit_code: Some(2),
            stdout: String::new(),
            stdout_truncated: false,
            failure: message.to_owned(),
        })
    }

    fn probe_error(message: &str) -> Result<HostRunnerCommandOutput, FailureMessage> {
        Err(failure_message(message))
    }

    #[test]
    fn detection_propagates_probe_errors() {
        let error = detect_firewall_backend(
            SupervisorBackend::Systemd,
            &mut RecordingRunner::with_outputs([probe_error("systemctl timed out")]),
        )
        .expect_err("probe failure is not inactivity");

        assert_eq!(error.as_str(), "systemctl timed out");
    }

    #[test]
    fn docker_only_rules_are_not_an_active_host_firewall() {
        let mut runner = RecordingRunner::with_outputs([
            Ok(inactive()),
            Ok(inactive()),
            Ok(inactive()),
            active(""),
            active("table ip nat { chain DOCKER { hook prerouting priority dstnat; } }"),
            Ok(inactive()),
            Ok(inactive()),
            active(""),
            active("-P INPUT ACCEPT\n-A FORWARD -j DOCKER-USER\n"),
            active("docker.service loaded active running Docker\n"),
        ]);

        assert_eq!(
            detect_firewall_backend(SupervisorBackend::Systemd, &mut runner).expect("detection"),
            FirewallBackend::None
        );
    }

    #[test]
    fn no_service_nft_input_hook_is_unmanaged() {
        let mut runner = RecordingRunner::with_outputs([
            Ok(inactive()),
            Ok(inactive()),
            Ok(inactive()),
            active(""),
            active("chain input { type filter hook input priority filter; policy drop; }"),
        ]);

        assert_eq!(
            detect_firewall_backend(SupervisorBackend::Systemd, &mut runner).expect("detection"),
            FirewallBackend::Unmanaged("nftables".to_owned())
        );
    }

    #[test]
    fn translated_ployz_corrosion_input_hook_is_not_unmanaged() {
        let ruleset = r#"
table ip6 filter {
    chain INPUT {
        type filter hook input priority filter; policy accept;
        iifname "ployz0" udp dport 8787 counter packets 4 bytes 320 jump PLOYZ-CORROSION
    }

    chain PLOYZ-CORROSION {
        ip6 saddr fd12:3456:789a:1::/112 counter packets 2 bytes 160 accept
        counter packets 2 bytes 160 reject with icmpv6 type port-unreachable
    }
}
"#;

        assert!(!nft::manages_input(ruleset));
    }

    #[test]
    fn detection_ignores_its_own_translated_corrosion_hook_after_restart() {
        let ruleset = r#"
table ip6 filter {
    chain INPUT {
        type filter hook input priority filter; policy accept;
        iifname "ployz0" udp dport 8787 jump PLOYZ-CORROSION
    }
    chain PLOYZ-CORROSION {
        ip6 saddr fd12:3456:789a:1::/112 accept
        reject with icmpv6 type port-unreachable
    }
}
"#;
        let mut runner = RecordingRunner::with_outputs([
            Ok(inactive()),
            Ok(inactive()),
            Ok(inactive()),
            active(""),
            active(ruleset),
            Ok(inactive()),
            Ok(inactive()),
            active(""),
            active("-P INPUT ACCEPT\n"),
            active(""),
        ]);

        assert_eq!(
            detect_firewall_backend(SupervisorBackend::Systemd, &mut runner).expect("detection"),
            FirewallBackend::None
        );
    }

    #[test]
    fn partial_owned_corrosion_chain_remains_recoverable_after_restart() {
        let ruleset = r#"
table ip6 filter {
    chain INPUT {
        type filter hook input priority filter; policy accept;
        iifname "ployz0" udp dport 8787 jump "PLOYZ-CORROSION"
    }
    chain "PLOYZ-CORROSION" {
    }
}
"#;

        assert!(!nft::manages_input(ruleset));
    }

    #[test]
    fn any_foreign_input_policy_rule_or_hook_stays_unmanaged() {
        for ruleset in [
            r#"
table ip6 filter {
    chain INPUT {
        type filter hook input priority filter; policy drop;
        iifname "ployz0" udp dport 8787 jump PLOYZ-CORROSION
    }
    chain PLOYZ-CORROSION { }
}
"#,
            r#"
table ip6 filter {
    chain INPUT {
        type filter hook input priority filter; policy accept;
        iifname "ployz0" udp dport 8787 jump PLOYZ-CORROSION
        tcp dport 22 accept
    }
    chain PLOYZ-CORROSION { }
}
"#,
            r#"
table inet foreign {
    chain input {
        type filter hook input priority filter; policy accept;
    }
}
"#,
            r#"
table ip6 filter {
    chain INPUT {
        type filter hook input priority filter; policy accept;
        iifname "ployz0" udp dport 8787 jump PLOYZ-CORROSION
    }
    chain PLOYZ-CORROSION {
        tcp dport 22 accept
    }
}
"#,
        ] {
            assert!(nft::manages_input(ruleset), "must refuse {ruleset}");
        }
    }

    #[test]
    fn iptables_input_policy_or_rule_is_unmanaged() {
        for rules in [
            "-P INPUT DROP\n",
            "-P INPUT ACCEPT\n-A INPUT -p tcp --dport 22 -j ACCEPT\n",
        ] {
            let mut runner = RecordingRunner::with_outputs([
                Ok(inactive()),
                Ok(inactive()),
                Ok(inactive()),
                Ok(absent()),
                Ok(inactive()),
                Ok(inactive()),
                active(""),
                active(rules),
            ]);
            assert!(matches!(
                detect_firewall_backend(SupervisorBackend::Systemd, &mut runner)
                    .expect("detection"),
                FirewallBackend::Unmanaged(_)
            ));
        }
    }

    #[test]
    fn unknown_active_firewall_service_is_unmanaged() {
        let mut runner = RecordingRunner::with_outputs([
            Ok(inactive()),
            Ok(inactive()),
            Ok(inactive()),
            Ok(absent()),
            Ok(inactive()),
            Ok(inactive()),
            Ok(absent()),
            active("custom-firewall.service loaded active running Custom Firewall\n"),
        ]);

        assert_eq!(
            detect_firewall_backend(SupervisorBackend::Systemd, &mut runner).expect("detection"),
            FirewallBackend::Unmanaged("custom-firewall.service".to_owned())
        );
    }

    #[test]
    fn no_raw_tools_and_no_active_manager_is_none() {
        let mut runner = RecordingRunner::with_outputs([
            Ok(inactive()),
            Ok(inactive()),
            Ok(inactive()),
            Ok(absent()),
            Ok(inactive()),
            Ok(inactive()),
            Ok(absent()),
            active(""),
        ]);

        assert_eq!(
            detect_firewall_backend(SupervisorBackend::Systemd, &mut runner).expect("detection"),
            FirewallBackend::None
        );
    }

    #[test]
    fn dash_command_v_absence_is_not_a_host_failure() {
        let mut runner = RecordingRunner::with_outputs([
            Ok(inactive()),
            Ok(inactive()),
            Ok(inactive()),
            Ok(dash_command_absent()),
            Ok(inactive()),
            Ok(inactive()),
            Ok(dash_command_absent()),
            active(""),
        ]);

        assert_eq!(
            detect_firewall_backend(SupervisorBackend::Systemd, &mut runner).expect("detection"),
            FirewallBackend::None
        );
    }

    #[test]
    fn firewalld_open_queries_then_adds_only_missing_runtime_rule() {
        let mut runner = RecordingRunner::with_outputs([Ok(absent()), active(""), active("")]);
        FirewallBackend::Firewalld
            .open_with(TCP_443, &mut runner)
            .expect("open port");
        assert_eq!(
            runner.calls,
            vec![
                "firewall-cmd --quiet --query-port=443/tcp",
                "firewall-cmd --permanent --quiet --query-port=443/tcp",
                "firewall-cmd --add-port=443/tcp"
            ]
        );
    }

    #[test]
    fn firewalld_queries_both_scopes_before_mutating() {
        let mut runner = RecordingRunner::with_outputs([
            Ok(absent()),
            command_failure("permanent query failed"),
        ]);

        let error = FirewallBackend::Firewalld
            .open_with(TCP_443, &mut runner)
            .expect_err("permanent query failure");

        assert_eq!(error.as_str(), "permanent query failed");
        assert_eq!(
            runner.calls,
            vec![
                "firewall-cmd --quiet --query-port=443/tcp",
                "firewall-cmd --permanent --quiet --query-port=443/tcp"
            ]
        );
    }

    #[test]
    fn firewalld_query_error_does_not_mutate() {
        let mut runner = RecordingRunner::with_outputs([command_failure("query failed")]);
        let error = FirewallBackend::Firewalld
            .open_with(TCP_443, &mut runner)
            .expect_err("query failure");
        assert_eq!(error.as_str(), "query failed");
        assert_eq!(
            runner.calls,
            vec!["firewall-cmd --quiet --query-port=443/tcp"]
        );
    }

    #[test]
    fn firewalld_api_ingress_uses_only_the_owned_interface_zone() {
        let mut runner = RecordingRunner::with_outputs([
            active("public ployz trusted\n"),
            active("Managed by Ployz built-in WireGuard\n"),
            active(""),
            active(""),
            active(""),
            active(""),
        ]);

        FirewallBackend::Firewalld
            .allow_control_plane_http_with("ployz0", 2_020, &mut runner)
            .expect("API ingress");

        assert_eq!(
            runner.calls,
            vec![
                "firewall-cmd --permanent --get-zones",
                "firewall-cmd --permanent --zone=ployz --get-description",
                "firewall-cmd --permanent --zone=ployz --change-interface=ployz0",
                "firewall-cmd --permanent --zone=ployz --list-rich-rules",
                "firewall-cmd --permanent --zone=ployz --add-rich-rule=rule family=\"ipv6\" port port=\"2020\" protocol=\"tcp\" accept",
                "firewall-cmd --reload",
            ]
        );
    }

    #[test]
    fn ufw_interface_and_forwarding_use_the_managed_backend() {
        let mut runner =
            RecordingRunner::with_outputs([active(""), active(""), active(""), active("")]);

        FirewallBackend::Ufw
            .allow_control_plane_http_with("ployz0", 2_020, &mut runner)
            .expect("API ingress");
        FirewallBackend::Ufw
            .allow_forwarding_between_with("ployz0", "br-ployz", &mut runner)
            .expect("forwarding");

        assert_eq!(
            runner.calls,
            vec![
                "ufw status numbered",
                "ufw allow in on ployz0 from ::/0 to any port 2020 proto tcp comment ployz-api-http",
                "ufw route allow in on ployz0 out on br-ployz",
                "ufw route allow in on br-ployz out on ployz0",
            ]
        );
    }

    #[test]
    fn machine_only_udp_chain_accepts_machines_and_rejects_roaming_peers() {
        let status = concat!(
            "[ 1] 51820/udp ALLOW IN Anywhere\n",
            "[ 2] 51820/udp (v6) ALLOW IN Anywhere (v6)\n",
        );
        let mut runner = RecordingRunner::with_outputs([
            active(status),
            active(status),
            active(""),
            active("-P INPUT ACCEPT\n"),
            active(""),
        ]);
        let sources = BTreeSet::from([
            "fd12:3456:789a:1::/112".parse().expect("source"),
            "fd12:3456:789a:2::/112".parse().expect("source"),
        ]);

        FirewallBackend::Ufw
            .restrict_udp_to_ipv6_sources_with("ployz0", 8_787, &sources, &mut runner)
            .expect("machine-only gossip");

        let [status, refreshed, ufw, observe, restore] = runner.calls.as_slice() else {
            panic!(
                "expected UFW backstop, save, and restore: {:?}",
                runner.calls
            );
        };
        assert_eq!(status, "ufw status numbered");
        assert_eq!(refreshed, "ufw status numbered");
        assert_eq!(
            ufw,
            "ufw insert 2 deny in on ployz0 from ::/0 to any port 8787 proto udp comment ployz-corrosion-gossip"
        );
        assert_eq!(observe, "ip6tables -S INPUT");
        assert!(restore.starts_with("ip6tables-restore --noflush --wait 5 "));
    }

    #[test]
    fn repeated_convergence_prunes_stale_duplicate_jumps_and_keeps_foreign_rules() {
        let mut runner = RecordingRunner::with_outputs([
            active("[ 1] 8787/udp (v6) on ployz0 DENY IN Anywhere (v6) # ployz-corrosion-gossip\n"),
            active(concat!(
                "-P INPUT ACCEPT\n",
                "-A INPUT -p tcp --dport 22 -j ACCEPT\n",
                "-A INPUT -i old0 -p udp -m udp --dport 7777 -j PLOYZ-CORROSION\n",
                "-A INPUT -i ployz0 -p udp -m udp --dport 8787 -j PLOYZ-CORROSION\n",
            )),
            active(""),
        ]);
        let sources = BTreeSet::from(["fd12:3456:789a:3::/112".parse().expect("source")]);

        FirewallBackend::Ufw
            .restrict_udp_to_ipv6_sources_with("ployz0", 8_787, &sources, &mut runner)
            .expect("repeat convergence");

        let [status, observe, restore] = runner.calls.as_slice() else {
            panic!(
                "expected UFW backstop, save, and restore: {:?}",
                runner.calls
            );
        };
        assert_eq!(status, "ufw status numbered");
        assert_eq!(observe, "ip6tables -S INPUT");
        assert!(restore.starts_with("ip6tables-restore --noflush --wait 5 "));
    }

    #[test]
    fn unmanaged_firewall_refuses_mesh_capabilities_without_mutation() {
        let mut runner = RecordingRunner::default();
        let backend = FirewallBackend::Unmanaged("shorewall".to_owned());

        assert!(
            backend
                .allow_control_plane_http_with("ployz0", 2_020, &mut runner)
                .is_err()
        );
        assert!(
            backend
                .restrict_udp_to_ipv6_sources_with("ployz0", 8_787, &BTreeSet::new(), &mut runner,)
                .is_err()
        );
        assert!(
            backend
                .allow_forwarding_between_with("ployz0", "br-ployz", &mut runner)
                .is_err()
        );
        assert!(runner.calls.is_empty());
    }

    #[test]
    fn ufw_open_is_idempotent() {
        let mut runner = RecordingRunner::with_outputs([active(
            "443/tcp                 ALLOW IN    Anywhere                   # managed\n",
        )]);
        FirewallBackend::Ufw
            .open_with(TCP_443, &mut runner)
            .expect("open port");
        assert_eq!(runner.calls, vec!["ufw status verbose"]);
    }

    #[test]
    fn ufw_open_rejects_scoped_allow_rule() {
        let scoped = "443/tcp                 ALLOW IN    192.0.2.0/24\n443/tcp                 ALLOW IN    Anywhere\n";
        let mut runner = RecordingRunner::with_outputs([
            active(scoped),
            active("[ 1] 443/tcp ALLOW IN 192.0.2.0/24\n"),
            active(""),
            active("443/tcp                 ALLOW IN    Anywhere\n"),
        ]);
        FirewallBackend::Ufw
            .open_with(TCP_443, &mut runner)
            .expect("broad allow opens");
        assert_eq!(
            runner.calls,
            vec![
                "ufw status verbose",
                "ufw status numbered",
                "ufw insert 1 allow 443/tcp",
                "ufw status verbose"
            ]
        );
    }

    #[test]
    fn ufw_open_uses_plain_allow_for_an_empty_ruleset_and_verifies_it() {
        let mut runner = RecordingRunner::with_outputs([
            active(""),
            active(""),
            active(""),
            active("443/tcp                 ALLOW IN    Anywhere\n"),
        ]);

        FirewallBackend::Ufw
            .open_with(TCP_443, &mut runner)
            .expect("missing broad allow opens");

        assert_eq!(
            runner.calls,
            vec![
                "ufw status verbose",
                "ufw status numbered",
                "ufw allow 443/tcp",
                "ufw status verbose"
            ]
        );
    }

    #[test]
    fn ufw_open_rejects_forward_allow_rule() {
        let mut runner = RecordingRunner::with_outputs([
            active(
                "443/tcp                 ALLOW FWD   Anywhere\n443/tcp                 ALLOW IN    Anywhere\n",
            ),
            active("[ 1] 443/tcp ALLOW FWD Anywhere\n"),
            active(""),
            active("443/tcp                 ALLOW IN    Anywhere\n"),
        ]);
        FirewallBackend::Ufw
            .open_with(TCP_443, &mut runner)
            .expect("inbound broad allow opens");
        assert_eq!(
            runner.calls,
            vec![
                "ufw status verbose",
                "ufw status numbered",
                "ufw insert 1 allow 443/tcp",
                "ufw status verbose"
            ]
        );
    }

    #[test]
    fn ufw_open_ignores_v6_only_rule() {
        let mut runner = RecordingRunner::with_outputs([
            active("443/tcp (v6)            ALLOW IN    Anywhere (v6)\n"),
            active("[ 1] 443/tcp (v6) ALLOW IN Anywhere (v6)\n"),
            active(""),
            active("443/tcp                 ALLOW IN    Anywhere\n"),
        ]);
        FirewallBackend::Ufw
            .open_with(TCP_443, &mut runner)
            .expect("IPv4 broad allow opens");
        assert_eq!(
            runner.calls,
            vec![
                "ufw status verbose",
                "ufw status numbered",
                "ufw insert 1 allow 443/tcp",
                "ufw status verbose"
            ]
        );
    }

    #[test]
    fn ufw_open_skips_v6_before_ipv4_allow() {
        let mut runner = RecordingRunner::with_outputs([active(
            "443/tcp (v6)            ALLOW IN    Anywhere (v6)\n443/tcp                 ALLOW IN    Anywhere\n",
        )]);
        FirewallBackend::Ufw
            .open_with(TCP_443, &mut runner)
            .expect("IPv4 broad allow already exists");
        assert_eq!(runner.calls, vec!["ufw status verbose"]);
    }

    #[test]
    fn ufw_open_propagates_query_and_allow_failures() {
        let mut query_failure = RecordingRunner::with_outputs([command_failure("query failed")]);
        let error = FirewallBackend::Ufw
            .open_with(TCP_443, &mut query_failure)
            .expect_err("query failure propagates");
        assert_eq!(error.as_str(), "query failed");
        assert_eq!(query_failure.calls, vec!["ufw status verbose"]);

        let mut allow_failure = RecordingRunner::with_outputs([
            active("443/tcp                 DENY IN     Anywhere\n"),
            active("[ 1] 443/tcp DENY IN Anywhere\n"),
            command_failure("allow failed"),
        ]);
        let error = FirewallBackend::Ufw
            .open_with(TCP_443, &mut allow_failure)
            .expect_err("allow failure propagates");
        assert_eq!(error.as_str(), "allow failed");
        assert_eq!(
            allow_failure.calls,
            vec![
                "ufw status verbose",
                "ufw status numbered",
                "ufw insert 1 allow 443/tcp"
            ]
        );
    }

    #[test]
    fn ufw_open_replaces_action_only_collision_and_verifies_the_result() {
        let deny_then_allow = "443/tcp                 DENY IN     Anywhere\n443/tcp                 ALLOW IN    Anywhere\n";
        let mut runner = RecordingRunner::with_outputs([
            active(deny_then_allow),
            active("[ 1] 443/tcp DENY IN Anywhere\n[ 2] 443/tcp ALLOW IN Anywhere\n"),
            active(""),
            active("443/tcp                 ALLOW IN    Anywhere\n"),
        ]);

        FirewallBackend::Ufw
            .open_with(TCP_443, &mut runner)
            .expect("plain allow replaces the colliding action");

        assert_eq!(
            runner.calls,
            vec![
                "ufw status verbose",
                "ufw status numbered",
                "ufw insert 1 allow 443/tcp",
                "ufw status verbose"
            ]
        );
    }

    #[test]
    fn ufw_open_fails_when_the_final_postcondition_is_unassured() {
        let deny_then_allow = "443/tcp                 DENY IN     Anywhere\n443/tcp                 ALLOW IN    Anywhere\n";
        let mut runner = RecordingRunner::with_outputs([
            active(deny_then_allow),
            active("[ 1] 443/tcp DENY IN Anywhere\n[ 2] 443/tcp ALLOW IN Anywhere\n"),
            active(""),
            active(deny_then_allow),
        ]);

        let error = FirewallBackend::Ufw
            .open_with(TCP_443, &mut runner)
            .expect_err("an unassured final rule is an error");

        assert_eq!(
            error.as_str(),
            "UFW did not assure inbound reachability for 443/tcp"
        );
        assert_eq!(
            runner.calls,
            vec![
                "ufw status verbose",
                "ufw status numbered",
                "ufw insert 1 allow 443/tcp",
                "ufw status verbose"
            ]
        );
    }

    #[test]
    fn ufw_open_propagates_post_allow_query_failure() {
        let mut runner = RecordingRunner::with_outputs([
            active(""),
            active(""),
            active(""),
            command_failure("post-insert query failed"),
        ]);

        let error = FirewallBackend::Ufw
            .open_with(TCP_443, &mut runner)
            .expect_err("query failure propagates");

        assert_eq!(error.as_str(), "post-insert query failed");
        assert_eq!(
            runner.calls,
            vec![
                "ufw status verbose",
                "ufw status numbered",
                "ufw allow 443/tcp",
                "ufw status verbose"
            ]
        );
    }

    #[test]
    fn ufw_open_propagates_numbered_status_failure_without_mutation() {
        let mut runner =
            RecordingRunner::with_outputs([active(""), command_failure("numbered status failed")]);

        let error = FirewallBackend::Ufw
            .open_with(TCP_443, &mut runner)
            .expect_err("numbered status failure propagates");

        assert_eq!(error.as_str(), "numbered status failed");
        assert_eq!(
            runner.calls,
            vec!["ufw status verbose", "ufw status numbered"]
        );
    }

    #[test]
    fn ufw_open_refuses_truncated_numbered_status_without_mutation() {
        let mut truncated = active("[ 1] 22/tcp ALLOW IN Anywhere\n").expect("output");
        truncated.stdout_truncated = true;
        let mut runner = RecordingRunner::with_outputs([active(""), Ok(truncated)]);

        let error = FirewallBackend::Ufw
            .open_with(TCP_443, &mut runner)
            .expect_err("truncated status is uncertain");

        assert_eq!(error.as_str(), "UFW numbered status was truncated");
        assert_eq!(
            runner.calls,
            vec!["ufw status verbose", "ufw status numbered"]
        );
    }

    #[test]
    fn close_remains_query_before_remove() {
        let mut runner = RecordingRunner::with_outputs([
            active("443/tcp                 ALLOW IN    Anywhere\n"),
            active(""),
        ]);
        FirewallBackend::Ufw
            .close_with(TCP_443, &mut runner)
            .expect("close port");
        assert_eq!(
            runner.calls,
            vec!["ufw status verbose", "ufw delete allow 443/tcp"]
        );
    }
}
