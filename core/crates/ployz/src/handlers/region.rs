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

pub(super) fn ls(root: &ArgMatches) -> Result<(), Error> {
    let store = store(root)?;
    let leaf = leaf_matches(root);
    let context = leaf.get_one::<String>("context").map(String::as_str);
    let machines = runtime()?.block_on(async {
        Ok::<_, Error>(match connect_or_none(leaf, context).await? {
            Some(mut client) => client.machines().await?,
            None => Vec::new(),
        })
    })?;
    let servers = machines
        .iter()
        .map(|observed| {
            let machine = &observed.machine;
            let region = machine
                .labels
                .iter()
                .find(|(key, _)| key.as_str() == REGION_LABEL)
                .map(|(_, region)| region.as_str());
            (machine.name.as_str(), region)
        })
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
    let named = store(root)?.write(&NameRegion { key, name })?;
    ui::done(
        &json!({ "region": named.key, "name": named.name }),
        format!("Renamed {} to {}. Nothing restarts.", named.key, named.name),
    )?;
    Ok(())
}
