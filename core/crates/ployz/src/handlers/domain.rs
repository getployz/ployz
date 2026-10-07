//! `ployz domain`: a Service's public domains in the Config Store. A generated one
//! lives under the Organization's Cluster Domain; a custom one needs Ployz Pro. Both
//! are staged until a Deploy, and each reads Ready, Setting up or Needs attention
//! with at most one action.

use clap::{ArgMatches, Command};
use ployz_core::DomainPrefix;
use ployz_store::{
    AddDomain, DomainAction, DomainQuery, DomainRow, DomainStaged, DomainStatus, DomainsQuery,
    Hostname, RemoveDomain, SetGeneratedDomain,
};

use super::store::{self, Next};
use super::{Error, leaf_matches, required};
use crate::cli::{positional, value};
use crate::ui::{self, Cell, Fields, Hint, Table, Tone};

pub(crate) fn command() -> Command {
    Command::new("domain")
        .about("Manage public domains")
        .subcommand(
            store::scoped(Command::new("add").about(
                "Give a Service a domain: HOST for a custom one (Ployz Pro), none for a generated one",
            ))
            .arg(positional("service", true))
            .arg(positional("host", false).help("Custom hostname, like app.example.com"))
            .arg(
                value("port", None)
                    .value_parser(clap::value_parser!(u16).range(1..))
                    .help("Container port it reaches [default: the container's PORT]"),
            ),
        )
        .subcommand(
            store::scoped(Command::new("set").about(
                "Change a Service's generated domain to PREFIX.CLUSTER-DOMAIN; staged until you deploy",
            ))
            .arg(positional("service", true))
            .arg(
                positional("prefix", true)
                    .help("One DNS label, unique among the Organization's generated domains"),
            )
            .arg(
                value("port", None)
                    .value_parser(clap::value_parser!(u16).range(1..))
                    .help("Container port it reaches [default: the container's PORT]"),
            ),
        )
        .subcommand(
            store::scoped(
                Command::new("ls")
                    .about("List domains: Ready, Setting up or Needs attention, and what to do"),
            )
            .arg(positional("service", false).help("Only this Service's domains")),
        )
        .subcommand(
            store::scoped(Command::new("rm").about("Remove a domain; it serves until a Deploy"))
                .arg(positional("domain", true).help("Hostname, or a generated domain's prefix")),
        )
        .subcommand(
            store::scoped(
                Command::new("check").about("Re-check a domain's DNS and certificate now"),
            )
            .arg(positional("domain", true).help("Hostname, or a generated domain's prefix")),
        )
}

pub(super) fn handler(path: &str) -> Option<super::Handler> {
    Some(match path {
        "add" => add,
        "set" => set,
        "ls" => list,
        "rm" => remove,
        "check" => check,
        _ => return None,
    })
}

fn add(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let hostname = matches
        .get_one::<String>("host")
        .map(|host| {
            // A rejected hostname is never echoed.
            Hostname::parse(host.trim().to_ascii_lowercase())
                .map_err(|_| Error::usage("Expected a hostname like app.example.com"))
        })
        .transpose()?;
    let add = AddDomain {
        environment: store::environment(matches)?,
        service: store::service_name(matches, "service")?,
        hostname,
        port: matches.get_one::<u16>("port").copied(),
    };
    let added = store::store(root)?.write(&add)?;
    staged(matches, &added, "Staged domain", "is already on")
}

fn set(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let set = SetGeneratedDomain {
        environment: store::environment(matches)?,
        service: store::service_name(matches, "service")?,
        prefix: DomainPrefix::parse(required(matches, "prefix")?.trim().to_ascii_lowercase())
            .map_err(|_| Error::usage("Expected one DNS label, like shop"))?,
        port: matches.get_one::<u16>("port").copied().map(Some),
    };
    let changed = store::store(root)?.write(&set)?;
    staged(
        matches,
        &changed,
        "Staged generated domain",
        "already reaches",
    )
}

fn remove(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let remove = RemoveDomain {
        environment: store::environment(matches)?,
        domain: required(matches, "domain")?,
    };
    let removed = store::store(root)?.write(&remove)?;
    staged(
        matches,
        &removed,
        "Staged removal of domain",
        "is already off",
    )
}

fn list(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let service = matches
        .get_one::<String>("service")
        .map(|_| store::service_name(matches, "service"))
        .transpose()?;
    let query = DomainsQuery {
        environment: store::environment(matches)?,
        service,
    };
    let view = store::store(root)?.read(&query)?;
    let next = view
        .domains
        .iter()
        .find_map(|row| next(matches, row.action.as_ref()));
    let mut table = Table::new(
        ["DOMAIN", "SERVICE", "PORT", "STATUS", "REASON"],
        format!(
            "No domains in {}/{} yet.",
            view.environment.project, view.environment.name
        ),
    );
    for row in &view.domains {
        table.row(cells(row));
    }
    ui::list(&Next::new(&view, next.clone()), &table)?;
    for row in &view.domains {
        say_dns(row);
    }
    if view.domains.is_empty() {
        ui::hint(&Hint::Next(store::next(
            matches,
            &["domain", "add", "SERVICE"],
        )));
    }
    say_next(next);
    Ok(())
}

fn check(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let query = DomainQuery {
        environment: store::environment(matches)?,
        domain: required(matches, "domain")?,
    };
    let view = store::store(root)?.read(&query)?;
    let next = next(matches, view.domain.action.as_ref());
    let row = &view.domain;
    let [domain, service, port, status, reason] = cells(row);
    let record = Fields::new()
        .field("domain", domain)
        .field("service", service)
        .field("port", port)
        .field("status", status)
        .field("reason", reason);
    ui::fields(&Next::new(&view, next.clone()), &record)?;
    say_dns(row);
    say_next(next);
    Ok(())
}

/// A staged domain change and, when it changed anything, `ployz deploy` to ship it.
/// A change that was already so says nothing changed: `already` joins the domain
/// to its Service, as in `example.com is already on web`.
fn staged(
    matches: &ArgMatches,
    result: &DomainStaged,
    what: &str,
    already: &str,
) -> Result<(), Error> {
    let where_ = format!("{}/{}", result.environment.project, result.environment.name);
    if result.staged.is_empty() {
        return ui::done(
            &Next::new(result, None),
            format_args!(
                "Nothing changed in {where_}; {} {already} {}.",
                result.domain.shown(),
                result.domain.service
            ),
        );
    }
    let hint = store::next(matches, &["deploy"]);
    ui::done(
        &Next::new(result, Some(hint.clone())),
        format_args!(
            "{what} {} on {} in {where_} (revision {}).",
            result.domain.shown(),
            result.domain.service,
            result.environment.revision
        ),
    )?;
    say_next(Some(hint));
    Ok(())
}

fn say_next(next: Option<String>) {
    if let Some(next) = next {
        ui::hint(&Hint::Next(next));
    }
}

/// The command a domain's action names, when it names one.
fn next(matches: &ArgMatches, action: Option<&DomainAction>) -> Option<String> {
    match action? {
        DomainAction::Deploy => Some(store::next(matches, &["deploy"])),
        DomainAction::AddServer => Some("ployz server add USER@HOST".to_owned()),
        DomainAction::Dns { .. } => None,
    }
}

fn cells(row: &DomainRow) -> [Cell; 5] {
    let status = match row.status {
        DomainStatus::Ready => Cell::status("ready", Tone::Good),
        DomainStatus::SettingUp => Cell::status("setting up", Tone::Change),
        DomainStatus::NeedsAttention => Cell::status("needs attention", Tone::Bad),
    };
    [
        Cell::from(row.domain.shown().to_string()),
        Cell::from(row.domain.service.to_string()),
        Cell::from(
            row.domain
                .port
                .map(|port| port.to_string())
                .unwrap_or_default(),
        ),
        status,
        Cell::from(row.reason.clone().unwrap_or_default()),
    ]
}

/// The DNS records a domain still needs, one per stderr line.
fn say_dns(row: &DomainRow) {
    if let Some(DomainAction::Dns { records }) = &row.action {
        for record in records {
            ui::note(format_args!(
                "Add DNS for {}: {} {} {}",
                row.domain.shown(),
                record.kind,
                record.name,
                record.value
            ));
        }
    }
}
