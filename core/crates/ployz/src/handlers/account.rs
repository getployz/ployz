//! `ployz token`, `ployz org` and `ployz billing`: acting in Cloud as this device's
//! sign-in or `PLOYZ_TOKEN`. None of them needs a Server.

use clap::{ArgMatches, Command};
use serde::Serialize;

use super::store::Next;
use super::{Error, Handler, config_path, leaf_matches, login::open_browser, runtime};
use crate::cli::{positional, value};
use ployz_core::RpcErrorCode;

use crate::cloud_account::{self, BillingPage, Credential, ServerClears};
use crate::cloud_login::{CredentialStore, LoginError};
use crate::ui::{self, Cell, Hint, Table, Tone};

pub(crate) fn token_command() -> Command {
    Command::new("token")
        .about("Manage Organization Tokens and signed-in devices")
        .arg_required_else_help(true)
        .subcommand(
            Command::new("new")
                .about("Make a token for PLOYZ_TOKEN; its secret is shown once")
                .arg(positional("name", true).help("What the token is for, e.g. ci"))
                .arg(
                    value("expires-in", None)
                        .value_name("DAYS")
                        .value_parser(clap::value_parser!(u16).range(1..=365))
                        .default_value("90")
                        .help("Days until the token stops working"),
                ),
        )
        .subcommand(
            Command::new("ls").about("List the Organization's tokens and your signed-in devices"),
        )
        .subcommand(
            Command::new("rm")
                .about("Revoke a token or sign out a device, at once")
                .arg(positional("id", true)),
        )
}

pub(crate) fn org_command() -> Command {
    Command::new("org")
        .about("List or switch Organizations")
        .arg_required_else_help(true)
        .subcommand(Command::new("ls").about("List the Organizations you can act in"))
        .subcommand(
            Command::new("use")
                .about("Act in another of your Organizations from this device")
                .arg(positional("organization", true).help("Organization slug")),
        )
        .subcommand(
            Command::new("build-order")
                .about("Show or set which Builders build Git Services, in turn")
                .long_about("Show or set the Organization's Build Order: which Builders a Git Service's build tries, in turn, after its Preferred Builder (SERVICE.preferredBuilder). A change applies to the next build. auto tries GitHub first, skipping it at once where the repository has no ployz-build.yml workflow.")
                .arg(positional("order", false).value_parser([
                    "auto",
                    "servers-only",
                    "github-then-servers",
                    "servers-then-github",
                    "github-only",
                ])),
        )
        .subcommand(
            Command::new("rm")
                .about("Delete the Organization you act in, once it has no Project")
                .long_about(
                    "Delete the Organization you act in. Remove its Projects first \
                     (ployz project rm). Every Server is unpaired: Cloud's key and every \
                     device key are cleared on it. A Server that doesn't confirm keeps the \
                     Organization, disabled, until the same command confirms it. Type its \
                     slug with --confirm.",
                )
                .arg(positional("organization", true).help("Organization slug"))
                .arg(
                    value("confirm", None)
                        .value_name("ORGANIZATION")
                        .help("The Organization's slug, typed to confirm its removal"),
                ),
        )
}

pub(crate) fn billing_command() -> Command {
    Command::new("billing")
        .about("Show the Organization's plan")
        .subcommand(Command::new("upgrade").about("Print the checkout link for Pro"))
        .subcommand(Command::new("manage").about("Print the billing portal link"))
}

pub(super) fn token_handler(path: &str) -> Option<Handler> {
    Some(match path {
        "new" => token_new,
        "ls" => token_list,
        "rm" => token_remove,
        _ => return None,
    })
}

pub(super) fn org_handler(path: &str) -> Option<Handler> {
    Some(match path {
        "ls" => org_list,
        "use" => org_use,
        "build-order" => org_build_order,
        "rm" => org_remove,
        _ => return None,
    })
}

pub(super) fn billing_handler(path: &str) -> Option<Handler> {
    Some(match path {
        "" => billing,
        "upgrade" => billing_upgrade,
        "manage" => billing_manage,
        _ => return None,
    })
}

/// Run `work` with `PLOYZ_TOKEN` or this device's sign-in.
pub(super) fn in_cloud<T>(
    root: &ArgMatches,
    work: impl AsyncFnOnce(&CredentialStore, &Credential) -> Result<T, LoginError>,
) -> Result<T, Error> {
    let store = CredentialStore::beside(&config_path(leaf_matches(root))?);
    runtime()?.block_on(async {
        let credential = cloud_account::from_env(&store).await?;
        Ok(work(&store, &credential).await?)
    })
}

fn token_new(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let name = matches.get_one::<String>("name").expect("name is required");
    let days = *matches
        .get_one::<u16>("expires-in")
        .expect("expires-in has a default");
    let token = in_cloud(root, async |_, credential| {
        cloud_account::new_token(credential, name, days).await
    })?;
    ui::finish(&serde_json::json!({ "token": token }), || {
        ui::note(format_args!(
            "Made token {} in Organization {}, expiring {}. Its secret is shown only now; set it as PLOYZ_TOKEN:",
            token.name.escape_debug(),
            token.organization,
            token.expires_at
        ));
        ui::stream(&token.secret);
    })
}

fn token_list(root: &ArgMatches) -> Result<(), Error> {
    let listed = in_cloud(root, async |_, credential| {
        cloud_account::credentials(credential).await
    })?;
    let mut table = Table::new(
        ["KIND", "ID", "NAME", "EXPIRES", "STATE"],
        "No tokens or signed-in devices.",
    );
    let current = |current| {
        if current {
            Cell::status("current", Tone::Good)
        } else {
            Cell::from("")
        }
    };
    for token in &listed.tokens {
        table.row([
            Cell::from("token"),
            Cell::from(&token.id),
            Cell::from(token.name.escape_debug().to_string()),
            Cell::from(token.expires_at.to_string()),
            if token.expired {
                Cell::status("expired", Tone::Bad)
            } else {
                current(token.current)
            },
        ]);
    }
    for device in &listed.devices {
        table.row([
            Cell::from("device"),
            Cell::from(&device.id),
            Cell::from(""),
            Cell::from(device.expires_at.to_string()),
            current(device.current),
        ]);
    }
    for revoking in &listed.revoking {
        table.row([
            Cell::from(super::store::word(&revoking.kind)),
            Cell::from(&revoking.id),
            Cell::from(""),
            Cell::from(""),
            Cell::status(
                format!(
                    "revoked; not yet cleared on {}",
                    super::joined(&revoking.unconfirmed)
                ),
                Tone::Change,
            ),
        ]);
    }
    ui::list(&listed, &table)
}

fn token_remove(root: &ArgMatches) -> Result<(), Error> {
    let id = leaf_matches(root)
        .get_one::<String>("id")
        .expect("id is required");
    let (removed, servers) = in_cloud(root, async |_, credential| {
        cloud_account::remove_token(credential, id).await
    })?;
    let next = retry_clears(&servers, id);
    let report = TokenRemoved {
        removed: &removed,
        servers: &servers,
        next: next.as_deref(),
    };
    ui::finish(&report, || {
        match removed.kind {
            cloud_account::CredentialKind::Device => {
                ui::stream(format_args!("Signed out device {}.", removed.id));
            }
            cloud_account::CredentialKind::Token => {
                ui::stream(format_args!("Revoked token {}.", removed.id));
            }
        }
        say_clears(&servers, next.as_deref());
    })?;
    unconfirmed(&servers)
}

/// Exit 3 while a Server hasn't confirmed its Clear: the result printed is partial.
pub(super) fn unconfirmed(servers: &ServerClears) -> Result<(), Error> {
    if servers.unconfirmed.is_empty() {
        Ok(())
    } else {
        Err(Error::partial())
    }
}

#[derive(Serialize)]
struct TokenRemoved<'a> {
    removed: &'a cloud_account::Removed,
    servers: &'a ServerClears,
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<&'a str>,
}

/// The retry for Servers that haven't confirmed clearing credential `id`'s key.
pub(super) fn retry_clears(servers: &ServerClears, id: &str) -> Option<String> {
    (!servers.unconfirmed.is_empty()).then(|| format!("ployz token rm {id}"))
}

pub(super) fn say_clears(servers: &ServerClears, retry: Option<&str>) {
    if !servers.confirmed.is_empty() {
        ui::note(format_args!(
            "Cleared its key on {} Server(s).",
            servers.confirmed.len()
        ));
    }
    if let Some(retry) = retry {
        ui::warn(format!(
            "{} did not confirm; Cloud already refuses it.",
            super::joined(&servers.unconfirmed)
        ));
        ui::hint(&Hint::Retry(retry.to_owned()));
    }
}

fn org_list(root: &ArgMatches) -> Result<(), Error> {
    let organizations = in_cloud(root, async |_, credential| {
        cloud_account::organizations(credential).await
    })?;
    let mut table = Table::new(["ORGANIZATION", "NAME", "CURRENT"], "No Organizations yet.");
    for organization in &organizations {
        table.row([
            Cell::from(organization.slug.to_string()),
            Cell::from(organization.name.to_string()),
            if organization.current {
                Cell::status("current", Tone::Good)
            } else {
                Cell::from("")
            },
        ]);
    }
    ui::list(
        &serde_json::json!({ "organizations": organizations }),
        &table,
    )
}

fn org_build_order(root: &ArgMatches) -> Result<(), Error> {
    let store = super::store::store(root)?;
    let order = leaf_matches(root).get_one::<String>("order");
    let view = match order {
        None => store.read(&ployz_store::BuildOrderQuery {})?,
        Some(order) => {
            let build_order = (order != "auto")
                .then(|| serde_json::from_value(serde_json::json!(order)))
                .transpose()
                .expect("clap accepts only Build Orders");
            store.write(&ployz_store::SetBuildOrder { build_order })?
        }
    };
    let builders = view
        .builders
        .iter()
        .map(|builder| match builder {
            ployz_store::Builder::Github => "GitHub",
            ployz_store::Builder::Servers => "your servers",
        })
        .collect::<Vec<_>>()
        .join(", then ");
    let mut json = serde_json::to_value(&view).expect("a Build Order is JSON");
    if let (Some(_), Some(fields)) = (order, json.as_object_mut()) {
        fields.insert("immediate".to_owned(), serde_json::json!(true));
    }
    crate::ui::finish(&json, || match (order, view.build_order) {
        (Some(_), _) => ui::stream(format_args!(
            "Builds try {builders} from the next build on."
        )),
        (None, None) => ui::stream(format_args!("Auto: builds try {builders}.")),
        (None, Some(_)) => ui::stream(format_args!("Builds try {builders}.")),
    })
}

fn org_use(root: &ArgMatches) -> Result<(), Error> {
    let slug = leaf_matches(root)
        .get_one::<String>("organization")
        .expect("organization is required");
    let organization = in_cloud(root, async |store, credential| {
        cloud_account::use_organization(store, credential, slug).await
    })?;
    crate::ui::finish(&serde_json::json!({ "organization": organization }), || {
        crate::ui::stream(format_args!(
            "This device now acts in Organization {}.",
            organization.slug
        ))
    })
}

/// Delete the Organization: Cloud refuses while it has a Project, then unpairs its
/// Servers. Not every Server confirmed: exit 3 naming the same command.
fn org_remove(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let slug = matches
        .get_one::<String>("organization")
        .expect("organization is required");
    let again = ["org", "rm", slug.as_str(), "--confirm", slug.as_str()];
    let retry = shell_words::join(std::iter::once("ployz").chain(again));
    super::teardown::confirm(matches, slug, "Organization", retry.clone(), || {
        let refusal = Error::detailed(
            RpcErrorCode::ConfirmationRequired,
            format!(
                "Removing Organization {slug} deletes it with its tokens, Servers' pairing and \
                 settings; this can't be undone. No changes made."
            ),
            serde_json::json!({ "organization": slug }),
        )
        .hint(Hint::Retry(retry.clone()));
        let loss = ui::Tree::new(
            format!("Removing Organization {slug} deletes, for good:"),
            vec![
                ui::Tree::leaf("its tokens and settings"),
                ui::Tree::leaf("its Servers' pairing"),
            ],
        );
        Ok((refusal, loss))
    })?;
    let store = CredentialStore::beside(&config_path(matches)?);
    let removal = runtime()?.block_on(async {
        let credential = cloud_account::from_env(&store).await?;
        cloud_account::remove_organization(&credential, slug).await
    })?;
    let next = (!removal.removed).then_some(retry.as_str());
    let report = Next::new(&removal, next.map(str::to_owned));
    ui::finish(&report, || {
        if !removal.servers.confirmed.is_empty() {
            ui::note(format_args!(
                "Unpaired {} Server(s).",
                removal.servers.confirmed.len()
            ));
        }
        match next {
            None => ui::stream(format_args!(
                "Removed Organization {}.",
                removal.organization
            )),
            Some(next) => {
                ui::stream(format_args!(
                    "Disabled Organization {}; it stays until its Servers confirm unpairing.",
                    removal.organization,
                ));
                ui::warn(format!(
                    "{} did not confirm unpairing.",
                    super::joined(&removal.servers.unconfirmed)
                ));
                ui::hint(&Hint::Retry(next.to_owned()));
            }
        }
    })?;
    match removal.removed {
        true => Ok(()),
        false => Err(Error::partial()),
    }
}

#[derive(Serialize)]
struct BillingReport {
    billing: cloud_account::Billing,
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<&'static str>,
}

fn billing(root: &ArgMatches) -> Result<(), Error> {
    let billing = in_cloud(root, async |_, credential| {
        cloud_account::billing(credential).await
    })?;
    let next = billing.plan.next();
    let report = BillingReport { billing, next };
    crate::ui::finish(&report, || {
        let billing = &report.billing;
        let plan = billing.plan.label();
        let domains = if billing.custom_domains {
            "allowed"
        } else {
            "need Pro"
        };
        ui::stream(format_args!(
            "Organization {}: {plan}; custom domains {domains}.",
            billing.organization
        ));
        if let Some(next) = report.next {
            ui::hint(&Hint::Next(next.to_owned()));
        }
    })
}

fn billing_upgrade(root: &ArgMatches) -> Result<(), Error> {
    billing_page(root, BillingPage::Checkout)
}

fn billing_manage(root: &ArgMatches) -> Result<(), Error> {
    billing_page(root, BillingPage::Portal)
}

fn billing_page(root: &ArgMatches, page: BillingPage) -> Result<(), Error> {
    let url = in_cloud(root, async |_, credential| {
        cloud_account::billing_url(credential, page).await
    })?;
    crate::ui::finish(&serde_json::json!({ "url": url }), || {
        ui::stream(&url);
        if ui::interactive() {
            open_browser(&url);
        }
    })
}
