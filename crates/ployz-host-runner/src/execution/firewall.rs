//! Host firewall detection and port operations.

use ployz_core::operation::FailureMessage;

use super::command::HostRunnerCommandRunner;
use super::supervisor::SupervisorBackend;
use crate::lifecycle::assigned_substrate::{AssignedHostPort, HostPortProtocol};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirewallBackend {
    Firewalld,
    Ufw,
    Unmanaged(String),
    None,
}

pub(crate) fn detect_firewall_backend(
    supervisor: SupervisorBackend,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<FirewallBackend, FailureMessage> {
    if service_active(supervisor, runner, "firewalld")? {
        return Ok(FirewallBackend::Firewalld);
    }
    if service_active(supervisor, runner, "ufw")? {
        let output = runner.command("ufw", &["status"])?;
        if !output.success {
            return Err(failure_message(output.failure));
        }
        if output
            .stdout
            .lines()
            .any(|line| line.trim().eq_ignore_ascii_case("status: active"))
        {
            return Ok(FirewallBackend::Ufw);
        }
    }
    let nft_service_active = service_active(supervisor, runner, "nftables")?;
    let nft_installed = command_probe(
        runner,
        "sh",
        &["-c", "command -v nft >/dev/null 2>&1"],
        &[1],
    )?;
    if nft_service_active || nft_installed {
        let output = runner.command("nft", &["list", "ruleset"])?;
        if !output.success {
            return Err(failure_message(output.failure));
        }
        if nft_manages_input(&output.stdout) {
            return Ok(FirewallBackend::Unmanaged("nftables".to_owned()));
        }
    }
    let mut iptables_service = None;
    for service in ["iptables", "netfilter-persistent"] {
        if service_active(supervisor, runner, service)? {
            iptables_service = Some(service);
        }
    }
    let iptables_installed = command_probe(
        runner,
        "sh",
        &["-c", "command -v iptables >/dev/null 2>&1"],
        &[1],
    )?;
    if iptables_service.is_some() || iptables_installed {
        let output = runner.command("iptables", &["-S", "INPUT"])?;
        if !output.success {
            return Err(failure_message(output.failure));
        }
        if iptables_manages_input(&output.stdout) {
            return Ok(FirewallBackend::Unmanaged(
                iptables_service.unwrap_or("iptables").to_owned(),
            ));
        }
    }

    let output = match supervisor {
        SupervisorBackend::Systemd => runner.command(
            "systemctl",
            &[
                "list-units",
                "--type=service",
                "--state=active",
                "--no-legend",
                "--plain",
            ],
        )?,
        SupervisorBackend::OpenRc => runner.command("rc-status", &["--servicelist"])?,
    };
    if !output.success {
        return Err(failure_message(output.failure));
    }
    if let Some(service) = unknown_firewall_service(&output.stdout) {
        return Ok(FirewallBackend::Unmanaged(service.to_owned()));
    }
    Ok(FirewallBackend::None)
}

fn service_active(
    supervisor: SupervisorBackend,
    runner: &mut impl HostRunnerCommandRunner,
    service: &str,
) -> Result<bool, FailureMessage> {
    match supervisor {
        SupervisorBackend::Systemd => {
            let unit = format!("{service}.service");
            command_probe(
                runner,
                "systemctl",
                &["is-active", "--quiet", &unit],
                &[3, 4],
            )
        }
        SupervisorBackend::OpenRc => {
            command_probe(runner, "rc-service", &[service, "status"], &[1, 3])
        }
    }
}

impl FirewallBackend {
    pub(crate) fn open_with(
        &self,
        port: AssignedHostPort,
        runner: &mut impl HostRunnerCommandRunner,
    ) -> Result<(), FailureMessage> {
        match self {
            Self::Firewalld => change_firewalld(port, runner, FirewallChange::Open),
            Self::Ufw if query_ufw(port, runner)? => Ok(()),
            Self::Ufw => {
                require_success(runner, "ufw", &["insert", "1", "allow", &render_port(port)])
            }
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

fn nft_manages_input(ruleset: &str) -> bool {
    ruleset.lines().any(|line| line.contains("hook input"))
}

fn iptables_manages_input(rules: &str) -> bool {
    rules.lines().any(|line| {
        line.starts_with("-A INPUT ")
            || line
                .strip_prefix("-P INPUT ")
                .is_some_and(|policy| !policy.eq_ignore_ascii_case("ACCEPT"))
    })
}

fn unknown_firewall_service(active_units: &str) -> Option<&str> {
    active_units.lines().find_map(|line| {
        let service = line.split_whitespace().next()?;
        let normalized = service.to_ascii_lowercase();
        let known = [
            "firewalld.service",
            "ufw.service",
            "nftables.service",
            "iptables.service",
            "netfilter-persistent.service",
        ];
        (!known.contains(&normalized.as_str())
            && ["firewall", "shorewall", "firehol", "ferm", "netfilter"]
                .iter()
                .any(|marker| normalized.contains(marker)))
        .then_some(service)
    })
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
mod tests {
    use std::collections::VecDeque;
    use std::path::Path;

    use super::super::command::HostRunnerCommandOutput;

    use super::*;

    const TCP_4222: AssignedHostPort = AssignedHostPort {
        port: 4222,
        protocol: HostPortProtocol::Tcp,
    };

    #[derive(Default)]
    struct RecordingRunner {
        calls: Vec<String>,
        outputs: VecDeque<Result<HostRunnerCommandOutput, FailureMessage>>,
    }

    impl RecordingRunner {
        fn with_outputs(
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

    fn active(stdout: &str) -> Result<HostRunnerCommandOutput, FailureMessage> {
        Ok(HostRunnerCommandOutput {
            success: true,
            exit_code: Some(0),
            stdout: stdout.to_owned(),
            stdout_truncated: false,
            failure: String::new(),
        })
    }

    fn inactive() -> HostRunnerCommandOutput {
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

    fn command_failure(message: &str) -> Result<HostRunnerCommandOutput, FailureMessage> {
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
    fn firewalld_open_queries_then_adds_only_missing_runtime_rule() {
        let mut runner = RecordingRunner::with_outputs([Ok(absent()), active(""), active("")]);
        FirewallBackend::Firewalld
            .open_with(TCP_4222, &mut runner)
            .expect("open port");
        assert_eq!(
            runner.calls,
            vec![
                "firewall-cmd --quiet --query-port=4222/tcp",
                "firewall-cmd --permanent --quiet --query-port=4222/tcp",
                "firewall-cmd --add-port=4222/tcp"
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
            .open_with(TCP_4222, &mut runner)
            .expect_err("permanent query failure");

        assert_eq!(error.as_str(), "permanent query failed");
        assert_eq!(
            runner.calls,
            vec![
                "firewall-cmd --quiet --query-port=4222/tcp",
                "firewall-cmd --permanent --quiet --query-port=4222/tcp"
            ]
        );
    }

    #[test]
    fn firewalld_query_error_does_not_mutate() {
        let mut runner = RecordingRunner::with_outputs([command_failure("query failed")]);
        let error = FirewallBackend::Firewalld
            .open_with(TCP_4222, &mut runner)
            .expect_err("query failure");
        assert_eq!(error.as_str(), "query failed");
        assert_eq!(
            runner.calls,
            vec!["firewall-cmd --quiet --query-port=4222/tcp"]
        );
    }

    #[test]
    fn ufw_open_is_idempotent() {
        let mut runner = RecordingRunner::with_outputs([active(
            "4222/tcp                 ALLOW IN    Anywhere                   # managed\n",
        )]);
        FirewallBackend::Ufw
            .open_with(TCP_4222, &mut runner)
            .expect("open port");
        assert_eq!(runner.calls, vec!["ufw status verbose"]);
    }

    #[test]
    fn ufw_open_rejects_earlier_deny_and_scoped_allow_rules() {
        for rule in [
            "4222/tcp                 DENY IN     Anywhere\n4222/tcp                 ALLOW IN    Anywhere\n",
            "4222/tcp                 ALLOW IN    192.0.2.0/24\n4222/tcp                 ALLOW IN    Anywhere\n",
        ] {
            let mut runner = RecordingRunner::with_outputs([active(rule), active("")]);
            FirewallBackend::Ufw
                .open_with(TCP_4222, &mut runner)
                .expect("broad allow opens");
            assert_eq!(
                runner.calls,
                vec!["ufw status verbose", "ufw insert 1 allow 4222/tcp"]
            );
        }
    }

    #[test]
    fn ufw_open_rejects_forward_allow_rule() {
        let mut runner = RecordingRunner::with_outputs([
            active(
                "4222/tcp                 ALLOW FWD   Anywhere\n4222/tcp                 ALLOW IN    Anywhere\n",
            ),
            active(""),
        ]);
        FirewallBackend::Ufw
            .open_with(TCP_4222, &mut runner)
            .expect("inbound broad allow opens");
        assert_eq!(
            runner.calls,
            vec!["ufw status verbose", "ufw insert 1 allow 4222/tcp"]
        );
    }

    #[test]
    fn ufw_open_ignores_v6_only_rule() {
        let mut runner = RecordingRunner::with_outputs([
            active("4222/tcp (v6)            ALLOW IN    Anywhere (v6)\n"),
            active(""),
        ]);
        FirewallBackend::Ufw
            .open_with(TCP_4222, &mut runner)
            .expect("IPv4 broad allow opens");
        assert_eq!(
            runner.calls,
            vec!["ufw status verbose", "ufw insert 1 allow 4222/tcp"]
        );
    }

    #[test]
    fn ufw_open_skips_v6_before_ipv4_allow() {
        let mut runner = RecordingRunner::with_outputs([active(
            "4222/tcp (v6)            ALLOW IN    Anywhere (v6)\n4222/tcp                 ALLOW IN    Anywhere\n",
        )]);
        FirewallBackend::Ufw
            .open_with(TCP_4222, &mut runner)
            .expect("IPv4 broad allow already exists");
        assert_eq!(runner.calls, vec!["ufw status verbose"]);
    }

    #[test]
    fn ufw_open_propagates_query_and_allow_failures() {
        let mut query_failure = RecordingRunner::with_outputs([command_failure("query failed")]);
        let error = FirewallBackend::Ufw
            .open_with(TCP_4222, &mut query_failure)
            .expect_err("query failure propagates");
        assert_eq!(error.as_str(), "query failed");
        assert_eq!(query_failure.calls, vec!["ufw status verbose"]);

        let mut allow_failure = RecordingRunner::with_outputs([
            active("4222/tcp                 DENY IN     Anywhere\n"),
            command_failure("allow failed"),
        ]);
        let error = FirewallBackend::Ufw
            .open_with(TCP_4222, &mut allow_failure)
            .expect_err("allow failure propagates");
        assert_eq!(error.as_str(), "allow failed");
        assert_eq!(
            allow_failure.calls,
            vec!["ufw status verbose", "ufw insert 1 allow 4222/tcp"]
        );
    }

    #[test]
    fn close_remains_query_before_remove() {
        let mut runner = RecordingRunner::with_outputs([
            active("4222/tcp                 ALLOW IN    Anywhere\n"),
            active(""),
        ]);
        FirewallBackend::Ufw
            .close_with(TCP_4222, &mut runner)
            .expect("close port");
        assert_eq!(
            runner.calls,
            vec!["ufw status verbose", "ufw delete allow 4222/tcp"]
        );
    }
}
