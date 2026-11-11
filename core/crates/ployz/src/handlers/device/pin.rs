//! `ployz device pin`: keep the Cluster's Sealing public key as a Server reports it over
//! the local socket or SSH, so a later seal through Cloud can check the key Cloud sends.

use std::path::Path;

use clap::{ArgMatches, Command};
use ployz_core::SealingPublicKey;
use serde::Serialize;

use super::super::{Error, config_path, leaf_matches, recovery, runtime, server};
use crate::cli::base;
use crate::cloud_account::{self, Credential};
use crate::cloud_login::{CredentialStore, Organization};
use crate::connect::Client;
use crate::sealing_pin::{Pin, Pins, SealTo, entry_pin};
use crate::ui::{self, Fields, Hint};

pub(crate) fn pin_command(command: Command) -> Command {
    base(command, "Pin the Cluster's Sealing Key on this computer")
        .long_about(
            "Reads the Sealing public key from a Server over SSH or its local socket, never \
             through Cloud, and keeps it for the Organization you're signed in to. \
             `ployz private-access on` and sealed variables then seal only to that key, and \
             refuse a different key from Ployz Cloud. Run it again if your Cluster's key \
             changes.",
        )
        .after_help(concat!(
            "Examples:\n",
            "  ployz device pin --connect ssh://root@203.0.113.10\n",
            "  sudo ployz device pin"
        ))
}

#[derive(Serialize)]
struct Pinned<'a> {
    organization: &'a str,
    server: String,
    public_key: SealingPublicKey,
    /// The key it replaced, when another was pinned.
    replaced: Option<SealingPublicKey>,
}

pub(crate) fn pin(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let runtime = runtime()?;
    let Some(credential) = server::signed_in(&runtime, matches)? else {
        return Err(Error::coded(
            ployz_core::RpcErrorCode::Unauthenticated,
            "Sign in first: the key is pinned for the Organization you're signed in to.",
        )
        .hint(Hint::Next("ployz login".to_owned())));
    };
    let pins = Pins::beside(&config_path(matches)?);
    let (organization, pin, replaced) = runtime.block_on(async {
        let mut client = recovery::dial(matches, "ployz device pin").await?;
        pin_through(&mut client, &credential, &pins).await
    })?;
    let pinned = Pinned {
        organization: &organization.slug,
        server: pin.server.to_string(),
        public_key: pin.public_key,
        replaced,
    };
    let mut record = Fields::new()
        .field("Organization", pinned.organization)
        .field("Server", &pinned.server)
        .field("Sealing Key", pinned.public_key.to_string());
    if let Some(old) = replaced {
        record = record.field("Replaced", old.to_string());
    }
    ui::fields(&serde_json::json!({ "pin": pinned }), &record)
}

/// Pin the key `client`'s entry reports for the Organization `credential` acts in,
/// answering the other key it replaced, if any.
///
/// # Errors
///
/// Refuses a connection Cloud chose or carries, or plain TCP, and a Server whose key
/// isn't the one Cloud backs up for the Organization. Nothing is pinned then.
async fn pin_through(
    client: &mut Client,
    credential: &Credential,
    pins: &Pins,
) -> Result<(Organization, Pin, Option<SealingPublicKey>), Error> {
    if !SealTo::direct(client) {
        return Err(Error::usage(
            "ployz device pin reads the key over SSH or the local socket only. Run it on the \
             Server, or pass --connect ssh://user@host.",
        ));
    }
    let pin = entry_pin(client).await?;
    let organization = cloud_account::acting_in(credential).await?;
    if let Some(row) = recovery::recovery_row(credential, &organization.id).await?
        && row.public_key != pin.public_key
    {
        return Err(Error::conflict(format!(
            "Server {} holds a Sealing Key other than the one Ployz Cloud has for {}, so \
             nothing was pinned. Pin from a Server in this Organization.",
            pin.server, organization.slug
        ))
        .hint(Hint::Inspect("ployz ctx".to_owned())));
    }
    let replaced = pins
        .set(&organization.id, pin.clone())?
        .map(|old| old.public_key)
        .filter(|old| *old != pin.public_key);
    Ok((organization, pin, replaced))
}

/// What a seal through `client` checks its key against: the entry's own report over a
/// connection the person chose, else this computer's pin for the Organization the
/// sign-in acts in.
///
/// # Errors
///
/// Fails when Cloud can't say which Organization the sign-in acts in, or the pins can't
/// be read.
pub(in crate::handlers) async fn seal_to(config: &Path, client: &Client) -> Result<SealTo, Error> {
    SealTo::of(client, async || {
        let credential = cloud_account::from_env(&CredentialStore::beside(config)).await?;
        let organization = cloud_account::acting_in(&credential).await?;
        Ok(Pins::beside(config).get(&organization.id)?)
    })
    .await
}

#[cfg(test)]
#[path = "pin_tests.rs"]
mod tests;
