mod cli;
mod error;
mod runner;
mod scenarios;
mod support;

use clap::Parser;
use cli::Cli;
use error::{Error, Result};
use runner::{CleanupReason, ScenarioRun};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::thread;

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

    if cli.parallel && scenarios.len() > 1 {
        return run_parallel(&cli, scenarios, &artifacts_dir);
    }

    run_serial(&cli, scenarios, &artifacts_dir)
}

fn run_serial(cli: &Cli, scenarios: Vec<cli::Scenario>, artifacts_dir: &Path) -> Result<()> {
    let mut failures = Vec::new();
    for scenario in scenarios {
        match run_single_scenario(scenario, &cli.image, artifacts_dir, cli.keep_failed) {
            Ok(()) => {}
            Err(error) => {
                if cli.fail_fast {
                    return Err(Error::Message(format!("{}: {error}", scenario.as_str())));
                }
                failures.push((scenario, error));
            }
        }
    }

    summarize_failures(failures)
}

fn run_parallel(cli: &Cli, scenarios: Vec<cli::Scenario>, artifacts_dir: &Path) -> Result<()> {
    let mut handles = Vec::with_capacity(scenarios.len());
    for scenario in scenarios {
        let image = cli.image.clone();
        let artifacts_dir = artifacts_dir.to_path_buf();
        let keep_failed = cli.keep_failed;
        handles.push(thread::spawn(move || {
            let result = run_single_scenario(scenario, &image, &artifacts_dir, keep_failed);
            (scenario, result)
        }));
    }

    let mut failures = Vec::new();
    for handle in handles {
        let (scenario, result) = handle
            .join()
            .map_err(|_| Error::Message("parallel e2e worker panicked".into()))?;
        if let Err(error) = result {
            failures.push((scenario, error));
        }
    }

    summarize_failures(failures)
}

fn run_single_scenario(
    scenario: cli::Scenario,
    image: &str,
    artifacts_dir: &Path,
    keep_failed: bool,
) -> Result<()> {
    let mut run = ScenarioRun::new(scenario, image, artifacts_dir, keep_failed)?;
    match run.execute() {
        Ok(()) => {
            println!("PASS {}", scenario.as_str());
            run.cleanup(CleanupReason::Success);
            Ok(())
        }
        Err(error) => {
            eprintln!("FAIL {}: {error}", scenario.as_str());
            let _ = run.collect_failure_artifacts();
            run.cleanup(CleanupReason::Failure);
            Err(error)
        }
    }
}

fn summarize_failures(failures: Vec<(cli::Scenario, Error)>) -> Result<()> {
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
