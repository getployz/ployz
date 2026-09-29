//! `ployz token`, `ployz org` and `ployz billing`: acting in Cloud as this device's
//! sign-in or `PLOYZ_TOKEN`. None of them needs a Server.

use clap::{ArgMatches, Command};
use serde::Serialize;

use super::{Error, Handler, Json, config_path, leaf_matches, login::open_browser, runtime};
use crate::cli::{env, positional, value};
use crate::cloud_account::{self, BillingPage, Credential};
use crate::cloud_login::{CredentialStore, LoginError};
use crate::output::say;

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
}

pub(crate) fn billing_command() -> Command {
    Command::new("billing")
        .about("Show the Organization's plan")
        .subcommand(Command::new("upgrade").about("Print the checkout link for Pro"))
        .subcommand(Command::new("manage").about("Print the billing portal link"))
}

pub(super) fn token_handler(path: &str) -> Option<(Handler, Json)> {
    Some(match path {
        "new" => (token_new, Json::Supported),
        "ls" => (token_list, Json::Supported),
        "rm" => (token_remove, Json::Supported),
        _ => return None,
    })
}

pub(super) fn org_handler(path: &str) -> Option<(Handler, Json)> {
    Some(match path {
        "ls" => (org_list, Json::Supported),
        "use" => (org_use, Json::Supported),
        _ => return None,
    })
}

pub(super) fn billing_handler(path: &str) -> Option<(Handler, Json)> {
    Some(match path {
        "" => (billing, Json::Supported),
        "upgrade" => (billing_upgrade, Json::Supported),
        "manage" => (billing_manage, Json::Supported),
        _ => return None,
    })
}

/// Run `work` with `PLOYZ_TOKEN` or this device's sign-in.
fn in_cloud<T>(
    root: &ArgMatches,
    work: impl AsyncFnOnce(&CredentialStore, &Credential) -> Result<T, LoginError>,
) -> Result<T, Error> {
    let store = CredentialStore::beside(&config_path(leaf_matches(root))?);
    let token = std::env::var(env::TOKEN).ok();
    let cloud = std::env::var(env::CLOUD_URL).ok();
    runtime()?.block_on(async {
        let credential = cloud_account::credential(&store, token, cloud).await?;
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
    crate::output::finish(&serde_json::json!({ "token": token }), || {
        say!(
            "Made token {} ({}) in Organization {}, expiring {}.",
            token.name,
            token.id,
            token.organization,
            token.expires_at
        );
        say!("Its secret is shown only now; set it as PLOYZ_TOKEN:");
        say!("{}", token.secret);
    })
}

fn token_list(root: &ArgMatches) -> Result<(), Error> {
    let listed = in_cloud(root, async |_, credential| {
        cloud_account::credentials(credential).await
    })?;
    crate::output::finish(&listed, || {
        say!("KIND\tID\tNAME\tEXPIRES");
        for token in &listed.tokens {
            let state = if token.expired { " (expired)" } else { "" };
            let current = if token.current { " *" } else { "" };
            say!(
                "token\t{}\t{}{current}\t{}{state}",
                token.id,
                token.name,
                token.expires_at
            );
        }
        for device in &listed.devices {
            let current = if device.current { " *" } else { "" };
            say!("device\t{}\t-{current}\t{}", device.id, device.expires_at);
        }
    })
}

fn token_remove(root: &ArgMatches) -> Result<(), Error> {
    let id = leaf_matches(root)
        .get_one::<String>("id")
        .expect("id is required");
    let removed = in_cloud(root, async |_, credential| {
        cloud_account::remove_token(credential, id).await
    })?;
    crate::output::finish(
        &serde_json::json!({ "removed": removed }),
        || match removed.kind.as_str() {
            "device" => say!("Signed out device {}.", removed.id),
            _ => say!("Revoked token {}.", removed.id),
        },
    )
}

fn org_list(root: &ArgMatches) -> Result<(), Error> {
    let organizations = in_cloud(root, async |_, credential| {
        cloud_account::organizations(credential).await
    })?;
    crate::output::finish(
        &serde_json::json!({ "organizations": organizations }),
        || {
            say!("ORGANIZATION\tNAME");
            for organization in &organizations {
                let current = if organization.current { " *" } else { "" };
                say!("{}{current}\t{}", organization.slug, organization.name);
            }
        },
    )
}

fn org_use(root: &ArgMatches) -> Result<(), Error> {
    let slug = leaf_matches(root)
        .get_one::<String>("organization")
        .expect("organization is required");
    let organization = in_cloud(root, async |store, credential| {
        cloud_account::use_organization(store, credential, slug).await
    })?;
    crate::output::finish(&serde_json::json!({ "organization": organization }), || {
        say!(
            "This device now acts in Organization {}.",
            organization.slug
        )
    })
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
    let next = match (billing.self_hosted, billing.pro) {
        (true, _) => None,
        (false, true) => Some("ployz billing manage"),
        (false, false) => Some("ployz billing upgrade"),
    };
    let report = BillingReport { billing, next };
    crate::output::finish(&report, || {
        let billing = &report.billing;
        let plan = match (billing.self_hosted, billing.pro) {
            (true, _) => "self-hosted, no billing",
            (false, true) => "Pro",
            (false, false) => "no plan",
        };
        let domains = if billing.custom_domains {
            "allowed"
        } else {
            "need Pro"
        };
        say!(
            "Organization {}: {plan}; custom domains {domains}.",
            billing.organization
        );
        if let Some(next) = report.next {
            say!("Next: {next}");
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
    crate::output::finish(&serde_json::json!({ "url": url }), || {
        say!("Open {url}");
        if crate::output::interactive() {
            open_browser(&url);
        }
    })
}
