//! `ployz schema` and `ployz explain`: the settings catalog, which needs no Store,
//! and completion of Setting paths.

use clap::{ArgMatches, Command};
use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};
use ployz_store::catalog::{self, Explained};
use ployz_store::{EnvironmentName, EnvironmentQuery, EnvironmentRef, ProjectName};
use serde::Serialize;
use serde_json::Value;

use super::{Error, leaf_matches};
use crate::cli::{env, positional};
use crate::output::say;

pub(crate) fn schema_command() -> Command {
    Command::new("schema")
        .about("Print the settings catalog as JSON Schema: all of it, one Service, or one Setting")
        .arg(
            positional("path", false)
                .help("SERVICE or SERVICE.SETTING")
                .add(setting_paths()),
        )
}

pub(crate) fn explain_command() -> Command {
    Command::new("explain")
        .about("Describe one Setting, with an example")
        .arg(
            positional("path", true)
                .help("SERVICE.SETTING, for example web.replicas")
                .add(setting_paths()),
        )
}

pub(super) fn schema(root: &ArgMatches) -> Result<(), Error> {
    let path = leaf_matches(root).get_one::<String>("path");
    crate::output::show(&catalog::schema(path.map(String::as_str))?)?;
    Ok(())
}

/// One Setting with a command that sets it.
#[derive(Serialize)]
struct Explanation {
    #[serde(flatten)]
    explained: Explained,
    example: String,
}

pub(super) fn explain(root: &ArgMatches) -> Result<(), Error> {
    let path = leaf_matches(root)
        .get_one::<String>("path")
        .expect("path is required");
    let explained = catalog::explain(path)?;
    let example = match explained.schema.pointer("/examples/0") {
        Some(Value::String(text)) => text.clone(),
        Some(value) => value.to_string(),
        None => String::new(),
    };
    let example = shell_words::join(["ployz", "set", &format!("{}={example}", explained.path)]);
    let explanation = Explanation { explained, example };
    let schema = &explanation.explained.schema;
    let text = |key: &str| schema.get(key).and_then(Value::as_str).unwrap_or_default();
    crate::output::finish(&explanation, || {
        say!("{} — {}", explanation.explained.path, text("title"));
        say!("{}", text("description"));
        say!("Type: {}", text("type"));
        if let Some(values) = schema.get("enum") {
            say!("Allowed: {values}");
        }
        if let Some(default) = schema.get("default") {
            say!("Default: {default}");
        }
        say!("Applies: {}", text("x-ployz-apply"));
        say!("Example: {}", explanation.example);
    })
}

/// Completes `SERVICE.SETTING`: Service names from the Store when it is reachable,
/// then the catalog's Settings once a Service is typed.
pub(crate) fn setting_paths() -> ArgValueCompleter {
    ArgValueCompleter::new(|current: &std::ffi::OsStr| {
        let current = current.to_string_lossy();
        let paths = if current.contains('.') {
            catalog::complete(&current)
        } else {
            services()
                .into_iter()
                .filter(|service| service.starts_with(current.as_ref()))
                .map(|service| format!("{service}."))
                .collect()
        };
        paths.into_iter().map(CompletionCandidate::new).collect()
    })
}

/// The Services of the scoped Environment, or none when the Store is out of reach.
/// Completion sees no flags, so scope comes from `PLOYZ_PROJECT` and `PLOYZ_ENV`.
fn services() -> Vec<String> {
    let scope = || -> Option<Vec<String>> {
        let config = std::env::var(crate::cli::env::CONFIG)
            .unwrap_or_else(|_| "~/.config/ployz/config.yaml".to_owned());
        let store =
            super::store::store_at(&crate::context::expand_home(std::path::Path::new(&config)))
                .ok()?;
        let parse = |name: &str| std::env::var(name).ok();
        let environment = EnvironmentRef {
            project: parse(env::PROJECT)
                .map(|name| ProjectName::parse(name.as_str()))
                .transpose()
                .ok()?,
            environment: parse(env::ENVIRONMENT)
                .map(|name| EnvironmentName::parse(name.as_str()))
                .transpose()
                .ok()?,
        };
        let query = EnvironmentQuery {
            environment,
            path: None,
            all: true,
        };
        let view = store.environment(&query).ok()?;
        let mut services = view
            .settings
            .into_iter()
            .map(|row| row.path.service().to_string())
            .collect::<Vec<_>>();
        services.dedup();
        Some(services)
    };
    scope().unwrap_or_default()
}
