use clap::{ArgMatches, Command};
use ployz_core::{DeployOutcome, DockerVolumeId, Namespace, RpcErrorCode};
use ployz_store::{NamespacesQuery, OwnedNamespace};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::super::teardown::confirm;
use super::super::{Error, leaf_matches, runtime, store};
use crate::approval::{self, Verb};
use crate::cli::{base, value};
use crate::cloud_account::{self, Credential};
use crate::deploy::VolumeFate;
use crate::ui::{Hint, Table, Tree};

pub(super) fn command() -> Command {
    base(
        "clean",
        "Remove a Namespace on your Servers that isn't in any Project; lists them without --namespace",
    )
    .long_about(
        "Remove a Namespace the Servers run that no Environment owns, such as one a failed \
         teardown left behind: its containers, and its Volumes with their data. Without \
         --namespace, lists those Namespaces. Type the Namespace with --confirm, or in a \
         terminal when it asks; elsewhere it fails with confirmation_required, naming the \
         Volumes whose data goes. Signed in to Cloud without --context or --connect, Cloud \
         removes it after the same typed confirmation, asking a human first when the \
         Organization wants that.",
    )
    .arg(value("namespace", None).value_name("NAMESPACE"))
    .arg(
        value("confirm", None)
            .value_name("NAMESPACE")
            .requires("namespace")
            .help("The Namespace, typed to confirm its removal"),
    )
    .arg(crate::cli::approval().requires("namespace"))
}

/// A Namespace no Environment owns, and what removing it takes.
#[derive(Serialize)]
struct Unowned {
    namespace: Namespace,
    services: Vec<ployz_core::ServiceName>,
    volumes: Vec<DockerVolumeId>,
}

/// What `--confirm` removed.
#[derive(Serialize)]
struct Cleaned {
    namespace: Namespace,
    volumes: Vec<DockerVolumeId>,
    outcome: DeployOutcome<ployz_core::ExecutionError>,
}

pub(super) fn clean(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let named = matches
        .get_one::<String>("namespace")
        .map(|name| {
            Namespace::parse(name.as_str())
                .map_err(|_| Error::usage("Expected a Namespace: lowercase letters, digits and -"))
        })
        .transpose()?;
    if let Some(namespace) = &named {
        let runtime = runtime()?;
        if let Some(credential) = super::cloud_runs(&runtime, matches)? {
            confirm_in_cloud(&runtime, matches, &credential, namespace)?;
            return through_cloud(&runtime, matches, &credential, namespace);
        }
    }
    let store = store::store(root)?;
    let context = matches.get_one::<String>("context").map(String::as_str);
    let owned = store.read(&NamespacesQuery {})?.namespaces;
    let runtime = runtime()?;
    let mut client = runtime.block_on(super::connect(matches, context))?;
    let (observed, gaps) = runtime.block_on(client.namespaces())?;
    let unowned = observed
        .into_iter()
        .filter(|namespace| owner(&owned, &namespace.name).is_none())
        .map(|observed| Unowned {
            namespace: observed.name,
            services: observed
                .services
                .iter()
                .map(|service| service.name.clone())
                .collect(),
            volumes: observed.volumes,
        });
    let Some(namespace) = named else {
        let unowned: Vec<_> = unowned.collect();
        let next = unowned
            .first()
            .map(|first| retry(matches, &first.namespace));
        let report = json!({
            "namespaces": unowned,
            "next": next,
            "failures": gaps.failures,
            "omitted": gaps.omitted,
        });
        let mut table = Table::new(
            ["NAMESPACE", "SERVICES", "VOLUMES"],
            "Every Namespace on the Servers that answered is in a Project.",
        );
        for namespace in &unowned {
            table.row([
                namespace.namespace.to_string(),
                super::super::joined(&namespace.services),
                volume_names(&namespace.volumes),
            ]);
        }
        gaps.warn();
        crate::ui::finish(&report, || {
            crate::ui::rows(&table);
            if let Some(next) = next.clone() {
                crate::ui::hint(&Hint::Next(next));
            }
        })?;
        return gaps.outcome();
    };
    if let Some(owner) = owner(&owned, &namespace) {
        return Err(Error::detailed(
            RpcErrorCode::Conflict,
            format!(
                "{namespace} runs {}/{}: remove that Environment instead",
                owner.project, owner.environment
            ),
            json!({
                "namespace": namespace,
                "project": owner.project,
                "environment": owner.environment,
            }),
        )
        .hint(Hint::Next(shell_words::join([
            "ployz",
            "env",
            "rm",
            owner.environment.as_str(),
            "--project",
            owner.project.as_str(),
        ]))));
    }
    let Some(found) = unowned
        .into_iter()
        .find(|unowned| unowned.namespace == namespace)
    else {
        if gaps.failures.is_empty() && gaps.omitted.is_empty() {
            return Err(Error::not_found(format!(
                "No Server runs Namespace {namespace}"
            )));
        }
        return crate::ui::finish_fanout("namespace", &None::<()>, &gaps, || {
            crate::ui::stream(format_args!(
                "Namespace {namespace} isn't on the Servers that answered, but some didn't; \
                 run the same command again."
            ));
        });
    };
    let next = retry(matches, &namespace);
    confirm(
        matches,
        namespace.as_str(),
        "Namespace",
        next.clone(),
        || {
            let refusal = Error::detailed(
                RpcErrorCode::ConfirmationRequired,
                format!(
                    "Removing Namespace {namespace} deletes its containers and the data of Volumes \
                 {}; this can't be undone. No changes made.",
                    volume_names(&found.volumes)
                ),
                json!({
                    "namespace": namespace,
                    "services": found.services,
                    "volumes": found.volumes,
                }),
            )
            .hint(Hint::Retry(next.clone()));
            let loss = Tree::new(
                format!("Removing Namespace {namespace} deletes, for good:"),
                vec![
                    Tree::leaf("its containers"),
                    Tree::leaf(format!(
                        "the data of Volumes {}",
                        volume_names(&found.volumes)
                    )),
                ],
            );
            Ok((refusal, loss))
        },
    )?;
    let (volumes, outcome) = runtime.block_on(async {
        let token = crate::cancellation::on_ctrl_c();
        // ponytail: the loss confirmed is the one observed now, which the refusal
        // named a moment ago; bind it to --accept-volume-loss if Volumes ever appear
        // in an orphaned Namespace between the two.
        let loss = client
            .data_loss_if_namespace_destroyed(&namespace, VolumeFate::Destroy)
            .await?;
        let confirmation = loss
            .confirm_names(loss.data_loss.iter().map(ployz_core::DataLoss::name))
            .map_err(ployz_core::UnconfirmedDataLoss::into_rpc_error)?;
        let volumes = loss
            .data_loss
            .into_iter()
            .map(|ployz_core::DataLoss::DockerVolume { id }| id)
            .collect::<Vec<_>>();
        let outcome = client
            .destroy_namespace(&namespace, &confirmation, VolumeFate::Destroy, &token, None)
            .await?;
        Ok::<_, ployz_core::RpcError>((volumes, outcome))
    })?;
    let removed = matches!(outcome, DeployOutcome::Success { .. });
    let report = Cleaned {
        namespace,
        volumes,
        outcome,
    };
    crate::ui::finish(&report, || match removed {
        true => crate::ui::stream(format_args!(
            "Removed Namespace {} and the data of Volumes {}.",
            report.namespace,
            volume_names(&report.volumes)
        )),
        false => crate::ui::stream(format_args!(
            "Removing Namespace {} did not finish; run the same command again.",
            report.namespace
        )),
    })?;
    match removed {
        true => Ok(()),
        false => Err(Error::partial()),
    }
}

fn confirm_in_cloud(
    runtime: &tokio::runtime::Runtime,
    matches: &ArgMatches,
    credential: &Credential,
    namespace: &Namespace,
) -> Result<(), Error> {
    #[derive(Deserialize)]
    struct Preview {
        volumes: Vec<DockerVolumeId>,
    }
    let next = retry(matches, namespace);
    confirm(
        matches,
        namespace.as_str(),
        "Namespace",
        next.clone(),
        || {
            let Preview { volumes } = runtime.block_on(cloud_account::refusable(
                credential,
                Method::GET,
                &format!("namespaces/{namespace}/clean"),
                None,
            ))?;
            let refusal = Error::detailed(
                RpcErrorCode::ConfirmationRequired,
                format!(
                    "Removing Namespace {namespace} deletes its containers and the data of Volumes \
                 {}; this can't be undone. No changes made.",
                    volume_names(&volumes)
                ),
                json!({ "namespace": namespace, "volumes": volumes }),
            )
            .hint(Hint::Retry(next.clone()));
            let loss = Tree::new(
                format!("Removing Namespace {namespace} deletes, for good:"),
                vec![
                    Tree::leaf("its containers"),
                    Tree::leaf(format!("the data of Volumes {}", volume_names(&volumes))),
                ],
            );
            Ok((refusal, loss))
        },
    )
}

fn through_cloud(
    runtime: &tokio::runtime::Runtime,
    matches: &ArgMatches,
    credential: &Credential,
    namespace: &Namespace,
) -> Result<(), Error> {
    let id = approval::approved(
        runtime,
        credential,
        Verb::Clean,
        matches.get_one::<String>("approval").cloned(),
        |id| {
            let namespace = namespace.to_string();
            let args = [
                "server",
                "clean",
                "--namespace",
                namespace.as_str(),
                "--confirm",
                namespace.as_str(),
            ];
            super::approved_rerun(matches, &args, id)
        },
        async |approval| {
            cloud_account::start_run(
                credential,
                Method::POST,
                &format!("namespaces/{namespace}/clean"),
                &json!({}),
                approval,
            )
            .await
        },
    )?;
    #[derive(Deserialize)]
    struct Finished {
        volumes: Vec<String>,
    }
    let settled = runtime.block_on(cloud_account::follow_operation::<Finished>(
        credential,
        &format!("namespace-cleanups/{id}"),
        &format!("Removing Namespace {namespace}"),
    ))?;
    let volumes = settled.finished()?.volumes;
    let report = json!({ "namespace": namespace, "volumes": volumes });
    crate::ui::finish(&report, || {
        crate::ui::stream(format_args!(
            "Removed Namespace {namespace} and the data of Volumes {}.",
            match volumes.is_empty() {
                true => "(none)".to_owned(),
                false => super::super::joined(&volumes),
            }
        ));
    })
}

fn owner<'a>(owned: &'a [OwnedNamespace], namespace: &Namespace) -> Option<&'a OwnedNamespace> {
    owned.iter().find(|owned| &owned.namespace == namespace)
}

/// Removing `namespace`, reaching the same Servers as this command.
fn retry(matches: &ArgMatches, namespace: &Namespace) -> String {
    let mut words = vec!["ployz", "server", "clean"];
    for flag in ["context", "connect"] {
        if let Some(value) = matches.get_one::<String>(flag) {
            words.extend([
                if flag == "context" {
                    "--context"
                } else {
                    "--connect"
                },
                value,
            ]);
        }
    }
    words.extend([
        "--namespace",
        namespace.as_str(),
        "--confirm",
        namespace.as_str(),
    ]);
    shell_words::join(words)
}

fn volume_names(volumes: &[DockerVolumeId]) -> String {
    if volumes.is_empty() {
        return "(none)".to_owned();
    }
    let names: Vec<_> = volumes.iter().map(|volume| &volume.name).collect();
    super::super::joined(&names)
}
