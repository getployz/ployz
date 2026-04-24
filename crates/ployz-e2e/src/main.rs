mod cli;
mod error;
mod runner;
mod scenarios;
mod support;

use clap::Parser;
use cli::Cli;
use error::{Error, Result};
use runner::ScenarioRun;
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
    let scenarios = if cli.scenario.is_empty() {
        cli::Scenario::default_order()
    } else {
        cli.scenario
    };
    let artifacts_dir = resolve_artifacts_dir(&cli.artifacts_dir)?;

    let failures = if cli.parallel {
        run_parallel_scenarios(scenarios, &cli.image, &artifacts_dir, cli.keep_failed)
    } else {
        run_sequential_scenarios(
            scenarios,
            &cli.image,
            &artifacts_dir,
            cli.keep_failed,
            cli.fail_fast,
        )?
    };

    if failures.is_empty() {
        return Ok(());
    }

    let mut message = String::new();
    for (scenario, error) in failures {
        let _ = writeln!(&mut message, "{}: {error}", scenario.as_str());
    }
    Err(Error::Message(message.trim_end().to_string()))
}

fn run_sequential_scenarios(
    scenarios: Vec<cli::Scenario>,
    image: &str,
    artifacts_dir: &Path,
    keep_failed: bool,
    fail_fast: bool,
) -> Result<Vec<(cli::Scenario, Error)>> {
    let mut failures = Vec::new();
    for scenario in scenarios {
        let outcome = run_scenario(scenario, image, artifacts_dir, keep_failed);
        if let Err((failed_scenario, error)) = outcome {
            if fail_fast {
                return Err(Error::Message(format!(
                    "{}: {error}",
                    failed_scenario.as_str()
                )));
            }
            failures.push((failed_scenario, error));
        }
    }
    Ok(failures)
}

fn run_parallel_scenarios(
    scenarios: Vec<cli::Scenario>,
    image: &str,
    artifacts_dir: &Path,
    keep_failed: bool,
) -> Vec<(cli::Scenario, Error)> {
    let mut handles = Vec::with_capacity(scenarios.len());
    for scenario in scenarios {
        let image = image.to_string();
        let artifacts_dir = artifacts_dir.to_path_buf();
        handles.push((
            scenario,
            thread::spawn(move || run_scenario(scenario, &image, &artifacts_dir, keep_failed)),
        ));
    }

    let mut failures = Vec::new();
    for (scenario, handle) in handles {
        match handle.join() {
            Ok(Ok(())) => {}
            Ok(Err((scenario, error))) => failures.push((scenario, error)),
            Err(_) => failures.push((
                scenario,
                Error::Message("parallel scenario thread panicked".into()),
            )),
        }
    }
    failures
}

fn run_scenario(
    scenario: cli::Scenario,
    image: &str,
    artifacts_dir: &Path,
    keep_failed: bool,
) -> std::result::Result<(), (cli::Scenario, Error)> {
    let mut run = ScenarioRun::new(scenario, image, artifacts_dir, keep_failed)
        .map_err(|error| (scenario, error))?;
    match run.execute() {
        Ok(()) => {
            println!("PASS {}", scenario.as_str());
            run.cleanup(false);
            Ok(())
        }
        Err(error) => {
            eprintln!("FAIL {}: {error}", scenario.as_str());
            let _ = run.collect_failure_artifacts();
            run.cleanup(true);
            Err((scenario, error))
        }
    }
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
}
