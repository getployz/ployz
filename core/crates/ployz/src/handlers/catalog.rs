//! `ployz schema` and `ployz explain`: the command tree and the settings catalog,
//! which need no Store, and completion of Setting paths.

use clap::{Arg, ArgAction, ArgMatches, Command};
use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};
use ployz_store::EnvironmentQuery;
use ployz_store::catalog::{self, Explained};
use serde::Serialize;
use serde_json::Value;

use super::{Error, Handler, leaf_matches};
use crate::cli::positional;
use crate::ui::{self, Fields};

/// Where agents find the catalog, stated as a fact at the end of `ployz --help`.
pub(crate) const SIGNPOST: &str = "Settings catalog: `ployz schema --json` lists every command and Setting; `ployz explain SERVICE.SETTING` describes one Setting.";

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
    crate::ui::show(&schema)?;
    Ok(())
}

/// Whether a call can destroy something live, and so whether it needs approval.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Approval {
    /// A read, or an edit that only stages Working State.
    Never,
    /// Destroys a live thing at once.
    Always,
    /// Destroys whatever the plan it approves destroys.
    Depends,
}

/// Where a command can run: against Cloud, only on this device, or only inside Ployz.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Surface {
    Cloud,
    Local,
    Internal,
}

/// A command's handler with its classification. Building one names both, so a new
/// command cannot be added unclassified.
#[derive(Clone, Copy)]
pub(crate) struct Runnable {
    pub run: Handler,
    pub approval: Approval,
    pub surface: Surface,
}

pub(crate) const fn cloud(approval: Approval, run: Handler) -> Runnable {
    Runnable {
        run,
        approval,
        surface: Surface::Cloud,
    }
}

pub(crate) const fn local(approval: Approval, run: Handler) -> Runnable {
    Runnable {
        run,
        approval,
        surface: Surface::Local,
    }
}

pub(crate) const fn internal(approval: Approval, run: Handler) -> Runnable {
    Runnable {
        run,
        approval,
        surface: Surface::Internal,
    }
}

/// One runnable command, read from the command tree itself.
#[derive(Serialize)]
pub(crate) struct CommandEntry {
    pub command: String,
    pub about: String,
    /// Whether it prints a `--json` result.
    pub json: bool,
    pub approval: Approval,
    pub surface: Surface,
    pub args: Vec<ArgEntry>,
}

/// One argument: `--flag` or a positional `NAME`.
#[derive(Serialize)]
pub(crate) struct ArgEntry {
    pub name: String,
    pub required: bool,
    /// Whether it takes a value; a flag without one is a switch.
    pub value: bool,
    #[serde(rename = "type")]
    pub kind: ArgType,
    pub multiple: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short: Option<char>,
    /// A positional's place, from 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    /// Takes every remaining word, `--flags` included.
    pub trailing: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    /// Arguments it cannot be given with.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ArgType {
    Boolean,
    Integer,
    String,
}

/// Every visible command with a handler, in path order, with its own arguments; the
/// global connection flags every command shares are left out.
pub(crate) fn commands() -> Vec<CommandEntry> {
    fn walk(command: &Command, parent: &str, out: &mut Vec<CommandEntry>) {
        for child in command
            .get_subcommands()
            .filter(|child| !child.is_hide_set() && child.get_name() != "help")
        {
            let path = format!("{parent}{}", child.get_name());
            if let Some(runnable) = super::handler_for(&path) {
                out.push(CommandEntry {
                    json: !super::json_refused(&path),
                    about: child
                        .get_about()
                        .map(ToString::to_string)
                        .unwrap_or_default(),
                    approval: runnable.approval,
                    surface: runnable.surface,
                    args: args(child),
                    command: path.clone(),
                });
            }
            walk(child, &format!("{path} "), out);
        }
    }
    let mut root = crate::cli::command();
    root.build();
    let mut out = Vec::new();
    walk(&root, "", &mut out);
    out.sort_by(|a, b| a.command.cmp(&b.command));
    out
}

/// The catalog the SDK ships, as `ployz-sdk/generated/commands.json` holds it.
#[must_use]
pub fn commands_json() -> String {
    let json = serde_json::to_string_pretty(&commands()).expect("the catalog serializes");
    format!("{json}\n")
}

fn args(command: &Command) -> Vec<ArgEntry> {
    let visible = |arg: &&Arg| {
        !arg.is_global_set()
            && !arg.is_hide_set()
            && !matches!(
                arg.get_action(),
                ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong | ArgAction::Version
            )
    };
    let name = |arg: &Arg| {
        arg.get_long().map_or_else(
            || arg.get_id().as_str().to_uppercase(),
            |long| format!("--{long}"),
        )
    };
    let mut entries: Vec<ArgEntry> = command
        .get_arguments()
        .filter(visible)
        .map(|arg| {
            let value = arg.get_action().takes_values();
            ArgEntry {
                name: name(arg),
                required: arg.is_required_set(),
                value,
                kind: arg_type(arg),
                multiple: value
                    && (matches!(arg.get_action(), ArgAction::Append)
                        || arg.get_num_args().is_some_and(|n| n.max_values() > 1)),
                default: value
                    .then(|| arg.get_default_values().first())
                    .flatten()
                    .map(|default| default.to_string_lossy().into_owned()),
                short: arg.get_short(),
                index: arg.get_index(),
                trailing: arg.is_trailing_var_arg_set(),
                help: arg.get_help().map(ToString::to_string),
                values: arg
                    .get_possible_values()
                    .iter()
                    .filter(|value| !value.is_hide_set())
                    .map(|value| value.get_name().to_owned())
                    .collect(),
                conflicts: command
                    .get_arg_conflicts_with(arg)
                    .into_iter()
                    .filter(visible)
                    .map(name)
                    .collect(),
            }
        })
        .collect();
    // clap records a conflict on one side; either side refuses the other.
    let pairs: Vec<(String, String)> = entries
        .iter()
        .flat_map(|entry| {
            entry
                .conflicts
                .iter()
                .map(|other| (other.clone(), entry.name.clone()))
        })
        .collect();
    for (of, with) in pairs {
        if let Some(entry) = entries.iter_mut().find(|entry| entry.name == of)
            && !entry.conflicts.contains(&with)
        {
            entry.conflicts.push(with);
        }
    }
    entries
}

fn arg_type(arg: &Arg) -> ArgType {
    use std::any::TypeId;
    if !arg.get_action().takes_values() {
        return if matches!(arg.get_action(), ArgAction::Count) {
            ArgType::Integer
        } else {
            ArgType::Boolean
        };
    }
    let parser = arg.get_value_parser().type_id();
    let integers = [
        TypeId::of::<u8>(),
        TypeId::of::<u16>(),
        TypeId::of::<u32>(),
        TypeId::of::<u64>(),
        TypeId::of::<usize>(),
        TypeId::of::<i32>(),
        TypeId::of::<i64>(),
    ];
    if parser == TypeId::of::<bool>() {
        ArgType::Boolean
    } else if integers.iter().any(|id| parser == *id) {
        ArgType::Integer
    } else {
        ArgType::String
    }
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
    let mut record = Fields::new()
        .field("setting", &explanation.explained.path)
        .field("title", text("title"))
        .field("description", text("description"))
        .field("type", type_of(schema));
    if let Some(values) = schema.get("enum") {
        record.push("allowed", values);
    }
    // The bounds the Store enforces, so a refused value needs no second read.
    let low = schema
        .get("minimum")
        .map(|low| format!("at least {low}"))
        .or_else(|| {
            schema
                .get("exclusiveMinimum")
                .map(|low| format!("above {low}"))
        });
    let high = schema.get("maximum").map(|high| format!("at most {high}"));
    match (low, high) {
        (Some(low), Some(high)) => record.push("range", format_args!("{low}, {high}")),
        (Some(bound), None) | (None, Some(bound)) => record.push("range", bound),
        (None, None) => {}
    }
    if let Some(default) = schema.get("default") {
        record.push("default", default);
    }
    record.push("applies", text("x-ployz-apply"));
    record.push("example", &explanation.example);
    ui::fields(&explanation, &record)
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
        let store = super::store::backend_at(&config).ok()??;
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
    fn a_command_that_asks_for_confirmation_is_not_never() {
        let confirming = commands()
            .into_iter()
            .filter(|entry| {
                entry.args.iter().any(|arg| {
                    matches!(
                        arg.name.as_str(),
                        "--confirm" | "--yes" | "--accept-volume-loss"
                    )
                })
            })
            .filter(|entry| entry.approval == Approval::Never)
            .map(|entry| entry.command)
            .collect::<Vec<_>>();
        assert!(
            confirming.is_empty(),
            "these ask for confirmation but are classified Never: {confirming:?}"
        );
    }

    #[test]
    fn a_variable_reads_as_its_alternatives() {
        let variable = catalog::explain("web.env.KEY").unwrap().schema;
        assert_eq!(type_of(&variable), "string or object");
        let replicas = catalog::explain("web.replicas").unwrap().schema;
        assert_eq!(type_of(&replicas), "integer");
    }
}
