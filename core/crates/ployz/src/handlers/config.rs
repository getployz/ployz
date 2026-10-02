//! `ployz get`, `set` and `unset`: read and edit Settings in the Config Store.
//!
//! Variables are `SERVICE.env.KEY`; a secret's value comes only from stdin
//! (`--secret`) or an env file, and reads show it as `{"secret": true}`.
//!
//! `get` narrows by depth: the whole Environment shows what differs from a default
//! (`--all` adds the rest), `get SERVICE` shows every Setting plus the `values`
//! object that `set SERVICE --patch` takes back.

use clap::{Arg, ArgAction, ArgMatches, Command};
use ployz_store::{Change, Edit, EnvironmentQuery, Instead, Revision, SettingPath, TypedAddresses};
use serde_json::{Value, json};

use super::store::{Next, environment, next, scoped, store, with_refresh_hint};
use super::{Error, leaf_matches};
use crate::cli::{positional, switch, value};
use crate::failure::USAGE_EXIT;
use crate::output::say;

pub(crate) fn get_command() -> Command {
    scoped(Command::new("get").about("Show Settings: every Service, one Service, or one Setting"))
        .arg(
            positional("path", false)
                .help("SERVICE, SERVICE.SETTING or SERVICE.env.KEY")
                .add(super::catalog::setting_paths()),
        )
        .arg(switch("all", None).help("Include Settings at their default across the Environment"))
}

pub(crate) fn set_command() -> Command {
    scoped(Command::new("set").about("Stage Setting values"))
        .arg(
            positional("assignment", true)
                .action(ArgAction::Append)
                .value_name("PATH=VALUE")
                .help("For example web.replicas=3 or web.env.LOG_LEVEL=info; the SERVICE a --patch or --from-env-file applies to; web.env.KEY or web.registryCredential for --secret")
                .add(super::catalog::setting_paths()),
        )
        .arg(
            value("patch", None)
                .conflicts_with_all(["secret", "from-env-file"])
                .value_name("JSON")
                .help("Set a Service's Settings from an object shaped like `get SERVICE --json` values; omitted Settings stay; - reads stdin"),
        )
        .arg(
            switch("secret", None)
                .help("Seal a value read from stdin: set web.env.KEY --secret, or set web.registryCredential --secret for a private image's token. With --from-env-file, seal every value"),
        )
        .arg(
            value("from-env-file", None)
                .value_name("FILE")
                .help("Set a Service's variables from a .env file; - reads stdin. Variables that are secret stay secret"),
        )
        .arg(expect())
}

pub(crate) fn unset_command() -> Command {
    scoped(Command::new("unset").about("Return Settings to their defaults"))
        .arg(
            positional("path", true)
                .action(ArgAction::Append)
                .help("SERVICE.SETTING, or SERVICE.env.KEY to delete a variable")
                .add(super::catalog::setting_paths()),
        )
        .arg(expect())
}

// Parsed by the handler, not clap: clap's error would echo the rejected value.
pub(super) fn expect() -> Arg {
    value("expect", None)
        .value_name("REVISION")
        .help("Refuse unless Working State is still at this revision")
}

pub(super) fn expected(matches: &ArgMatches) -> Result<Option<Revision>, Error> {
    matches
        .get_one::<String>("expect")
        .map(|revision| {
            revision.parse().map(Revision).map_err(|_| {
                Error::usage("Expected --expect REVISION to be a revision number, for example 3")
                    .with_exit(USAGE_EXIT)
            })
        })
        .transpose()
}

pub(super) fn get(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let query = EnvironmentQuery {
        environment: environment(matches)?,
        path: matches
            .get_one::<String>("path")
            .map(|path| SettingPath::parse(path))
            .transpose()?,
        all: matches.get_flag("all"),
    };
    let view = store(root)?.read(&query)?;
    crate::output::finish(&view, || {
        if view.settings.is_empty() {
            say!(
                "No Services in {}/{}.",
                view.environment.project,
                view.environment.name
            );
        }
        for row in &view.settings {
            let default = if row.value == row.default {
                " (default)"
            } else {
                ""
            };
            say!("{} = {}{default}", row.path, display_value(&row.value));
        }
    })
}

pub(super) fn set(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    if let Some(file) = matches.get_one::<String>("from-env-file") {
        return set_from_env_file(root, file);
    }
    if let Some(patch) = matches.get_one::<String>("patch") {
        let inline = patch != "-";
        let service = one_service(
            matches,
            "--patch takes one SERVICE, for example set web --patch '{\"replicas\":3}'",
        )?;
        let patch = if inline {
            patch.clone()
        } else {
            std::io::read_to_string(std::io::stdin())?
        };
        let value: Value = serde_json::from_str(&patch).map_err(|_| {
            Error::usage("--patch expects a JSON object, for example '{\"replicas\":3}'")
                .with_exit(USAGE_EXIT)
        })?;
        // A new secret on the command line would land in shell history.
        let sealing = |variable: &Value| {
            [Some(variable), variable.get("value")]
                .into_iter()
                .flatten()
                .any(|value| value.get("secret").is_some_and(Value::is_string))
        };
        let inline_secret = value
            .get("env")
            .and_then(Value::as_object)
            .is_some_and(|env| env.values().any(sealing))
            || value.get("registryCredential").is_some_and(sealing);
        if inline_secret && inline {
            return Err(Error::usage(
                "A new secret never goes on the command line: use --patch -, --secret or --from-env-file",
            )
            .with_exit(USAGE_EXIT));
        }
        return edit(
            matches,
            vec![Change::Patch {
                path: SettingPath::parse(&service)?,
                value,
            }],
        );
    }
    if matches.get_flag("secret") {
        let path = one_service(
            matches,
            "--secret reads one value from stdin: set web.env.KEY --secret",
        )?;
        if path.contains('=') {
            return Err(Error::usage(
                "--secret reads the value from stdin, never the command line: set web.env.KEY --secret",
            )
            .with_exit(USAGE_EXIT));
        }
        let mut secret = std::io::read_to_string(std::io::stdin())?;
        // `echo VALUE |` ends it with a newline that is not part of the secret.
        if secret.ends_with('\n') {
            secret.pop();
            if secret.ends_with('\r') {
                secret.pop();
            }
        }
        return edit(
            root,
            vec![Change::Set {
                path: SettingPath::parse(&path)?,
                value: json!({ "secret": secret }),
            }],
        );
    }
    let changes = matches
        .get_many::<String>("assignment")
        .into_iter()
        .flatten()
        .map(|assignment| {
            let (path, value) = assignment.split_once('=').ok_or_else(|| {
                Error::usage("Expected PATH=VALUE, for example web.replicas=3")
                    .with_exit(USAGE_EXIT)
            })?;
            Ok(Change::Set {
                path: SettingPath::parse(path)?,
                value: Value::String(value.to_owned()),
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    if let Some(path) = repeated(&changes) {
        return Err(
            Error::usage(format!("{path} is given twice; set it once")).with_exit(USAGE_EXIT)
        );
    }
    edit(root, changes)
}

/// A path two of `changes` set: only the last would land, without a word.
fn repeated(changes: &[Change]) -> Option<String> {
    let mut seen = std::collections::BTreeSet::new();
    changes.iter().find_map(|change| match change {
        Change::Set { path, .. } => (!seen.insert(path.to_string())).then(|| path.to_string()),
        Change::Unset { .. } | Change::Patch { .. } => None,
    })
}

/// The one positional a flag applies to.
fn one_service(matches: &ArgMatches, usage: &'static str) -> Result<String, Error> {
    match matches.get_many::<String>("assignment") {
        Some(mut paths) if paths.len() == 1 => paths.next().cloned(),
        Some(_) | None => None,
    }
    .ok_or_else(|| Error::usage(usage).with_exit(USAGE_EXIT))
}

/// `set SERVICE --from-env-file FILE [--secret]`: every variable in the file, in one
/// edit. A variable the Service already holds as a secret is sealed again, never
/// made plain.
fn set_from_env_file(root: &ArgMatches, file: &str) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let service = one_service(
        matches,
        "--from-env-file takes one SERVICE: set web --from-env-file .env",
    )?;
    let service = SettingPath::parse(&service)?;
    let text = if file == "-" {
        std::io::read_to_string(std::io::stdin())?
    } else {
        std::fs::read_to_string(file)
            .map_err(|_| Error::usage("Can't read the env file").with_exit(USAGE_EXIT))?
    };
    let variables = env_file(&text)?;
    let seal_all = matches.get_flag("secret");
    let secret = if seal_all {
        Vec::new()
    } else {
        let view = store(root)?.read(&EnvironmentQuery {
            environment: environment(matches)?,
            path: Some(service.clone()),
            all: false,
        })?;
        let marker = json!({ "secret": true });
        view.values
            .and_then(|mut values| values.remove("env"))
            .and_then(|env| env.as_object().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, value)| *value == marker || value.get("value") == Some(&marker))
            .map(|(key, _)| key)
            .collect()
    };
    let changes = variables
        .into_iter()
        .map(|(key, value)| {
            let sealed = seal_all || secret.contains(&key.to_ascii_uppercase());
            Ok(Change::Set {
                path: SettingPath::parse(&format!("{service}.env.{key}"))?,
                value: if sealed {
                    json!({ "secret": value })
                } else {
                    Value::String(value)
                },
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    if changes.is_empty() {
        return Err(Error::usage("The env file sets no variables").with_exit(USAGE_EXIT));
    }
    edit(root, changes)
}

/// `KEY=VALUE` lines of a .env file, read by core's one parser (the dashboard's raw editor uses it too).
/// Errors name the line, never its text.
fn env_file(text: &str) -> Result<Vec<(String, String)>, Error> {
    ployz_core::config::parse_env_file(text)
        .map(|entries| {
            entries
                .into_iter()
                .map(|entry| (entry.key, entry.value))
                .collect()
        })
        .map_err(|error| {
            Error::usage(format!("Env file {}", lowercase_first(&error.message)))
                .with_exit(USAGE_EXIT)
        })
}

fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(chars).collect()
    })
}

pub(super) fn unset(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let changes = matches
        .get_many::<String>("path")
        .into_iter()
        .flatten()
        .map(|path| {
            Ok(Change::Unset {
                path: SettingPath::parse(path)?,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    edit(root, changes)
}

/// Apply `changes`.
fn edit(root: &ArgMatches, changes: Vec<Change>) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let edit = Edit {
        environment: environment(matches)?,
        expect: expected(matches)?,
        changes,
    };
    let store = store(root)?;
    let edited = store
        .try_write(&edit)
        .map_err(|error| store.fail(with_refresh_hint(error, matches, "get")))?;
    let hint = (!edited.staged.is_empty()).then(|| next(matches, &["diff"]));
    crate::output::finish(&Next::new(&edited, hint), || {
        let where_ = format!("{}/{}", edited.environment.project, edited.environment.name);
        if !edited.staged.is_empty() {
            say!(
                "Staged {} in {where_} (revision {}).",
                super::joined(&edited.staged),
                edited.environment.revision
            );
        }
        if !edited.immediate.is_empty() {
            say!("Applied {} in {where_}.", super::joined(&edited.immediate));
        }
        // A mutation always says what it did, nothing included.
        if edited.staged.is_empty() && edited.immediate.is_empty() {
            say!("No change in {where_}: already set.");
        }
        for typed in &edited.typed_addresses {
            say_typed_addresses(matches, typed);
        }
    })
}

/// What a variable's Typed Addresses cost, and what to set instead.
fn say_typed_addresses(matches: &ArgMatches, typed: &TypedAddresses) {
    let (path, services) = (&typed.path, super::joined(&typed.services));
    let them = if typed.services.len() == 1 {
        "it"
    } else {
        "them"
    };
    let consumer = path.node();
    say!(
        "{path} types the private address of {services}, so Ployz can't see that {consumer} uses {them}: a Branch that doesn't copy {them} can't reach {them}, and a Deploy won't start {them} first."
    );
    match &typed.instead {
        Instead::Reference { value } => {
            say!(
                "Set the reference instead: {}",
                next(matches, &["set", &format!("{path}={value}")])
            );
        }
        Instead::Sealed => say!(
            "It is sealed, so it can't hold a reference: seal only the password, in its own variable, and set {path} from references to it and to the address."
        ),
    }
}

/// A Setting value as a person reads it: escaped text, anything else as JSON.
fn display_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.escape_debug().to_string(),
        Value::Null => "-".to_owned(),
        Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_set_twice_is_named() {
        let set = |path: &str, value: &str| Change::Set {
            path: SettingPath::parse(path).unwrap(),
            value: json!(value),
        };
        assert_eq!(
            repeated(&[set("web.replicas", "2"), set("web.image", "a")]),
            None
        );
        assert_eq!(
            repeated(&[set("web.replicas", "2"), set("WEB.replicas", "3")]).as_deref(),
            Some("web.replicas")
        );
    }
}
