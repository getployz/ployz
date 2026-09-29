//! CLI handlers for neutral Ingress Proxy operations.

use clap::{ArgMatches, Command};

use crate::cli::{base, many, switch, value};

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
        crate::deploy::emit_outcome(
            crate::deploy::apply_requested(
                &mut client,
                &requested,
                force_recreate,
                skip_health_monitor,
                context,
            )
            .await,
        )
    })
}

pub(crate) fn command() -> Command {
    base("ingress", "Manage the Ingress Proxy")
        .arg_required_else_help(true)
        .subcommand(
            base("deploy", "Deploy the Ingress Proxy")
                .arg(value("image", None))
                .arg(many("constraint", None))
                .arg(switch("recreate", None))
                .arg(switch("skip-health", None)),
        )
}

pub(super) fn handler(path: &str) -> Option<(super::Handler, super::Json)> {
    use super::Json::Supported;
    Some(match path {
        "deploy" => (deploy, Supported),
        _ => return None,
    })
}
