//! `ployz domain`: a Service's public domains in the Config Store. A generated one
//! lives under the Organization's Cluster Domain; a custom one needs Ployz Pro. Both
//! are staged until a Deploy, and each reads Ready, Setting up or Needs attention
//! with at most one action.

use clap::{ArgMatches, Command};
use ployz_store::{
    AddDomain, DomainAction, DomainQuery, DomainRow, DomainStaged, DomainStatus, DomainsQuery,
    Hostname, RemoveDomain, SetGeneratedDomain,
};

use super::store::{self, Next};
use super::{Error, leaf_matches, required};
use crate::cli::{positional, value};
use crate::failure::USAGE_EXIT;
use crate::output::{self, say};

pub(crate) fn command() -> Command {
    Command::new("domain")
        .about("Manage public domains")
        .arg_required_else_help(true)
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
            Hostname::parse(host.trim().to_ascii_lowercase()).map_err(|_| {
                Error::usage("Expected a hostname like app.example.com").with_exit(USAGE_EXIT)
            })
        })
        .transpose()?;
    let add = AddDomain {
        environment: store::environment(matches)?,
        service: store::service_name(matches, "service")?,
        hostname,
        port: matches.get_one::<u16>("port").copied(),
    };
    let added = store::store(root)?.write(&add)?;
    staged(matches, &added, "Staged domain")
}

fn set(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let set = SetGeneratedDomain {
        environment: store::environment(matches)?,
        service: store::service_name(matches, "service")?,
        prefix: required(matches, "prefix")?,
    };
    let changed = store::store(root)?
        .write(&set)?;
    staged(matches, &changed, "Staged generated domain")
}

fn remove(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let remove = RemoveDomain {
        environment: store::environment(matches)?,
        domain: required(matches, "domain")?,
    };
    let removed = store::store(root)?.write(&remove)?;
    staged(matches, &removed, "Staged removal of domain")
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
    let view = store::store(root)?
        .read(&query)?;
    let next = view
        .domains
        .iter()
        .find_map(|row| next(matches, row.action.as_ref()));
    output::finish(&Next::new(&view, next), || {
        if view.domains.is_empty() {
            say!(
                "No domains in {}/{}. Add one with: ployz domain add SERVICE",
                view.environment.project,
                view.environment.name
            );
        }
        for row in &view.domains {
            show(row);
        }
    })
}

fn check(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let query = DomainQuery {
        environment: store::environment(matches)?,
        domain: required(matches, "domain")?,
    };
    let view = store::store(root)?.read(&query)?;
    let next = next(matches, view.domain.action.as_ref());
    output::finish(&Next::new(&view, next), || show(&view.domain))
}

/// A staged domain change and, when it changed anything, `ployz deploy` to ship it.
fn staged(matches: &ArgMatches, result: &DomainStaged, what: &str) -> Result<(), Error> {
    let hint = (!result.staged.is_empty()).then(|| store::next(matches, &["deploy"]));
    output::finish(&Next::new(result, hint), || {
        say!(
            "{what} {} on {} in {}/{} (revision {}).",
            result.domain.shown(),
            result.domain.service,
            result.environment.project,
            result.environment.name,
            result.environment.revision
        );
    })
}

/// The command a domain's action names, when it names one.
fn next(matches: &ArgMatches, action: Option<&DomainAction>) -> Option<String> {
    match action? {
        DomainAction::Deploy => Some(store::next(matches, &["deploy"])),
        DomainAction::AddServer => Some("ployz server add USER@HOST".to_owned()),
        DomainAction::Dns { .. } => None,
    }
}

fn show(row: &DomainRow) {
    let status = match row.status {
        DomainStatus::Ready => "Ready",
        DomainStatus::SettingUp => "Setting up",
        DomainStatus::NeedsAttention => "Needs attention",
    };
    let port = row
        .domain
        .port
        .map_or_else(|| "PORT".to_owned(), |port| port.to_string());
    let reason = row
        .reason
        .as_deref()
        .map_or_else(String::new, |reason| format!(" · {reason}"));
    say!(
        "{}\t{} → {port}\t{status}{reason}",
        row.domain.shown(),
        row.domain.service
    );
    if let Some(DomainAction::Dns { records }) = &row.action {
        for record in records {
            say!(
                "  add DNS {}\t{}\t{}",
                record.kind,
                record.name,
                record.value
            );
        }
    }
}
