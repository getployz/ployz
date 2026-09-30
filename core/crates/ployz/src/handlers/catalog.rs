//! `ployz schema` and `ployz explain`: the command tree and the settings catalog,
//! which need no Store, and completion of Setting paths.

use clap::{ArgMatches, Command};
use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};
use ployz_store::EnvironmentQuery;
use ployz_store::catalog::{self, Explained};
use serde::Serialize;
use serde_json::Value;

use super::{Error, leaf_matches};
use crate::cli::positional;
use crate::output::say;

pub(crate) fn schema_command() -> Command {
    Command::new("schema")
        .about("Print the settings catalog as JSON Schema, with every command at the root: all of it, one Service, or one Setting")
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
    let mut schema = catalog::schema(path.map(String::as_str)).map_err(|mut error| {
        let service = path.and_then(|path| path.split('.').next()).unwrap_or("SERVICE");
        error.message = format!(
            "{}. `ployz schema` takes SERVICE or SERVICE.SETTING; `ployz schema {service}` lists its Settings",
            error.message.trim_end_matches('.')
        );
        error
    })?;
    if let (None, Some(object)) = (path, schema.as_object_mut()) {
        object.insert("x-ployz-commands".into(), serde_json::to_value(commands())?);
    }
    crate::output::show(&schema)?;
    Ok(())
}

/// One runnable command, read from the command tree itself.
#[derive(Serialize)]
pub(crate) struct CommandEntry {
    pub command: String,
    pub about: String,
    /// Whether it prints a `--json` result.
    pub json: bool,
    pub args: Vec<ArgEntry>,
}

/// One argument: `--flag` or a positional `NAME`.
#[derive(Serialize)]
pub(crate) struct ArgEntry {
    name: String,
    required: bool,
    /// Whether it takes a value; a flag without one is a switch.
    value: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    help: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    values: Vec<String>,
}

/// Every visible leaf command in path order, with its own arguments; the global
/// connection flags every command shares are left out.
pub(crate) fn commands() -> Vec<CommandEntry> {
    fn walk(command: &Command, parent: &str, out: &mut Vec<CommandEntry>) {
        for child in command
            .get_subcommands()
            .filter(|child| !child.is_hide_set())
        {
            let path = format!("{parent}{}", child.get_name());
            if child.has_subcommands() {
                walk(child, &format!("{path} "), out);
                continue;
            }
            let args = child
                .get_arguments()
                .filter(|arg| !arg.is_global_set() && !arg.is_hide_set())
                .map(|arg| ArgEntry {
                    name: arg.get_long().map_or_else(
                        || arg.get_id().as_str().to_uppercase(),
                        |long| format!("--{long}"),
                    ),
                    required: arg.is_required_set(),
                    value: arg.get_action().takes_values(),
                    help: arg.get_help().map(ToString::to_string),
                    values: arg
                        .get_possible_values()
                        .iter()
                        .filter(|value| !value.is_hide_set())
                        .map(|value| value.get_name().to_owned())
                        .collect(),
                })
                .collect();
            out.push(CommandEntry {
                json: super::handler_for(&path).is_some() && !super::json_refused(&path),
                about: child
                    .get_about()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                command: path,
                args,
            });
        }
    }
    let mut out = Vec::new();
    walk(&crate::cli::command(), "", &mut out);
    out.sort_by(|a, b| a.command.cmp(&b.command));
    out
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
        say!("Type: {}", type_of(schema));
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

/// A schema's type, or its alternatives' (`string or object`) when it has several.
fn type_of(schema: &Value) -> String {
    if let Some(kind) = schema.get("type").and_then(Value::as_str) {
        return kind.to_owned();
    }
    let mut kinds: Vec<&str> = schema
        .get("oneOf")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|alternative| alternative.get("type")?.as_str())
        .collect();
    kinds.dedup();
    kinds.join(" or ")
}

/// Completes `SERVICE.SETTING`: Service names from the Store when it is reachable,
/// then the catalog's Settings once a Service is typed, and the Environment's own
/// variables (`SERVICE.env.KEY`) and mounts (`SERVICE.mounts.VOLUME`).
pub(crate) fn setting_paths() -> ArgValueCompleter {
    ArgValueCompleter::new(|current: &std::ffi::OsStr| {
        let current = current.to_string_lossy();
        let stored = stored_paths();
        let paths = if current.contains('.') {
            let mut paths = catalog::complete(&current);
            paths.extend(
                stored
                    .into_iter()
                    .filter(|path| path.starts_with(current.as_ref())),
            );
            paths.sort();
            paths.dedup();
            paths
        } else {
            let mut services: Vec<String> = stored
                .iter()
                .filter_map(|path| path.split('.').next())
                .filter(|service| service.starts_with(current.as_ref()))
                .map(|service| format!("{service}."))
                .collect();
            services.dedup();
            services
        };
        paths.into_iter().map(CompletionCandidate::new).collect()
    })
}

/// Every Setting path of the scoped Environment, or none when the Store is out of
/// reach. Completion sees no flags, so scope comes from `PLOYZ_PROJECT`, `PLOYZ_ENV`
/// and the directory link.
fn stored_paths() -> Vec<String> {
    let scope = || -> Option<Vec<String>> {
        let config = std::env::var(crate::cli::env::CONFIG)
            .unwrap_or_else(|_| "~/.config/ployz/config.yaml".to_owned());
        let config = crate::context::expand_home(std::path::Path::new(&config));
        let store = super::store::store_at(&config).ok()?;
        let environment = super::link::scope_from_env(&config).ok()?.at();
        let query = EnvironmentQuery {
            environment,
            path: None,
            all: true,
        };
        let view = store.read(&query).ok()?;
        Some(
            view.settings
                .into_iter()
                .map(|row| row.path.to_string())
                .collect(),
        )
    };
    scope().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_variable_reads_as_its_alternatives() {
        let variable = catalog::explain("web.env.KEY").unwrap().schema;
        assert_eq!(type_of(&variable), "string or object");
        let replicas = catalog::explain("web.replicas").unwrap().schema;
        assert_eq!(type_of(&replicas), "integer");
    }
}
