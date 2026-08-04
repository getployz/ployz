use ployz_core::operation::FailureMessage;

use super::{FirewallBackend, command_probe, failure_message, nft};
use crate::{HostRunnerCommandRunner, SupervisorBackend};

pub fn detect_firewall_backend(
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
        &[1, 127],
    )?;
    if nft_service_active || nft_installed {
        let output = runner.command("nft", &["list", "ruleset"])?;
        if !output.success {
            return Err(failure_message(output.failure));
        }
        if nft::manages_input(&output.stdout) {
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
        &[1, 127],
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
