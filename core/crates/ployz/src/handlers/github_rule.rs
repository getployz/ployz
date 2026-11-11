//! `ployz github rule`: a Device's standing approval of every push to one branch, so a
//! Cluster whose Servers enforce device approval deploys each push with no prompt.
//!
//! Approve reads the Environment's Saved settings from Cloud, drafts the rule the way
//! the dashboard does, shows the person what it pins, and signs it with this CLI's Device
//! key. Cloud only keeps and relays the signed rule: each Server checks the signature,
//! the builder's GitHub tokens, the branch head and the settings before it runs anything.

use clap::{Arg, ArgAction, ArgMatches, Command};
use ployz_core::authority::rule::{BUILDER_SHA, Builder};
use ployz_core::authority::rule_draft::RuleDraftError;
use ployz_core::authority::shown::Shown;
use ployz_core::authority::{
    AutoDeployRule, CommitSha, RuleId, RuleSummary, RuleTerms, Signed, Signer, SignerKey, Timestamp,
};
use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{ApproveRule, RuleQuery};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::store::{self, Store};
use super::{Error, deploy::approve, leaf_matches};
use crate::cli::value;

pub(super) fn group(command: Command) -> Command {
    command.about("Deploy every push of a branch on servers that enforce device approval")
}

pub(super) fn approve_command(command: Command) -> Command {
    store::scoped(command)
        .about("Approve deploying every push of these services' branch with their current settings")
        .long_about(
            "Sign an automatic deploy rule with this device's key: every push to the services' \
             branch deploys with the settings shown, built by the pinned builder, with no prompt. \
             A change to those settings stops automatic deploys until you approve again. Your \
             servers check the rule, GitHub's tokens and the branch head themselves.",
        )
        .arg(
            Arg::new("service")
                .required(true)
                .action(ArgAction::Append)
                .value_name("SERVICE")
                .help("A service that builds from GitHub"),
        )
        .arg(
            value("builder", None)
                .value_name("WORKFLOW@REF")
                .help(format!(
                    "Another builder's reusable workflow, as GitHub names it in job_workflow_ref \
             [default: {}]",
                    Builder::pinned().r#ref
                )),
        )
        .arg(value("builder-sha", None).value_name("SHA").help(format!(
            "That builder's commit. Give it with --builder [default: {BUILDER_SHA}]"
        )))
        .arg(
            value("platform", None)
                .value_name("PLATFORM")
                .help("What GitHub builds for [default: linux/amd64]"),
        )
}

pub(super) fn end_command(command: Command) -> Command {
    store::scoped(command)
        .about("Stop deploying every push automatically")
        .long_about(
            "Sign the automatic deploy rule's last generation with this device's key. Your \
             servers refuse every build and deploy under the rule from then on.",
        )
}

/// What the command did, for `--json`.
#[derive(Serialize)]
struct Approved<'a> {
    rule: RuleId,
    generation: u64,
    ended: bool,
    repository: &'a str,
    branch: &'a str,
    builder: &'a Builder,
    services: Vec<&'a ServiceName>,
}

pub(super) fn approve(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let signer = signer(root)?;
    let names: Vec<ServiceName> = matches
        .get_many::<String>("service")
        .into_iter()
        .flatten()
        .map(|name| {
            ServiceName::parse(name.as_str())
                .map_err(|_| Error::usage(format!("{name} isn't a service name")))
        })
        .collect::<Result<_, _>>()?;
    let builder = builder(matches)?;
    let platform = matches
        .get_one::<String>("platform")
        .cloned()
        .unwrap_or_else(|| "linux/amd64".to_owned());
    let store = store::store(root)?;
    let environment = store::environment(matches)?;
    let enforcing = enforcing(root)?;
    let view = store.read(&RuleQuery {
        environment: environment.clone(),
    })?;
    let basis = view.basis();
    let terms = RuleTerms {
        cluster: enforcing.cluster,
        machines: enforcing.machines,
        owner_id: 0,
        builder,
        platform,
        new_rule: RuleId::from_bytes(*uuid::Uuid::new_v4().as_bytes()),
        issued_at: Timestamp::now(),
        signer: SignerKey::Device(signer.public().into()),
    };
    let mut rule = AutoDeployRule::draft(&basis, &names, terms).map_err(drafting)?;
    rule.source.owner_id = owner(root, &rule.source.repository, rule.source.repository_id)?;
    let repository = rule.source.repository.clone();
    let branch = rule.source.branch.clone();
    show(view.environment.name.as_str(), &rule.summary(&basis));
    keep(&store, environment, &rule, &signer)?;
    crate::ui::stream(format_args!(
        "Approved. Every push to {branch} now deploys {} with these settings.",
        joined(&names)
    ));
    crate::ui::emit(&Approved {
        rule: rule.rule,
        generation: rule.generation,
        ended: false,
        repository: &repository,
        branch: &branch,
        builder: &rule.source.builder,
        services: names.iter().collect(),
    })?;
    Ok(())
}

pub(super) fn end(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let signer = signer(root)?;
    let store = store::store(root)?;
    let environment = store::environment(matches)?;
    let view = store.read(&RuleQuery {
        environment: environment.clone(),
    })?;
    let basis = view.basis();
    if basis.kept().is_some_and(|kept| kept.ended) {
        crate::ui::stream("Automatic deploys are already off for this environment.");
        return Ok(());
    }
    let rule = AutoDeployRule::ending(
        &basis,
        Timestamp::now(),
        SignerKey::Device(signer.public().into()),
    )
    .map_err(|_| {
        Error::coded(
            RpcErrorCode::NotFound,
            format!(
                "{} has no automatic deploy rule to end.",
                view.environment.name
            ),
        )
    })?;
    keep(&store, environment, &rule, &signer)?;
    crate::ui::stream(format_args!(
        "Automatic deploys are off. Pushes to {} no longer deploy on servers that enforce \
         device approval.",
        rule.source.branch
    ));
    crate::ui::emit(&Approved {
        rule: rule.rule,
        generation: rule.generation,
        ended: true,
        repository: &rule.source.repository,
        branch: &rule.source.branch,
        builder: &rule.source.builder,
        services: rule.services.iter().map(|service| &service.name).collect(),
    })?;
    Ok(())
}

/// This CLI's Device signer. Only a Device signs a rule: it stands for every future push.
pub(super) fn signer(root: &ArgMatches) -> Result<Signer, Error> {
    let key = super::device::key::path(&super::config_path(root)?);
    match super::device::key::load(&key) {
        Ok(Some(secret)) => Ok(Signer::from_seed(&secret)),
        _ => Err(Error::coded(
            RpcErrorCode::Unauthenticated,
            "This CLI has no device key, so it can't sign a rule. Run ployz device request, \
             then approve it from a device your servers trust.",
        )),
    }
}

pub(super) fn builder(matches: &ArgMatches) -> Result<Builder, Error> {
    match (
        matches.get_one::<String>("builder"),
        matches.get_one::<String>("builder-sha"),
    ) {
        (None, None) => Ok(Builder::pinned()),
        (Some(r#ref), Some(sha)) => Ok(Builder {
            r#ref: r#ref.clone(),
            sha: CommitSha::parse(sha)
                .map_err(|_| Error::usage("--builder-sha must be a full lowercase Git SHA"))?,
        }),
        _ => Err(Error::usage(
            "Give --builder and --builder-sha together, or neither",
        )),
    }
}

/// The Cluster's key and Servers, as the Servers report them, through Cloud's relay when
/// signed in. A key or Server Cloud gets wrong can't widen the rule: each Server admits
/// it only when it names that Server's own Cluster key and Machine ID.
pub(super) fn enforcing(root: &ArgMatches) -> Result<approve::Enforcing, Error> {
    match approve::probe(root) {
        approve::Probe::Enforcing(enforcing) => Ok(enforcing),
        approve::Probe::Off => Err(Error::coded(
            RpcErrorCode::Conflict,
            "Your servers don't enforce device approval, so pushes already deploy without a \
             rule. Turn it on with ployz device enforce, then approve the rule.",
        )),
        approve::Probe::NoClusterKey => Err(Error::coded(
            RpcErrorCode::Unavailable,
            "None of your servers reported the cluster's key, so this device can't sign a \
             rule. Run ployz status, then try again.",
        )),
        approve::Probe::Unknown => Err(Error::coded(
            RpcErrorCode::Unavailable,
            "Couldn't reach your servers to read the cluster's key. Check ployz status, then \
             try again.",
        )),
    }
}

#[derive(Deserialize)]
struct Repository {
    repository_id: u64,
    owner_id: u64,
}

/// The repository owner's GitHub ID, which each GitHub token the rule takes must name.
pub(super) fn owner(root: &ArgMatches, repository: &str, repository_id: u64) -> Result<u64, Error> {
    let found: Repository =
        super::github::read_repository(root, "branches", &json!({ "repository": repository }))?;
    if found.repository_id != repository_id {
        return Err(Error::coded(
            RpcErrorCode::Conflict,
            format!(
                "{repository} now names another repository. Connect your services to it again, then approve."
            ),
        ));
    }
    Ok(found.owner_id)
}

fn keep(
    store: &Store<'_>,
    environment: ployz_store::EnvironmentRef,
    rule: &AutoDeployRule,
    signer: &Signer,
) -> Result<(), Error> {
    let signed = Signed::sign(rule, signer).map_err(Error::command)?;
    store.write(&ApproveRule {
        environment,
        rule: Box::new(signed),
    })?;
    Ok(())
}

/// Why a rule can't be drafted, as a coded error.
fn drafting(error: RuleDraftError) -> Error {
    let code = match error {
        RuleDraftError::NotSaved(_) | RuleDraftError::NoRule => RpcErrorCode::NotFound,
        RuleDraftError::Sealed(_) | RuleDraftError::Canonical(_) => return Error::command(error),
        RuleDraftError::NotGit(_)
        | RuleDraftError::Disconnected(_)
        | RuleDraftError::TwoBranches { .. }
        | RuleDraftError::Nothing => RpcErrorCode::InvalidArgument,
    };
    Error::caused(code, "Can't draft the automatic deploy rule", error)
}

/// What the person approves, before it is signed. The summary's strings came through
/// [`Shown`] already.
fn show(environment: &str, summary: &RuleSummary) {
    crate::ui::stream(format_args!(
        "Approving automatic deploys for {}:",
        Shown::text(environment)
    ));
    crate::ui::stream(format_args!(
        "  repository  {} on {}",
        summary.repository, summary.branch
    ));
    crate::ui::stream(format_args!(
        "  workflow    {}, on push and on rebuilds you approve",
        summary.workflow
    ));
    crate::ui::stream(format_args!("  builder     {}", summary.builder));
    crate::ui::stream(format_args!(
        "  servers     {} in the cluster",
        summary.servers
    ));
    let changed: Vec<&str> = summary
        .services
        .iter()
        .filter(|service| !service.changes.is_empty())
        .map(|service| service.name.as_str())
        .collect();
    if !changed.is_empty() {
        crate::ui::stream(format_args!(
            "  changed     {} since the last approval",
            changed.join(", ")
        ));
    }
    for service in &summary.services {
        crate::ui::stream(format_args!("  {}", service.name));
        crate::ui::stream(format_args!("    builds    {}", service.builds));
        if !service.changes.is_empty() {
            crate::ui::stream(format_args!("    changed   {}", service.changes.join(", ")));
        }
        for line in &service.settings {
            crate::ui::stream(format_args!("    {line}"));
        }
    }
    if !summary.pinned {
        crate::ui::note("This rule pins a builder other than Ployz's own release.");
    }
}

pub(super) fn joined(names: &[ServiceName]) -> String {
    names
        .iter()
        .map(ServiceName::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}
