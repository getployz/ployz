//! `ployz server clean`: remove a Namespace the Servers run that no Environment owns,
//! such as one a failed teardown or a Store reset left behind. Without `--namespace`
//! it lists them; removing one takes its name typed with `--confirm`.

use clap::{ArgMatches, Command};
use ployz_core::{DeployOutcome, DockerVolumeId, Namespace, RpcErrorCode};
use ployz_store::{NamespacesQuery, OwnedNamespace};
use serde::Serialize;
use serde_json::json;

use super::super::teardown::confirmed;
use super::super::{Error, leaf_matches, runtime, store};
use crate::cli::{base, value};
use crate::deploy::VolumeFate;
use crate::output::{Gaps, say};

pub(super) fn command() -> Command {
    base(
        "clean",
        "Remove a Namespace on your Servers that isn't in any Project; lists them without --namespace",
    )
    .long_about(
        "Remove a Namespace the Servers run that no Environment owns, such as one a failed \
         teardown left behind: its containers, and its Volumes with their data. Without \
         --namespace, lists those Namespaces. Type the Namespace with --confirm; without it \
         the command fails with confirmation_required, naming the Volumes whose data goes.",
    )
    .arg(value("namespace", None).value_name("NAMESPACE"))
    .arg(
        value("confirm", None)
            .value_name("NAMESPACE")
            .requires("namespace")
            .help("The Namespace, typed to confirm its removal"),
    )
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
            Namespace::parse(name.as_str()).map_err(|_| {
                Error::usage("Expected a Namespace: lowercase letters, digits and -")
                    .with_exit(crate::failure::USAGE_EXIT)
            })
        })
        .transpose()?;
    let store = store::store(root)?;
    let context = matches.get_one::<String>("context").map(String::as_str);
    // Ownership comes from the signed-in Organization, so the Servers read must be
    // that Organization's: another Cluster's owned Namespaces would read as stray.
    if matches!(store.backend(), store::Backend::Cloud(..))
        && (context.is_some() || matches.get_one::<String>("connect").is_some())
    {
        return Err(Error::usage(
            "server clean reads your Organization's Servers; drop --context and --connect",
        )
        .with_exit(crate::failure::USAGE_EXIT));
    }
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
        crate::output::finish(&report, || {
            if unowned.is_empty() {
                say!("Every Namespace on the Servers that answered is in a Project.");
            }
            for namespace in &unowned {
                say!(
                    "{}: {} Service(s), Volumes {}",
                    namespace.namespace,
                    namespace.services.len(),
                    volume_names(&namespace.volumes)
                );
            }
            if let Some(next) = &next {
                say!("Remove one: {next}");
            }
            unanswered(&gaps);
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
                "next": shell_words::join([
                    "ployz", "env", "rm", owner.environment.as_str(),
                    "--project", owner.project.as_str(),
                ]),
            }),
        ));
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
        unanswered(&gaps);
        return Err(Error::detailed(
            RpcErrorCode::Unavailable,
            format!(
                "Namespace {namespace} isn't on the Servers that answered, but some didn't; \
                 run the same command again"
            ),
            json!({
                "namespace": namespace,
                "failures": gaps.failures,
                "omitted": gaps.omitted,
            }),
        ));
    };
    if !confirmed(matches, namespace.as_str(), "Namespace")? {
        let next = retry(matches, &namespace);
        return Err(Error::detailed(
            RpcErrorCode::ConfirmationRequired,
            format!(
                "Removing Namespace {namespace} deletes its containers and the data of Volumes \
                 {}; this can't be undone. No changes made.\nRetry: {next}",
                volume_names(&found.volumes)
            ),
            json!({
                "namespace": namespace,
                "services": found.services,
                "volumes": found.volumes,
                "next": next,
            }),
        ));
    }
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
    crate::output::finish(&report, || match removed {
        true => say!(
            "Removed Namespace {} and the data of Volumes {}.",
            report.namespace,
            volume_names(&report.volumes)
        ),
        false => say!(
            "Removing Namespace {} did not finish; run the same command again.",
            report.namespace
        ),
    })?;
    match removed {
        true => Ok(()),
        false => Err(Error::partial()),
    }
}

/// Name the Servers whose Namespaces went unseen.
fn unanswered(gaps: &Gaps) {
    let unseen: Vec<_> = gaps
        .failures
        .iter()
        .map(|failure| failure.machine_id)
        .chain(gaps.omitted.iter().copied())
        .collect();
    if !unseen.is_empty() {
        say!(
            "Servers {} didn't answer; what they run isn't listed.",
            super::super::joined(&unseen)
        );
    }
}

fn owner<'a>(owned: &'a [OwnedNamespace], namespace: &Namespace) -> Option<&'a OwnedNamespace> {
    owned.iter().find(|owned| &owned.namespace == namespace)
}

/// Removing `namespace`, reaching the same Servers as this command.
fn retry(matches: &ArgMatches, namespace: &Namespace) -> String {
    let mut words = vec!["ployz", "server", "clean"];
    for flag in ["context", "connect"] {
        if let Some(value) = matches.get_one::<String>(flag) {
            words.extend([if flag == "context" { "--context" } else { "--connect" }, value]);
        }
    }
    words.extend(["--namespace", namespace.as_str(), "--confirm", namespace.as_str()]);
    shell_words::join(words)
}

fn volume_names(volumes: &[DockerVolumeId]) -> String {
    if volumes.is_empty() {
        return "(none)".to_owned();
    }
    let names: Vec<_> = volumes.iter().map(|volume| &volume.name).collect();
    super::super::joined(&names)
}
