//! `ployz region`: the regions Servers are in, and their names. A Server's region is its
//! `region` label; a name is only what people see, so renaming restarts nothing.

use clap::{ArgMatches, Command};
use ployz_store::{NameRegion, REGION_LABEL, RegionKey, RegionName, RegionsQuery, region_servers};
use serde_json::json;

use super::route::connect_or_none;
use super::store::store;
use super::{Error, leaf_matches, required, runtime};
use crate::cli::positional;
use crate::ui::{self, Table};

pub(super) fn group(command: Command) -> Command {
    command.about("List and rename regions")
}

pub(super) fn ls_command(command: Command) -> Command {
    command.about("List regions and their Servers")
}

pub(super) fn rename_command(command: Command) -> Command {
    command
        .about("Rename a region; nothing restarts")
        .arg(positional("region", true).help("The region's key, such as default"))
        .arg(positional("name", true).help("Its new name, up to 40 characters"))
}

/// The Servers the context's Cluster has, none when it has no Cluster reachable.
fn machines(leaf: &ArgMatches) -> Result<Vec<ployz_core::MachineObservation>, Error> {
    let context = leaf.get_one::<String>("context").map(String::as_str);
    runtime()?.block_on(async {
        Ok::<_, Error>(match connect_or_none(leaf, context).await? {
            Some(mut client) => client.machines().await?,
            None => Vec::new(),
        })
    })
}

/// A Server's name and its `region` label's value.
fn server_region(machine: &ployz_core::Machine) -> (&str, Option<&str>) {
    let region = machine
        .labels
        .iter()
        .find(|(key, _)| key.as_str() == REGION_LABEL)
        .map(|(_, region)| region.as_str());
    (machine.name.as_str(), region)
}

pub(super) fn ls(root: &ArgMatches) -> Result<(), Error> {
    let store = store(root)?;
    let machines = machines(leaf_matches(root))?;
    let servers = machines
        .iter()
        .map(|observed| server_region(&observed.machine))
        .collect::<Vec<_>>();
    let labels = servers
        .iter()
        .filter_map(|(_, label)| label.map(str::to_owned))
        .collect();
    let view = store.read(&RegionsQuery { labels })?;
    let listed = region_servers(view.regions, &servers);
    let mut table = Table::new(
        ["REGION", "KEY", "SERVERS"],
        if machines.is_empty() {
            "No regions yet."
        } else {
            "No Server is in a region yet."
        },
    );
    for region in &listed.regions {
        table.row([
            region.name.clone(),
            region.key.to_string(),
            region.servers.join(", "),
        ]);
    }
    let next = listed
        .no_region
        .first()
        .map(|server| format!("ployz server set {server} --label-add region=default"));
    let result = ui::Next::new(&listed, next);
    ui::finish(&result, || {
        ui::rows(&table);
        if !listed.no_region.is_empty() {
            ui::note(format_args!(
                "Not in a region: {}.",
                listed.no_region.join(", ")
            ));
        }
        result.say();
    })?;
    Ok(())
}

pub(super) fn rename(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let key = RegionKey::parse(required(matches, "region")?)?;
    let name = RegionName::parse(required(matches, "name")?)?;
    let labels = machines(matches)?
        .iter()
        .filter_map(|observed| server_region(&observed.machine).1.map(str::to_owned))
        .collect();
    let named = store(root)?.write(&NameRegion {
        key,
        name,
        labels,
        creates: false,
    })?;
    ui::done(
        &json!({ "region": named.key, "name": named.name }),
        format!("Renamed {} to {}. Nothing restarts.", named.key, named.name),
    )?;
    Ok(())
}
