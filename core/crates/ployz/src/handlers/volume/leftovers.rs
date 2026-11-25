//! Copies a deleted Volume left on its Servers. Ployz Cloud deletes such copies by itself,
//! except one whose Server may hold writes no other Server has, or can't say: only a person
//! deletes that, after seeing it.

use clap::{ArgMatches, Command};
use ployz_store::OperationVerb;
use reqwest::Method;
use serde::{Deserialize, Serialize};

use super::super::store;
use super::super::{Error, leaf_matches};
use super::run::{self, OneRun, Pacing};
use crate::approval::{self, Asked};
use crate::cli::{approval as approval_arg, positional, value};
use crate::cloud::account::StoreCallError;
use crate::ui::{Cell, Table, Tone};

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Snapshot {
    name: String,
    created_unix_seconds: i64,
}

/// One copy a deleted Volume left, as Cloud lists it.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct Leftover {
    docker_volume: String,
    server: String,
    slot: String,
    newest: Snapshot,
    /// `None` when its Server's Ployz doesn't say.
    written_here: Option<bool>,
    bound_bytes: Option<u64>,
    /// What `rm --confirm` names, so it deletes this copy as shown.
    confirm: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct Leftovers {
    leftovers: Vec<Leftover>,
}

pub(crate) fn list_command(command: Command) -> Command {
    command.about(
        "List the copies deleted Volumes left on Servers that only you can delete (Ployz Cloud)",
    )
}

pub(crate) fn remove_command(command: Command) -> Command {
    command
        .about("Delete a copy a deleted Volume left on a Server (Ployz Cloud); without --confirm it only shows the copy")
        .arg(
            positional("volume", true)
                .value_name("DOCKER-VOLUME")
                .help("The deleted Volume's Docker name, as `ployz volume leftovers` lists it"),
        )
        .arg(
            value("on", None)
                .value_name("SERVER")
                .required(true)
                .help("The Server that holds the copy"),
        )
        .arg(value("confirm", None).value_name("COPY").help(
            "The CONFIRM value `ployz volume leftovers` shows for the copy",
        ))
        .arg(approval_arg())
        .arg(run::wait_flag())
}

fn writes(written_here: Option<bool>) -> Cell {
    match written_here {
        Some(true) => Cell::status("only here", Tone::Bad),
        Some(false) => Cell::from("copied"),
        None => Cell::status("unknown", Tone::Muted),
    }
}

fn size(bytes: Option<u64>) -> String {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a size shown to one decimal place"
    )]
    bytes.map_or_else(String::new, |bytes| format!("{:.1} GB", bytes as f64 / 1e9))
}

pub(crate) fn list(root: &ArgMatches) -> Result<(), Error> {
    let store = store::store(root)?;
    let (runtime, credential) = run::cloud(store.backend())?;
    let listed: Leftovers = runtime
        .block_on(crate::cloud::account::refusable_operation(
            credential,
            Method::GET,
            "volume-leftovers",
            None,
        ))
        .map_err(|error| store.fail(error))?;
    let mut table = Table::new(
        [
            "VOLUME",
            "SERVER",
            "SLOT",
            "LAST SNAPSHOT",
            "WRITES",
            "SIZE",
            "CONFIRM",
        ],
        "No copies of deleted Volumes.",
    );
    for copy in &listed.leftovers {
        table.row([
            Cell::from(&copy.docker_volume),
            Cell::from(&copy.server),
            Cell::from(&copy.slot),
            Cell::from(&copy.newest.name),
            writes(copy.written_here),
            Cell::from(size(copy.bound_bytes)),
            Cell::from(&copy.confirm),
        ]);
    }
    crate::ui::list(&listed, &table)?;
    Ok(())
}

pub(crate) fn remove(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let volume = super::super::required(matches, "volume")?;
    let server = super::super::required(matches, "on")?;
    if volume.contains(['/', '?', '#', '%']) {
        return Err(Error::usage(
            "Expected a Docker Volume name, as `ployz volume leftovers` lists it",
        ));
    }
    let confirm = matches.get_one::<String>("confirm");
    let store = store::store(root)?;
    let (runtime, credential) = run::cloud(store.backend())?;
    let body = match confirm {
        Some(confirm) => serde_json::json!({ "server": server, "confirm": confirm }),
        None => serde_json::json!({ "server": server }),
    };
    let path = format!("volume-leftovers/{volume}/rm");
    let mut words = vec![
        "volume",
        "leftovers",
        "rm",
        volume.as_str(),
        "--on",
        server.as_str(),
    ];
    if let Some(confirm) = confirm {
        words.extend(["--confirm", confirm.as_str()]);
    }
    // Cloud asks a person before it deletes the copy. Only that ask leaves the outer
    // result; every other refusal comes back inner, with the store's hints.
    let asked = approval::approved(
        runtime,
        OperationVerb::RemoveMirror,
        matches.get_one::<String>("approval").cloned(),
        |id| crate::cli::approved_rerun(matches, &words, id),
        async |approval| {
            let answer = crate::cloud::account::approved_answer::<OneRun>(
                credential,
                Method::POST,
                &path,
                &body,
                approval,
            )
            .await;
            match answer {
                Err(StoreCallError::Refused(refusal)) if Asked::of(&refusal).is_some() => {
                    Err(StoreCallError::Refused(refusal))
                }
                other => Ok(other),
            }
        },
    )?;
    let run = asked.map_err(|error| store.fail(error))?.run;
    let started = format!("delete of {volume}'s copy on {server}");
    let follow = store::next(matches, &["volume", "leftovers"]);
    if !matches.get_flag("wait") {
        return crate::ui::finish(&run::Started { run: &run }, || {
            crate::ui::stream(format_args!("Started the {started}: run {}.", run.id));
        });
    }
    crate::ui::note(format_args!(
        "Started the {started}: run {}. Waiting for it to end.",
        run.id
    ));
    let run = runtime
        .block_on(run::settle(credential, run, Pacing::default()))
        .map_err(|error| store.fail(error))?;
    run::ended(&run, &started, follow)
}
