mod actor_contract;
mod assertions;
mod authority_contract;
mod bridge_contract;
mod bus_contract;
mod bus_syntax;
mod deploy_commit_drain_contract;
mod iroh_docs_contract;
mod lease_acme_contract;
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

use std::env;
use std::process;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

struct Scenario {
    name: &'static str,
    run: fn() -> Result<(), String>,
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "bus-contract",
        run: bus_contract::run,
    },
    Scenario {
        name: "actor-contract",
        run: actor_contract::run,
    },
    Scenario {
        name: "authority-contract",
        run: authority_contract::run,
    },
    Scenario {
        name: "bridge-contract",
        run: bridge_contract::run,
    },
    Scenario {
        name: "projection-contract",
        run: projection_contract::run,
    },
    Scenario {
        name: "iroh-docs-contract",
        run: iroh_docs_contract::run,
    },
    Scenario {
        name: "lease-acme-contract",
        run: lease_acme_contract::run,
    },
    Scenario {
        name: "deploy-commit-drain-contract",
        run: deploy_commit_drain_contract::run,
    },
    Scenario {
        name: "steady-state-serving-contract",
        run: steady_state_serving_contract::run,
    },
    Scenario {
        name: "process-role-serving-contract",
        run: process_role_serving_contract::run,
    },
    Scenario {
        name: "wire-serving-contract",
        run: wire_serving_contract::run,
    },
    Scenario {
        name: "membership-wireguard-contract",
        run: membership_wireguard_contract::run,
    },
    Scenario {
        name: "scale",
        run: scale::run,
    },
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
            let process_cleanup = process_role_serving_contract::cleanup_orphaned_children();
            let wire_cleanup = wire_serving_contract::cleanup_orphaned_children();
            let mesh_cleanup = membership_wireguard_contract::cleanup_orphaned_children();
            match (process_cleanup, wire_cleanup, mesh_cleanup) {
                (Ok(()), Ok(()), Ok(())) => Err(format!("all scenario exceeded {budget:?} budget")),
                (process_result, wire_result, mesh_result) => Err(format!(
                    "all scenario exceeded {budget:?} budget; process-role cleanup={process_result:?}; wire cleanup={wire_result:?}; mesh cleanup={mesh_result:?}"
                )),
            }
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
        assert!(names.contains(&"steady-state-serving-contract"));
        assert!(names.contains(&"process-role-serving-contract"));
        assert!(names.contains(&"wire-serving-contract"));
        assert!(scenario_help().contains("lease-acme-contract"));
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

        static BLOCKING: &[Scenario] = &[Scenario {
            name: "blocking",
            run: blocking_scenario,
        }];

        let error = run_scenarios_with_budget(BLOCKING, Duration::from_millis(10))
            .expect_err("budget should expire before blocking scenario returns");

        assert!(error.contains("exceeded"));
    }
}
