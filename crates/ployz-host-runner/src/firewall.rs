//! Host firewall detection and port operations.

use ployz_core::ops::FailureMessage;

use crate::assigned_substrate::{AssignedHostPort, HostPortProtocol};
use crate::command::HostRunnerCommandRunner;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirewallBackend {
    Firewalld,
    Ufw,
    RawRules,
    None,
}

pub(crate) fn detect_firewall_backend(
    runner: &mut impl HostRunnerCommandRunner,
) -> FirewallBackend {
    detect(runner)
}

impl FirewallBackend {
    pub(crate) fn query_with(
        self,
        port: AssignedHostPort,
        runner: &mut impl HostRunnerCommandRunner,
    ) -> Result<bool, FailureMessage> {
        match self {
            Self::Firewalld => {
                let port = render_port(port);
                Ok(success(
                    runner,
                    "firewall-cmd",
                    &["--quiet", &format!("--query-port={port}")],
                ) && success(
                    runner,
                    "firewall-cmd",
                    &["--permanent", "--quiet", &format!("--query-port={port}")],
                ))
            }
            Self::Ufw => {
                let output = runner.command("ufw", &["status"])?;
                if !output.success {
                    return Err(failure_message(output.failure));
                }
                let port = render_port(port);
                Ok(output
                    .stdout
                    .lines()
                    .any(|line| line.split_whitespace().next() == Some(port.as_str())))
            }
            Self::None => Ok(true),
            Self::RawRules => Err(unmanaged_raw_rules()),
        }
    }

    pub(crate) fn open_with(
        self,
        port: AssignedHostPort,
        runner: &mut impl HostRunnerCommandRunner,
    ) -> Result<(), FailureMessage> {
        match self {
            Self::Firewalld => change_firewalld(port, runner, FirewallChange::Open),
            Self::Ufw if self.query_with(port, runner)? => Ok(()),
            Self::Ufw => require_success(runner, "ufw", &["allow", &render_port(port)]),
            Self::None => Ok(()),
            Self::RawRules => Err(unmanaged_raw_rules()),
        }
    }

    pub fn close_with(
        self,
        port: AssignedHostPort,
        runner: &mut impl HostRunnerCommandRunner,
    ) -> Result<(), FailureMessage> {
        match self {
            Self::Firewalld => change_firewalld(port, runner, FirewallChange::Close),
            Self::Ufw if !self.query_with(port, runner)? => Ok(()),
            Self::Ufw => require_success(runner, "ufw", &["delete", "allow", &render_port(port)]),
            Self::None => Ok(()),
            Self::RawRules => Err(unmanaged_raw_rules()),
        }
    }
}

#[derive(Clone, Copy)]
enum FirewallChange {
    Open,
    Close,
}

fn detect(runner: &mut impl HostRunnerCommandRunner) -> FirewallBackend {
    if success(runner, "systemctl", &["is-active", "--quiet", "firewalld"]) {
        return FirewallBackend::Firewalld;
    }
    if runner.command("ufw", &["status"]).is_ok_and(|output| {
        output.success
            && output
                .stdout
                .lines()
                .any(|line| line.trim().eq_ignore_ascii_case("status: active"))
    }) {
        return FirewallBackend::Ufw;
    }
    if runner
        .command("nft", &["list", "ruleset"])
        .is_ok_and(|output| output.success && !output.stdout.trim().is_empty())
        || runner.command("iptables", &["-S"]).is_ok_and(|output| {
            output.success && output.stdout.lines().any(|line| line.starts_with("-A "))
        })
    {
        return FirewallBackend::RawRules;
    }
    FirewallBackend::None
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
        FirewallChange::Close => format!("--remove-port={port}"),
    };
    if success(runner, "firewall-cmd", &["--quiet", &query])
        == matches!(change, FirewallChange::Close)
    {
        require_success(runner, "firewall-cmd", &[&action])?;
    }
    if success(runner, "firewall-cmd", &["--permanent", "--quiet", &query])
        == matches!(change, FirewallChange::Close)
    {
        require_success(runner, "firewall-cmd", &["--permanent", &action])?;
    }
    Ok(())
}

fn success(runner: &mut impl HostRunnerCommandRunner, program: &str, args: &[&str]) -> bool {
    runner
        .command(program, args)
        .is_ok_and(|output| output.success)
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

fn unmanaged_raw_rules() -> FailureMessage {
    failure_message("active raw nftables/iptables rules are not managed by Ployz")
}

fn failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message).expect("firewall failure message is non-empty")
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::Path;

    use crate::command::HostRunnerCommandOutput;

    use super::*;

    const TCP_4222: AssignedHostPort = AssignedHostPort {
        port: 4222,
        protocol: HostPortProtocol::Tcp,
    };

    #[derive(Default)]
    struct RecordingRunner {
        calls: Vec<String>,
        outputs: VecDeque<HostRunnerCommandOutput>,
    }

    impl RecordingRunner {
        fn with_outputs(outputs: impl IntoIterator<Item = HostRunnerCommandOutput>) -> Self {
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
            Ok(self.outputs.pop_front().unwrap_or_else(failed))
        }
        fn is_linux(&mut self) -> bool {
            unreachable!()
        }
        fn current_uid(&mut self) -> Result<u32, FailureMessage> {
            unreachable!()
        }
        fn systemctl(&mut self, _: &[&str]) -> Result<(), FailureMessage> {
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
        fn enable_docker_service(&mut self) -> Result<(), FailureMessage> {
            unreachable!()
        }
        fn run_docker_install_script(&mut self, _: &Path) -> Result<(), FailureMessage> {
            unreachable!()
        }
        fn prepare_dataplane_host(&mut self) -> Result<(), FailureMessage> {
            unreachable!()
        }
    }

    fn succeeded(stdout: &str) -> HostRunnerCommandOutput {
        HostRunnerCommandOutput {
            success: true,
            stdout: stdout.to_owned(),
            failure: String::new(),
        }
    }

    fn failed() -> HostRunnerCommandOutput {
        HostRunnerCommandOutput {
            success: false,
            stdout: String::new(),
            failure: "command failed".to_owned(),
        }
    }

    #[test]
    fn detection_prefers_active_firewalld() {
        assert_eq!(
            detect(&mut RecordingRunner::with_outputs([succeeded("")])),
            FirewallBackend::Firewalld
        );
    }

    #[test]
    fn detection_uses_active_ufw_when_firewalld_is_inactive() {
        let mut runner = RecordingRunner::with_outputs([failed(), succeeded("Status: active\n")]);
        assert_eq!(detect(&mut runner), FirewallBackend::Ufw);
    }

    #[test]
    fn detection_reports_active_raw_rules() {
        let mut runner = RecordingRunner::with_outputs([
            failed(),
            succeeded("Status: inactive\n"),
            succeeded("table inet filter {}\n"),
        ]);
        assert_eq!(detect(&mut runner), FirewallBackend::RawRules);
    }

    #[test]
    fn detection_reports_none_when_managers_and_raw_rules_are_inactive() {
        let mut runner = RecordingRunner::with_outputs([
            failed(),
            succeeded("Status: inactive\n"),
            succeeded(""),
            succeeded("-P INPUT ACCEPT\n"),
        ]);
        assert_eq!(detect(&mut runner), FirewallBackend::None);
    }

    #[test]
    fn firewalld_open_queries_then_adds_only_missing_runtime_rule() {
        let mut runner = RecordingRunner::with_outputs([failed(), succeeded(""), succeeded("")]);
        FirewallBackend::Firewalld
            .open_with(TCP_4222, &mut runner)
            .expect("open port");
        assert_eq!(
            runner.calls,
            vec![
                "firewall-cmd --quiet --query-port=4222/tcp",
                "firewall-cmd --add-port=4222/tcp",
                "firewall-cmd --permanent --quiet --query-port=4222/tcp"
            ]
        );
    }

    #[test]
    fn firewalld_query_requires_runtime_and_permanent_rules() {
        let mut runner = RecordingRunner::with_outputs([succeeded(""), failed()]);
        assert!(
            !FirewallBackend::Firewalld
                .query_with(TCP_4222, &mut runner)
                .expect("query port")
        );
    }

    #[test]
    fn firewalld_close_removes_present_runtime_and_permanent_rules() {
        let mut runner = RecordingRunner::with_outputs([
            succeeded(""),
            succeeded(""),
            succeeded(""),
            succeeded(""),
        ]);
        FirewallBackend::Firewalld
            .close_with(TCP_4222, &mut runner)
            .expect("close port");
        assert_eq!(
            runner.calls,
            vec![
                "firewall-cmd --quiet --query-port=4222/tcp",
                "firewall-cmd --remove-port=4222/tcp",
                "firewall-cmd --permanent --quiet --query-port=4222/tcp",
                "firewall-cmd --permanent --remove-port=4222/tcp"
            ]
        );
    }

    #[test]
    fn ufw_open_is_idempotent() {
        let mut runner = RecordingRunner::with_outputs([succeeded("4222/tcp ALLOW Anywhere\n")]);
        FirewallBackend::Ufw
            .open_with(TCP_4222, &mut runner)
            .expect("open port");
        assert_eq!(runner.calls, vec!["ufw status"]);
    }

    #[test]
    fn ufw_close_queries_then_deletes_present_rule() {
        let mut runner =
            RecordingRunner::with_outputs([succeeded("4222/tcp ALLOW Anywhere\n"), succeeded("")]);
        FirewallBackend::Ufw
            .close_with(TCP_4222, &mut runner)
            .expect("close port");
        assert_eq!(
            runner.calls,
            vec!["ufw status", "ufw delete allow 4222/tcp"]
        );
    }

    #[test]
    fn raw_rules_refuse_port_changes() {
        let error = FirewallBackend::RawRules
            .open_with(TCP_4222, &mut RecordingRunner::default())
            .expect_err("raw rules are unmanaged");
        assert!(error.as_str().contains("not managed"));
    }
}
