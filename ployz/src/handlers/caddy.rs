use std::{fs, path::Path};

use clap::ArgMatches;
use ployz_core::{GetCaddyConfigRequest, MachineTarget, op};

use super::{Error, connect_client, leaf_matches, runtime, string_values};
use crate::connect::TARGET_RPC_TIMEOUT;

pub(super) fn config(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let selector = matches.get_one::<String>("machine").cloned();
    runtime()?.block_on(async {
        let mut client = connect_client(root, None).await?;
        let target = selector.map(MachineTarget::parse).transpose()?;
        let caddyfile = match target.as_ref() {
            None => {
                client
                    .call::<op::GetCaddyConfig>(GetCaddyConfigRequest {}, None)
                    .await?
            }
            Some(target) => {
                client
                    .invoke::<op::GetCaddyConfig>(
                        GetCaddyConfigRequest {},
                        target,
                        Some(TARGET_RPC_TIMEOUT),
                    )
                    .await?
            }
        };
        print!("{}", caddyfile.caddyfile);
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
        .map_err(|error| Error::usage(format!("read Caddyfile: {error}")))?;
    let machines = string_values(matches, "machine")
        .into_iter()
        .map(MachineTarget::parse)
        .collect::<Result<Vec<_>, _>>()?;
    let force_recreate = matches.get_flag("recreate");
    let skip_health_monitor = matches.get_flag("skip-health");
    runtime()?.block_on(async {
        let image = match image {
            Some(image) => image,
            None => crate::caddy::latest_image().await?,
        };
        let requested = crate::caddy::service_spec(image, machines, caddy_config);
        let mut client = connect_client(root, None).await?;
        crate::deploy::deploy_spec(
            &mut client,
            &requested,
            force_recreate,
            skip_health_monitor,
            None,
        )
        .await?;
        crate::dns::update_records_if_reserved(&mut client).await?;
        Ok(())
    })
}
