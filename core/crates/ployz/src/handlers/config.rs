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
use std::io::{IsTerminal as _, Read};

use ployz_core::{MachineId, MachineObservation, MachineTarget, RpcErrorCode};
use ployz_store::{
    Change, DomainRow, DomainsQuery, DomainsView, Edit, EnvironmentQuery, EnvironmentView, Instead,
    NodeName, Revision, SettingPath, SettingRow, TypedAddresses,
};
use serde::Serialize;
use serde_json::{Value, json};

use super::store::{Next, environment, next, scoped, store, with_refresh_hint};
use super::{Error, MAX_CLI_INPUT_BYTES, input_too_long, leaf_matches, read_bounded};
use crate::cli::{positional, switch, value};

pub(super) fn get_command(command: Command) -> Command {
    scoped(command.about("Show Settings: every Service, one Service, or one Setting"))
        .arg(
            positional("path", false)
                .help("SERVICE, SERVICE.SETTING, SERVICE.env.KEY, or setup.SERVICE for a Branch's Setup Command")
                .add(super::catalog::setting_paths()),
        )
        .arg(switch("all", None).help("Include Settings at their default across the Environment"))
}

pub(super) fn set_command(command: Command) -> Command {
    scoped(command.about("Stage Setting values"))
        .arg(
            positional("assignment", true)
                .action(ArgAction::Append)
                .value_name("PATH=VALUE")
                .help("For example web.replicas=3, web.env.LOG_LEVEL=info, or setup.web='pnpm db:seed' for a Branch's Setup Command; the SERVICE a --patch or --from-env-file applies to; web.env.KEY or web.registryCredential for --secret")
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
        .arg(expect())
}

pub(super) fn unset_command(command: Command) -> Command {
    scoped(command.about("Return Settings to their defaults"))
        .arg(
            positional("path", true)
                .action(ArgAction::Append)
                .help("SERVICE.SETTING, SERVICE.env.KEY to delete a variable, or setup.SERVICE to remove a Branch's Setup Command")
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
    // Settings, alone at `SERVICE.domains`, and never beside one Setting.
    let only = path.as_ref().and_then(SettingPath::domains_of).cloned();
    let (service, every) = match path.as_ref().map(|path| NodeName::parse(&path.to_string())) {
        None => (Some(None), all),
        Some(Ok(NodeName::Service(service))) => (Some(Some(service)), true),
        Some(_) => (only.clone().map(Some), false),
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
    let lines = match &only {
        Some(service) => vec![format!(
            "{service}.domains = {}",
            super::store::domain_list(domains.as_deref().unwrap_or_default(), service.as_str())
        )],
        None => got(
            &view.settings,
            domains.as_deref().unwrap_or_default(),
            every,
        ),
    };
    let got = Got {
        view: &view,
        domains: domains.as_deref(),
    };
    crate::ui::finish(&got, || {
        if view.settings.is_empty() && only.is_none() {
            ui::note(format_args!(
                "No Services in {}/{} yet.",
                view.environment.project, view.environment.name
            ));
        }
        for line in &lines {
            crate::ui::stream(line);
        }
        for note in noted(view.settings.iter().filter_map(|row| row.note.as_ref())) {
            ui::note(note);
        }
    })
}

/// Each distinct note once, in order: Settings that share a caveat say it once.
fn noted<'a>(notes: impl Iterator<Item = &'a String>) -> Vec<&'a String> {
    let mut seen = Vec::new();
    for note in notes {
        if !seen.contains(&note) {
            seen.push(note);
        }
    }
    seen
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
        let stdin = std::io::stdin();
        let stdin_is_terminal = stdin.is_terminal();
        let secret = read_secret(&path, stdin, stdin_is_terminal)?;
        return edit(
            root,
            vec![Change::Set {
                path: SettingPath::parse(&path)?,
                value: json!({ "secret": secret }),
            }],
        );
    }
    let assignments = matches
        .get_many::<String>("assignment")
        .into_iter()
        .flatten()
        .map(|assignment| {
            assignment
                .split_once('=')
                .ok_or_else(|| Error::usage("Expected PATH=VALUE, for example web.replicas=3"))
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let naming = assignments
        .iter()
        .any(|(path, value)| names_a_server(path, value));
    // Without Server access the values go as typed: Cloud resolves a name from the
    // Servers it keeps, for every role.
    let servers = naming
        .then(|| super::server::servers(matches).ok())
        .flatten();
    let changes = assignments
        .into_iter()
        .map(|(path, value)| {
            Ok(Change::Set {
                path: SettingPath::parse(path)?,
                value: Value::String(resolved(path, value, servers.as_deref())?),
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    edit(root, changes)
}

/// `value` as `set` sends it for `path`: a Server named as a Preferred Builder becomes
/// its Machine ID, which is all the Store takes, by the names the Servers report.
/// It resolves as the `server` commands do: an exact Machine ID first, then a name. An
/// ID no Server lists, or any value without `servers`, goes as given: a preference needs
/// no live Server.
fn resolved(
    path: &str,
    value: &str,
    servers: Option<&[MachineObservation]>,
) -> Result<String, Error> {
    let Some(servers) = servers.filter(|_| names_a_server(path, value)) else {
        return Ok(value.to_owned());
    };
    // The Store trims a Preferred Builder before it reads one.
    let value = value.trim();
    match crate::cluster::visible_machine(&MachineTarget::parse(value)?, servers) {
        Ok(server) => Ok(server.machine.id.to_string()),
        Err(error) if error.code == RpcErrorCode::NotFound && MachineId::parse(value).is_ok() => {
            Ok(value.to_owned())
        }
        Err(error) => Err(error.into()),
    }
}

/// Whether `value` names a Server, by ID or name, as `path`'s Preferred Builder.
fn names_a_server(path: &str, value: &str) -> bool {
    matches!(path.split_once('.'), Some((service, "preferredBuilder")) if !service.contains('.'))
        && value != "github"
}

/// The one positional a flag applies to.
fn one_service(matches: &ArgMatches, usage: &'static str) -> Result<String, Error> {
    match matches.get_many::<String>("assignment") {
        Some(mut paths) if paths.len() == 1 => paths.next().cloned(),
        Some(_) | None => None,
    }
    .ok_or_else(|| Error::usage(usage))
}

/// The value for `set PATH --secret`. Typed at a terminal, it is asked for with
/// echo off, so it never shows on screen; piped, stdin is read to its end.
fn read_secret(path: &str, stdin: impl Read, stdin_is_terminal: bool) -> Result<String, Error> {
    if stdin_is_terminal {
        return ui::secret(
            &format!("Value for {path}"),
            || {
                Error::usage(format!(
                    "{path}: this run can't prompt (--json, CI, TERM=dumb or stderr not a terminal), and a typed secret would show; pipe it in: printf %s \"$VALUE\" | ployz set {path} --secret"
                ))
            },
            "Cancelled. Nothing was staged.",
        );
    }
    let secret = read_bounded(stdin, MAX_CLI_INPUT_BYTES)?
        .ok_or_else(|| input_too_long("stdin", "a secret"))?;
    // `internal`, as when stdin was read as a string.
    let mut secret = String::from_utf8(secret).map_err(|_| {
        Error::coded(
            RpcErrorCode::Internal,
            format!("{path}: the secret on stdin isn't UTF-8 text"),
        )
    })?;
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
    Ok(secret)
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
    let (bytes, from) = if file == "-" {
        (
            read_bounded(std::io::stdin(), MAX_CLI_INPUT_BYTES)?,
            "stdin",
        )
    } else {
        (
            std::fs::File::open(file)
                .and_then(|from| read_bounded(from, MAX_CLI_INPUT_BYTES))
                .map_err(|error| {
                    Error::caused(
                        RpcErrorCode::InvalidArgument,
                        format!("Can't read the env file {file}"),
                        error,
                    )
                })?,
            file,
        )
    };
    let bytes = bytes.ok_or_else(|| input_too_long(from, "an env file"))?;
    // Stdin that isn't text stays `internal`, as when it was read as a string.
    let text = String::from_utf8(bytes).map_err(|_| {
        let code = if file == "-" {
            RpcErrorCode::Internal
        } else {
            RpcErrorCode::InvalidArgument
        };
        Error::coded(code, format!("{from}: an env file is UTF-8 text"))
    })?;
    // A key the file repeats takes its last value, in its first place; variables are
    // uppercase, so case doesn't make a new key.
    let mut variables: Vec<(String, String)> = Vec::new();
    for (key, value) in env_file(&text)? {
        match variables
            .iter_mut()
            .find(|(seen, _)| seen.eq_ignore_ascii_case(&key))
        {
            Some((_, seen)) => *seen = value,
            None => variables.push((key, value)),
        }
    }
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

fn edit(root: &ArgMatches, changes: Vec<Change>) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let unchanged = unchanged(&changes);
    let edit = Edit {
        environment: environment(matches)?,
        expect: expected(matches)?,
        changes,
    };
    let store = store(root)?;
    let fail = |error| store.fail(with_refresh_hint(error, matches, "get"));
    let edit = store.seal_edit(edit, fail)?;
    let edited = store.try_write(&edit).map_err(fail)?;
    let hint = (!edited.staged.is_empty()).then(|| next(matches, &["deploy"]));
    ui::finish(&Next::new(&edited, hint.clone()), || {
        let where_ = format!("{}/{}", edited.environment.project, edited.environment.name);
        if !edited.staged.is_empty() {
            crate::ui::stream(format_args!(
                "Staged {} in {where_}.",
                super::joined(&edited.staged)
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
        for note in noted(edited.notes.iter().map(|noted| &noted.note)) {
            ui::note(note);
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
    let Instead::Reference { value } = &typed.instead;
    ui::hint(&Hint::Retry(next(
        matches,
        &["set", &format!("{path}={value}")],
    )));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(seed: u8, name: &str) -> MachineObservation {
        MachineObservation::new(
            ployz_core::Machine {
                labels: Default::default(),
                accepts_builds: true,
                accepts_services: true,
                accepts_ingress: true,
                id: ployz_core::MachineId::parse(format!("{seed:032x}")).unwrap(),
                name: ployz_core::MachineName::parse(name).unwrap(),
                subnet: format!("10.210.{seed}.0/24").parse().unwrap(),
                public_key: ployz_core::WireGuardPublicKey([seed; 32]),
                public_ip: None,
                advertised_endpoints: Vec::new(),
                runtime: Default::default(),
                build_concurrency: None,
            },
            ployz_core::MembershipObservation::Up,
        )
    }

    #[test]
    fn a_secret_is_never_read_from_a_terminal_that_cant_hide_it_nor_named_in_an_error() {
        const SENTINEL: &[u8] = b"SENTINEL-3414-do-not-echo\n";
        // Tests run in Plain mode, where nothing prompts: the terminal is refused, not read.
        let mut typed = SENTINEL;
        let refused = read_secret("web.env.TOKEN", &mut typed, true).unwrap_err();
        assert_eq!(typed, SENTINEL, "stdin was read");
        let message = refused.to_string();
        assert!(message.contains("web.env.TOKEN"), "{message}");
        assert!(message.contains("pipe it in"), "{message}");
        assert_eq!(refused.report().code, RpcErrorCode::InvalidArgument);
        assert!(!message.contains("SENTINEL"), "{message}");

        // Piped, the value is read to its end, without the newline `echo` adds.
        assert_eq!(
            read_secret("web.env.TOKEN", SENTINEL, false).unwrap(),
            "SENTINEL-3414-do-not-echo"
        );
        for (piped, code) in [
            (&b"\n"[..], RpcErrorCode::InvalidArgument),
            (b"SENTINEL-3414\xff", RpcErrorCode::Internal),
        ] {
            let error = read_secret("web.env.TOKEN", piped, false).unwrap_err();
            assert_eq!(error.report().code, code);
            let message = error.to_string();
            assert!(!message.contains("SENTINEL"), "{message}");
        }
    }

    #[test]
    fn a_preferred_builder_named_by_its_server_is_sent_as_its_machine_id() {
        let servers = [server(1, "machine-1"), server(2, "machine-2")];
        let id = format!("{:032x}", 2);
        assert_eq!(
            resolved("web.preferredBuilder", "machine-2", Some(&servers)).unwrap(),
            id
        );
        for (path, value) in [
            ("web.preferredBuilder", "github"),
            ("web.preferredBuilder", id.as_str()),
            ("web.env.preferredBuilder", "machine-2"),
            ("web.image", "machine-2"),
        ] {
            assert_eq!(
                resolved(path, value, None).unwrap(),
                value,
                "{path}={value}"
            );
        }
        let missing = resolved("web.preferredBuilder", "machine-9", Some(&servers)).unwrap_err();
        assert!(missing.to_string().contains("machine-9"), "{missing}");
    }

    #[test]
    fn a_value_resolves_without_its_surrounding_whitespace_as_the_store_reads_it() {
        let servers = [server(1, "machine-1")];
        for (value, id) in [
            (format!(" {:032x}", 7), format!("{:032x}", 7)),
            (format!("{:032x} ", 1), format!("{:032x}", 1)),
            (" machine-1 ".to_owned(), format!("{:032x}", 1)),
        ] {
            assert_eq!(
                resolved("web.preferredBuilder", &value, Some(&servers)).unwrap(),
                id,
                "{value:?}"
            );
        }
    }

    #[test]
    fn an_id_the_servers_dont_list_is_sent_as_typed() {
        let servers = [server(1, "machine-1")];
        let unlisted = format!("{:032x}", 7);
        assert_eq!(
            resolved("web.preferredBuilder", &unlisted, Some(&servers)).unwrap(),
            unlisted,
            "a preference needs no live Server; build selection falls back without it"
        );
    }

    #[test]
    fn an_id_shaped_value_resolves_as_server_commands_do_an_exact_id_first() {
        let named_like_an_id = "a".repeat(32);
        let servers = [server(1, &named_like_an_id), server(0xaa, "machine-aa")];
        assert_eq!(
            resolved(
                "web.preferredBuilder",
                &format!("{:032x}", 0xaa),
                Some(&servers)
            )
            .unwrap(),
            format!("{:032x}", 0xaa),
            "an exact Machine ID wins"
        );
        assert_eq!(
            resolved(
                "web.preferredBuilder",
                &named_like_an_id,
                Some(&servers[..1])
            )
            .unwrap(),
            format!("{:032x}", 1),
            "a Server named like an ID resolves to its own ID"
        );
        let both = [server(1, &format!("{:032x}", 2)), server(2, "machine-2")];
        assert_eq!(
            resolved("web.preferredBuilder", &format!("{:032x}", 2), Some(&both)).unwrap(),
            format!("{:032x}", 2),
            "one Server's ID beats another's name"
        );
    }

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
                "web.restartPolicy = \"always\" (default)",
                "web.domains = shop.acme.ployz.app (port 3000)",
                "volumes.data.name = \"data\"",
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
