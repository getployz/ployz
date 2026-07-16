use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use ployz::commands::PloyzctlCliError;
use ployz::deploy::compose::{ComposeInput, UnsupportedFieldMode, parse_deploy_file};
use ployz_core::deploy::DeployRequest;

const FIXTURE_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/compose");

#[test]
fn compose_fixtures_match_goldens() {
    let update = std::env::var("UPDATE_FIXTURES").is_ok_and(|value| value == "1");
    let mut failures = Vec::new();

    for case_dir in case_dirs() {
        let case_name = case_name(&case_dir);
        match run_case(&case_dir) {
            Ok(actual) if update => {
                if let Err(error) = update_goldens(&case_dir, &actual) {
                    failures.push(format!("{case_name}: {error}"));
                }
            }
            Ok(actual) => {
                failures.extend(
                    compare_goldens(&case_dir, &actual)
                        .into_iter()
                        .map(|failure| format!("{case_name}: {failure}")),
                );
            }
            Err(error) => failures.push(format!("{case_name}: {error}")),
        }
    }

    if !failures.is_empty() {
        panic!("compose fixture failures:\n{}", failures.join("\n"));
    }
}

#[test]
fn compose_fixture_directories_have_required_files() {
    let mut failures = Vec::new();

    for case_dir in case_dirs() {
        let case_name = case_name(&case_dir);
        if !case_dir.join("compose.yaml").is_file() {
            failures.push(format!("{case_name}: missing compose.yaml"));
        }
        if !case_dir.join("expected.request.json").is_file()
            && !case_dir.join("expected.diagnostics").is_file()
        {
            failures.push(format!("{case_name}: missing expected.* file"));
        }
    }

    if !failures.is_empty() {
        panic!(
            "compose fixture structure failures:\n{}",
            failures.join("\n")
        );
    }
}

struct ActualOutput {
    request_json: Option<String>,
    diagnostics: Option<String>,
}

fn run_case(case_dir: &Path) -> Result<ActualOutput, String> {
    let tempdir = tempfile::tempdir().map_err(|error| format!("create tempdir: {error}"))?;
    let compose_path = tempdir.path().join("compose.yaml");
    fs::copy(case_dir.join("compose.yaml"), &compose_path)
        .map_err(|error| format!("copy compose.yaml: {error}"))?;
    copy_case_files(case_dir, tempdir.path())?;

    let mut interpolation_env = BTreeMap::new();
    let dotenv = case_dir.join("dotenv");
    if dotenv.is_file() {
        interpolation_env.extend(parse_env_assignments(&dotenv)?);
        fs::copy(&dotenv, tempdir.path().join(".env"))
            .map_err(|error| format!("copy dotenv: {error}"))?;
    }
    let case_env = case_dir.join("case.env");
    if case_env.is_file() {
        interpolation_env.extend(parse_env_assignments(&case_env)?);
    }

    let source =
        fs::read_to_string(&compose_path).map_err(|error| format!("read compose.yaml: {error}"))?;
    let mode = if case_dir.join("allow_unsupported").is_file() {
        UnsupportedFieldMode::AllowUnsupported
    } else {
        UnsupportedFieldMode::Strict
    };
    let namespace_override = read_namespace_override(case_dir)?;

    match parse_deploy_file(ComposeInput {
        source: &source,
        base_dir: tempdir.path(),
        interpolation_env,
        namespace_override,
        mode,
    }) {
        Ok((parsed, warnings)) => {
            let request = DeployRequest {
                namespace_id: parsed.namespace_id,
                origin: None,
                volumes: parsed.volumes,
                services: parsed.services,
            };
            let request_json = serde_json::to_string_pretty(&request)
                .map(|json| format!("{json}\n"))
                .map_err(|error| format!("serialize request: {error}"))?;
            let diagnostics = render_diagnostics(warnings.into_iter().map(|warning| warning.0));
            Ok(ActualOutput {
                request_json: Some(request_json),
                diagnostics,
            })
        }
        Err(PloyzctlCliError::ComposeRejected { rendered }) => Ok(ActualOutput {
            request_json: None,
            diagnostics: Some(format!("{rendered}\n")),
        }),
        Err(error) => Ok(ActualOutput {
            request_json: None,
            diagnostics: Some(format!("{error}\n")),
        }),
    }
}

fn read_namespace_override(
    case_dir: &Path,
) -> Result<Option<ployz_core::ids::NamespaceId>, String> {
    let path = case_dir.join("namespace");
    match fs::read_to_string(&path) {
        Ok(value) => ployz_core::ids::NamespaceId::try_new(value.trim().to_owned())
            .map(Some)
            .map_err(|error| format!("read namespace: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("read namespace: {error}")),
    }
}

fn copy_case_files(case_dir: &Path, run_dir: &Path) -> Result<(), String> {
    for entry in fs::read_dir(case_dir).map_err(|error| format!("read case dir: {error}"))? {
        let entry = entry.map_err(|error| format!("read case entry: {error}"))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        match name.as_ref() {
            "compose.yaml"
            | "case.env"
            | "dotenv"
            | "allow_unsupported"
            | "namespace"
            | "expected.request.json"
            | "expected.diagnostics" => {}
            _ => {
                fs::copy(&path, run_dir.join(name.as_ref()))
                    .map_err(|error| format!("copy {name}: {error}"))?;
            }
        }
    }
    Ok(())
}

fn parse_env_assignments(path: &Path) -> Result<BTreeMap<String, String>, String> {
    let contents =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let mut values = BTreeMap::new();
    for (line_index, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            return Err(format!(
                "{}:{}: expected NAME=VALUE",
                path.display(),
                line_index + 1
            ));
        };
        values.insert(name.to_owned(), value.to_owned());
    }
    Ok(values)
}

fn render_diagnostics(lines: impl Iterator<Item = String>) -> Option<String> {
    let rendered = lines.collect::<Vec<_>>();
    if rendered.is_empty() {
        None
    } else {
        Some(format!("{}\n", rendered.join("\n")))
    }
}

fn compare_goldens(case_dir: &Path, actual: &ActualOutput) -> Vec<String> {
    let mut failures = Vec::new();
    compare_one(
        case_dir,
        "expected.request.json",
        actual.request_json.as_deref(),
        &mut failures,
    );
    compare_one(
        case_dir,
        "expected.diagnostics",
        actual.diagnostics.as_deref(),
        &mut failures,
    );
    failures
}

fn compare_one(case_dir: &Path, file_name: &str, actual: Option<&str>, failures: &mut Vec<String>) {
    let path = case_dir.join(file_name);
    match (fs::read_to_string(&path), actual) {
        (Ok(expected), Some(actual)) if expected != actual => {
            failures.push(format!(
                "{file_name} mismatch\nexpected:\n{expected}\nactual:\n{actual}"
            ));
        }
        (Ok(_expected), None) => failures.push(format!("{file_name} exists but was not produced")),
        (Err(error), Some(actual)) if error.kind() == std::io::ErrorKind::NotFound => {
            failures.push(format!("{file_name} missing; actual:\n{actual}"));
        }
        (Err(error), None) if error.kind() == std::io::ErrorKind::NotFound => {}
        (Err(error), _) => failures.push(format!("read {file_name}: {error}")),
        (Ok(_expected), Some(_actual)) => {}
    }
}

fn update_goldens(case_dir: &Path, actual: &ActualOutput) -> Result<(), String> {
    update_one(
        case_dir,
        "expected.request.json",
        actual.request_json.as_deref(),
    )?;
    update_one(
        case_dir,
        "expected.diagnostics",
        actual.diagnostics.as_deref(),
    )
}

fn update_one(case_dir: &Path, file_name: &str, actual: Option<&str>) -> Result<(), String> {
    let path = case_dir.join(file_name);
    match actual {
        Some(actual) => {
            fs::write(&path, actual).map_err(|error| format!("write {}: {error}", path.display()))
        }
        None if path.exists() => {
            fs::remove_file(&path).map_err(|error| format!("remove {}: {error}", path.display()))
        }
        None => Ok(()),
    }
}

fn case_dirs() -> Vec<PathBuf> {
    let mut paths = fs::read_dir(FIXTURE_ROOT)
        .unwrap_or_else(|error| panic!("read fixture root {FIXTURE_ROOT}: {error}"))
        .map(|entry| {
            entry
                .unwrap_or_else(|error| panic!("read fixture root entry: {error}"))
                .path()
        })
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn case_name(case_dir: &Path) -> String {
    case_dir
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| case_dir.display().to_string())
}
