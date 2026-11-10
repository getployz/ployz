//! `ployz github preview`: a Device's standing approval of previews of an Environment,
//! so a Cluster whose Servers enforce device approval deploys each trusted pull
//! request's preview with no prompt.
//!
//! Approve asks Cloud to rehearse opening a preview, shows the person what every
//! preview will run and what it shares with the Environment, and signs a rule with this
//! CLI's Device key. Each Server derives a preview's Namespace and hostnames itself and
//! checks the rule, the builder's GitHub tokens and the settings before it runs
//! anything.

use std::collections::BTreeSet;

use clap::{Arg, ArgAction, ArgMatches, Command};
use ployz_core::authority::preview::{secrets_digest, template};
use ployz_core::authority::rule::{PolicyCache, RuleEvent, RuleSource, Runner};
use ployz_core::authority::rule_draft::{WORKFLOW, settings};
use ployz_core::authority::{
    BuildPolicy, PreviewRule, PreviewService, RuleId, Signed, SignerKey, Timestamp,
};
use ployz_core::{RpcErrorCode, SealedRecord, ServiceName};
use ployz_store::{ApprovePreviewRule, PlanPreviewRule, PreviewRuleView, RuleQuery};
use serde::Serialize;

use super::github_rule::{builder, enforcing, joined, owner, signer};
use super::store::{self, Store};
use super::{Error, leaf_matches};
use crate::cli::value;

pub(super) fn group(command: Command) -> Command {
    command.about("Deploy previews of pull requests on servers that enforce device approval")
}

pub(super) fn approve_command(command: Command) -> Command {
    store::scoped(command)
        .about("Approve previews of pull requests that change these services")
        .long_about(
            "Sign a preview rule with this device's key: a pull request from the repository by \
             an account you trust opens a preview with the settings shown, built by the pinned \
             builder, with no prompt. Each preview gets its own namespace and URLs, which your \
             servers work out themselves. A change to those settings stops previews until you \
             approve again.",
        )
        .arg(
            Arg::new("service")
                .required(true)
                .action(ArgAction::Append)
                .value_name("SERVICE")
                .help("A service that builds from GitHub"),
        )
        .arg(
            value("trust", None)
                .action(ArgAction::Append)
                .value_name("GITHUB_ID")
                .help(
                    "A GitHub account ID whose pull requests open a preview without review \
                     [default: your organization's members]",
                ),
        )
        .arg(
            Arg::new("share-mounts")
                .long("share-mounts")
                .action(ArgAction::SetTrue)
                .help("Let previews mount this environment's volumes they don't copy"),
        )
        .arg(
            value("builder", None).value_name("WORKFLOW@REF").help(
                "Another builder's reusable workflow, as GitHub names it in job_workflow_ref",
            ),
        )
        .arg(
            value("builder-sha", None)
                .value_name("SHA")
                .help("That builder's commit. Give it with --builder"),
        )
        .arg(
            value("platform", None)
                .value_name("PLATFORM")
                .help("What GitHub builds for [default: linux/amd64]"),
        )
}

pub(super) fn end_command(command: Command) -> Command {
    store::scoped(command)
        .about("Stop opening previews automatically")
        .long_about(
            "Sign the preview rule's last generation with this device's key. Your servers refuse \
             every build and deploy under the rule from then on.",
        )
}

/// What the command did, for `--json`.
#[derive(Serialize)]
struct Approved<'a> {
    rule: RuleId,
    generation: u64,
    ended: bool,
    repository: &'a str,
    domain: &'a str,
    trusted: &'a [u64],
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
    let trust: Vec<u64> = matches
        .get_many::<String>("trust")
        .into_iter()
        .flatten()
        .map(|id| {
            id.parse()
                .map_err(|_| Error::usage(format!("{id} isn't a GitHub account ID")))
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
    let saved = store.read(&RuleQuery {
        environment: environment.clone(),
    })?;
    let (repository, branch, repository_id) = one_repository(&saved.sources, &names)?;
    let owner = owner(root, &repository, repository_id)?;
    // A plan's templates hold no Namespace, so any rule ID rehearses the same preview.
    let view = store.write(&PlanPreviewRule {
        environment: environment.clone(),
        repository_id,
        rule: RuleId::from_bytes(*uuid::Uuid::new_v4().as_bytes()),
    })?;
    let domain = view.domain.clone().ok_or_else(|| {
        Error::coded(
            RpcErrorCode::Conflict,
            "Previews need your organization's generated domain. Add a domain to a service, \
             then approve again.",
        )
    })?;
    let shared: Vec<&ServiceName> = names
        .iter()
        .filter(|name| view.shared.contains_key(*name))
        .collect();
    if !shared.is_empty() && !matches.get_flag("share-mounts") {
        return Err(Error::coded(
            RpcErrorCode::Conflict,
            format!(
                "Previews of {} would mount this environment's volumes, not copies of their own. \
                 Copy those volumes in the PR plan, or approve again with --share-mounts.",
                shared
                    .iter()
                    .map(|name| name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    let mut services = Vec::new();
    for name in &names {
        services.push(preview_service(&view, name, &platform)?);
    }
    services.sort_by(|left, right| left.name.cmp(&right.name));
    let (id, generation) = next(&view);
    let mut trusted = if trust.is_empty() {
        view.members.clone()
    } else {
        trust
    };
    trusted.sort_unstable();
    trusted.dedup();
    let rule = PreviewRule {
        cluster: enforcing.cluster,
        rule: id,
        generation,
        machines: enforcing.machines,
        parent: view.namespace.clone(),
        parent_id: uuid::Uuid::parse_str(view.environment.id.as_str()).map_err(Error::command)?,
        domain: domain.clone(),
        source: RuleSource {
            repository_id,
            owner_id: owner,
            repository: repository.clone(),
            branch,
            workflow: WORKFLOW.to_owned(),
            events: vec![RuleEvent::Push, RuleEvent::WorkflowDispatch],
            builder,
            runner: Runner::GithubHosted,
        },
        trusted,
        services,
        issued_at: Timestamp::now(),
        ended: false,
        signer: SignerKey::Device(signer.public().into()),
    };
    show(&view, &rule, &names);
    keep(&store, environment, &rule, &signer)?;
    crate::ui::stream(format_args!(
        "Approved. Pull requests to {repository} by the accounts shown now open a preview of {}.",
        joined(&names)
    ));
    crate::ui::emit(&Approved {
        rule: rule.rule,
        generation: rule.generation,
        ended: false,
        repository: &repository,
        domain: &domain,
        trusted: &rule.trusted,
        services: names.iter().collect(),
    })?;
    Ok(())
}

pub(super) fn end(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let signer = signer(root)?;
    let store = store::store(root)?;
    let environment = store::environment(matches)?;
    let saved = store.read(&RuleQuery {
        environment: environment.clone(),
    })?;
    let Some(source) = saved.sources.first() else {
        return Err(not_found(&saved.environment.name));
    };
    let view = store.write(&PlanPreviewRule {
        environment: environment.clone(),
        repository_id: source.repository_id,
        rule: RuleId::from_bytes(*uuid::Uuid::new_v4().as_bytes()),
    })?;
    let Some(kept) = view.rule.as_ref().and_then(|rule| rule.open().ok()) else {
        return Err(not_found(&view.environment.name));
    };
    if kept.ended {
        crate::ui::stream("Previews are already off for this environment.");
        return Ok(());
    }
    let rule = PreviewRule {
        generation: kept.generation + 1,
        issued_at: Timestamp::now(),
        ended: true,
        signer: SignerKey::Device(signer.public().into()),
        ..kept
    };
    keep(&store, environment, &rule, &signer)?;
    crate::ui::stream(
        "Previews are off. Pull requests no longer open a preview on servers that enforce \
         device approval.",
    );
    crate::ui::emit(&Approved {
        rule: rule.rule,
        generation: rule.generation,
        ended: true,
        repository: &rule.source.repository,
        domain: &rule.domain,
        trusted: &rule.trusted,
        services: rule.services.iter().map(|service| &service.name).collect(),
    })?;
    Ok(())
}

fn not_found(environment: &ployz_store::EnvironmentName) -> Error {
    Error::coded(
        RpcErrorCode::NotFound,
        format!("{environment} has no preview rule to end."),
    )
}

/// The one repository every named Service builds from, and the Environment's branch,
/// where reviewed builds run.
fn one_repository(
    sources: &[ployz_store::RuleSourceView],
    names: &[ServiceName],
) -> Result<(String, String, u64), Error> {
    let mut found: Option<(String, String, u64)> = None;
    for name in names {
        let source = sources
            .iter()
            .find(|source| source.service == *name)
            .ok_or_else(|| {
                Error::coded(
                    RpcErrorCode::InvalidArgument,
                    format!("{name} doesn't build from GitHub, so a preview can't build it."),
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
            Some(first) if first.2 == this.2 && first.1 == this.1 => {}
            Some((repository, ..)) => {
                return Err(Error::coded(
                    RpcErrorCode::InvalidArgument,
                    format!(
                        "A preview rule covers one repository and branch, and {name} builds from \
                         {} at {}, not {repository}. Approve a rule for each.",
                        this.0, this.1
                    ),
                ));
            }
        }
    }
    found.ok_or_else(|| Error::usage("Name at least one service"))
}

/// What the rule approves for `name` in every preview: the rehearsed preview's settings
/// with its derived names put back, with and without its Setup Commands, the build
/// policy, and the build's Sealed values, which only the parent's can be.
fn preview_service(
    view: &PreviewRuleView,
    name: &ServiceName,
    platform: &str,
) -> Result<PreviewService, Error> {
    let spec = view
        .target
        .iter()
        .find(|spec| spec.name == *name)
        .ok_or_else(|| {
            Error::coded(
                RpcErrorCode::NotFound,
                format!("{name} isn't in a preview of this environment. Add it to the PR plan."),
            )
        })?;
    let source = view
        .sources
        .iter()
        .find(|source| source.service == *name)
        .ok_or_else(|| {
            Error::coded(
                RpcErrorCode::InvalidArgument,
                format!("{name} doesn't build from GitHub, so a preview can't build it."),
            )
        })?;
    let policy = BuildPolicy::derive(
        &source.root_dir,
        &source.build,
        spec,
        BTreeSet::from([platform.to_owned()]),
        PolicyCache::Actions,
    );
    let secrets = SealedRecord::of(spec)
        .map_err(Error::command)?
        .map(|record| secrets_digest(&record))
        .transpose()
        .map_err(Error::command)?;
    let mut service = PreviewService {
        name: name.clone(),
        // Replaced below: a template reads only the generators and inherited keys.
        template: ployz_core::authority::PayloadDigest::parse(&"0".repeat(64))
            .map_err(Error::command)?,
        setup: None,
        policy,
        secrets,
        inherit: view.inherited.get(name).cloned().unwrap_or_default(),
        generated: view.generated.get(name).cloned().unwrap_or_default(),
    };
    service.template = template(spec, &view.sample, &service).map_err(Error::command)?;
    service.setup = view
        .setup
        .iter()
        .find(|setup| setup.name == *name)
        .map(|setup| template(setup, &view.sample, &service))
        .transpose()
        .map_err(Error::command)?;
    Ok(service)
}

/// The kept rule's next generation, or a new rule's first.
fn next(view: &PreviewRuleView) -> (RuleId, u64) {
    match view.rule.as_ref().and_then(|rule| rule.open().ok()) {
        Some(kept) => (kept.rule, kept.generation + 1),
        None => (RuleId::from_bytes(*uuid::Uuid::new_v4().as_bytes()), 1),
    }
}

fn keep(
    store: &Store<'_>,
    environment: ployz_store::EnvironmentRef,
    rule: &PreviewRule,
    signer: &ployz_core::authority::Signer,
) -> Result<(), Error> {
    let signed = Signed::sign(rule, signer).map_err(Error::command)?;
    store.write(&ApprovePreviewRule {
        environment,
        rule: Box::new(signed),
    })?;
    Ok(())
}

/// What the person approves, before it is signed: who opens previews without review,
/// what each preview reads from the Environment, and where it is served.
fn show(view: &PreviewRuleView, rule: &PreviewRule, names: &[ServiceName]) {
    let source = &rule.source;
    crate::ui::stream(format_args!(
        "Approving previews of {}:",
        view.environment.name
    ));
    crate::ui::stream(format_args!("  repository  {}", source.repository));
    let builder = &source.builder;
    crate::ui::stream(format_args!("  builder     {}", builder.r#ref));
    crate::ui::stream(format_args!(
        "  urls        <service>-pv-<id>.{}",
        rule.domain
    ));
    crate::ui::stream(format_args!(
        "  trusted     {}",
        match rule.trusted.is_empty() {
            true => "nobody: every pull request needs a review".to_owned(),
            false => rule
                .trusted
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        }
    ));
    crate::ui::stream(format_args!(
        "  servers     {} in the cluster",
        rule.machines.len()
    ));
    for name in names {
        let Some(approved) = rule.service(name) else {
            continue;
        };
        crate::ui::stream(format_args!("  {name}"));
        if !approved.inherit.is_empty() {
            crate::ui::stream(format_args!(
                "    reads     {} from {}",
                approved
                    .inherit
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
                view.environment.name
            ));
        }
        if let Some(shared) = view.shared.get(name) {
            crate::ui::stream(format_args!(
                "    shares    {} with {}",
                shared.iter().cloned().collect::<Vec<_>>().join(", "),
                view.environment.name
            ));
        }
        if approved.setup.is_some() {
            crate::ui::stream("    setup     runs once, on each preview's first deploy");
        }
        if let Some(spec) = view.target.iter().find(|spec| spec.name == *name) {
            // A Generated Value shows by name: the rehearsal's value is no preview's.
            let mut shown = spec.clone();
            shown
                .container
                .environment
                .retain(|key, _| !approved.generated.contains_key(key));
            // The rehearsal's Namespace is no preview's, so its hostnames show the derived part as `<id>`.
            for line in settings(&shown) {
                let line = line.replace(view.sample.as_str(), "pv-<id>");
                crate::ui::stream(format_args!("    {line}"));
            }
        }
        if !approved.generated.is_empty() {
            crate::ui::stream(format_args!(
                "    generated {}, new in each preview",
                approved
                    .generated
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
}
