use std::{fs, path::Path};

use clap::ArgMatches;
use ployz_core::MachineSelector;

use super::{Error, connect_client, leaf_matches, runtime, string_values};

pub(super) fn config(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let selector = matches.get_one::<String>("machine").cloned();
    runtime()?.block_on(async {
        let mut client = connect_client(root, None).await?;
        let target = selector
            .map(MachineSelector::parse)
            .transpose()
            .map_err(|error| error.to_string())?;
        let caddyfile = client
            .get_caddy_config(target)
            .await
            .map_err(|error| error.to_string())?;
        print!("{caddyfile}");
        Ok(())
    })
}

pub(super) fn deploy(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let image = matches.get_one::<String>("image").cloned();
    let caddy_config = matches
        .get_one::<String>("caddyfile")
        .map(|path| fs::read_to_string(Path::new(path)))
        .transpose()
        .map_err(|error| format!("read Caddyfile: {error}"))?;
    let machines = string_values(matches, "machine")
        .into_iter()
        .map(MachineSelector::parse)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    runtime()?.block_on(async {
        let image = match image {
            Some(image) => image,
            None => crate::caddy::latest_image().await?,
        };
        let requested = crate::caddy::service_spec(image, machines, caddy_config);
        let mut client = connect_client(root, None).await?;
        super::workflow::deploy_requested(&mut client, &requested).await
    })
}
