//! `ployz github rule`: a Device's standing approval of every push to one branch, so a
//! Cluster whose Servers enforce device approval deploys each push with no prompt.
//!
//! Approve reads the Environment's Saved settings from Cloud, shows the person what the
//! rule pins, and signs it with this CLI's Device key. Cloud only keeps and relays the
//! signed rule: each Server checks the signature, the builder's GitHub tokens, the
//! branch head and the settings before it runs anything.

use std::collections::BTreeSet;

use clap::{Arg, ArgAction, ArgMatches, Command};
use ployz_core::authority::rule::{
    BUILDER_SHA, Builder, PolicyCache, RuleEvent, RuleService, RuleSource, Runner, template,
};
use ployz_core::authority::{
    AutoDeployRule, BuildPolicy, CommitSha, RuleId, SealedDigest, Signed, Signer, SignerKey,
    Timestamp,
};
use ployz_core::{RequestedServiceSpec, RpcErrorCode, SealedRecord, ServiceName};
use ployz_store::{ApproveRule, RuleQuery, RuleView};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::store::{self, Store};
use super::{Error, deploy::approve, leaf_matches};
use crate::cli::value;

/// The caller workflow every rule builds with, as `ployz github` sets it up.
const WORKFLOW: &str = ".github/workflows/ployz-build.yml";

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
    let (repository, branch, repository_id) = one_branch(&view, &names)?;
    let owner = owner(root, &repository, repository_id)?;
    let mut services = Vec::new();
    for name in &names {
        services.push(rule_service(&view, name, &platform)?);
    }
    services.sort_by(|left, right| left.name.cmp(&right.name));
    let (id, generation) = next(&view);
    let rule = AutoDeployRule {
        cluster: enforcing.cluster,
        rule: id,
        generation,
        namespace: view.namespace.clone(),
        machines: enforcing.machines,
        source: RuleSource {
            repository_id,
            owner_id: owner,
            repository: repository.clone(),
            branch: branch.clone(),
            workflow: WORKFLOW.to_owned(),
            events: vec![RuleEvent::Push],
            builder,
            runner: Runner::GithubHosted,
        },
        services,
        issued_at: Timestamp::now(),
        ended: false,
        signer: SignerKey::Device(signer.public().into()),
    };
    show(&view, &rule, &names);
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
    let Some(kept) = view.rule.as_ref().and_then(|rule| rule.open().ok()) else {
        return Err(Error::coded(
            RpcErrorCode::NotFound,
            format!(
                "{} has no automatic deploy rule to end.",
                view.environment.name
            ),
        ));
    };
    if kept.ended {
        crate::ui::stream("Automatic deploys are already off for this environment.");
        return Ok(());
    }
    let rule = AutoDeployRule {
        generation: kept.generation + 1,
        issued_at: Timestamp::now(),
        ended: true,
        signer: SignerKey::Device(signer.public().into()),
        ..kept
    };
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
fn signer(root: &ArgMatches) -> Result<Signer, Error> {
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

fn builder(matches: &ArgMatches) -> Result<Builder, Error> {
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

/// The Cluster's key and Servers, from the Servers themselves: Cloud can't name them.
fn enforcing(root: &ArgMatches) -> Result<approve::Enforcing, Error> {
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

/// The one repository and branch every named Service builds from.
fn one_branch(view: &RuleView, names: &[ServiceName]) -> Result<(String, String, u64), Error> {
    let mut found: Option<(String, String, u64)> = None;
    for name in names {
        let source = view
            .sources
            .iter()
            .find(|source| source.service == *name)
            .ok_or_else(|| {
                Error::coded(
                    RpcErrorCode::InvalidArgument,
                    format!("{name} doesn't build from GitHub, so a rule can't deploy it."),
                )
            })?;
        let branch = source.branch.clone().ok_or_else(|| {
            Error::coded(
                RpcErrorCode::InvalidArgument,
                format!("{name}'s branch is disconnected. Connect it, then approve again."),
            )
        })?;
        let this = (source.repository.clone(), branch, source.repository_id);
        match &found {
            None => found = Some(this),
            Some(first) if *first == this => {}
            Some((repository, branch, _)) => {
                return Err(Error::coded(
                    RpcErrorCode::InvalidArgument,
                    format!(
                        "A rule deploys one branch, and {name} builds from {} at {}, not \
                         {repository} at {branch}. Approve a rule for each.",
                        this.0, this.1
                    ),
                ));
            }
        }
    }
    found.ok_or_else(|| Error::usage("Name at least one service"))
}

#[derive(Deserialize)]
struct Repository {
    repository_id: u64,
    owner_id: u64,
}

/// The repository owner's GitHub ID, which each GitHub token the rule takes must name.
fn owner(root: &ArgMatches, repository: &str, repository_id: u64) -> Result<u64, Error> {
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

/// What the rule approves for `name`: its Saved settings less the image, the build
/// policy the runner must build with, and its build's Sealed values.
fn rule_service(view: &RuleView, name: &ServiceName, platform: &str) -> Result<RuleService, Error> {
    let spec = spec(view, name)?;
    let source = view
        .sources
        .iter()
        .find(|source| source.service == *name)
        .expect("one_branch found every source");
    let policy = BuildPolicy::derive(
        &source.root_dir,
        &source.build,
        spec,
        BTreeSet::from([platform.to_owned()]),
        PolicyCache::Actions,
    );
    let secrets = SealedRecord::of(spec)
        .map_err(Error::command)?
        .map(|record| SealedDigest::of(&record))
        .transpose()
        .map_err(Error::command)?;
    Ok(RuleService {
        name: name.clone(),
        template: template(spec, &view.namespace).map_err(Error::command)?,
        policy,
        secrets,
    })
}

fn spec<'a>(view: &'a RuleView, name: &ServiceName) -> Result<&'a RequestedServiceSpec, Error> {
    view.target
        .iter()
        .find(|spec| spec.name == *name)
        .ok_or_else(|| {
            Error::coded(
                RpcErrorCode::NotFound,
                format!("{name} isn't saved in this environment. Publish it, then approve again."),
            )
        })
}

/// The kept rule's next generation, or a new rule's first.
fn next(view: &RuleView) -> (RuleId, u64) {
    match view.rule.as_ref().and_then(|rule| rule.open().ok()) {
        Some(kept) => (kept.rule, kept.generation + 1),
        None => (RuleId::from_bytes(*uuid::Uuid::new_v4().as_bytes()), 1),
    }
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

/// What the person approves, before it is signed.
fn show(view: &RuleView, rule: &AutoDeployRule, names: &[ServiceName]) {
    let source = &rule.source;
    crate::ui::stream(format_args!(
        "Approving automatic deploys for {}:",
        view.environment.name
    ));
    crate::ui::stream(format_args!(
        "  repository  {} on {}",
        source.repository, source.branch
    ));
    crate::ui::stream(format_args!("  workflow    {}, on push", source.workflow));
    let builder = &source.builder;
    let sha = builder.sha.to_string();
    match builder.r#ref.ends_with(&sha) {
        true => crate::ui::stream(format_args!("  builder     {}", builder.r#ref)),
        false => crate::ui::stream(format_args!("  builder     {} at {sha}", builder.r#ref)),
    }
    crate::ui::stream(format_args!(
        "  servers     {} in the cluster",
        rule.machines.len()
    ));
    if !view.changed.is_empty() {
        crate::ui::stream(format_args!(
            "  changed     {} since the last approval",
            joined(&view.changed)
        ));
    }
    for name in names {
        let Some(approved) = rule.service(name) else {
            continue;
        };
        crate::ui::stream(format_args!("  {name}"));
        let policy = &approved.policy;
        let method = match &policy.method {
            ployz_core::authority::rule::PolicyMethod::Dockerfile { path } => path.clone(),
            ployz_core::authority::rule::PolicyMethod::Railpack { .. } => "Railpack".to_owned(),
        };
        crate::ui::stream(format_args!(
            "    builds    {method} in {}, for {}{}",
            policy.context,
            policy
                .platforms
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
            match policy.cache {
                PolicyCache::Actions => ", with the Actions cache",
                PolicyCache::Off => "",
            }
        ));
        if let Ok(spec) = spec(view, name) {
            for line in settings(spec) {
                crate::ui::stream(format_args!("    {line}"));
            }
        }
    }
    if rule.source.builder.sha.to_string() != BUILDER_SHA {
        crate::ui::note("This rule pins a builder other than Ployz's own release.");
    }
}

/// The settings a person reads before approving: each variable, Sealed ones by name only,
/// and what reaches the host.
fn settings(spec: &RequestedServiceSpec) -> Vec<String> {
    let container = &spec.container;
    let mut lines = Vec::new();
    let variables: Vec<String> = container
        .environment
        .iter()
        .map(|(name, value)| match value.text() {
            Some(text) => format!("{name}={text}"),
            None => format!("{name} (sealed)"),
        })
        .collect();
    if !variables.is_empty() {
        lines.push(format!("variables {}", variables.join(", ")));
    }
    if let Some(hook) = &spec.pre_deploy {
        lines.push(format!(
            "pre-deploy {}",
            serde_json::to_string(&hook.command).unwrap_or_default()
        ));
    }
    if !spec.ports.is_empty() {
        lines.push(format!(
            "host ports {}",
            serde_json::to_string(&spec.ports).unwrap_or_default()
        ));
    }
    if container.privileged {
        lines.push("privileged".to_owned());
    }
    lines
}

fn joined(names: &[ServiceName]) -> String {
    names
        .iter()
        .map(ServiceName::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}
