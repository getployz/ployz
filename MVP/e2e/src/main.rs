mod actor_contract;
mod assertions;
mod authority_contract;
mod bridge_contract;
mod bus_contract;
mod bus_syntax;
mod metrics;
mod projection_contract;
mod scale;

use std::env;
use std::process;
use std::sync::mpsc;
use std::time::Duration;

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
        "actor-contract" => actor_contract::run(),
        "authority-contract" => authority_contract::run(),
        "bridge-contract" => bridge_contract::run(),
        "bus-contract" => bus_contract::run(),
        "projection-contract" => projection_contract::run(),
        "all" => run_all_with_budget(),
        "scale" => scale::run(),
        "help" | "--help" | "-h" => {
            println!(
                "usage: cargo run -p mvp-e2e -- <bus-contract|actor-contract|authority-contract|bridge-contract|projection-contract|all|scale>"
            );
            Ok(())
        }
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
    bus_contract::run()?;
    actor_contract::run()?;
    authority_contract::run()?;
    bridge_contract::run()?;
    projection_contract::run()?;
    scale::run()
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
