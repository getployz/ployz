//! Authored Services in the Config Store: add, list, inspect, rename and remove.
//! Every change is staged in Working State until a Deploy ships it.

use clap::{ArgMatches, Command};
use ployz_core::config::ReviewLifecycleKind;
use ployz_store::{
    CreateGitService, CreateService, DomainRow, DomainsQuery, DomainsView, EnvironmentQuery,
    EnvironmentView, RemoveService, RenameService, ServiceLineageId, ServiceQuery, ServiceStaged,
    ServiceView, ServicesQuery, SettingPath, SettingRow,
};
use serde::Serialize;

use super::super::store::{self, Next};
use super::super::{Error, leaf_matches};
use crate::cli::{positional, value};
use crate::ui::{Cell, Fields, Hint, Table, Tone};

pub(crate) fn add_command(command: Command) -> Command {
    store::scoped(command.about("Add a Service; it is staged until a Deploy"))
        .arg(positional("name", true).help("Service name, also its Private DNS name"))
        .arg(
            value("image", None)
                .value_name("REF")
                .help("Container image to run [default: none, an empty Service]"),
        )
        .arg(
            value("repo", None)
                .value_name("OWNER/REPO[@BRANCH]")
                .conflicts_with("image")
                .help("GitHub repository to build; Ployz checks it and the branch (default: its default branch)"),
        )
}

pub(crate) fn list_command(command: Command) -> Command {
    store::scoped(command.about("List Services and what the next Deploy does to them"))
}

pub(crate) fn inspect_command(command: Command) -> Command {
    store::scoped(command.about("Show a Service, its Settings and staged changes"))
        .arg(positional("service", true))
}

pub(crate) fn rename_command(command: Command) -> Command {
    store::scoped(
        command.about("Rename a Service; its Private DNS name and references stay the same"),
    )
    .arg(positional("service", true))
    .arg(positional("name", true).help("The new name"))
}

pub(crate) fn remove_command(command: Command) -> Command {
    store::scoped(command.about("Remove a Service; it keeps running until a Deploy removes it"))
        .arg(positional("service", true))
}

/// Add an image, GitHub repository or empty Service to Working State.
pub(crate) fn add(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let id = ServiceLineageId::parse(store::mint())?;
    let environment = store::environment(matches)?;
    let name = service_name(matches, "name")?;
    let store = store::store(root)?;
    let created = if let Some(repo) = matches.get_one::<String>("repo") {
        let (repository, branch) = match repo.split_once('@') {
            Some((repository, branch)) => {
                (repository, Some(ployz_store::BranchName::parse(branch)?))
            }
            None => (repo.as_str(), None),
        };
        store.write(&CreateGitService {
            id,
            environment,
            name: name.clone(),
            repository: ployz_store::RepositoryName::parse(repository)?,
            branch,
        })?
    } else {
        let image = matches.get_one::<String>("image").cloned();
        store.write(&CreateService {
            id,
            environment,
            name: name.clone(),
            image,
            template: None,
        })?
    };
    staged(matches, &created, "Staged new Service")
}

/// List Working State's Services, and deployed ones a removal dropped from it.
pub(crate) fn list(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let query = ServicesQuery {
        environment: store::environment(matches)?,
    };
    let view = store::store(root)?.read(&query)?;
    let mut table = Table::new(
        ["SERVICE", "PRIVATE DNS", "SOURCE", "NEXT DEPLOY"],
        format!("No Services in {} yet.", view.environment.name),
    );
    for listing in &view.services {
        table.row([
            Cell::from(listing.service.name.to_string()),
            Cell::from(listing.service.private_dns.to_string()),
            Cell::from(store::word(&listing.source)),
            listing.change.as_ref().map_or_else(
                || Cell::from(""),
                |change| Cell::status(store::word(change), Tone::Change),
            ),
        ]);
    }
    crate::ui::list(&view, &table)?;
    if view.services.is_empty() {
        crate::ui::hint(&Hint::Next(store::next(
            matches,
            &["service", "add", "NAME", "--image", "REF"],
        )));
    }
    Ok(())
}

/// Show one Service: its identity, every Setting with its default, its domains and
/// staged changes.
pub(crate) fn inspect(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let environment = store::environment(matches)?;
    let name = service_name(matches, "service")?;
    let store = store::store(root)?;
    let read = || -> Result<_, Error> {
        let view = store.read(&ServiceQuery {
            environment: environment.clone(),
            service: name.clone(),
        })?;
        // A Service staged for removal is gone from Working State: it shows what is
        // deployed, with no Settings or domains left to edit.
        if view.service.change == Some(ReviewLifecycleKind::Delete) {
            return Ok((view, None));
        }
        let settings = store.read(&EnvironmentQuery {
            environment: environment.clone(),
            path: Some(SettingPath::parse(name.as_str())?),
            all: false,
        })?;
        let domains = store.read(&DomainsQuery {
            environment: environment.clone(),
            service: Some(name.clone()),
        })?;
        Ok((view, Some((settings, domains))))
    };
    let (view, rest) = store::at_one_revision("service inspect", read, torn)?;
    let removed = rest.is_none();
    let (settings, domains) = rest.map_or_else(Default::default, |(settings, domains)| {
        (settings.settings, domains.domains)
    });
    let service = &view.service.service;
    let mut record = Fields::new()
        .field("service", &service.name)
        .field("private dns", &service.private_dns)
        .field("source", store::word(&view.service.source));
    if let Some(change) = &view.service.change {
        record.push("next deploy", store::word(change));
    }
    let prefix = format!("{}.", service.name);
    for row in &settings {
        let path = row.path.to_string();
        let setting = path.strip_prefix(&prefix).unwrap_or(&path).to_owned();
        record.push(setting, store::valued(row));
    }
    if removed {
        for (setting, value) in &view.values {
            record.push(setting.clone(), store::plain(value));
        }
    }
    record.push(
        "domains",
        store::domain_list(&domains, service.name.as_str()),
    );
    for change in &view.changes {
        for line in store::changed(change) {
            record.push("staged", line);
        }
    }
    let inspected = Inspected {
        view: &view,
        settings: &settings,
        domains: &domains,
        domain_changes: store::domain_changes(&view.changes),
    };
    crate::ui::fields(&inspected, &record)
}

/// Whether `service inspect`'s Settings or domains were read in another Environment or
/// at another revision than the Service. Without `--env` each read resolves the
/// Default Environment itself.
fn torn((view, rest): &(ServiceView, Option<(EnvironmentView, DomainsView)>)) -> bool {
    rest.as_ref().is_some_and(|(settings, domains)| {
        settings.environment != view.environment || domains.environment != view.environment
    })
}

/// `service inspect`'s result: the Service, every Setting with its default, and its
/// domains.
#[derive(Serialize)]
struct Inspected<'view> {
    #[serde(flatten)]
    view: &'view ServiceView,
    settings: &'view [SettingRow],
    domains: &'view [DomainRow],
    /// Each staged domain change as people read it; `changes` stores it as `managedHostnames` or `routes`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    domain_changes: Vec<store::DomainLine>,
}

/// Rename a Service in Working State.
pub(crate) fn rename(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let rename = RenameService {
        environment: store::environment(matches)?,
        service: service_name(matches, "service")?,
        name: service_name(matches, "name")?,
    };
    let renamed = store::store(root)?.write(&rename)?;
    staged(matches, &renamed, "Staged rename of Service")
}

/// Remove a Service from Working State; the next Deploy removes it from the Servers.
pub(crate) fn remove(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let remove = RemoveService {
        environment: store::environment(matches)?,
        service: service_name(matches, "service")?,
    };
    let removed = store::store(root)?.write(&remove)?;
    staged(matches, &removed, "Staged removal of Service")
}

/// A staged Service change and, when it changed anything, `ployz deploy` to ship it.
fn staged(matches: &ArgMatches, result: &ServiceStaged, what: &str) -> Result<(), Error> {
    let hint = (!result.staged.is_empty()).then(|| store::next(matches, &["deploy"]));
    crate::ui::finish(&Next::new(result, hint.clone()), || {
        // A mutation always says what it did, nothing included.
        if result.staged.is_empty() {
            crate::ui::stream(format_args!(
                "Nothing changed in {}/{}; {} already has that name.",
                result.environment.project, result.environment.name, result.service.name
            ));
            return;
        }
        crate::ui::stream(format_args!(
            "{what} {} in {}/{} (revision {}).",
            result.service.name,
            result.environment.project,
            result.environment.name,
            result.environment.revision
        ));
        if let Some(hint) = hint.clone() {
            crate::ui::hint(&Hint::Next(hint));
        }
    })
}

use crate::handlers::store::service_name;

#[cfg(test)]
mod tests {
    use ployz_store::EnvironmentRef;

    use super::*;

    #[test]
    fn settings_or_domains_read_elsewhere_or_after_an_edit_are_torn_from_the_service() {
        let root = crate::cli::command()
            .try_get_matches_from(["ployz", "service", "inspect", "web1"])
            .unwrap();
        let (store, edit) = store::store_with_edits(leaf_matches(&root));
        edit();
        let web = ployz_core::ServiceName::parse("web1").unwrap();
        let settings = || {
            store
                .read(&EnvironmentQuery {
                    environment: EnvironmentRef::default(),
                    path: Some(SettingPath::parse("web1").unwrap()),
                    all: false,
                })
                .unwrap()
        };
        let domains = || {
            store
                .read(&DomainsQuery {
                    environment: EnvironmentRef::default(),
                    service: Some(web.clone()),
                })
                .unwrap()
        };
        let view = store
            .read(&ServiceQuery {
                environment: EnvironmentRef::default(),
                service: web.clone(),
            })
            .unwrap();
        let (before, domains_before) = (settings(), domains());
        assert!(!torn(&(view.clone(), None)));
        assert!(!torn(&(
            view.clone(),
            Some((before.clone(), domains_before.clone()))
        )));
        let mut elsewhere = domains_before.clone();
        elsewhere.environment.id = ployz_store::EnvironmentId::parse(store::mint()).unwrap();
        assert!(torn(&(view.clone(), Some((before.clone(), elsewhere)))));
        edit();
        assert!(torn(&(view.clone(), Some((settings(), domains_before)))));
        assert!(torn(&(view, Some((before, domains())))));
    }
}
