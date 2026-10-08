use super::catalog::{Approval::*, Runnable, local};
use crate::ui::{self, Cell, Hint, Table, Tone};
use std::path::Path;

use clap::{ArgMatches, Command};

use crate::cli::{base, positional, value};
use serde::Serialize;
use serde_json::json;

use crate::context::{
    Config, ConnectionError, RemovedContext, expand_home, is_management_capability,
};

use super::{Error, leaf_matches, required};

fn config(matches: &ArgMatches) -> Result<Config, Error> {
    if matches
        .get_one::<String>("connect")
        .is_some_and(|value| !value.is_empty())
    {
        return Err(Error::usage(
            "context management is unavailable with a direct connection",
        ));
    }
    let path = matches
        .get_one::<String>("ployz-config")
        .map(Path::new)
        .map(expand_home)
        .ok_or_else(|| Error::usage("Ployz config path is required"))?;
    Ok(Config::load_or_empty(path)?)
}

#[derive(Serialize)]
struct ContextEntry<'a> {
    name: &'a str,
    current: bool,
    /// Connection labels; the first is the default.
    connections: Vec<String>,
}

pub(super) fn list(matches: &ArgMatches) -> Result<(), Error> {
    let config = config(matches)?;
    let contexts = config
        .contexts
        .iter()
        .map(|(name, context)| ContextEntry {
            name,
            current: Some(name.as_str()) == config.current_context(),
            connections: context
                .connections
                .iter()
                .map(ToString::to_string)
                .collect(),
        })
        .collect::<Vec<_>>();
    let mut table = Table::new(
        ["NAME", "CURRENT", "DEFAULT", "CONNECTIONS"],
        "No contexts yet.",
    );
    for context in &contexts {
        table.row([
            Cell::from(context.name.to_string()),
            if context.current {
                Cell::status("current", Tone::Good)
            } else {
                Cell::from("")
            },
            Cell::from(context.connections.first().map_or("", String::as_str)),
            Cell::from(context.connections.len().to_string()),
        ]);
    }
    ui::list(&json!({ "contexts": contexts }), &table)
}

/// `ployz ctx use CONTEXT`, keeping a config file named on the command line;
/// one from the environment is still set when the retry runs.
fn retry_use(leaf: &ArgMatches) -> String {
    let mut args = vec!["ployz", "ctx", "use"];
    if leaf.value_source("ployz-config") == Some(clap::parser::ValueSource::CommandLine)
        && let Some(path) = leaf.get_one::<String>("ployz-config")
    {
        args.extend(["--ployz-config", path]);
    }
    args.push("CONTEXT");
    shell_words::join(args)
}

pub(super) fn select(matches: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(matches);
    let mut config = config(matches)?;
    if config.contexts.is_empty() {
        return Err(Error::not_found(format!(
            "no contexts found in Ployz config {}",
            config.path().display()
        )));
    }
    let requested_connection = leaf.get_one::<String>("connection");
    let selected = match leaf.get_one::<String>("context-name") {
        Some(name) => name.clone(),
        // Choosing a connection alone keeps the current context.
        None if requested_connection.is_some() => {
            config.current_context().map(str::to_owned).ok_or_else(|| {
                Error::usage(format!(
                    "current context is not set in Ployz config {}",
                    config.path().display()
                ))
            })?
        }
        None => {
            let names = config.contexts.keys().collect::<Vec<_>>();
            let current = names
                .iter()
                .position(|name| Some(name.as_str()) == config.current_context());
            let choices = names
                .iter()
                .enumerate()
                .map(|(index, name)| {
                    if Some(index) == current {
                        format!("{name} (current)")
                    } else {
                        name.to_string()
                    }
                })
                .collect::<Vec<_>>();
            let index = ui::select(
                "Select a context",
                &choices,
                current.unwrap_or(0),
                || {
                    Error::usage("Choosing a context needs a terminal; name one instead.")
                        .hint(Hint::Retry(retry_use(leaf)))
                        .hint(Hint::valid(names.iter().map(|name| name.as_str())))
                },
                "Cancelled. The current context is unchanged.",
            )?;
            names
                .get(index)
                .expect("selection came from the context list")
                .to_string()
        }
    };
    config.set_current_context(Some(selected.clone()))?;
    let context = config
        .contexts
        .get_mut(&selected)
        .expect("set_current_context accepted a known context");
    if let Some(requested) = requested_connection {
        let index = connection_index(&selected, &context.connections, requested)?;
        context.select_connection(index);
    }
    let connection = context.connections.first().map(ToString::to_string);
    config.save()?;
    crate::ui::finish(
        &json!({ "context": selected, "connection": connection }),
        || {
            crate::ui::stream(format_args!(
                "Current context is now {}.",
                selected.escape_debug()
            ));
            if let Some(connection) = &connection
                && requested_connection.is_some()
            {
                crate::ui::stream(format_args!(
                    "Default connection is now {}.",
                    connection.escape_debug()
                ));
            }
        },
    )
}

pub(super) fn remove(matches: &ArgMatches) -> Result<(), Error> {
    let mut config = config(matches)?;
    let name = required(leaf_matches(matches), "context-name")?;
    let removed = config.remove_context(&name)?;
    config.save()?;
    let was_current = removed == RemovedContext::Current;
    crate::ui::finish(
        &json!({ "removed": name, "was_current": was_current }),
        || {
            crate::ui::stream(format_args!("Removed context {}.", name.escape_debug()));
            if was_current {
                ui::note("Current context is now unset.");
            }
        },
    )
}

/// Resolve a connection label or 1-based index within one context.
fn connection_index(
    context: &str,
    connections: &[crate::context::Connection],
    requested: &str,
) -> Result<usize, Error> {
    if connections.is_empty() {
        return Err(Error::not_found(format!(
            "no connections found in context {}",
            context.escape_debug()
        )));
    }
    if is_management_capability(requested) {
        return Err(Error::usage(
            ConnectionError::ManagementConfigOnly.to_string(),
        ));
    }
    if let Ok(index) = requested.parse::<usize>() {
        return index
            .checked_sub(1)
            .filter(|index| *index < connections.len())
            .ok_or_else(|| Error::usage("connection index is out of range"));
    }
    let mut matches = connections
        .iter()
        .enumerate()
        .filter(|(_, connection)| connection.to_string() == requested);
    let (index, _) = matches.next().ok_or_else(|| {
        if requested.starts_with("management:") {
            Error::not_found("management connection label not found; select its 1-based index")
        } else {
            Error::not_found(format!("connection {requested:?} not found"))
        }
    })?;
    if matches.next().is_some() {
        return Err(Error::ambiguous(
            "connection label is ambiguous; select its 1-based index",
        ));
    }
    Ok(index)
}

pub(crate) fn command() -> Command {
    base("ctx", "Show where commands act, or manage local contexts")
        .subcommand(base("ls", "List contexts"))
        .subcommand(
            base(
                "use",
                "Select a context and optionally its default connection",
            )
            .arg(positional("context-name", false))
            .arg(
                value("connection", None)
                    .help("Connection label or 1-based index in the selected context"),
            ),
        )
        .subcommand(base("rm", "Remove a local context").arg(positional("context-name", true)))
}

pub(super) fn handler(path: &str) -> Option<Runnable> {
    Some(match path {
        "" => local(Never, super::link::show),
        "ls" => local(Never, list),
        "rm" => local(Never, remove),
        "use" => local(Never, select),
        _ => return None,
    })
}
