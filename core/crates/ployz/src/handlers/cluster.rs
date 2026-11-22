use clap::{ArgMatches, Command};
use ployz_core::{ClusterName, ClusterNamed, Hint};
use serde::Serialize;
use serde_json::json;

use super::{Error, leaf_matches, route, runtime};
use crate::approval;
use crate::cli::{base, positional};
use crate::cloud::account::Credential;
use crate::cloud::http::Via;
use crate::cluster::name::{parse, read, set};
use crate::published;
use crate::ui;
use ployz_store::OperationVerb;

pub(crate) fn group(command: Command) -> Command {
    command.about("Name the Cluster")
}

pub(crate) fn name_command(command: Command) -> Command {
    base(command, "Show or set the Cluster's name")
        .long_about("Show or set the Cluster's name. A rename changes every Service's long private address, such as db.prod.acme.ployz.internal. Through Ployz Cloud, a rename asks for an Approval first.")
        .after_help(concat!(
            "Examples:\n",
            "  ployz cluster name\n",
            "  ployz cluster name acme"
        ))
        .arg(positional("name", false).help("The new name, such as acme"))
        .arg(crate::cli::approval())
}

#[derive(Serialize)]
struct Named {
    cluster: ClusterNamed,
}

pub(crate) fn name(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let given = leaf
        .get_one::<String>("name")
        .map(|name| parse(name))
        .transpose()?;
    let context = leaf.get_one::<String>("context").map(String::as_str);
    let runtime = runtime()?;
    if let Some(name) = &given
        && let Some(credential) = route::cloud_runs(&runtime, leaf)?
    {
        return through_cloud(&runtime, leaf, &credential, name);
    }
    runtime.block_on(async {
        let client = route::connect(leaf, context).await?;
        match given {
            Some(name) => {
                let named = set(&client, name).await?;
                let sentence = format!(
                    "Named the Cluster {}.",
                    named.name.as_ref().map_or("", ClusterName::as_str)
                );
                ui::done(&Named { cluster: named }, sentence)
            }
            None => {
                let named = read(&client).await?;
                let shown = named.name.clone();
                ui::finish(&Named { cluster: named }, || match shown {
                    Some(name) => ui::stream(name),
                    None => {
                        ui::note("This Cluster has no name.");
                        ui::hint(&Hint::Next("ployz cluster name <name>".into()));
                    }
                })
            }
        }
    })
}

fn through_cloud(
    runtime: &tokio::runtime::Runtime,
    leaf: &ArgMatches,
    credential: &Credential,
    name: &ClusterName,
) -> Result<(), Error> {
    let ran = approval::approved(
        runtime,
        credential,
        OperationVerb::SetClusterName,
        leaf.get_one::<String>("approval").cloned(),
        |id| crate::cli::approved_rerun(leaf, &["cluster", "name", name.as_str()], id),
        async |approval| {
            let key = uuid::Uuid::new_v4().to_string();
            let input = json!({ "name": name.as_str() });
            published::run_on(
                credential,
                Via::Cli,
                "cluster_rename",
                input,
                approval,
                &key,
            )
            .await
            .0
        },
    )?;
    let named = Named {
        cluster: ClusterNamed {
            name: Some(name.clone()),
        },
    };
    let sentence = ran.message.unwrap_or_else(|| saved(name.as_str()));
    ui::done(&named, sentence)
}

fn saved(name: &str) -> String {
    format!("Saved {name} as the Cluster's name. It applies when a Server is online.")
}
