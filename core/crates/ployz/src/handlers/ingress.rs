//! CLI handlers for neutral Ingress Proxy operations.

use clap::ArgMatches;

use super::{Error, connect_client, leaf_matches, runtime, string_values};

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
        let outcome = crate::deploy::apply_requested(
            &mut client,
            &requested,
            force_recreate,
            skip_health_monitor,
            context,
        )
        .await
        .map_err(Error::from)?;
        crate::deploy::emit_outcome(&outcome)
    })
}
