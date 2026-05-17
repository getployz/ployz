mod actor_contract;
mod assertions;
mod authority_contract;
mod bridge_contract;
mod bus_contract;
mod bus_syntax;
mod iroh_docs_contract;
mod metrics;
mod projection_contract;
mod scale;

use std::env;
use std::process;
use std::sync::mpsc;
use std::time::Duration;

const ALL_SCENARIOS: &[&str] = &[
    "bus-contract",
    "actor-contract",
    "authority-contract",
    "bridge-contract",
    "projection-contract",
    "iroh-docs-contract",
    "scale",
];

fn main() {
    if let Err(error) = run() {
        eprintln!("FAIL {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let scenario = env::args()
        .nth(1)
        .unwrap_or_else(|| String::from("bus-contract"));
    match scenario.as_str() {
        "actor-contract"
        | "authority-contract"
        | "bridge-contract"
        | "bus-contract"
        | "iroh-docs-contract"
        | "projection-contract"
        | "scale" => run_named_scenario(scenario.as_str()),
        "all" => run_all_with_budget(),
        "help" | "--help" | "-h" => {
            println!(
                "usage: cargo run -p mvp-e2e -- <bus-contract|actor-contract|authority-contract|bridge-contract|projection-contract|iroh-docs-contract|all|scale>"
            );
            Ok(())
        }
        other => Err(format!("unknown MVP E2E scenario '{other}'")),
    }
}

fn run_named_scenario(scenario: &str) -> Result<(), String> {
    match scenario {
        "actor-contract" => actor_contract::run(),
        "authority-contract" => authority_contract::run(),
        "bridge-contract" => bridge_contract::run(),
        "bus-contract" => bus_contract::run(),
        "iroh-docs-contract" => iroh_docs_contract::run(),
        "projection-contract" => projection_contract::run(),
        "scale" => scale::run(),
        other => Err(format!("unknown MVP E2E scenario '{other}'")),
    }
}

fn run_all_with_budget() -> Result<(), String> {
    let budget = e2e_all_budget()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = run_all();
        let _ = tx.send(result);
    });
    match rx.recv_timeout(budget) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            Err(format!("all scenario exceeded {budget:?} budget"))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err("all scenario worker exited before reporting a result".to_string())
        }
    }
}

fn run_all() -> Result<(), String> {
    for scenario in ALL_SCENARIOS {
        run_named_scenario(scenario)?;
    }
    Ok(())
}

fn e2e_all_budget() -> Result<Duration, String> {
    let value = env::var("MVP_E2E_ALL_TIMEOUT").unwrap_or_else(|_| "120s".to_string());
    parse_duration(&value).ok_or_else(|| {
        format!("MVP_E2E_ALL_TIMEOUT must be a positive duration like 120s, got '{value}'")
    })
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
    use super::{ALL_SCENARIOS, parse_duration};
    use std::time::Duration;

    #[test]
    fn all_scenarios_include_iroh_docs_contract() {
        assert!(ALL_SCENARIOS.contains(&"iroh-docs-contract"));
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
}
