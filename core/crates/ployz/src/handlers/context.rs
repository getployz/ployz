use crate::ui::{self, Cell, Hint, Table, Tone};
use std::path::Path;

use clap::{ArgMatches, Command};

use crate::cli::{base, positional, value};
use serde::Serialize;
use serde_json::json;

use crate::context::{
    Config, ConnectionError, ContextError, RemovedContext, expand_home, is_management_capability,
};

use super::{Error, leaf_matches, required};

/// The command that gets a user with no contexts past them, keeping a config file named on
/// the command line. Signed out, that is `ployz login`: Cloud Servers need no context. Signed
/// in, it is their Cloud Servers. `PLOYZ_CONTEXT` outlives both and sends them back here, so
/// with it set no command does. Read from local state only: choosing a hint never asks Cloud.
fn no_contexts_next(leaf: &ArgMatches) -> Option<String> {
    if std::env::var_os(crate::cli::env::CONTEXT).is_some() {
        return None;
    }
    let command: &[&str] = if super::server::signed_in_locally(leaf) {
        &["server", "ls"]
    } else {
        &["login"]
    };
    let mut args = vec!["ployz"];
    if leaf.value_source("ployz-config") == Some(clap::parser::ValueSource::CommandLine)
        && let Some(path) = leaf.get_one::<String>("ployz-config")
    {
        args.extend(["--ployz-config", path]);
    }
    args.extend(command);
    Some(shell_words::join(args))
}

/// Whatever command failed for want of a context gets the line `ctx ls` prints.
pub(super) fn with_no_contexts_next(failure: Error, leaf: &ArgMatches) -> Error {
    if !failure.has_no_contexts() {
        return failure;
    }
    failure.hint(no_contexts_next(leaf).map(Hint::Next))
}

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
        "No contexts yet. Contexts reach Clusters outside Ployz Cloud; your Cloud Servers need none.",
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
    if !contexts.is_empty() {
        return ui::list(&json!({ "contexts": contexts }), &table);
    }
    let Some(next) = no_contexts_next(leaf_matches(matches)) else {
        return ui::list(&json!({ "contexts": contexts }), &table);
    };
    ui::list(&json!({ "contexts": contexts, "next": next }), &table)?;
    ui::hint(&Hint::Next(next));
    Ok(())
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
        return Err(ContextError::NoContexts(config.path().to_owned()).into());
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
    let connection = save_selection(&config, &selected, requested_connection.map(String::as_str))?;
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

/// Save `selected` as the current context, and `requested` as its default
/// connection, in the latest config. Returns the default connection.
fn save_selection(
    config: &Config,
    selected: &str,
    requested: Option<&str>,
) -> Result<Option<String>, Error> {
    let loaded = &config
        .contexts
        .get(selected)
        .expect("set_current_context accepted a known context")
        .connections;
    let requested = requested
        .map(|requested| {
            connection_index(selected, loaded, requested).map(|index| {
                loaded
                    .get(index)
                    .expect("connection_index checks the range")
            })
        })
        .transpose()?;
    let mut connection = None;
    config.update(|latest| {
        latest.set_current_context(Some(selected.to_owned()))?;
        let context = latest
            .contexts
            .get_mut(selected)
            .expect("set_current_context accepted a known context");
        if let Some(requested) = requested {
            let index = context
                .connections
                .iter()
                .position(|connection| connection.is_same(requested))
                .ok_or_else(|| {
                    Error::not_found(format!("connection {:?} not found", requested.to_string()))
                })?;
            context.select_connection(index);
        }
        connection = context.connections.first().map(ToString::to_string);
        Ok::<_, Error>(())
    })?;
    Ok(connection)
}

pub(super) fn remove(matches: &ArgMatches) -> Result<(), Error> {
    let mut config = config(matches)?;
    let name = required(leaf_matches(matches), "context-name")?;
    let mut removed = config.remove_context(&name)?;
    config.update(|latest| {
        removed = latest.remove_context(&name)?;
        Ok::<_, Error>(())
    })?;
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

pub(super) fn command(command: Command) -> Command {
    base(command, "Show where commands act, or manage local contexts")
}

pub(super) fn list_command(command: Command) -> Command {
    base(command, "List contexts")
}

pub(super) fn select_command(command: Command) -> Command {
    base(
        command,
        "Select a context and optionally its default connection",
    )
    .arg(
        positional("context-name", false)
            .help("The context to select; the current one when omitted"),
    )
    .arg(
        value("connection", None).help("Connection label or 1-based index in the selected context"),
    )
}

pub(super) fn remove_command(command: Command) -> Command {
    base(command, "Remove a local context")
        .arg(positional("context-name", true).help("The context to remove"))
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, os::unix::fs::PermissionsExt};

    use ployz_core::{ManagementCapability, ManagementIdentity};

    use super::save_selection;
    use crate::context::{Config, Connection, Context};

    fn saved(dir: &tempfile::TempDir, connections: Vec<Connection>) -> Config {
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = Config::new(
            dir.path().join("config.yaml"),
            None,
            BTreeMap::from([("first".into(), Context { connections })]),
        );
        config.save().unwrap();
        config
    }

    #[test]
    fn a_connection_removed_since_load_fails_and_saves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let unix = Connection::unix("/tmp/ployz.sock").unwrap();
        let ssh: Connection = "ssh://root@example.com".parse().unwrap();
        let label = ssh.to_string();
        let stale = saved(&dir, vec![unix.clone(), ssh]);
        // Another command drops the requested connection before the update.
        let mut latest = Config::load(stale.path()).unwrap();
        latest.contexts.get_mut("first").unwrap().connections = vec![unix];
        latest.save().unwrap();
        let error = save_selection(&stale, "first", Some(&label)).unwrap_err();
        assert!(error.to_string().contains("not found"), "{error}");
        assert_eq!(Config::load(stale.path()).unwrap(), latest);
    }

    #[test]
    fn a_connection_reordered_since_load_is_still_the_one_selected() {
        let dir = tempfile::tempdir().unwrap();
        let unix = Connection::unix("/tmp/ployz.sock").unwrap();
        let ssh: Connection = "ssh://root@example.com".parse().unwrap();
        let stale = saved(&dir, vec![unix.clone(), ssh.clone()]);
        // Another `ctx use --connection` moves ssh to the front before the update.
        let mut latest = Config::load(stale.path()).unwrap();
        latest.contexts.get_mut("first").unwrap().connections = vec![ssh.clone(), unix.clone()];
        latest.save().unwrap();
        assert_eq!(
            save_selection(&stale, "first", Some("2")).unwrap(),
            Some(ssh.to_string())
        );
        assert_eq!(
            Config::load(stale.path())
                .unwrap()
                .contexts
                .get("first")
                .unwrap()
                .connections,
            vec![ssh, unix]
        );
    }

    #[test]
    fn a_connection_rotated_since_load_is_still_selected() {
        let dir = tempfile::tempdir().unwrap();
        let old = ManagementCapability::new(ManagementIdentity::from_bytes([1; 32]), [2; 32]);
        let new = ManagementCapability::new(*old.machine(), [3; 32]);
        let unix = Connection::unix("/tmp/ployz.sock").unwrap();
        let stale = saved(
            &dir,
            vec![
                unix.clone(),
                Connection::management(old.to_secret_string()).unwrap(),
            ],
        );
        stale.save_management_capability(&new).unwrap();
        let rotated = Connection::management(new.to_secret_string()).unwrap();
        assert_eq!(
            save_selection(&stale, "first", Some("2")).unwrap(),
            Some(rotated.to_string())
        );
        let saved = Config::load(stale.path()).unwrap();
        assert_eq!(saved.current_context(), Some("first"));
        assert_eq!(
            saved.contexts.get("first").unwrap().connections,
            vec![rotated, unix]
        );
    }
}
