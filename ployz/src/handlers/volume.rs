use std::{
    collections::BTreeMap,
    future::Future,
    io::{self, IsTerminal, Write},
    path::Path,
};

use clap::ArgMatches;
use ployz_core::{
    CreateVolumeRequest, DockerVolumeName, MachineObservation, MachineSelector, NameMatches,
    PartialResult, RpcError, resolve_machine_selectors,
};

use crate::{
    connect::{Client, connect, rpc_error},
    context::expand_home,
    volume::{
        MachineVolume, filter_volumes, machine_volumes, parse_assignments, remove_volumes_with,
    },
};

use super::{Error, leaf_matches, string_values};

pub(super) fn create(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    // EO-011: an explicit non-empty name is required; anonymous Docker Volumes stay unsupported.
    let name = DockerVolumeName::parse(required(matches, "volume-name")?)
        .map_err(|error| error.to_string())?;
    let driver = required(matches, "driver")?;
    let options = parse_assignments(string_values(matches, "opt").iter().map(String::as_str))?;
    let labels = parse_assignments(string_values(matches, "label").iter().map(String::as_str))?;
    let selector = matches.get_one::<String>("machine").cloned();
    run(async move {
        let mut client = connect_from(root).await?;
        let machines = client
            .list_machines()
            .await
            .map_err(|error| error.to_string())?;
        let Some(machine) = select_create_machine(&machines, selector.as_deref())? else {
            println!("Cancelled. No volume was created.");
            return Ok(());
        };
        let volume = client
            .create_volume(
                &machine.machine.id,
                CreateVolumeRequest {
                    name,
                    driver,
                    options,
                    labels,
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        println!("{}\t{}", machine.machine.name, volume.id.name);
        Ok(())
    })
}

pub(super) fn list(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let selectors = string_values(matches, "machine");
    let quiet = matches.get_flag("quiet");
    run(async move {
        let mut client = connect_from(root).await?;
        let (volumes, result) = discover(&mut client, &selectors).await?;
        if quiet {
            for volume in &volumes {
                println!("{}", volume.volume.id.name);
            }
        } else {
            println!("MACHINE\tVOLUME\tDRIVER");
            for volume in &volumes {
                println!(
                    "{}\t{}\t{}",
                    volume.machine_name, volume.volume.id.name, volume.volume.driver
                );
            }
        }
        report_failures(&result);
        Ok(())
    })
}

pub(super) fn inspect(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let name = DockerVolumeName::parse(required(matches, "volume-name")?)
        .map_err(|error| error.to_string())?;
    let selectors = matches
        .get_one::<String>("machine")
        .cloned()
        .into_iter()
        .collect::<Vec<_>>();
    run(async move {
        let mut client = connect_from(root).await?;
        let (volumes, result) = discover(&mut client, &selectors).await?;
        if !result.all_targets_succeeded() {
            return Err(failure_summary(&result));
        }
        match NameMatches::from_matches(filter_volumes(&volumes, std::slice::from_ref(&name))) {
            NameMatches::None => Err(format!("Docker Volume {name:?} was not found")),
            NameMatches::One(mut volume) => {
                volume.volume = client
                    .inspect_volume(&volume.volume.id)
                    .await
                    .map_err(|error| error.to_string())?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&volume).map_err(|error| error.to_string())?
                );
                Ok(())
            }
            NameMatches::Ambiguous(volumes) => Err(format!(
                "Docker Volume {name:?} is ambiguous; select one Machine: {}",
                volumes
                    .iter()
                    .map(|volume| volume.machine_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    })
}

pub(super) fn remove(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let names = matches
        .get_many::<String>("volume-name")
        .into_iter()
        .flatten()
        .map(|name| DockerVolumeName::parse(name.clone()).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let selectors = string_values(matches, "machine");
    let force = matches.get_flag("force");
    let yes = matches.get_flag("yes");
    run(async move {
        let mut client = connect_from(root).await?;
        let (volumes, result) = discover(&mut client, &selectors).await?;
        let volumes = filter_volumes(&volumes, &names);
        if result.all_targets_succeeded()
            && let Some(name) = names
                .iter()
                .find(|name| !volumes.iter().any(|volume| &volume.volume.id.name == *name))
        {
            return Err(format!("Docker Volume {name:?} was not found"));
        }
        if volumes.is_empty() {
            return Err(failure_summary(&result));
        }
        println!("The following Docker Volumes will be removed:");
        for volume in &volumes {
            println!("  {}/{}", volume.machine_name, volume.volume.id.name);
        }
        let confirmed = yes || confirm()?;
        if !confirmed {
            println!("Cancelled. No volumes were removed.");
            return Ok(());
        }
        let removal_client = client.clone();
        let removal = remove_volumes_with(&volumes, force, move |id, force| {
            let mut client = removal_client.clone();
            async move { client.remove_volume(id, force).await.map_err(rpc_error) }
        })
        .await;
        report_failures(&result);
        match (
            (!result.all_targets_succeeded()).then(|| failure_summary(&result)),
            (!removal.all_targets_succeeded()).then(|| failure_summary(&removal)),
        ) {
            (None, None) => Ok(()),
            (Some(failure), None) | (None, Some(failure)) => Err(failure),
            (Some(discovery), Some(removal)) => Err(format!("{discovery}; {removal}")),
        }
    })
}

async fn discover(
    client: &mut Client,
    selectors: &[String],
) -> Result<
    (
        Vec<MachineVolume>,
        PartialResult<Vec<ployz_core::DockerVolume>, RpcError>,
    ),
    Error,
> {
    let machines = selected_machines(
        client
            .list_machines()
            .await
            .map_err(|error| error.to_string())?,
        selectors,
    )?;
    let result = client.list_volumes(&machines).await;
    Ok((machine_volumes(&machines, &result), result))
}

async fn connect_from(root: &ArgMatches) -> Result<Client, Error> {
    let path = root
        .get_one::<String>("ployz-config")
        .map(Path::new)
        .map(expand_home)
        .ok_or_else(|| "Ployz config path is required".to_owned())?;
    connect(
        &path,
        root.get_one::<String>("connect").map(String::as_str),
        root.get_one::<String>("context").map(String::as_str),
    )
    .await
    .map_err(|error| error.to_string())
}

fn selected_machines(
    machines: Vec<MachineObservation>,
    selectors: &[String],
) -> Result<Vec<MachineObservation>, Error> {
    if selectors.is_empty() {
        return Ok(machines);
    }
    let selectors = selectors
        .iter()
        .map(|selector| MachineSelector::parse(selector.clone()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let visible = machines
        .iter()
        .map(|observation| observation.machine.clone())
        .collect::<Vec<_>>();
    let selected =
        resolve_machine_selectors(&visible, &selectors).map_err(|error| error.to_string())?;
    let mut observations = machines
        .into_iter()
        .map(|observation| (observation.machine.id.clone(), observation))
        .collect::<BTreeMap<_, _>>();
    selected
        .into_iter()
        .map(|machine| {
            observations
                .remove(&machine.id)
                .ok_or_else(|| "selected Machine disappeared from the snapshot".into())
        })
        .collect()
}

fn select_create_machine(
    machines: &[MachineObservation],
    selector: Option<&str>,
) -> Result<Option<MachineObservation>, Error> {
    if let Some(selector) = selector {
        let selected = selected_machines(machines.to_vec(), &[selector.into()])?;
        return match selected.as_slice() {
            [machine] => Ok(Some(machine.clone())),
            _ => Err(format!(
                "Machine selector {selector:?} matched multiple Machines"
            )),
        };
    }
    match machines {
        [] => Err("no Machines are available".into()),
        [machine] => Ok(Some(machine.clone())),
        _ if !io::stdin().is_terminal() || !io::stdout().is_terminal() => {
            Err("multiple Machines are available; specify --machine".into())
        }
        _ => {
            println!("Select a Machine (blank or q cancels):");
            for (index, machine) in machines.iter().enumerate() {
                println!("  {}. {}", index + 1, machine.machine.name);
            }
            print!("> ");
            io::stdout().flush().map_err(|error| error.to_string())?;
            let mut input = String::new();
            io::stdin()
                .read_line(&mut input)
                .map_err(|error| error.to_string())?;
            let input = input.trim();
            if input.is_empty() || input.eq_ignore_ascii_case("q") {
                return Ok(None);
            }
            let index = input
                .parse::<usize>()
                .ok()
                .and_then(|value| value.checked_sub(1))
                .filter(|index| *index < machines.len())
                .ok_or_else(|| "invalid selection".to_owned())?;
            machines
                .get(index)
                .cloned()
                .map(Some)
                .ok_or_else(|| "invalid selection".to_owned())
        }
    }
}

fn report_failures<T>(result: &PartialResult<T, RpcError>) {
    for failure in &result.failures {
        eprintln!("{}: {}", failure.machine_id, failure.error.message);
    }
    for machine_id in &result.omissions {
        eprintln!("{machine_id}: no terminal response");
    }
}

fn failure_summary<T>(result: &PartialResult<T, RpcError>) -> String {
    let failures = result
        .failures
        .iter()
        .map(|failure| format!("{}: {}", failure.machine_id, failure.error.message))
        .chain(
            result
                .omissions
                .iter()
                .map(|machine_id| format!("{machine_id}: no terminal response")),
        )
        .collect::<Vec<_>>()
        .join("; ");
    format!("one or more Machines failed: {failures}")
}

fn confirm() -> Result<bool, Error> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("confirmation requires a terminal; pass --yes to continue".into());
    }
    print!("Continue? [y/N] ");
    io::stdout().flush().map_err(|error| error.to_string())?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|error| error.to_string())?;
    Ok(matches!(input.trim(), "y" | "Y" | "yes" | "YES"))
}

fn required(matches: &ArgMatches, name: &str) -> Result<String, Error> {
    matches
        .get_one::<String>(name)
        .cloned()
        .ok_or_else(|| format!("{name} is required"))
}

fn run(future: impl Future<Output = Result<(), Error>>) -> Result<(), Error> {
    tokio::runtime::Runtime::new()
        .map_err(|error| error.to_string())?
        .block_on(future)
}
