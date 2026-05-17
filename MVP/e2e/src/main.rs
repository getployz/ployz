mod bus_contract;

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
        "bus-contract" => bus_contract::run(),
        "all" => bus_contract::run(),
        "scale" => Err(String::from(
            "scale scenario is planned but not implemented in slice 001",
        )),
        "help" | "--help" | "-h" => {
            println!("usage: cargo run -p mvp-e2e -- <bus-contract|all|scale>");
            Ok(())
        }
        other => Err(format!("unknown MVP E2E scenario '{other}'")),
    }
}
