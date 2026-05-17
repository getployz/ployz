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
        "all" => {
            bus_contract::run()?;
            actor_contract::run()?;
            authority_contract::run()?;
            bridge_contract::run()?;
            projection_contract::run()?;
            scale::run()
        }
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
