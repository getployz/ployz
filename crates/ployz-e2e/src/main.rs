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

    let mut failures = Vec::new();
    for scenario in scenarios {
        let mut run = ScenarioRun::new(scenario, &cli.image, &artifacts_dir, cli.keep_failed)?;
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
