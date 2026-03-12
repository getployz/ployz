mod cli;
mod error;
mod runner;
mod scenarios;
mod support;

use clap::Parser;
use cli::Cli;
use error::{Error, Result};
use runner::ScenarioRun;
use std::fmt::Write as _;
use std::fs;
use std::process;

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let scenarios = if cli.scenario.is_empty() {
        cli::Scenario::default_order()
    } else {
        cli.scenario
    };

    fs::create_dir_all(&cli.artifacts_dir).map_err(|error| {
        Error::Io(format!(
            "create artifacts dir '{}': {error}",
            cli.artifacts_dir.display()
        ))
    })?;

    let mut failures = Vec::new();
    for scenario in scenarios {
        let mut run = ScenarioRun::new(scenario, &cli.image, &cli.artifacts_dir, cli.keep_failed)?;
        match run.execute() {
            Ok(()) => {
                println!("PASS {}", scenario.as_str());
                run.cleanup(false);
            }
            Err(error) => {
                eprintln!("FAIL {}: {error}", scenario.as_str());
                let _ = run.collect_failure_artifacts();
                run.cleanup(true);
                failures.push((scenario, error));
            }
        }
    }

    if failures.is_empty() {
        return Ok(());
    }

    let mut message = String::new();
    for (scenario, error) in failures {
        let _ = writeln!(&mut message, "{}: {error}", scenario.as_str());
    }
    Err(Error::Message(message.trim_end().to_string()))
}
