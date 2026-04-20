mod cli;
mod error;
mod junit;
mod runner;
mod scenarios;
mod support;

use clap::Parser;
use cli::Cli;
use error::{Error, Result};
use junit::ScenarioOutcome;
use runner::{CleanupReason, ScenarioRun, SharedPayload, prepare_shared_payload};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::thread;
use std::time::Instant;

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    if cli.parallel && cli.fail_fast {
        return Err(Error::Message(
            "--parallel cannot be combined with --fail-fast".into(),
        ));
    }
    let scenarios = if cli.scenario.is_empty() {
        cli::Scenario::default_order()
    } else {
        cli.scenario.clone()
    };
    let artifacts_dir = resolve_artifacts_dir(&cli.artifacts_dir)?;
    let shared_payload = prepare_shared_payload(&cli.image, &artifacts_dir)?;

    let outcomes = if cli.parallel && scenarios.len() > 1 {
        run_parallel(&cli, &shared_payload, scenarios, &artifacts_dir)?
    } else {
        run_serial(&cli, &shared_payload, scenarios, &artifacts_dir)?
    };

    if let Some(path) = &cli.junit_path {
        junit::write(path, &outcomes)?;
    }

    let failures: Vec<(cli::Scenario, String)> = outcomes
        .iter()
        .filter_map(|o| o.failure.as_ref().map(|e| (o.scenario, e.clone())))
        .collect();
    summarize_failures(failures)
}

fn run_serial(
    cli: &Cli,
    shared_payload: &SharedPayload,
    scenarios: Vec<cli::Scenario>,
    artifacts_dir: &Path,
) -> Result<Vec<ScenarioOutcome>> {
    let mut outcomes = Vec::with_capacity(scenarios.len());
    for scenario in scenarios {
        let outcome = run_single_scenario(
            scenario,
            &cli.image,
            shared_payload,
            artifacts_dir,
            cli.keep_failed,
        );
        let failed = outcome.failure.is_some();
        outcomes.push(outcome);
        if failed && cli.fail_fast {
            break;
        }
    }
    Ok(outcomes)
}

fn run_parallel(
    cli: &Cli,
    shared_payload: &SharedPayload,
    scenarios: Vec<cli::Scenario>,
    artifacts_dir: &Path,
) -> Result<Vec<ScenarioOutcome>> {
    let mut handles = Vec::with_capacity(scenarios.len());
    for scenario in scenarios {
        let image = cli.image.clone();
        let artifacts_dir = artifacts_dir.to_path_buf();
        let shared_payload = shared_payload.clone();
        let keep_failed = cli.keep_failed;
        handles.push(thread::spawn(move || {
            run_single_scenario(
                scenario,
                &image,
                &shared_payload,
                &artifacts_dir,
                keep_failed,
            )
        }));
    }

    let mut outcomes = Vec::with_capacity(handles.len());
    for handle in handles {
        let outcome = handle
            .join()
            .map_err(|_| Error::Message("parallel e2e worker panicked".into()))?;
        outcomes.push(outcome);
    }

    Ok(outcomes)
}

fn run_single_scenario(
    scenario: cli::Scenario,
    image: &str,
    shared_payload: &SharedPayload,
    artifacts_dir: &Path,
    keep_failed: bool,
) -> ScenarioOutcome {
    let started = Instant::now();
    let failure = match ScenarioRun::new(scenario, image, shared_payload, artifacts_dir, keep_failed) {
        Ok(mut run) => match run.execute() {
            Ok(()) => {
                println!("PASS {}", scenario.as_str());
                run.cleanup(CleanupReason::Success);
                None
            }
            Err(error) => {
                eprintln!("FAIL {}: {error}", scenario.as_str());
                let _ = run.collect_failure_artifacts();
                run.cleanup(CleanupReason::Failure);
                Some(error.to_string())
            }
        },
        Err(error) => {
            eprintln!("FAIL {}: {error}", scenario.as_str());
            Some(error.to_string())
        }
    };
    ScenarioOutcome {
        scenario,
        duration: started.elapsed(),
        failure,
    }
}

fn summarize_failures(failures: Vec<(cli::Scenario, String)>) -> Result<()> {
    if failures.is_empty() {
        return Ok(());
    }
    let mut message = String::new();
    for (scenario, error) in failures {
        let _ = writeln!(&mut message, "{}: {error}", scenario.as_str());
    }
    Err(Error::Message(message.trim_end().to_string()))
}

fn resolve_artifacts_dir(path: &Path) -> Result<PathBuf> {
    let current_dir =
        env::current_dir().map_err(|error| Error::Io(format!("resolve current dir: {error}")))?;
    resolve_artifacts_dir_from(&current_dir, path)
}

fn resolve_artifacts_dir_from(current_dir: &Path, path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        current_dir.join(path)
    };

    fs::create_dir_all(&absolute).map_err(|error| {
        Error::Io(format!(
            "create artifacts dir '{}': {error}",
            absolute.display()
        ))
    })?;

    fs::canonicalize(&absolute).map_err(|error| {
        Error::Io(format!(
            "canonicalize artifacts dir '{}': {error}",
            absolute.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::resolve_artifacts_dir_from;
    use crate::cli::Cli;
    use clap::Parser;
    use std::ffi::OsStr;
    use std::fs;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn resolves_relative_artifacts_dir_to_absolute_path() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        let temp_root = std::env::temp_dir().join(format!("ployz-e2e-main-test-{unique}"));
        let current_dir = temp_root.join("workspace");
        fs::create_dir_all(&current_dir).expect("create workspace dir");

        let resolved = resolve_artifacts_dir_from(&current_dir, Path::new(".e2e-artifacts"))
            .expect("resolve relative artifacts dir");
        let canonical_current_dir =
            fs::canonicalize(&current_dir).expect("canonicalize workspace dir");

        assert!(resolved.is_absolute());
        assert!(resolved.starts_with(&canonical_current_dir));
        assert_eq!(resolved.file_name(), Some(OsStr::new(".e2e-artifacts")));

        let _ = fs::remove_dir_all(&temp_root);
    }

    #[test]
    fn parse_parallel_flag() {
        let cli = Cli::try_parse_from(["ployz-e2e", "--parallel"]).expect("parallel args parse");
        assert!(cli.parallel);
        assert!(!cli.fail_fast);
    }
}
