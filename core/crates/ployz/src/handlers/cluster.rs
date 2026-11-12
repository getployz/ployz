use crate::cluster::Call;
use clap::{ArgMatches, Command};
use ployz_core::{
    ClusterName, ClusterNamed, Hint, READ_CLUSTER_NAME_CAPABILITY, ReadClusterNameRequest,
    RpcErrorCode, SET_CLUSTER_NAME_CAPABILITY, SetClusterNameRequest, op,
};
use serde::Serialize;
use serde_json::json;

use super::{Error, leaf_matches, runtime, server};
use crate::approval::{self, Verb};
use crate::cli::{base, positional};
use crate::cloud_account::{Credential, Via};
use crate::cluster::Client;
use crate::published;
use crate::ui;

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
        && let Some(credential) = server::cloud_runs(&runtime, leaf)?
    {
        return through_cloud(&runtime, leaf, &credential, name);
    }
    runtime.block_on(async {
        let client = server::connect(leaf, context).await?;
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
        Verb::SetClusterName,
        leaf.get_one::<String>("approval").cloned(),
        |id| server::approved_rerun(leaf, &["cluster", "name", name.as_str()], id),
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

pub(crate) const VALID_NAME: &str =
    "Use up to 63 lowercase letters, digits and hyphens. Start and end with a letter or digit.";

pub(crate) fn parse(name: &str) -> Result<ClusterName, Error> {
    ClusterName::parse(name).map_err(|_| {
        Error::detailed(
            RpcErrorCode::InvalidArgument,
            VALID_NAME,
            json!({ "name": name }),
        )
    })
}

pub(crate) async fn set(client: &Client, name: ClusterName) -> Result<ClusterNamed, Error> {
    require(client, SET_CLUSTER_NAME_CAPABILITY).await?;
    Ok(client
        .call_shared::<op::SetClusterName>(SetClusterNameRequest { name }, Call::once())
        .await
        .map_err(crate::connect::rpc_error)?)
}

async fn read(client: &Client) -> Result<ClusterNamed, Error> {
    require(client, READ_CLUSTER_NAME_CAPABILITY).await?;
    Ok(client
        .call_shared::<op::ReadClusterName>(ReadClusterNameRequest {}, Call::once())
        .await
        .map_err(crate::connect::rpc_error)?)
}

async fn require(client: &Client, capability: &str) -> Result<(), Error> {
    if client
        .supports(None, capability)
        .await
        .map_err(crate::connect::rpc_error)?
    {
        return Ok(());
    }
    Err(Error::detailed(
        RpcErrorCode::Unsupported,
        "This Server's Ployz release can't name a Cluster.",
        json!({ "capability": capability }),
    )
    .hint(Hint::Next("ployz server upgrade".into())))
}
