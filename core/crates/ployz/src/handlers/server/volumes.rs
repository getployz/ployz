//! `server volumes`: the anonymous volumes a Server kept when it removed a container no
//! copy replaced, and `prune`, which deletes the ones no container uses. The Server
//! decides what it may delete; this command only picks which to ask for.

use chrono::{DateTime, Utc};
use clap::{Arg, ArgAction, ArgMatches, Command};
use ployz_core::{
    DockerVolumeName, Hint, KeptVolume, KeptVolumeRemoval, KeptVolumeRemovalOutcome,
    LIST_KEPT_VOLUMES_CAPABILITY, ListKeptVolumesRequest, MachineTarget,
    REMOVE_KEPT_VOLUMES_CAPABILITY, RemoveKeptVolumesRequest, RpcErrorCode, op,
};
use serde_json::json;

use crate::cli::{Rerun, base, positional, rerun, value};
use crate::cluster::Call;
use crate::connect::{Client, ConnectError};
use crate::handlers::shared::disk::size;
use crate::handlers::{Error, leaf_matches, route, runtime};
use crate::ui::{self, Table, printable, short_id};

pub(crate) fn command(command: Command) -> Command {
    base(
        command,
        "List the volumes a Server kept from removed containers, or delete them with prune",
    )
    .long_about(
        "When a Server removes a container that no new copy replaces, as a drain or \
         scale-down does, it keeps the container's anonymous volumes so their data survives. \
         This lists them: the Service each came from, when it was kept and its size. \
         With prune, it deletes them: every one, the ones you name, or with --older-than \
         only those kept at least that long ago. It shows what it will delete and asks first; \
         --yes skips the ask, and outside a terminal it fails with confirmation_required. \
         It never deletes a volume the Server didn't keep, or one a container uses.",
    )
    .arg(positional("server", true).help("Server name or ID"))
    .arg(
        positional("action", false)
            .value_parser(["prune"])
            .help("prune deletes the kept volumes"),
    )
    .arg(
        Arg::new("volume")
            .num_args(0..)
            .action(ArgAction::Append)
            .requires("action")
            .help("Volumes to delete, by name or the start of one; without any, every kept volume"),
    )
    .arg(
        value("older-than", None)
            .value_name("DURATION")
            .requires("action")
            .help("Delete only volumes kept at least this long ago, such as 30d or 12h"),
    )
    .arg(crate::cli::yes("Delete without asking"))
}

pub(crate) fn volumes(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let server = matches
        .get_one::<String>("server")
        .ok_or_else(|| Error::usage("server is required"))?;
    let target = MachineTarget::parse(server)?;
    let kept_by = matches
        .get_one::<String>("older-than")
        .map(|age| kept_by(age, Utc::now()))
        .transpose()?;
    let context = matches.get_one::<String>("context").map(String::as_str);
    let runtime = runtime()?;
    let (client, kept) = runtime.block_on(async {
        let client = route::connect(matches, context).await?;
        let kept = list(&client, &target, server).await?;
        Ok::<_, Error>((client, kept))
    })?;
    if matches.get_one::<String>("action").is_none() {
        return show(matches, server, &kept);
    }
    let named: Vec<&str> = matches
        .get_many::<String>("volume")
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect();
    let chosen = choose(&kept, &named, kept_by, server)
        .map_err(|error| error.hint(Hint::Next(rerun(matches, &["server", "volumes", server]))))?;
    if chosen.is_empty() {
        let why = match kept_by {
            Some(_) => format!("{server} kept no volume that long ago. Nothing deleted."),
            None => format!("{server} has no kept volumes. Nothing deleted."),
        };
        return ui::done(&json!({ "removals": [] }), printable(&why));
    }
    let described = match chosen.len() {
        1 => "1 kept volume".to_owned(),
        count => format!("{count} kept volumes"),
    };
    if !matches.get_flag("yes") {
        if !ui::json() {
            let _ = table(&chosen, "").write(&mut ui::stderr(), ui::stderr_is_terminal());
        }
        ui::confirm(
            &format!("Delete {described} and their data?"),
            || {
                Error::detailed(
                    RpcErrorCode::ConfirmationRequired,
                    format!("Deleting {described} needs confirmation. Nothing deleted."),
                    json!({ "kept_volumes": chosen }),
                )
                .hint(Rerun::again(root).switch("yes"))
            },
            "Nothing deleted.",
        )?;
    }
    let names = chosen.iter().map(|volume| volume.name.clone()).collect();
    let removals = runtime.block_on(remove(&client, &target, server, names))?;
    report(server, &removals)
}

/// The kept volumes on `server`, refused legibly when its daemon predates them.
async fn list(
    client: &Client,
    target: &MachineTarget,
    server: &str,
) -> Result<Vec<KeptVolume>, Error> {
    client
        .call_shared::<op::ListKeptVolumes>(ListKeptVolumesRequest {}, Call::once().to(target))
        .await
        .map(|kept| kept.volumes)
        .map_err(|error| {
            refused(
                error,
                server,
                LIST_KEPT_VOLUMES_CAPABILITY,
                "list its kept volumes",
                "Could not list the kept volumes on",
            )
        })
}

async fn remove(
    client: &Client,
    target: &MachineTarget,
    server: &str,
    volumes: Vec<DockerVolumeName>,
) -> Result<Vec<KeptVolumeRemoval>, Error> {
    client
        .call_shared::<op::RemoveKeptVolumes>(
            RemoveKeptVolumesRequest { volumes },
            Call::once().to(target),
        )
        .await
        .map(|removed| removed.results)
        .map_err(|error| {
            refused(
                error,
                server,
                REMOVE_KEPT_VOLUMES_CAPABILITY,
                "delete its kept volumes",
                "Could not delete the kept volumes on",
            )
        })
}

fn refused(
    error: ConnectError,
    server: &str,
    capability: &str,
    action: &str,
    failed: &str,
) -> Error {
    let error = crate::connect::rpc_error(error);
    if error.code == RpcErrorCode::Unsupported {
        return Error::detailed(
            RpcErrorCode::Unsupported,
            format!("{} needs a daemon upgrade to {action}.", printable(server)),
            json!({ "capability": capability, "server": server }),
        )
        .hint(Hint::Next("ployz server upgrade".into()));
    }
    Error::from(error).context(format!("{failed} {}.", printable(server)))
}

/// When a volume was kept at the latest to be at least `age` old at `now`. An age that is
/// zero or reaches past the calendar is refused: it would pick every volume, or none.
fn kept_by(age: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, Error> {
    crate::operator::input::go_duration(age)
        .filter(|age| !age.is_zero())
        .and_then(|age| chrono::Duration::from_std(age).ok())
        .and_then(|age| now.checked_sub_signed(age))
        .ok_or_else(|| {
            Error::usage(format!(
                "--older-than takes a duration longer than zero, such as 30d or 12h, not '{}'",
                printable(age)
            ))
        })
}

/// The kept volumes a prune deletes: those `named`, or every one, kept at or before
/// `kept_by`. A name matches exactly or as the start of one kept volume's name.
fn choose(
    kept: &[KeptVolume],
    named: &[&str],
    kept_by: Option<DateTime<Utc>>,
    server: &str,
) -> Result<Vec<KeptVolume>, Error> {
    let mut chosen: Vec<&KeptVolume> = if named.is_empty() {
        kept.iter().collect()
    } else {
        let mut chosen = Vec::new();
        for name in named {
            let exact = kept.iter().find(|volume| volume.name.as_str() == *name);
            let matching: Vec<&KeptVolume> = match exact {
                Some(volume) => vec![volume],
                None => kept
                    .iter()
                    .filter(|volume| volume.name.as_str().starts_with(name))
                    .collect(),
            };
            match matching.as_slice() {
                [volume] => chosen.push(*volume),
                [] => {
                    return Err(Error::not_found(format!(
                        "{} kept no volume named {}. Nothing deleted.",
                        printable(server),
                        printable(name)
                    )));
                }
                several => {
                    return Err(Error::detailed(
                        RpcErrorCode::Ambiguous,
                        format!(
                            "{} kept volumes start with {}. Give more of the name. Nothing deleted.",
                            several.len(),
                            printable(name)
                        ),
                        json!({ "volumes": several.iter().map(|volume| &volume.name).collect::<Vec<_>>() }),
                    ));
                }
            }
        }
        chosen
    };
    chosen.sort_by(|left, right| left.name.cmp(&right.name));
    chosen.dedup_by(|left, right| left.name == right.name);
    if let Some(kept_by) = kept_by {
        chosen.retain(|volume| volume.kept_at <= kept_by.timestamp());
    }
    chosen.sort_by_key(|volume| volume.kept_at);
    Ok(chosen.into_iter().cloned().collect())
}

fn show(matches: &ArgMatches, server: &str, kept: &[KeptVolume]) -> Result<(), Error> {
    let empty = format!("{} has no kept volumes.", printable(server));
    let table = table(kept, &empty);
    ui::finish(&json!({ "kept_volumes": kept }), || {
        ui::rows(&table);
        if !kept.is_empty() {
            ui::hint(&Hint::Next(rerun(
                matches,
                &["server", "volumes", server, "prune"],
            )));
        }
    })
}

fn table(kept: &[KeptVolume], empty: &str) -> Table {
    let now = Utc::now();
    let mut table = Table::new(["VOLUME", "SERVICE", "PATH", "KEPT", "SIZE"], empty);
    for volume in kept {
        table.row([
            printable(short_id(volume.name.as_str())),
            printable(&format!("{}/{}", volume.namespace, volume.service)),
            printable(volume.path.as_str()),
            DateTime::from_timestamp(volume.kept_at, 0)
                .map_or_else(|| "-".to_owned(), |at| crate::ui::ago(at, now)),
            volume.size_bytes.map_or_else(|| "unknown".to_owned(), size),
        ]);
    }
    table
}

/// What a prune did with each volume; a failure when any stayed.
fn report(server: &str, removals: &[KeptVolumeRemoval]) -> Result<(), Error> {
    let mut table = Table::new(["VOLUME", "RESULT"], "");
    for removal in removals {
        table.row([
            printable(short_id(removal.volume.as_str())),
            printable(&result(&removal.outcome)),
        ]);
    }
    ui::finish(&json!({ "removals": removals }), || ui::rows(&table))?;
    let deleted = removals
        .iter()
        .filter(|removal| removal.outcome == KeptVolumeRemovalOutcome::Removed)
        .count();
    if deleted == removals.len() {
        return Ok(());
    }
    let message = if deleted == 0 {
        format!("Nothing deleted on {}.", printable(server))
    } else {
        format!(
            "{} deleted {deleted} of {} kept volumes; the rest are still there.",
            printable(server),
            removals.len()
        )
    };
    Err(Error::detailed(
        RpcErrorCode::Conflict,
        message,
        json!({ "removals": removals }),
    )
    .outcome())
}

fn result(outcome: &KeptVolumeRemovalOutcome) -> String {
    match outcome {
        KeptVolumeRemovalOutcome::Removed => "deleted".into(),
        KeptVolumeRemovalOutcome::InUse { containers } if containers.is_empty() => {
            "still there: a container uses it".into()
        }
        KeptVolumeRemovalOutcome::InUse { containers } => format!(
            "still there: container {} uses it",
            containers
                .iter()
                .map(|container| short_id(container.as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        KeptVolumeRemovalOutcome::NotKept => "still there: the Server didn't keep it".into(),
        KeptVolumeRemovalOutcome::Failed { message } => format!("not deleted: {message}"),
        KeptVolumeRemovalOutcome::Unrecognized => "unknown; upgrade ployz to read it".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(name: &str, kept_at: i64) -> KeptVolume {
        KeptVolume {
            name: DockerVolumeName::parse(name).unwrap(),
            namespace: ployz_core::Namespace::parse("app").unwrap(),
            service: ployz_core::ServiceName::parse("db").unwrap(),
            container: ployz_core::ContainerId::parse("c".repeat(64)).unwrap(),
            path: ployz_core::ContainerPath::parse("/var/lib/postgresql/data").unwrap(),
            kept_at,
            size_bytes: None,
        }
    }

    fn names(chosen: &[KeptVolume]) -> Vec<&str> {
        chosen.iter().map(|volume| volume.name.as_str()).collect()
    }

    #[test]
    fn older_than_leaves_out_a_volume_kept_more_recently() {
        let now = DateTime::from_timestamp(100 * 86_400, 0).unwrap();
        let kept = [kept("aaa", 10 * 86_400), kept("bbb", 99 * 86_400)];
        let kept_by = kept_by("30d", now).unwrap();
        let chosen = choose(&kept, &[], Some(kept_by), "web-1").unwrap();
        assert_eq!(names(&chosen), ["aaa"]);
    }

    #[test]
    fn a_name_the_server_did_not_keep_deletes_nothing() {
        let kept = [kept("aaa", 0), kept("abb", 0)];
        assert_eq!(
            choose(&kept, &["aaa", "zzz"], None, "web-1")
                .unwrap_err()
                .report()
                .code,
            RpcErrorCode::NotFound
        );
        assert_eq!(
            choose(&kept, &["a"], None, "web-1")
                .unwrap_err()
                .report()
                .code,
            RpcErrorCode::Ambiguous,
            "a start two names share picks neither"
        );
        assert_eq!(
            names(&choose(&kept, &["ab"], None, "web-1").unwrap()),
            ["abb"]
        );
    }
}
