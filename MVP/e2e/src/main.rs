mod actor_contract;
mod assertions;
mod authority_contract;
mod bridge_contract;
mod bus_contract;
mod bus_syntax;
mod deploy_candidate_cleanup_contract;
mod deploy_commit_drain_contract;
mod deploy_restart_recovery_contract;
mod docs_backed_acme_http01_contract;
#[cfg(unix)]
mod environment_branch_promote_rollback_contract;
#[cfg(not(unix))]
mod environment_branch_promote_rollback_contract {
    pub(crate) fn run() -> Result<(), String> {
        Err("environment-branch-promote-rollback-contract uses process roles".to_string())
    }

    pub(crate) fn cleanup_orphaned_children() -> Result<(), String> {
        Ok(())
    }
}
mod iroh_docs_contract;
mod lease_acme_contract;
#[cfg(unix)]
mod machine_remove_contract;
#[cfg(not(unix))]
mod machine_remove_contract {
    pub(crate) fn run() -> Result<(), String> {
        Err("machine-remove-contract uses process roles in the MVP harness".to_string())
    }

    pub(crate) fn cleanup_orphaned_children() -> Result<(), String> {
        Ok(())
    }
}
#[cfg(unix)]
mod membership_wireguard_contract;
#[cfg(not(unix))]
mod membership_wireguard_contract {
    pub(crate) fn run() -> Result<(), String> {
        Err("membership-wireguard-contract uses process roles in the MVP harness".to_string())
    }

    pub(crate) fn cleanup_orphaned_children() -> Result<(), String> {
        Ok(())
    }
}
mod metrics;
mod p2panda_acme_http01_contract;
mod p2panda_fact_source_contract;
mod p2panda_net_fact_node_contract;
#[cfg(unix)]
mod p2panda_net_process_serving_contract;
#[cfg(not(unix))]
mod p2panda_net_process_serving_contract {
    pub(crate) fn run() -> Result<(), String> {
        Err("p2panda-net-process-serving-contract uses Unix sockets in the MVP harness".to_string())
    }

    pub(crate) fn cleanup_orphaned_children() -> Result<(), String> {
        Ok(())
    }
}
#[cfg(unix)]
mod p2panda_process_role_serving_contract;
mod p2panda_projection_fixture;
mod p2panda_sync_fact_source_contract;
#[cfg(not(unix))]
mod p2panda_process_role_serving_contract {
    pub(crate) fn run() -> Result<(), String> {
        Err(
            "p2panda-process-role-serving-contract uses Unix sockets in the MVP harness"
                .to_string(),
        )
    }

    pub(crate) fn cleanup_orphaned_children() -> Result<(), String> {
        Ok(())
    }
}
mod process_fact_source;
#[cfg(unix)]
mod process_role_harness;
#[cfg(unix)]
mod process_role_serving_contract;
#[cfg(unix)]
mod wire_serving_contract;
#[cfg(not(unix))]
mod process_role_serving_contract {
    pub(crate) fn run() -> Result<(), String> {
        Err("process-role-serving-contract uses Unix sockets in the MVP harness".to_string())
    }

    pub(crate) fn cleanup_orphaned_children() -> Result<(), String> {
        Ok(())
    }
}
#[cfg(not(unix))]
mod wire_serving_contract {
    pub(crate) fn run() -> Result<(), String> {
        Err("wire-serving-contract uses Unix sockets in the MVP harness".to_string())
    }

    pub(crate) fn cleanup_orphaned_children() -> Result<(), String> {
        Ok(())
    }
}
mod projection_contract;
mod projection_harness;
mod scale;
mod steady_state_serving_contract;
mod volume_transfer_contract;

use std::env;
use std::process;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

struct Scenario {
    name: &'static str,
    run: fn() -> Result<(), String>,
    cleanup: Option<fn() -> Result<(), String>>,
}

impl Scenario {
    #[must_use]
    const fn new(name: &'static str, run: fn() -> Result<(), String>) -> Self {
        Self {
            name,
            run,
            cleanup: None,
        }
    }

    #[must_use]
    const fn with_cleanup(
        name: &'static str,
        run: fn() -> Result<(), String>,
        cleanup: fn() -> Result<(), String>,
    ) -> Self {
        Self {
            name,
            run,
            cleanup: Some(cleanup),
        }
    }
}

const SCENARIOS: &[Scenario] = &[
    Scenario::new("bus-contract", bus_contract::run),
    Scenario::new("actor-contract", actor_contract::run),
    Scenario::new("authority-contract", authority_contract::run),
    Scenario::new("bridge-contract", bridge_contract::run),
    Scenario::new("projection-contract", projection_contract::run),
    Scenario::new(
        "p2panda-fact-source-contract",
        p2panda_fact_source_contract::run,
    ),
    Scenario::new(
        "p2panda-sync-fact-source-contract",
        p2panda_sync_fact_source_contract::run,
    ),
    Scenario::new(
        "p2panda-acme-http01-contract",
        p2panda_acme_http01_contract::run,
    ),
    Scenario::new(
        "p2panda-net-fact-node-contract",
        p2panda_net_fact_node_contract::run,
    ),
    Scenario::new("iroh-docs-contract", iroh_docs_contract::run),
    Scenario::new("lease-acme-contract", lease_acme_contract::run),
    Scenario::new(
        "docs-backed-acme-http01-contract",
        docs_backed_acme_http01_contract::run,
    ),
    Scenario::new(
        "deploy-commit-drain-contract",
        deploy_commit_drain_contract::run,
    ),
    Scenario::new(
        "deploy-candidate-cleanup-contract",
        deploy_candidate_cleanup_contract::run,
    ),
    Scenario::new(
        "deploy-restart-recovery-contract",
        deploy_restart_recovery_contract::run,
    ),
    Scenario::with_cleanup(
        "environment-branch-promote-rollback-contract",
        environment_branch_promote_rollback_contract::run,
        environment_branch_promote_rollback_contract::cleanup_orphaned_children,
    ),
    Scenario::new(
        "steady-state-serving-contract",
        steady_state_serving_contract::run,
    ),
    Scenario::with_cleanup(
        "process-role-serving-contract",
        process_role_serving_contract::run,
        process_role_serving_contract::cleanup_orphaned_children,
    ),
    Scenario::with_cleanup(
        "p2panda-process-role-serving-contract",
        p2panda_process_role_serving_contract::run,
        p2panda_process_role_serving_contract::cleanup_orphaned_children,
    ),
    Scenario::with_cleanup(
        "p2panda-net-process-serving-contract",
        p2panda_net_process_serving_contract::run,
        p2panda_net_process_serving_contract::cleanup_orphaned_children,
    ),
    Scenario::with_cleanup(
        "wire-serving-contract",
        wire_serving_contract::run,
        wire_serving_contract::cleanup_orphaned_children,
    ),
    Scenario::with_cleanup(
        "membership-wireguard-contract",
        membership_wireguard_contract::run,
        membership_wireguard_contract::cleanup_orphaned_children,
    ),
    Scenario::with_cleanup(
        "machine-remove-contract",
        machine_remove_contract::run,
        machine_remove_contract::cleanup_orphaned_children,
    ),
    Scenario::new("volume-transfer-contract", volume_transfer_contract::run),
    Scenario::new("scale", scale::run),
];

fn main() {
    if let Err(error) = run() {
        eprintln!("FAIL {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args();
    let _bin = args.next();
    let scenario = args.next().unwrap_or_else(|| String::from("bus-contract"));
    if scenario == "role" {
        return run_role(args.collect());
    }
    match scenario.as_str() {
        "all" => run_all_with_budget(),
        "help" | "--help" | "-h" => {
            println!("usage: cargo run -p mvp-e2e -- <{}|all>", scenario_help());
            Ok(())
        }
        other => run_named_scenario(other),
    }
}

#[cfg(unix)]
fn run_role(args: Vec<String>) -> Result<(), String> {
    process_role_harness::run_role(args)
}

#[cfg(not(unix))]
fn run_role(_args: Vec<String>) -> Result<(), String> {
    Err("process roles use Unix sockets in the MVP harness".to_string())
}

fn run_named_scenario(scenario: &str) -> Result<(), String> {
    for candidate in SCENARIOS {
        if candidate.name == scenario {
            return (candidate.run)();
        }
    }
    Err(format!("unknown MVP E2E scenario '{scenario}'"))
}

fn run_all_with_budget() -> Result<(), String> {
    let budget = e2e_all_budget()?;
    run_scenarios_with_budget(SCENARIOS, budget)
}

fn run_scenarios_with_budget(
    scenarios: &'static [Scenario],
    budget: Duration,
) -> Result<(), String> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(run_scenarios(scenarios));
    });
    match receiver.recv_timeout(budget) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let cleanup = cleanup_timed_out_scenarios(scenarios);
            Err(format!(
                "all scenario exceeded {budget:?} budget; cleanup={cleanup}"
            ))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err("all scenario worker exited without a result".to_string())
        }
    }
}

fn run_scenarios(scenarios: &[Scenario]) -> Result<(), String> {
    for scenario in scenarios {
        (scenario.run)()?;
    }
    Ok(())
}

fn cleanup_timed_out_scenarios(scenarios: &[Scenario]) -> String {
    let results = scenarios
        .iter()
        .filter_map(|scenario| {
            scenario
                .cleanup
                .map(|cleanup| format!("{}={:?}", scenario.name, cleanup()))
        })
        .collect::<Vec<_>>();
    if results.is_empty() {
        "none".to_string()
    } else {
        results.join("; ")
    }
}

fn e2e_all_budget() -> Result<Duration, String> {
    let value = env::var("MVP_E2E_ALL_TIMEOUT").unwrap_or_else(|_| "120s".to_string());
    parse_duration(&value).ok_or_else(|| {
        format!("MVP_E2E_ALL_TIMEOUT must be a positive duration like 120s, got '{value}'")
    })
}

fn scenario_help() -> String {
    SCENARIOS
        .iter()
        .map(|scenario| scenario.name)
        .collect::<Vec<_>>()
        .join("|")
}

fn parse_duration(value: &str) -> Option<Duration> {
    let value = value.trim();
    let seconds = value
        .strip_suffix('s')
        .unwrap_or(value)
        .parse::<u64>()
        .ok()?;
    if seconds == 0 {
        return None;
    }
    Some(Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::{SCENARIOS, Scenario, parse_duration, run_scenarios_with_budget, scenario_help};
    use std::time::Duration;

    #[test]
    fn all_scenarios_include_iroh_and_lease_contracts() {
        let names = SCENARIOS
            .iter()
            .map(|scenario| scenario.name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"iroh-docs-contract"));
        assert!(names.contains(&"lease-acme-contract"));
        assert!(names.contains(&"deploy-commit-drain-contract"));
        assert!(names.contains(&"environment-branch-promote-rollback-contract"));
        assert!(names.contains(&"steady-state-serving-contract"));
        assert!(names.contains(&"process-role-serving-contract"));
        assert!(names.contains(&"p2panda-process-role-serving-contract"));
        assert!(names.contains(&"p2panda-net-process-serving-contract"));
        assert!(names.contains(&"wire-serving-contract"));
        assert!(names.contains(&"membership-wireguard-contract"));
        assert!(names.contains(&"machine-remove-contract"));
        assert!(scenario_help().contains("lease-acme-contract"));
        assert!(scenario_help().contains("environment-branch-promote-rollback-contract"));
        assert!(scenario_help().contains("p2panda-process-role-serving-contract"));
        assert!(scenario_help().contains("p2panda-net-process-serving-contract"));
        assert!(scenario_help().contains("membership-wireguard-contract"));
        assert!(scenario_help().contains("machine-remove-contract"));
    }

    #[test]
    fn parse_duration_accepts_positive_seconds_with_or_without_suffix() {
        assert_eq!(parse_duration("3s"), Some(Duration::from_secs(3)));
        assert_eq!(parse_duration("4"), Some(Duration::from_secs(4)));
    }

    #[test]
    fn parse_duration_rejects_zero_and_invalid_values() {
        assert_eq!(parse_duration("0s"), None);
        assert_eq!(parse_duration("0"), None);
        assert_eq!(parse_duration("soon"), None);
    }

    #[test]
    fn all_budget_is_enforced_while_scenario_is_running() {
        fn blocking_scenario() -> Result<(), String> {
            std::thread::sleep(Duration::from_millis(100));
            Ok(())
        }

        fn cleanup_blocking_scenario() -> Result<(), String> {
            Err("cleaned".to_string())
        }

        static BLOCKING: &[Scenario] = &[Scenario::with_cleanup(
            "blocking",
            blocking_scenario,
            cleanup_blocking_scenario,
        )];

        let error = run_scenarios_with_budget(BLOCKING, Duration::from_millis(10))
            .expect_err("budget should expire before blocking scenario returns");

        assert!(error.contains("exceeded"));
        assert!(error.contains("cleanup=blocking=Err(\"cleaned\")"));
    }
}
