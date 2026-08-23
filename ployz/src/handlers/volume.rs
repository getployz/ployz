use std::{
    collections::BTreeMap,
    io::{self, IsTerminal, Write},
};

use clap::ArgMatches;
use ployz_core::{
    CreateVolumeRequest, DockerVolumeName, FanoutSelector, InspectVolumeRequest,
    ListMachinesRequest, MachineObservation, MachineTarget, NameMatches, PartialResult,
    RemoveVolumeRequest, RpcError, op, resolve_machine_selectors,
};

use crate::{
    connect::{Client, TARGET_RPC_TIMEOUT},
    volume::{
        MachineVolume, filter_volumes, machine_volumes, parse_assignments, remove_volumes_with,
    },
};

use super::{Error, confirm, leaf_matches, required, string_values, with_client};

pub(super) fn create(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    // EO-011: an explicit non-empty name is required; anonymous Docker Volumes stay unsupported.
    let name = DockerVolumeName::parse(required(matches, "volume-name")?)?;
    let driver = required(matches, "driver")?;
    let options = parse_assignments(string_values(matches, "opt").iter().map(String::as_str))?;
    let labels = parse_assignments(string_values(matches, "label").iter().map(String::as_str))?;
    let selector = matches.get_one::<String>("machine").cloned();
    with_client(root, |client| {
        Box::pin(async move {
            let machines = client
                .call::<op::ListMachines>(ListMachinesRequest {}, None)
                .await?;
            let Some(machine) = select_create_machine(&machines.machines, selector.as_deref())?
            else {
                println!("Cancelled. No volume was created.");
                return Ok(());
            };
            let volume = client
                .invoke::<op::CreateVolume>(
                    CreateVolumeRequest {
                        name,
                        driver,
                        options,
                        labels,
                    },
                    &MachineTarget::from(&machine.machine.id),
                    Some(TARGET_RPC_TIMEOUT),
                )
                .await?;
            println!("{}\t{}", machine.machine.name, volume.id.name);
            Ok(())
        })
    })
}

pub(super) fn list(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let selectors = string_values(matches, "machine");
    let quiet = matches.get_flag("quiet");
    let json = matches.get_one::<String>("output").map(String::as_str) == Some("json");
    with_client(root, |client| {
        Box::pin(async move {
            let (volumes, result) = discover(client, &selectors).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&volumes)?);
            } else if quiet {
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
    })
}

pub(super) fn inspect(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let name = DockerVolumeName::parse(required(matches, "volume-name")?)?;
    let selectors = matches
        .get_one::<String>("machine")
        .cloned()
        .into_iter()
        .collect::<Vec<_>>();
    with_client(root, |client| {
        Box::pin(async move {
            let (volumes, result) = discover(client, &selectors).await?;
            if !result.all_targets_succeeded() {
                return Err(Error::usage(failure_summary(&result)));
            }
            match NameMatches::from_matches(filter_volumes(&volumes, std::slice::from_ref(&name))) {
                NameMatches::None => Err(Error::usage(format!(
                    "Docker Volume {name:?} was not found"
                ))),
                NameMatches::One(mut volume) => {
                    volume.volume = client
                        .invoke::<op::InspectVolume>(
                            InspectVolumeRequest {
                                name: volume.volume.id.name.clone(),
                            },
                            &MachineTarget::from(&volume.volume.id.machine_id),
                            Some(TARGET_RPC_TIMEOUT),
                        )
                        .await?;
                    println!("{}", serde_json::to_string_pretty(&volume)?);
                    Ok(())
                }
                NameMatches::Ambiguous(volumes) => Err(Error::usage(format!(
                    "Docker Volume {name:?} is ambiguous; select one Machine: {}",
                    volumes
                        .iter()
                        .map(|volume| volume.machine_name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))),
            }
        })
    })
}

pub(super) fn remove(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let names = matches
        .get_many::<String>("volume-name")
        .into_iter()
        .flatten()
        .map(|name| DockerVolumeName::parse(name.clone()))
        .collect::<Result<Vec<_>, _>>()?;
    let selectors = string_values(matches, "machine");
    let force = matches.get_flag("force");
    let yes = matches.get_flag("yes");
    with_client(root, |client| {
        Box::pin(async move {
            let (volumes, result) = discover(client, &selectors).await?;
            let volumes = filter_volumes(&volumes, &names);
            if result.all_targets_succeeded()
                && let Some(name) = names
                    .iter()
                    .find(|name| !volumes.iter().any(|volume| &volume.volume.id.name == *name))
            {
                return Err(Error::usage(format!(
                    "Docker Volume {name:?} was not found"
                )));
            }
            report_partial_removal_discovery(&result);
            if volumes.is_empty() {
                return Err(Error::usage(failure_summary(&result)));
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
                let client = removal_client.clone();
                async move {
                    client
                        .invoke::<op::RemoveVolume>(
                            RemoveVolumeRequest {
                                name: id.name,
                                force,
                            },
                            &MachineTarget::from(&id.machine_id),
                            Some(TARGET_RPC_TIMEOUT),
                        )
                        .await
                        .map(drop)
                }
            })
            .await;
            if removal.all_targets_succeeded() {
                Ok(())
            } else {
                Err(Error::usage(failure_summary(&removal)))
            }
        })
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
            .call::<op::ListMachines>(ListMachinesRequest {}, None)
            .await
            .map(|list| list.machines)?,
        selectors,
    )?;
    let result = client.list_volumes(&machines).await;
    Ok((machine_volumes(&machines, &result), result))
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
        .map(|selector| FanoutSelector::parse(selector.as_str()))
        .collect::<Result<Vec<_>, _>>()?;
    let visible = machines
        .iter()
        .map(|observation| observation.machine.clone())
        .collect::<Vec<_>>();
    let selected = resolve_machine_selectors(&visible, &selectors)?;
    let mut observations = machines
        .into_iter()
        .map(|observation| (observation.machine.id, observation))
        .collect::<BTreeMap<_, _>>();
    selected
        .into_iter()
        .map(|machine| {
            observations
                .remove(&machine.id)
                .ok_or_else(|| Error::usage("selected Machine disappeared from the snapshot"))
        })
        .collect()
}

fn select_create_machine(
    machines: &[MachineObservation],
    selector: Option<&str>,
) -> Result<Option<MachineObservation>, Error> {
    if let Some(selector) = selector {
        let target = MachineTarget::parse(selector)?;
        return match target.resolve(machines.iter().map(|machine| &machine.machine)) {
            NameMatches::One(machine) => Ok(Some(
                machines
                    .iter()
                    .find(|observation| observation.machine.id == machine.id)
                    .expect("resolve returned a Machine from this snapshot")
                    .clone(),
            )),
            NameMatches::None => Err(Error::usage(format!("Machine {selector:?} was not found"))),
            NameMatches::Ambiguous(_) => Err(Error::usage(format!(
                "Machine Target {selector:?} matched multiple Machines"
            ))),
        };
    }
    match machines {
        [] => Err(Error::usage("no Machines are available")),
        [machine] => Ok(Some(machine.clone())),
        _ if !io::stdin().is_terminal() || !io::stdout().is_terminal() => Err(Error::usage(
            "multiple Machines are available; specify --machine",
        )),
        _ => {
            println!("Select a Machine (blank or q cancels):");
            for (index, machine) in machines.iter().enumerate() {
                println!("  {}. {}", index + 1, machine.machine.name);
            }
            print!("> ");
            io::stdout().flush()?;
            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            let input = input.trim();
            if input.is_empty() || input.eq_ignore_ascii_case("q") {
                return Ok(None);
            }
            let index = input
                .parse::<usize>()
                .ok()
                .and_then(|value| value.checked_sub(1))
                .filter(|index| *index < machines.len())
                .ok_or_else(|| Error::usage("invalid selection"))?;
            Ok(machines
                .get(index)
                .cloned()
                .map(Some)
                .ok_or_else(|| Error::usage("invalid selection"))?)
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

fn report_partial_removal_discovery<T>(result: &PartialResult<T, RpcError>) {
    for failure in &result.failures {
        eprintln!(
            "WARNING: Machine {} was not checked and may hold a same-named Docker Volume: {}",
            failure.machine_id, failure.error.message
        );
    }
    for machine_id in &result.omissions {
        eprintln!(
            "WARNING: Machine {machine_id} was not checked and may hold a same-named Docker Volume: no terminal response"
        );
    }
}

fn failure_summary<T>(result: &PartialResult<T, RpcError>) -> String {
    let failures = crate::failure::partial_failure_details(result);
    format!("one or more Machines failed: {failures}")
}

#[cfg(test)]
mod tests {
    use ployz_core::{
        Machine, MachineId, MachineName, MachineObservation, ManagementAddress,
        MembershipObservation, WireGuardPublicKey,
    };

    use super::*;

    #[test]
    fn volume_selection_uses_fanout_for_lists_and_identity_for_create() {
        let machines = [machine(1, "edge"), machine(2, "all")];
        assert_eq!(selected_machines(machines.to_vec(), &[]).unwrap().len(), 2);
        assert_eq!(
            selected_machines(machines.to_vec(), &["*".into()])
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            selected_machines(machines.to_vec(), &["all".into()])
                .unwrap()
                .first()
                .unwrap()
                .machine
                .name
                .as_str(),
            "all"
        );
        assert!(selected_machines(machines.to_vec(), &["missing".into()]).is_err());
        assert!(select_create_machine(&machines, Some("*")).is_err());
        assert_eq!(
            select_create_machine(&machines, Some("all"))
                .unwrap()
                .unwrap()
                .machine
                .name
                .as_str(),
            "all"
        );
    }

    fn machine(seed: u8, name: &str) -> MachineObservation {
        MachineObservation {
            machine: Machine {
                id: MachineId::parse(format!("{seed:032x}")).unwrap(),
                name: MachineName::parse(name).unwrap(),
                subnet: format!("10.210.{seed}.0/24").parse().unwrap(),
                management_address: ManagementAddress("fd00::1".parse().unwrap()),
                public_key: WireGuardPublicKey([seed; 32]),
                public_ip: None,
                advertised_endpoints: Vec::new(),
                runtime: Default::default(),
            },
            membership: MembershipObservation::Up,
            selected_endpoint: None,
            rtt: None,
        }
    }
}
