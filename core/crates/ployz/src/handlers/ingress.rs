//! CLI handlers for neutral Ingress Proxy operations.

use clap::ArgMatches;
use ployz_core::{GetIngressProxyConfigRequest, MachineTarget, op};

use super::{Error, connect_client, leaf_matches, runtime, string_values};
use crate::connect::TARGET_RPC_TIMEOUT;

pub(super) fn config(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let selector = matches.get_one::<String>("machine").cloned();
    runtime()?.block_on(async {
        let mut client = connect_client(root, None).await?;
        let target = selector.map(MachineTarget::parse).transpose()?;
        let config = match target.as_ref() {
            None => {
                client
                    .call::<op::GetIngressProxyConfig>(GetIngressProxyConfigRequest {}, None)
                    .await?
            }
            Some(target) => {
                client
                    .invoke::<op::GetIngressProxyConfig>(
                        GetIngressProxyConfigRequest {},
                        target,
                        Some(TARGET_RPC_TIMEOUT),
                    )
                    .await?
            }
        };
        print!("{}", config.config());
        Ok(())
    })
}

pub(super) fn deploy(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let image = matches.get_one::<String>("image").cloned();
    let constraints = string_values(matches, "constraint")
        .into_iter()
        .map(ployz_core::PlacementConstraint::parse)
        .collect::<Result<std::collections::BTreeSet<_>, _>>()
        .map_err(|error| Error::usage(error.to_string()))?;
    let force_recreate = matches.get_flag("recreate");
    let skip_health_monitor = matches.get_flag("skip-health");
    runtime()?.block_on(async {
        let context = root
            .get_one::<String>("context")
            .map(String::as_str)
            .unwrap_or("default");
        let mut client =
            connect_client(root, root.get_one::<String>("context").map(String::as_str)).await?;
        let requested = crate::ingress::service_spec(image, constraints).await?;
        crate::deploy::apply_requested(
            &mut client,
            &requested,
            force_recreate,
            skip_health_monitor,
            context,
        )
        .await
        .map_err(Error::from)?;
        Ok(())
    })
}
