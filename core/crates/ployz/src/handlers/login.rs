//! `ployz login` and `ployz logout`: this device's Cloud sign-in. Neither needs a Server.

use clap::{ArgMatches, Command};
use serde::Serialize;

use super::{Error, config_path, leaf_matches, runtime};
use crate::cli::{env, switch, value};
use crate::cloud_login::{
    self, Account, CredentialStore, DEFAULT_CLOUD, Organization, Pending, SignedIn, Start,
};

pub(crate) fn login_command() -> Command {
    Command::new("login")
        .about("Sign this device in to Ployz Cloud")
        .long_about("Sign this device in to Ployz Cloud. Opens the approval page and waits; with --json, prints the page and code at once unless --wait is given. Run again, or with --wait, to finish a pending sign-in.")
        .arg(
            value("cloud-url", None)
                .env(env::CLOUD_URL)
                .help("Cloud to sign in to [default: the pending or signed-in Cloud, else ployz.dev]"),
        )
        .arg(switch("wait", None).help("Wait for approval, also with --json"))
}

pub(crate) fn logout_command() -> Command {
    Command::new("logout").about("End this device's Ployz Cloud sign-in")
}

/// Signed-in result: who and where, never the bearer.
#[derive(Serialize)]
struct SignInReport<'a> {
    status: &'static str,
    cloud: &'a str,
    account: &'a Account,
    organization: &'a Organization,
}

impl<'a> SignInReport<'a> {
    fn of(signed_in: &'a SignedIn) -> Self {
        Self {
            status: "signed_in",
            cloud: &signed_in.cloud,
            account: &signed_in.account,
            organization: &signed_in.organization,
        }
    }
}

#[derive(Serialize)]
struct PendingReport<'a> {
    status: &'static str,
    url: &'a str,
    code: &'a str,
    expires_in: u64,
    next: &'static str,
}

pub(super) fn login(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let store = CredentialStore::beside(&config_path(matches)?);
    let cloud = match matches.get_one::<String>("cloud-url") {
        Some(cloud) => cloud.clone(),
        None => store.cloud()?.unwrap_or_else(|| DEFAULT_CLOUD.to_owned()),
    };
    let wait = matches.get_flag("wait") || !crate::ui::json();
    runtime()?.block_on(async {
        let (pending, resumed) = match cloud_login::start(&store, &cloud).await? {
            Start::SignedIn(signed_in) => return report(&signed_in),
            Start::Pending { pending, resumed } => (pending, resumed),
        };
        if !wait {
            return crate::ui::emit(&PendingReport {
                status: "pending",
                url: &pending.url,
                code: &pending.code,
                expires_in: pending.expires_in(),
                next: "ployz login --wait",
            });
        }
        announce(&pending, resumed);
        let signed_in = cloud_login::wait(&store, pending).await?;
        report(&signed_in)
    })
}

fn announce(pending: &Pending, resumed: bool) {
    crate::ui::note(format_args!(
        "Open {} and confirm the code {}.",
        pending.url, pending.code
    ));
    // Only a person at a terminal gets a browser; agents show the URL themselves.
    if !resumed && crate::ui::interactive() {
        open_browser(&pending.url);
    }
    crate::ui::note_inline(format_args!("Waiting for approval... "));
}

fn report(signed_in: &SignedIn) -> Result<(), Error> {
    crate::ui::finish(&SignInReport::of(signed_in), || {
        crate::ui::stream(format_args!(
            "Signed in to {} as {} in Organization {}.",
            signed_in.cloud, signed_in.account.email, signed_in.organization.slug
        ));
    })
}

pub(super) fn logout(root: &ArgMatches) -> Result<(), Error> {
    let store = CredentialStore::beside(&config_path(leaf_matches(root))?);
    let out = runtime()?.block_on(cloud_login::logout(&store))?;
    let next = out.as_ref().and_then(|out| {
        let device = out.device.as_deref()?;
        // Run from another signed-in device or with PLOYZ_TOKEN: this one is signed out.
        super::account::retry_clears(&out.servers, device)
    });
    let report = LogoutReport {
        signed_out: out.is_some(),
        cloud: out.as_ref().map(|out| out.cloud.as_str()),
        device: out.as_ref().and_then(|out| out.device.as_deref()),
        servers: out.as_ref().map(|out| &out.servers),
        next: next.as_deref(),
    };
    crate::ui::finish(&report, || match &out {
        Some(out) => {
            crate::ui::stream(format_args!("Signed out of {}.", out.cloud));
            super::account::say_clears(&out.servers, next.as_deref());
        }
        None => crate::ui::stream(format_args!("Not signed in.")),
    })?;
    out.as_ref()
        .map_or(Ok(()), |out| super::account::unconfirmed(&out.servers))
}

#[derive(serde::Serialize)]
struct LogoutReport<'a> {
    signed_out: bool,
    cloud: Option<&'a str>,
    device: Option<&'a str>,
    servers: Option<&'a crate::cloud_account::ServerClears>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<&'a str>,
}

pub(super) fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    // Best effort: the URL is already printed.
    let _ = std::process::Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
