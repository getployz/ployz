//! `ployz volume trash`: list, restore and empty one Server's Trash. It reaches that Server's
//! own socket, on the Server or over `--connect ssh://`, never Cloud, so it needs no approval:
//! the socket is Full. Cloud restores or empties only with a person's passkey approval of the
//! entries on that Server, which the Server checks itself (DESIGN.md bet 7).

use chrono::DateTime;
use clap::{ArgAction, ArgMatches, Command};
use ployz_core::{
    DockerVolumeName, EmptyTrashRequest, Hint, ListTrashRequest, MachineId, MachineLayout,
    PoolRead, RestoreTrashRequest, RpcErrorCode, RuntimeWatchFrame, TrashEmptied, TrashEntry,
    TrashId, TrashList, TrashPlace, TrashRestore, TrashRestored, UnreadableTrash, op,
};
use ployz_store::{NamespacesQuery, OwnedNamespace};
use serde_json::json;

use super::restore::{Seen, answered, copies};
use crate::cli::{Rerun, base, config_path, positional};
use crate::cluster::{Call, Deadline};
use crate::connect::{Client, TARGET_RPC_TIMEOUT};
use crate::handlers::shared::disk::size;
use crate::handlers::{Error, leaf_matches, runtime};
use crate::ui::{Cell, Table};

pub(in crate::handlers) fn group(command: Command) -> Command {
    base(command, "List, restore and empty a Server's Trash")
}

pub(in crate::handlers) fn list_command(command: Command) -> Command {
    base(
        command,
        "List the deleted Volume data a Server keeps for 7 days",
    )
}

pub(in crate::handlers) fn restore_command(command: Command) -> Command {
    base(
        command,
        "Put deleted Volume data back from a Server's Trash",
    )
    .arg(id_arg(true).help("Trash entry ID, from ployz volume trash ls"))
}

pub(in crate::handlers) fn empty_command(command: Command) -> Command {
    base(command, "Delete data in a Server's Trash for good")
        .arg(
            id_arg(false)
                .num_args(0..)
                .action(ArgAction::Append)
                .help("Trash entry IDs; with none, every entry"),
        )
        .arg(
            clap::Arg::new("keep-restoring")
                .long("keep-restoring")
                .action(ArgAction::SetTrue)
                .help("Keep entries being restored, even when named"),
        )
}

fn id_arg(required: bool) -> clap::Arg {
    positional("id", required)
        .value_name("ID")
        .value_parser(|id: &str| TrashId::parse(id).map_err(|error| error.to_string()))
}

/// One Server's socket and its name for every line.
struct Server {
    client: Client,
    id: MachineId,
    name: String,
}

pub(in crate::handlers) fn list(root: &ArgMatches) -> Result<(), Error> {
    let (server, trash) = runtime()?.block_on(async {
        let server = connect(root).await?;
        let trash = read(&server).await?;
        Ok::<_, Error>((server, trash))
    })?;
    let mut table = Table::new(
        [
            "SERVER", "ID", "VOLUME", "COPY", "SIZE", "DELETED", "EXPIRES",
        ],
        if trash.unreadable.is_empty() {
            format!("{}'s Trash is empty.", server.name)
        } else {
            format!("{}'s Trash holds nothing Ployz can read.", server.name)
        },
    );
    for entry in &trash.entries {
        table.row([
            Cell::from(server.name.clone()),
            Cell::from(entry.id.to_string()),
            Cell::from(entry.name.to_string()),
            Cell::from(copy(entry)),
            Cell::from(size(entry.used_bytes)),
            Cell::from(date(entry.trashed_at)),
            Cell::from(date(entry.expires_at)),
        ]);
    }
    // Warned before the result, so `--json` lists them under `warnings`.
    let unread = unreadable(&server.name, &trash.unreadable);
    crate::ui::list(&json!({ "server": server.name, "trash": trash }), &table)?;
    unread
}

pub(in crate::handlers) fn restore(root: &ArgMatches) -> Result<(), Error> {
    let id = *leaf_matches(root)
        .get_one::<TrashId>("id")
        .ok_or_else(|| Error::usage("ID is required"))?;
    let (server, restored) = runtime()?.block_on(async {
        let server = connect(root).await?;
        let first = restore_call(&server, id, false).await?;
        let restored = match first.state {
            // A deleted root waits, pinned, until every Server shows it holds no copy.
            TrashRestore::Pinned { .. } => {
                // The members asked are the ones after the pin: a Server that joined before
                // it, holding its own entry, reads this pin or is read here.
                let frame = server.client.runtime_frame().await?;
                let seen = copies(&server.client, &frame, &first.name, true).await;
                proven(&first.name, id, server.id, &frame, &seen).map_err(|refused| {
                    refused.context(format!(
                        "Didn't restore {id}. It stays in {}'s Trash as restoring for 7 more \
                         days.",
                        server.name
                    ))
                })?;
                restore_call(&server, id, true)
                    .await
                    .map_err(|error| uncertain(root, &server, id, error))?
            }
            TrashRestore::Landed | TrashRestore::Restored { .. } => first,
        };
        Ok::<_, Error>((server, restored))
    })?;
    let (note, next) = match restored.place {
        TrashPlace::Root => volume_next(root, &restored.name),
        TrashPlace::Slot | TrashPlace::Copy => (None, Vec::new()),
    };
    crate::ui::finish(
        &json!({ "server": server.name, "restored": restored, "next": next }),
        || {
            crate::ui::stream(match restored.place {
                TrashPlace::Root => format!(
                    "Restored {} on {}. No service mounts it yet.",
                    restored.name, server.name
                ),
                TrashPlace::Slot | TrashPlace::Copy => format!(
                    "Restored {} on {} as an old copy.",
                    restored.name, server.name
                ),
            });
            if let Some(note) = &note {
                crate::ui::note(note);
            }
            for next in &next {
                crate::ui::hint(&Hint::Next(next.clone()));
            }
        },
    )
}

async fn restore_call(server: &Server, id: TrashId, promote: bool) -> Result<TrashRestored, Error> {
    server
        .client
        .call_shared::<op::RestoreTrash>(
            RestoreTrashRequest {
                id,
                promote,
                approval: None,
            },
            once(),
        )
        .await
        .map_err(|error| {
            let error = crate::connect::rpc_error(error);
            if error.code == RpcErrorCode::NotFound {
                Error::not_found(format!(
                    "{name}'s Trash holds no {id}, and no Volume on {name} came from that entry.",
                    name = server.name
                ))
                .hint(Hint::Inspect("ployz volume ls".to_owned()))
            } else {
                error.into()
            }
        })
}

/// A promote that failed without the Server refusing it may have committed, so it says the
/// same command finishes it: a rerun takes up a restore that happened and makes none twice.
fn uncertain(root: &ArgMatches, server: &Server, id: TrashId, error: Error) -> Error {
    match error.report().code {
        RpcErrorCode::Conflict
        | RpcErrorCode::NotFound
        | RpcErrorCode::InvalidArgument
        | RpcErrorCode::Unsupported
        | RpcErrorCode::Unauthenticated
        | RpcErrorCode::ConfirmationRequired
        | RpcErrorCode::Ambiguous => error,
        RpcErrorCode::Unavailable | RpcErrorCode::Internal | RpcErrorCode::Unknown(_) => error
            .context(format!(
                "{} didn't confirm restoring {id}, so it may have happened. Run this again to \
                 finish it; a second run changes nothing a first one did.",
                server.name
            ))
            .hint(Rerun::again(root).sudo_as_invoked()),
    }
}

/// After a deleted Volume comes back: the deployment that mounts it, the discard that keeps
/// the next deploy from deleting it again, and the save that keeps an automatic deploy from
/// doing so, in the Project and Environment that own it when a Store says which.
fn volume_next(root: &ArgMatches, name: &DockerVolumeName) -> (Option<String>, Vec<String>) {
    let owner = owner(root, name);
    let (scope, where_) = match &owner {
        Some(owned) => (
            format!(
                " --project {} --env {}",
                shell_words::quote(owned.project.as_str()),
                shell_words::quote(owned.environment.as_str())
            ),
            format!("In {}/{}", owned.project, owned.environment),
        ),
        None => (
            " --project PROJECT --env ENV".to_owned(),
            format!(
                "In the environment whose Namespace is {}",
                name.as_str()
                    .split_once('_')
                    .map_or(name.as_str(), |(namespace, _)| namespace)
            ),
        ),
    };
    let note = format!(
        "{where_}, redeploy a deployment from before the removal, then discard the removal. \
         Review it with `ployz diff` and save it before any automatic deploy runs, or that \
         deploy deletes the Volume again."
    );
    let next = vec![
        format!("ployz deployment redeploy DEPLOYMENT{scope}"),
        format!("ployz discard volumes.VOLUME{scope}"),
        format!("ployz deploy --skip-deploy{scope}"),
    ];
    (Some(note), next)
}

/// The Project and Environment whose Namespace `name` sits in, when this CLI reaches a Store.
fn owner(root: &ArgMatches, name: &DockerVolumeName) -> Option<OwnedNamespace> {
    let (namespace, _) = name.as_str().split_once('_')?;
    let store = crate::handlers::store::reachable(root).ok().flatten()?;
    store
        .try_read(&NamespacesQuery {})
        .ok()?
        .namespaces
        .into_iter()
        .find(|owned| owned.namespace.as_str() == namespace)
}

/// The IDs `trash empty` names.
fn named(root: &ArgMatches) -> Vec<TrashId> {
    leaf_matches(root)
        .get_many::<TrashId>("id")
        .into_iter()
        .flatten()
        .copied()
        .collect()
}

/// What `trash empty` sends for `ids`. With no IDs named, or with `--keep-restoring`, the
/// Server keeps any of them a restore pinned, judged under its lock.
fn empty_request(root: &ArgMatches, ids: Vec<TrashId>) -> EmptyTrashRequest {
    EmptyTrashRequest {
        bulk: named(root).is_empty() || leaf_matches(root).get_flag("keep-restoring"),
        ids,
        approval: None,
    }
}

/// What a refused `trash empty` with no IDs offers to run, rebuilt from what it parsed:
/// the entries it would have deleted, still keeping any a restore pins before the retry
/// reaches the Server.
fn empty_retry(root: &ArgMatches, ids: &[TrashId]) -> Rerun {
    let ids: Vec<String> = ids.iter().map(ToString::to_string).collect();
    let args: Vec<&str> = ["volume", "trash", "empty", "--keep-restoring"]
        .into_iter()
        .chain(ids.iter().map(String::as_str))
        .collect();
    Rerun::then(root, &args).sudo_as_invoked()
}

pub(in crate::handlers) fn empty(root: &ArgMatches) -> Result<(), Error> {
    let named = named(root);
    let runtime = runtime()?;
    let (server, ids, kept, restoring) = runtime.block_on(async {
        let server = connect(root).await?;
        // Emptying everything leaves entries a restore pinned, so it names only the others.
        let (ids, kept, restoring) = if named.is_empty() {
            let trash = read(&server).await?;
            let (restoring, ids): (Vec<&TrashEntry>, Vec<&TrashEntry>) =
                trash.entries.iter().partition(|entry| entry.restoring);
            let id = |entry: &&TrashEntry| entry.id;
            (
                ids.iter().map(id).collect(),
                trash.unreadable,
                restoring.iter().map(id).collect(),
            )
        } else {
            (named.clone(), Vec::new(), Vec::new())
        };
        Ok::<_, Error>((server, ids, kept, restoring))
    })?;
    let unread = unreadable(&server.name, &kept);
    let nothing = || {
        crate::ui::finish(
            &json!({
                "server": server.name,
                "emptied": TrashEmptied {
                    restoring: restoring.clone(),
                    ..TrashEmptied::default()
                },
            }),
            || {
                crate::ui::stream(if kept.is_empty() && restoring.is_empty() {
                    format!("{}'s Trash is empty.", server.name)
                } else {
                    format!("Deleted nothing in {}'s Trash.", server.name)
                });
                for id in &restoring {
                    crate::ui::note(format_args!(
                        "{id} is being restored, so it stays in {}'s Trash.",
                        server.name
                    ));
                }
            },
        )
    };
    if ids.is_empty() {
        nothing()?;
        return unread;
    }
    if named.is_empty()
        && let Err(refused) = confirm_all(&server.name, empty_retry(root, &ids), ids.len())
    {
        // With unreadable entries left, the result says nothing was deleted and the
        // refusal, with its retry naming the readable entries, ends it partial.
        if !kept.is_empty() {
            nothing()?;
        }
        return Err(refused);
    }
    let mut emptied = runtime.block_on(async {
        server
            .client
            .call_shared::<op::EmptyTrash>(empty_request(root, ids), once())
            .await
            .map_err(crate::connect::rpc_error)
    })?;
    // The entries left out of the request stay too, beside any the Server found pinned.
    emptied.restoring.extend(restoring);
    crate::ui::finish(
        &json!({ "server": server.name, "emptied": emptied }),
        || {
            crate::ui::stream(if emptied.emptied.is_empty() {
                format!("Deleted nothing in {}'s Trash.", server.name)
            } else {
                format!(
                    "Deleted {} from {}'s Trash for good.",
                    entries(emptied.emptied.len()),
                    server.name
                )
            });
            for id in &emptied.missing {
                crate::ui::note(format_args!("{id} isn't in {}'s Trash.", server.name));
            }
            for id in &emptied.restoring {
                crate::ui::note(format_args!(
                    "{id} is being restored, so it stays in {}'s Trash.",
                    server.name
                ));
            }
            for failed in &emptied.failed {
                crate::ui::warn(format!("{} wasn't deleted: {}", failed.id, failed.reason));
            }
        },
    )?;
    unread?;
    if emptied.failed.is_empty() {
        Ok(())
    } else {
        Err(Error::partial())
    }
}

/// Warns once for each entry whose marks don't read, with the command that deletes it on
/// `server`, and exits 3 when there is any: no `ployz` command deletes or restores one.
fn unreadable(server: &str, kept: &[UnreadableTrash]) -> Result<(), Error> {
    for entry in kept {
        crate::ui::warn(format!(
            "{} on {server} ({}) can't be read, so the Trash keeps it: {}. To delete it, on \
             {server}: sudo zfs destroy -r \"$(zfs list -H -o name | grep '/ployz-trash/{}$')\"",
            entry.id,
            size(entry.used_bytes),
            entry.reason,
            entry.id
        ));
    }
    if kept.is_empty() {
        Ok(())
    } else {
        Err(Error::partial())
    }
}

/// Ask before emptying every entry; `--json` and a script get `retry`, which names each one.
fn confirm_all(server: &str, retry: Rerun, count: usize) -> Result<(), Error> {
    crate::ui::confirm(
        &format!("Delete {} in {server}'s Trash for good?", entries(count)),
        || {
            Error::coded(
                RpcErrorCode::ConfirmationRequired,
                "Emptying the Trash needs confirmation. Nothing deleted.",
            )
            .hint(retry)
        },
        "Nothing deleted.",
    )
}

/// Refuses unless every member of `frame` answered, the frame lists this Server `here`, and
/// each member proved from the Pool it knows is its own, healthy, that it holds no copy of
/// `name` and no entry of it a restore pinned, besides `id` here: the pinned root then
/// can't become a second writer. An answer without that proof, from a Ployz that doesn't
/// know how, proves nothing.
pub(crate) fn proven(
    name: &DockerVolumeName,
    id: TrashId,
    here: MachineId,
    frame: &RuntimeWatchFrame,
    seen: &[Seen],
) -> Result<(), Error> {
    answered(name, seen)?;
    let members_answered = frame
        .machines
        .iter()
        .all(|observed| seen.iter().any(|seen| seen.id == observed.machine.id));
    let listed = frame
        .machines
        .iter()
        .any(|observed| observed.machine.id == here);
    if !members_answered || !listed || !frame.incomplete_ids.machines.is_empty() {
        return Err(Error::unavailable(
            "Restore needs every Server of the Cluster to answer, and they aren't all known yet. \
             Try again.",
        ));
    }
    let holding: Vec<&str> = seen
        .iter()
        .filter(|seen| matches!(seen.copy, Ok(Some(_))))
        .map(|seen| seen.name.as_str())
        .collect();
    if !holding.is_empty() {
        return Err(Error::conflict(format!(
            "Volume {name} has a copy on {}, so this data would come back as a second copy.",
            holding.join(", ")
        )));
    }
    for seen in seen {
        let unproven = |why: &str| {
            Err(Error::unavailable(format!(
                "{} can't show that it holds no copy of {name}, since {why}.",
                seen.name
            )))
        };
        let older = || {
            Err(Error::unavailable(format!(
                "{} runs a Ployz too old to show that it holds no copy of {name}. Upgrade Ployz \
                 there first.",
                seen.name
            )))
        };
        match &seen.read {
            None | Some(PoolRead::Unknown) => return older(),
            Some(PoolRead::Unproven { why }) => return unproven(why),
            Some(PoolRead::Pool {
                name: pool, health, ..
            }) if health != "ONLINE" => {
                return unproven(&format!("its Pool {pool} is {health}"));
            }
            Some(PoolRead::Pool { .. } | PoolRead::Unheld) => {}
        }
        let Some(pinned) = &seen.pinned else {
            return older();
        };
        if let Some(other) = pinned
            .iter()
            .find(|pinned| !(seen.id == here && **pinned == id))
        {
            return Err(Error::conflict(format!(
                "Volume {name} is also being restored on {} from Trash entry {other}. Pick the \
                 one to keep, and delete the other with `sudo ployz volume trash empty ID` on \
                 its Server.",
                seen.name
            )));
        }
        // This Server must still show the pin it just took, in the Pool it proves from: an
        // answer from another Pool, or none, says nothing about the one the promote lands in.
        if seen.id == here
            && (!matches!(seen.read, Some(PoolRead::Pool { .. })) || pinned.as_slice() != [id])
        {
            return Err(Error::unavailable(format!(
                "{} no longer shows {id} pinned in its own Pool. Run the restore again.",
                seen.name
            )));
        }
    }
    Ok(())
}

/// This Server's own socket: `--connect`, `--context`, the current context or the local
/// daemon, never Cloud. A context listing several Servers is refused rather than raced,
/// since each keeps its own Trash.
async fn connect(root: &ArgMatches) -> Result<Server, Error> {
    let matches = leaf_matches(root);
    let selected = crate::connect::resolve_connections(
        &config_path(matches)?,
        crate::cli::connection(matches),
        matches.get_one::<String>("context").map(String::as_str),
        &MachineLayout::production().socket(),
    )?;
    if selected.connections.len() > 1 {
        return Err(Error::usage(format!(
            "This context reaches {} Servers, and each has its own Trash. Pick one.",
            selected.connections.len()
        ))
        .hint(
            Rerun::again(root)
                .placeholder("connect", "CONNECT")
                .sudo_as_invoked(),
        ));
    }
    let client =
        crate::connect::connect_selected_with(selected, crate::cli::connector(matches)).await?;
    let id = client.entry_id().await?;
    let frame = client.runtime_frame().await?;
    let name = frame
        .machines
        .iter()
        .find(|observed| observed.machine.id == id)
        .map_or_else(
            || id.to_string(),
            |observed| observed.machine.name.to_string(),
        );
    Ok(Server { client, id, name })
}

async fn read(server: &Server) -> Result<TrashList, Error> {
    server
        .client
        .call_shared::<op::ListTrash>(ListTrashRequest {}, once())
        .await
        .map_err(|error| {
            let error = Error::from(crate::connect::rpc_error(error));
            if error.report().code == RpcErrorCode::Unsupported {
                error.context(format!(
                    "{} runs a Ployz with no Trash, so it deletes Volume data at once. Upgrade \
                     it to keep deleted data for 7 days.",
                    server.name
                ))
            } else {
                error
            }
        })
}

fn once() -> crate::cluster::Once<'static> {
    Call::once().within(Deadline::Target(TARGET_RPC_TIMEOUT))
}

fn copy(entry: &TrashEntry) -> &'static str {
    match (entry.place, entry.restoring) {
        (TrashPlace::Root, false) => "volume",
        (TrashPlace::Root, true) => "volume (restoring)",
        (TrashPlace::Slot, _) => "mirror or old copy",
        (TrashPlace::Copy, _) => "mirror's old data",
    }
}

fn date(unix_seconds: i64) -> String {
    DateTime::from_timestamp(unix_seconds, 0).map_or_else(
        || unix_seconds.to_string(),
        |at| at.format("%Y-%m-%d %H:%M UTC").to_string(),
    )
}

fn entries(count: usize) -> String {
    if count == 1 {
        "1 entry".to_owned()
    } else {
        format!("{count} entries")
    }
}

#[cfg(test)]
#[path = "trash_tests.rs"]
mod tests;
