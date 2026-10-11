//! `ployz get`, `set` and `unset`: read and edit Settings in the Config Store.
//!
//! Variables are `SERVICE.env.KEY`; a secret's value comes only from stdin
//! (`--secret`) or an env file, and reads show it as `{"secret": true}`.
//!
//! `get` narrows by depth: the whole Environment shows what differs from a default
//! (`--all` adds the rest), `get SERVICE` shows every Setting and its domains plus
//! the `values` object that `set SERVICE --patch` takes back.

use crate::ui::{self, Hint};
use clap::{Arg, ArgAction, ArgMatches, Command};
use std::collections::BTreeSet;

use ployz_store::{
    Change, DomainRow, DomainsQuery, DomainsView, Edit, EnvironmentQuery, EnvironmentView,
    HoldSecret, Instead, NodeName, PullRequestNumber, Revision, SettingPath, SettingRow,
    TypedAddresses,
};
use serde::Serialize;
use serde_json::{Value, json};

use super::store::{Next, environment, next, scoped, store, with_refresh_hint};
use super::{Error, leaf_matches};
use crate::cli::{positional, switch, value};

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
        .arg(
            value("rename", None)
                .value_name("NAME")
                .conflicts_with_all(["patch", "secret", "from-env-file"])
                .help("Rename the variable SERVICE.env.KEY to NAME; its value moves with it, and a secret stays sealed"),
        )
        .arg(
            switch("update-references", None)
                .requires("rename")
                .help("With --rename: switch the variables that reference it to the new name too"),
        )
        .arg(
            value("at-merge", None)
                .value_name("PR")
                .requires("secret")
                .conflicts_with_all(["patch", "from-env-file", "expect"])
                .help("With --secret: hold the --env Destination's value for a secret pull request PR syncs there by name only; it lands with the merge"),
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
            })
        })
        .transpose()
}

pub(super) fn get(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let path = matches
        .get_one::<String>("path")
        .map(|path| SettingPath::parse(path))
        .transpose()?;
    let all = matches.get_flag("all");
    let store = store(root)?;
    let environment = environment(matches)?;
    // Domains are no Setting: `get` reads them beside a Service's or the Environment's
    // Settings, and never beside one Setting.
    let (service, every) = match path.as_ref().map(|path| NodeName::parse(&path.to_string())) {
        None => (Some(None), all),
        Some(Ok(NodeName::Service(service))) => (Some(Some(service)), true),
        Some(_) => (None, false),
    };
    let read = || -> Result<_, Error> {
        let domains = service
            .clone()
            .map(|service| {
                store.read(&DomainsQuery {
                    environment: environment.clone(),
                    service,
                })
            })
            .transpose()?;
        let view = store.read(&EnvironmentQuery {
            environment: environment.clone(),
            path: path.clone(),
            all,
        })?;
        Ok((view, domains))
    };
    let (view, domains) = super::store::at_one_revision("get", read, torn)?;
    let domains = domains.map(|domains| domains.domains);
    let lines = got(
        &view.settings,
        domains.as_deref().unwrap_or_default(),
        every,
    );
    let got = Got {
        view: &view,
        domains: domains.as_deref(),
    };
    crate::ui::finish(&got, || {
        if view.settings.is_empty() {
            ui::note(format_args!(
                "No Services in {}/{} yet.",
                view.environment.project, view.environment.name
            ));
        }
        for line in &lines {
            crate::ui::stream(line);
        }
    })
}

/// Whether the domains were read in another Environment or at another revision than
/// the Settings. Without `--env` each read resolves the Default Environment itself.
fn torn((view, domains): &(EnvironmentView, Option<DomainsView>)) -> bool {
    domains
        .as_ref()
        .is_some_and(|domains| domains.environment != view.environment)
}

/// `get`'s result: the Settings read, and the domains of the Services it covers;
/// none when it read one Setting.
#[derive(Serialize)]
struct Got<'view> {
    #[serde(flatten)]
    view: &'view EnvironmentView,
    #[serde(skip_serializing_if = "Option::is_none")]
    domains: Option<&'view [DomainRow]>,
}

/// Each Service's Settings, then its `SERVICE.domains` line. With `every`, a
/// Service without domains still gets one, reading `-`.
fn got(settings: &[SettingRow], domains: &[DomainRow], every: bool) -> Vec<String> {
    let mut lines = Vec::new();
    let mut shown = BTreeSet::new();
    for rows in settings.chunk_by(|one, next| one.path.node() == next.path.node()) {
        lines.extend(
            rows.iter()
                .map(|row| format!("{} = {}", row.path, super::store::valued(row))),
        );
        if let Some(NodeName::Service(service)) = rows.first().map(|row| row.path.node()) {
            let listed = super::store::domain_list(domains, service.as_str());
            if every || listed != "-" {
                lines.push(format!("{service}.domains = {listed}"));
            }
            shown.insert(service);
        }
    }
    for row in domains {
        let service = &row.domain.service;
        if shown.insert(service.clone()) {
            let listed = super::store::domain_list(domains, service.as_str());
            lines.push(format!("{service}.domains = {listed}"));
        }
    }
    lines
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
            ));
        }
        return edit(
            matches,
            vec![Change::Patch {
                path: SettingPath::parse(&service)?,
                value,
            }],
        );
    }
    if let Some(to) = matches.get_one::<String>("rename") {
        let path = one_service(
            matches,
            "--rename takes one variable: set web.env.API_URL --rename PUBLIC_API_URL",
        )?;
        if path.contains('=') {
            return Err(Error::usage(
                "--rename takes a variable, not a value: set web.env.API_URL --rename PUBLIC_API_URL",
            ));
        }
        return edit(
            root,
            vec![Change::Rename {
                path: SettingPath::parse(&path)?,
                to: to.clone(),
                update_references: matches.get_flag("update-references"),
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
            ));
        }
        let mut secret = std::io::read_to_string(std::io::stdin())?;
        // `echo VALUE |` ends it with a newline that is not part of the secret.
        if secret.ends_with('\n') {
            secret.pop();
            if secret.ends_with('\r') {
                secret.pop();
            }
        }
        if secret.is_empty() {
            return Err(Error::usage(format!(
                "No secret on stdin for {path}; pipe one in: printf %s \"$VALUE\" | ployz set {path} --secret"
            )));
        }
        if let Some(number) = matches.get_one::<String>("at-merge") {
            return hold(root, number, &path, secret);
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
            let (path, value) = assignment
                .split_once('=')
                .ok_or_else(|| Error::usage("Expected PATH=VALUE, for example web.replicas=3"))?;
            Ok(Change::Set {
                path: SettingPath::parse(path)?,
                value: Value::String(value.to_owned()),
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    if let Some(path) = repeated(&changes) {
        return Err(Error::usage(format!("{path} is given twice; set it once")));
    }
    edit(root, changes)
}

/// A path two of `changes` set: only the last would land, without a word.
fn repeated(changes: &[Change]) -> Option<String> {
    let mut seen = std::collections::BTreeSet::new();
    changes.iter().find_map(|change| match change {
        Change::Set { path, .. } => (!seen.insert(path.to_string())).then(|| path.to_string()),
        Change::Unset { .. } | Change::Patch { .. } | Change::Rename { .. } => None,
    })
}

/// The one positional a flag applies to.
fn one_service(matches: &ArgMatches, usage: &'static str) -> Result<String, Error> {
    match matches.get_many::<String>("assignment") {
        Some(mut paths) if paths.len() == 1 => paths.next().cloned(),
        Some(_) | None => None,
    }
    .ok_or_else(|| Error::usage(usage))
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
        std::fs::read_to_string(file).map_err(|_| Error::usage("Can't read the env file"))?
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
        return Err(Error::usage("The env file sets no variables"));
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
        .map_err(|error| Error::usage(format!("Env file {}", lowercase_first(&error.message))))
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
    let unchanged = unchanged(&changes);
    let edit = Edit {
        environment: environment(matches)?,
        expect: expected(matches)?,
        changes,
    };
    let store = store(root)?;
    let edited = store
        .try_write(&edit)
        .map_err(|error| store.fail(with_refresh_hint(error, matches, "get")))?;
    let hint = (!edited.staged.is_empty()).then(|| next(matches, &["deploy"]));
    ui::finish(&Next::new(&edited, hint.clone()), || {
        let where_ = format!("{}/{}", edited.environment.project, edited.environment.name);
        if !edited.staged.is_empty() {
            crate::ui::stream(format_args!(
                "Staged {} in {where_} (revision {}).",
                super::joined(&edited.staged),
                edited.environment.revision
            ));
        }
        if !edited.immediate.is_empty() {
            crate::ui::stream(format_args!(
                "Applied {} in {where_}.",
                super::joined(&edited.immediate)
            ));
        }
        // A mutation always says what it did, nothing included.
        if edited.staged.is_empty() && edited.immediate.is_empty() {
            ui::stream(format_args!("Nothing changed in {where_}; {unchanged}."));
        }
        for typed in &edited.typed_addresses {
            say_typed_addresses(matches, typed);
        }
        if let Some(hint) = hint {
            ui::hint(&Hint::Next(hint));
        }
    })
}

/// Why an edit that changed nothing had nothing to change.
fn unchanged(changes: &[Change]) -> String {
    if let [Change::Rename { path, to, .. }] = changes {
        return format!("{path} is already named {to}");
    }
    let paths = changes
        .iter()
        .map(|change| match change {
            Change::Set { path, .. }
            | Change::Unset { path }
            | Change::Patch { path, .. }
            | Change::Rename { path, .. } => path.to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let unset = changes
        .iter()
        .all(|change| matches!(change, Change::Unset { .. }));
    match (unset, changes.len() > 1) {
        (true, false) => format!("{paths} is already unset"),
        (true, true) => format!("{paths} are already unset"),
        (false, false) => format!("{paths} already has that value"),
        (false, true) => format!("{paths} already have those values"),
    }
}

/// Hold `secret` as the Destination's value of the row `asked` names for pull
/// request `number`'s merge.
fn hold(root: &ArgMatches, number: &str, asked: &str, secret: String) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let pull_request = number
        .trim_start_matches('#')
        .parse()
        .ok()
        .and_then(|number| PullRequestNumber::parse(number).ok())
        .ok_or_else(|| {
            Error::usage("Expected --at-merge PR to be a pull request number, for example 142")
        })?;
    let environment = environment(matches)?;
    let store = store(root)?;
    let request = HoldSecret {
        environment,
        pull_request,
        row: asked.into(),
        value: secret,
    };
    let held = store.write(&request)?;
    crate::ui::finish(&held, || {
        crate::ui::stream(format_args!(
            "Holding {}'s value of {asked} for #{}'s merge.",
            held.environment.name, held.pull_request
        ));
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
    ui::warn(format!(
        "{path} types the private address of {services}, so Ployz can't see that {consumer} uses {them}: a Branch that doesn't copy {them} can't reach {them}, and a Deploy won't start {them} first."
    ));
    match &typed.instead {
        Instead::Reference { value } => {
            ui::hint(&Hint::Retry(next(
                matches,
                &["set", &format!("{path}={value}")],
            )));
        }
        Instead::Sealed => ui::note(format_args!(
            "It is sealed, so it can't hold a reference: seal only the password, in its own variable, and set {path} from references to it and to the address."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, value: Value, default: Value) -> SettingRow {
        serde_json::from_value(json!({
            "path": path, "value": value, "default": default, "apply": "staged",
        }))
        .unwrap()
    }

    fn domain(service: &str, prefix: &str, port: Option<u16>) -> DomainRow {
        serde_json::from_value(json!({
            "service": service, "kind": "generated", "prefix": prefix,
            "hostname": format!("{prefix}.acme.ployz.app"), "port": port,
            "status": "ready", "reason": null, "action": null,
        }))
        .unwrap()
    }

    #[test]
    fn get_marks_defaults_and_shows_domains_beside_their_service() {
        let settings = [
            row("web.replicas", json!(3), json!(1)),
            row("web.restartPolicy", json!("always"), json!("always")),
            row("volumes.data.name", json!("data"), Value::Null),
            row("worker.replicas", json!(1), json!(1)),
        ];
        let domains = [
            domain("web", "shop", Some(3000)),
            domain("api", "api", None),
        ];
        assert_eq!(
            got(&settings, &domains, true),
            [
                "web.replicas = 3 (default 1)",
                "web.restartPolicy = always (default)",
                "web.domains = shop.acme.ployz.app (port 3000)",
                "volumes.data.name = data",
                "worker.replicas = 1 (default)",
                "worker.domains = -",
                "api.domains = api.acme.ployz.app",
            ]
        );
        let changed = got(&settings[3..], &[], false);
        assert_eq!(changed, ["worker.replicas = 1 (default)"]);
    }

    #[test]
    fn domains_read_in_another_environment_or_revision_than_the_settings_are_torn() {
        let at = |revision: u64| {
            json!({
                "id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", "project": "shop",
                "name": "production", "revision": revision,
            })
        };
        let view: EnvironmentView =
            serde_json::from_value(json!({ "environment": at(3), "settings": [] })).unwrap();
        let domains = |revision| -> DomainsView {
            serde_json::from_value(json!({ "environment": at(revision), "domains": [] })).unwrap()
        };
        assert!(!torn(&(view.clone(), None)));
        assert!(!torn(&(view.clone(), Some(domains(3)))));
        assert!(torn(&(view.clone(), Some(domains(4)))));
        let mut elsewhere = domains(3);
        elsewhere.environment.id =
            ployz_store::EnvironmentId::parse("cccccccc-cccc-4ccc-8ccc-cccccccccccc").unwrap();
        assert!(torn(&(view, Some(elsewhere))));
    }

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

    #[test]
    fn a_no_op_edit_agrees_with_how_many_paths_it_names() {
        let unset = |path: &str| Change::Unset {
            path: SettingPath::parse(path).unwrap(),
        };
        let set = Change::Set {
            path: SettingPath::parse("web.replicas").unwrap(),
            value: json!("2"),
        };
        assert_eq!(
            unchanged(&[unset("web.env.A")]),
            "web.env.A is already unset"
        );
        assert_eq!(
            unchanged(&[unset("web.env.A"), unset("web.env.B")]),
            "web.env.A, web.env.B are already unset"
        );
        assert_eq!(
            unchanged(std::slice::from_ref(&set)),
            "web.replicas already has that value"
        );
        assert_eq!(
            unchanged(&[set, unset("web.env.A")]),
            "web.replicas, web.env.A already have those values"
        );
    }
}
