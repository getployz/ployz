use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, IsTerminal, Write},
};

use clap::ArgMatches;
use ployz_core::{
    CreateVolumeRequest, DockerVolumeName, DockerVolumeStorageObservation, FanoutSelector,
    ListMachinesRequest, MachineObservation, MachineTarget, NameMatches, PartialResult,
    QualifiedService, RemoveVolumesRequest, RpcError, RpcErrorCode, VolumeInventory, VolumeRemoval,
    VolumeRemovalOutcome, op, resolve_machine_selectors,
};

use crate::{
    connect::{Client, TARGET_RPC_TIMEOUT},
    volume::{MachineVolume, filter_volumes, machine_volumes, parse_assignments},
};

use super::{Error, data_loss, leaf_matches, required, string_values, with_client};

pub(super) fn create(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    // An explicit non-empty name is required; anonymous Docker Volumes stay unsupported.
    let name = DockerVolumeName::parse(required(matches, "volume-name")?)?;
    let size = matches
        .get_one::<crate::volume::ProvisionedVolumeSize>("size")
        .cloned();
    let (driver, options) = match &size {
        Some(size) => (
            "ployz".to_owned(),
            BTreeMap::from([("size".to_owned(), size.as_str().to_owned())]),
        ),
        None => (
            required(matches, "driver")?,
            parse_assignments(string_values(matches, "opt").iter().map(String::as_str))?,
        ),
    };
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
            let report = client
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
            let volume = crate::service::verified_created_volume(report)?;
            if size.as_ref().is_some_and(|size| !size.matches(&volume)) {
                return Err(Error::usage(format!(
                    "Docker Volume {:?} already exists with a different Provisioned shape; resizing is not supported; use the future `ployz volume update` capability",
                    volume.id.name
                )));
            }
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
                println!("MACHINE\tVOLUME\tTYPE\tQUOTA\tUSED\tDRIVER");
                for volume in &volumes {
                    let (kind, bound, used) = format_storage(&volume.volume.storage);
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}",
                        volume.machine_name,
                        volume.volume.id.name,
                        kind,
                        bound,
                        used,
                        volume.volume.driver()
                    );
                }
                for failure in volume_failures(&result) {
                    println!(
                        "{}\t{}\tUNAVAILABLE\t-\t-\t-",
                        failure.id.machine_id, failure.id.name
                    );
                }
            }
            report_failures(&result);
            report_inventory_failures(&result);
            if inventories_complete(&result) {
                Ok(())
            } else {
                Err(Error::exit(1))
            }
        })
    })
}

fn format_storage(storage: &DockerVolumeStorageObservation) -> (&'static str, String, String) {
    match storage {
        DockerVolumeStorageObservation::Plain { .. } => ("PLAIN", "-".into(), "-".into()),
        DockerVolumeStorageObservation::Provisioned {
            bound_bytes,
            used_bytes,
            ..
        } => (
            "PROVISIONED",
            bound_bytes.to_string(),
            used_bytes.to_string(),
        ),
    }
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
            let machines = selected_machines(
                client
                    .call::<op::ListMachines>(ListMachinesRequest {}, None)
                    .await?
                    .machines,
                &selectors,
            )?;
            let result = client.inspect_volumes(&machines, &name).await;
            if result
                .failures
                .iter()
                .any(|failure| failure.error.code != RpcErrorCode::NotFound)
                || !result.omissions.is_empty()
            {
                return Err(Error::usage(failure_summary(&result)));
            }
            let names = machines
                .iter()
                .map(|machine| (machine.machine.id, machine.machine.name.clone()))
                .collect::<BTreeMap<_, _>>();
            let volumes = result
                .successes
                .into_iter()
                .map(|success| MachineVolume {
                    machine_name: names
                        .get(&success.machine_id)
                        .cloned()
                        .expect("inspect target came from the Machine snapshot"),
                    volume: success.value,
                })
                .collect();
            match NameMatches::from_matches(volumes) {
                NameMatches::None => Err(Error::usage(format!(
                    "Docker Volume {name:?} was not found"
                ))),
                NameMatches::One(volume) => {
                    println!("{}", serde_json::to_string_pretty(&volume)?);
                    Ok(())
                }
                volumes @ NameMatches::Ambiguous { .. } => Err(Error::usage(format!(
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
    let command = root.clone();
    with_client(root, |client| {
        Box::pin(async move {
            let (volumes, result) = discover(client, &selectors).await?;
            let volumes = filter_volumes(&volumes, &names);
            let unavailable = volume_failures(&result)
                .filter(|failure| names.is_empty() || names.contains(&failure.id.name))
                .map(ToString::to_string)
                .reduce(|mut summary, failure| {
                    summary.push_str("; ");
                    summary.push_str(&failure);
                    summary
                });
            if let Some(unavailable) = unavailable {
                return Err(Error::usage(format!(
                    "refusing to remove unavailable Docker Volumes: {unavailable}"
                )));
            }
            if inventories_complete(&result)
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
                return Err(Error::usage(volume_failure_summary(&result)));
            }
            let context = match client.connection_source() {
                crate::context::ConnectionSource::Context(name) => name.as_str(),
                crate::context::ConnectionSource::Direct => "direct connection",
                crate::context::ConnectionSource::LocalSocket => "local socket",
            };
            println!(
                "Remove volumes\nContext: {context}\nBased on what the connected machine can see; other machines may have additional resources.\nPermanently delete these volumes and their data ({}):",
                volumes.len()
            );
            for volume in &volumes {
                println!(
                    "  {} on {} (machine ID: {})",
                    volume.volume.id.name, volume.machine_name, volume.volume.id.machine_id
                );
            }
            if !data_loss::confirm_ordinary(&command, client)? {
                return Ok(());
            }
            let removal = client
                .remove_volumes(RemoveVolumesRequest {
                    volumes: volumes.into_iter().map(|volume| volume.volume.id).collect(),
                    force,
                })
                .await?;
            refuse_unless_removed(removal)
        })
    })
}

async fn discover(
    client: &mut Client,
    selectors: &[String],
) -> Result<(Vec<MachineVolume>, PartialResult<VolumeInventory, RpcError>), Error> {
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
            NameMatches::Ambiguous { .. } => Err(Error::usage(format!(
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

fn volume_failures(
    result: &PartialResult<VolumeInventory, RpcError>,
) -> impl Iterator<Item = &ployz_core::VolumeObservationFailure> {
    result
        .successes
        .iter()
        .flat_map(|success| success.value.failures.iter())
}

fn inventories_complete(result: &PartialResult<VolumeInventory, RpcError>) -> bool {
    result.all_targets_succeeded() && volume_failures(result).next().is_none()
}

fn report_inventory_failures(result: &PartialResult<VolumeInventory, RpcError>) {
    for failure in volume_failures(result) {
        eprintln!("{failure}");
    }
}

fn report_partial_removal_discovery(result: &PartialResult<VolumeInventory, RpcError>) {
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
    for failure in volume_failures(result) {
        eprintln!("WARNING: {failure}; this Volume will not be removed");
    }
}

fn volume_failure_summary(result: &PartialResult<VolumeInventory, RpcError>) -> String {
    let mut failures = crate::failure::partial_failure_details(result);
    for failure in volume_failures(result) {
        if !failures.is_empty() {
            failures.push_str("; ");
        }
        failures.push_str(&failure.to_string());
    }
    format!("one or more Docker Volume observations failed: {failures}")
}

pub(super) fn refuse_unless_removed(removals: Vec<VolumeRemoval>) -> Result<(), Error> {
    for removal in &removals {
        if matches!(removal.outcome, VolumeRemovalOutcome::Removed) {
            println!(
                "Deleted volume {} on {}",
                removal.id.name, removal.id.machine_id
            );
        }
    }
    if removals
        .iter()
        .all(|removal| matches!(removal.outcome, VolumeRemovalOutcome::Removed))
    {
        Ok(())
    } else {
        Err(Error::usage(removal_failure_summary(&removals)))
    }
}

pub(super) fn removal_failure_summary(removals: &[VolumeRemoval]) -> String {
    let failures = removals
        .iter()
        .filter_map(|removal| {
            let message = match &removal.outcome {
                VolumeRemovalOutcome::Removed => return None,
                VolumeRemovalOutcome::Failed { error } => error.message.as_str(),
                VolumeRemovalOutcome::Omitted => {
                    "not attempted: Machine absent or not inviting RPC"
                }
            };
            Some(format!(
                "{}/{}: {message}",
                removal.id.machine_id, removal.id.name
            ))
        })
        .collect::<Vec<_>>()
        .join("; ");
    let mut summary =
        format!("one or more Docker Volume removals failed or were omitted: {failures}");
    if let Some(hint) = volume_in_use_hint(removals) {
        summary.push('\n');
        summary.push_str(&hint);
    }
    summary
}

fn volume_in_use_hint(removals: &[VolumeRemoval]) -> Option<String> {
    let mut services = BTreeSet::new();
    for removal in removals {
        let VolumeRemovalOutcome::Failed { error } = &removal.outcome else {
            continue;
        };
        let Some(names) = error
            .details
            .get("in_use_by")
            .and_then(|value| value.as_array())
        else {
            continue;
        };
        for name in names {
            if let Some(name) = name.as_str()
                && let Ok(service) = QualifiedService::parse(name)
            {
                services.insert(service);
            }
        }
    }
    match services.len() {
        0 => None,
        1 => Some(format!(
            "remove the Service first: ployz rm {}",
            services.iter().next().expect("checked len")
        )),
        _ => Some(format!(
            "remove the Services first: ployz rm {}",
            services
                .into_iter()
                .map(|service| service.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        )),
    }
}

fn failure_summary<T>(result: &PartialResult<T, RpcError>) -> String {
    let failures = crate::failure::partial_failure_details(result);
    format!("one or more Machines failed: {failures}")
}

#[cfg(test)]
mod tests {
    use ployz_core::{
        DockerVolumeStorageObservation, Machine, MachineId, MachineName, MachineObservation,
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

    #[test]
    fn volume_columns_distinguish_plain_and_provisioned_usage() {
        assert_eq!(
            format_storage(&DockerVolumeStorageObservation::Plain {
                driver: "local".into(),
            }),
            ("PLAIN", "-".into(), "-".into())
        );
        assert_eq!(
            format_storage(&DockerVolumeStorageObservation::Provisioned {
                mountpoint: ployz_core::MachinePath::parse("/var/lib/ployz-volumes/data").unwrap(),
                bound_bytes: std::num::NonZeroU64::new(1_073_741_824).unwrap(),
                used_bytes: 966_367_642,
            }),
            ("PROVISIONED", "1073741824".into(), "966367642".into())
        );
    }

    #[test]
    fn in_use_volume_removal_names_the_service_to_remove() {
        let removal = VolumeRemoval {
            id: ployz_core::DockerVolumeId {
                machine_id: MachineId::parse("a".repeat(32)).unwrap(),
                name: ployz_core::DockerVolumeName::parse("busy").unwrap(),
            },
            outcome: VolumeRemovalOutcome::Failed {
                error: RpcError {
                    code: RpcErrorCode::Conflict,
                    message: "volume is in use by cashdash/cashdash-singlestore (2 containers)"
                        .into(),
                    details: serde_json::json!({
                        "in_use_by": ["cashdash/cashdash-singlestore"]
                    }),
                },
            },
        };
        let summary = removal_failure_summary(&[removal]);
        assert!(
            summary.contains("volume is in use by cashdash/cashdash-singlestore"),
            "{summary}"
        );
        assert!(
            summary.contains("remove the Service first: ployz rm cashdash/cashdash-singlestore"),
            "{summary}"
        );
    }

    #[test]
    fn in_use_volume_removal_names_each_service_to_remove() {
        let removal = VolumeRemoval {
            id: ployz_core::DockerVolumeId {
                machine_id: MachineId::parse("a".repeat(32)).unwrap(),
                name: ployz_core::DockerVolumeName::parse("busy").unwrap(),
            },
            outcome: VolumeRemovalOutcome::Failed {
                error: RpcError {
                    code: RpcErrorCode::Conflict,
                    message: "volume is in use by app/web, app/db (3 containers)".into(),
                    details: serde_json::json!({
                        "in_use_by": ["app/db", "app/web"]
                    }),
                },
            },
        };
        let summary = removal_failure_summary(&[removal]);
        assert!(
            summary.contains("remove the Services first: ployz rm app/db app/web"),
            "{summary}"
        );
    }

    fn machine(seed: u8, name: &str) -> MachineObservation {
        MachineObservation::new(
            Machine {
                id: MachineId::parse(format!("{seed:032x}")).unwrap(),
                name: MachineName::parse(name).unwrap(),
                subnet: format!("10.210.{seed}.0/24").parse().unwrap(),
                public_key: WireGuardPublicKey([seed; 32]),
                public_ip: None,
                advertised_endpoints: Vec::new(),
                runtime: Default::default(),
            },
            MembershipObservation::Up,
        )
    }
}
