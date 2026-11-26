//! `ployz github rule`: ending the standing approval of every push to one branch, and what
//! `ployz deploy` shows and reads when it signs that approval with this CLI's Device key.
//! Cloud only keeps and relays the signed rule: each Server checks the signature, the
//! builder's GitHub tokens, the branch head and the settings before it runs anything.

use clap::{ArgMatches, Command};
use ployz_core::authority::rule::Builder;
use ployz_core::authority::shown::Shown;
use ployz_core::authority::{
    AutoDeployRule, Births, RuleId, RuleSummary, Signed, Signer, SignerKey, Timestamp,
};
use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{ApproveRule, RuleQuery};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::store;
use super::{Error, leaf_matches};

pub(super) fn group(command: Command) -> Command {
    command
        .about("Deploy every push of a branch on servers that enforce device approval")
        .after_help("Turn it on with Deploy every push in Ployz Cloud.")
}

/// The verb an older script still calls, kept hidden: it says where turning the rule on went.
pub(super) fn approve_command(command: Command) -> Command {
    command.hide(true).arg(
        clap::Arg::new("args")
            .num_args(0..)
            .trailing_var_arg(true)
            .allow_hyphen_values(true),
    )
}

pub(super) fn approve(_: &ArgMatches) -> Result<(), Error> {
    Err(Error::coded(
        RpcErrorCode::InvalidArgument,
        ployz_store::RULE_APPROVE_GONE,
    ))
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
struct Ended<'a> {
    rule: RuleId,
    generation: u64,
    ended: bool,
    repository: &'a str,
    branch: &'a str,
    builder: &'a Builder,
    services: Vec<&'a ServiceName>,
}

pub(super) fn end(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let signer = signer(root)?;
    let store = store::store(root)?;
    let environment = store::environment(matches)?;
    let view = store.read(&RuleQuery {
        environment: environment.clone(),
        staged: false,
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
    store.write(&ApproveRule {
        environment,
        rule: Box::new(Signed::sign(&rule, &signer).map_err(Error::command)?),
        births: Births::new(),
    })?;
    crate::ui::stream(format_args!(
        "Automatic deploys are off. Pushes to {} no longer deploy on servers that enforce \
         device approval.",
        rule.source.branch
    ));
    crate::ui::emit(&Ended {
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
fn signer(root: &ArgMatches) -> Result<Signer, Error> {
    let key = crate::device::key::path(&super::config_path(root)?);
    match crate::device::key::load(&key)? {
        Some(secret) => Ok(Signer::from_seed(&secret)),
        None => Err(Error::coded(
            RpcErrorCode::Unauthenticated,
            "This CLI has no device key, so it can't sign a rule. Run ployz device request, \
             then approve it from a device your servers trust.",
        )),
    }
}

#[derive(Deserialize)]
struct Repository {
    repository_id: u64,
    owner_id: u64,
}

/// The repository owner's GitHub ID, which each GitHub token the rule takes must name.
/// `then` is what the person runs once they reconnect, such as `deploy again`.
pub(super) fn owner(
    root: &ArgMatches,
    (repository, repository_id): (&str, u64),
    then: &str,
) -> Result<u64, Error> {
    let found: Repository =
        super::github::read_repository(root, "branches", &json!({ "repository": repository }))?;
    if found.repository_id != repository_id {
        return Err(Error::coded(
            RpcErrorCode::Conflict,
            format!(
                "{repository} now names another repository. Connect your services to it again, then {then}."
            ),
        ));
    }
    Ok(found.owner_id)
}

/// What the person approves, before it is signed. The summary's strings came through
/// [`Shown`] already.
pub(super) fn show(environment: &str, summary: &RuleSummary) {
    crate::ui::stream(format_args!(
        "Approving automatic deploys for {}:",
        Shown::text(environment)
    ));
    crate::ui::stream(format_args!("  namespace   {}", summary.namespace));
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
