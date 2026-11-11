//! `ployz github preview`: a Device's standing approval of previews of an Environment,
//! so a Cluster whose Servers enforce device approval deploys each trusted pull
//! request's preview with no prompt.
//!
//! Approve asks Cloud to rehearse opening a preview, shows the person what every
//! preview will run and what it shares with the Environment, and signs a rule with this
//! CLI's Device key. Each Server derives a preview's Namespace and hostnames itself and
//! checks the rule, the builder's GitHub tokens and the settings before it runs
//! anything.
//!
//! The accounts a rule trusts come only from `--trust`, each read from GitHub by this
//! CLI. Cloud's member rows never fill the list: a person can't judge a numeric ID Cloud
//! relays, and a planted one would build and deploy its pushes with the parent's
//! inherited values and no review.

use std::collections::BTreeSet;
use std::time::Duration;

use clap::{Arg, ArgAction, ArgMatches, Command};
use ployz_core::authority::preview::{secrets_digest, template};
use ployz_core::authority::rule::{PolicyCache, RuleEvent, RuleSource, Runner};
use ployz_core::authority::rule_draft::{WORKFLOW, builds, settings};
use ployz_core::authority::{
    BuildPolicy, PreviewRule, PreviewService, RuleId, Signed, SignerKey, Timestamp,
};
use ployz_core::{RequestedServiceSpec, RpcErrorCode, SealedRecord, ServiceName};
use ployz_store::{ApprovePreviewRule, PlanPreviewRule, PreviewRuleView, RuleQuery};
use reqwest::{StatusCode, header};
use serde::{Deserialize, Serialize};

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
             builder, with no prompt. Anyone else's pull request opens one only once you review \
             its commit. Each preview gets its own namespace and URLs, which your servers work \
             out themselves. A change to those settings stops previews until you approve again.",
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
                .value_name("ACCOUNT")
                .help(
                    "A GitHub login or numeric account ID whose pull requests open a preview \
                     without review [default: nobody, so every pull request needs a review]",
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
    trusted: &'a [Account],
    services: Vec<&'a ServiceName>,
}

/// A GitHub account as GitHub named it to this CLI. `login` is `None` when GitHub didn't
/// answer for an account this CLI only shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Account {
    id: u64,
    login: Option<String>,
}

/// An account as `--trust` names it. Digits are an account ID, as GitHub's own IDs are.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Named {
    Id(u64),
    Login(String),
}

impl Named {
    fn parse(text: &str) -> Result<Self, Error> {
        if let Ok(id) = text.parse() {
            return Ok(Self::Id(id));
        }
        let login = !text.is_empty()
            && text.len() <= 39
            && !text.starts_with('-')
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
        match login {
            true => Ok(Self::Login(text.to_owned())),
            false => Err(Error::usage(format!(
                "{text} isn't a GitHub login or account ID"
            ))),
        }
    }
}

impl std::fmt::Display for Named {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Id(id) => write!(f, "account ID {id}"),
            Self::Login(login) => write!(f, "{login}"),
        }
    }
}

/// Why GitHub didn't name an account.
#[derive(Debug, thiserror::Error)]
enum Lookup {
    #[error("GitHub has no such account")]
    NotFound,
    #[error("GitHub answered {0}")]
    Status(StatusCode),
    #[error("GitHub's answer named another account")]
    Mismatch,
    #[error("GitHub didn't answer")]
    Unavailable(#[source] reqwest::Error),
}

const GITHUB_API: &str = "https://api.github.com";

/// `named`, as GitHub's public REST API names it now. This CLI asks GitHub itself, never
/// Cloud, and checks the answer is about the account asked for.
async fn lookup(client: &reqwest::Client, named: &Named) -> Result<Account, Lookup> {
    let path = match named {
        Named::Id(id) => format!("user/{id}"),
        Named::Login(login) => format!("users/{login}"),
    };
    let response = client
        .get(format!("{GITHUB_API}/{path}"))
        .header(header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(Lookup::Unavailable)?;
    match response.status() {
        StatusCode::OK => {}
        StatusCode::NOT_FOUND => return Err(Lookup::NotFound),
        status => return Err(Lookup::Status(status)),
    }
    #[derive(Deserialize)]
    struct User {
        id: u64,
        login: String,
    }
    let user: User = response.json().await.map_err(Lookup::Unavailable)?;
    let same = match named {
        Named::Id(id) => user.id == *id,
        Named::Login(login) => user.login.eq_ignore_ascii_case(login),
    };
    match same {
        true => Ok(Account {
            id: user.id,
            login: Some(user.login),
        }),
        false => Err(Lookup::Mismatch),
    }
}

fn github() -> Result<reqwest::Client, Error> {
    reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .user_agent(concat!("ployz-cli/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(Error::command)
}

/// The accounts the rule trusts: each one `--trust` names, as GitHub names it. Every
/// one must resolve, so the person sees whom they trust before the rule is signed.
async fn trusted_accounts(
    named: &[Named],
    lookup: impl AsyncFn(&Named) -> Result<Account, Lookup>,
) -> Result<Vec<Account>, Error> {
    let mut accounts = Vec::new();
    for named in named {
        let account = lookup(named).await.map_err(|error| match error {
            Lookup::NotFound => Error::coded(
                RpcErrorCode::NotFound,
                format!("GitHub has no account {named}. Check --trust."),
            ),
            error @ (Lookup::Status(_) | Lookup::Mismatch | Lookup::Unavailable(_)) => {
                Error::caused(
                    RpcErrorCode::Unavailable,
                    format!(
                        "Couldn't read {named} from GitHub, so it can't be trusted. Try again."
                    ),
                    error,
                )
            }
        })?;
        accounts.push(account);
    }
    accounts.sort_by_key(|account| account.id);
    accounts.dedup_by_key(|account| account.id);
    Ok(accounts)
}

/// `ids` as GitHub names them, for showing only: an ID GitHub doesn't answer for shows
/// without a login.
async fn shown_accounts(
    ids: &[u64],
    lookup: impl AsyncFn(&Named) -> Result<Account, Lookup>,
) -> Vec<Account> {
    let mut accounts = Vec::new();
    for &id in ids {
        accounts.push(
            lookup(&Named::Id(id))
                .await
                .unwrap_or(Account { id, login: None }),
        );
    }
    accounts
}

/// `accounts` as a person reads them: each login with its ID.
fn trusted_line(accounts: &[Account]) -> String {
    if accounts.is_empty() {
        return "nobody: every pull request needs a review".to_owned();
    }
    accounts
        .iter()
        .map(|account| match &account.login {
            Some(login) => format!("{login} ({})", account.id),
            None => format!("{} (GitHub didn't name it)", account.id),
        })
        .collect::<Vec<_>>()
        .join(", ")
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
    let trust: Vec<Named> = matches
        .get_many::<String>("trust")
        .into_iter()
        .flatten()
        .map(|text| Named::parse(text))
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
    let client = github()?;
    let runtime = super::runtime()?;
    let accounts = runtime.block_on(trusted_accounts(&trust, async |named| {
        lookup(&client, named).await
    }))?;
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
        trusted: accounts.iter().map(|account| account.id).collect(),
        services,
        issued_at: Timestamp::now(),
        ended: false,
        signer: SignerKey::Device(signer.public().into()),
    };
    show(&view, &rule, &names, &accounts);
    let dropped: Vec<u64> = view
        .rule
        .as_ref()
        .and_then(|kept| kept.open().ok())
        .filter(|kept| !kept.ended)
        .map(|kept| kept.trusted)
        .unwrap_or_default()
        .into_iter()
        .filter(|id| !rule.trusted.contains(id))
        .collect();
    if !dropped.is_empty() {
        let dropped = runtime.block_on(shown_accounts(&dropped, async |named| {
            lookup(&client, named).await
        }));
        crate::ui::note(format_args!(
            "The rule you replace also trusted {}. Name each with --trust to keep it.",
            trusted_line(&dropped)
        ));
    }
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
        trusted: &accounts,
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
    let client = github()?;
    let accounts = super::runtime()?.block_on(shown_accounts(&rule.trusted, async |named| {
        lookup(&client, named).await
    }));
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
        trusted: &accounts,
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
fn show(view: &PreviewRuleView, rule: &PreviewRule, names: &[ServiceName], trusted: &[Account]) {
    let source = &rule.source;
    crate::ui::stream(format_args!(
        "Approving previews of {}:",
        view.environment.name
    ));
    crate::ui::stream(format_args!(
        "  repository  {}, parent branch {}",
        source.repository, source.branch
    ));
    let builder = &source.builder;
    crate::ui::stream(format_args!("  builder     {}", builder.r#ref));
    crate::ui::stream(format_args!(
        "  urls        <service>-pv-<id>.{}",
        rule.domain
    ));
    crate::ui::stream(format_args!("  trusted     {}", trusted_line(trusted)));
    crate::ui::stream(format_args!(
        "  servers     {} in the cluster",
        rule.machines.len()
    ));
    for name in names {
        let Some(approved) = rule.service(name) else {
            continue;
        };
        crate::ui::stream(format_args!("  {name}"));
        crate::ui::stream(format_args!("    builds    {}", builds(&approved.policy)));
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
        // A Generated Value shows by name, and the Service ID label not at all: neither
        // rehearsal value is a preview's, and the template leaves the label out. The
        // rehearsal's Namespace is no preview's either, so its hostnames show the derived
        // part as `<id>`.
        let lines = |spec: &RequestedServiceSpec| {
            let mut shown = spec.clone();
            shown
                .container
                .environment
                .retain(|key, _| !approved.generated.contains_key(key));
            let mut labels = shown.container.labels.as_map().clone();
            labels.remove(ployz_core::config::SERVICE_ID_LABEL);
            if let Ok(labels) = ployz_core::ContainerLabels::parse(labels) {
                shown.container.labels = labels;
            }
            settings(&shown)
                .into_iter()
                .map(|line| line.replace(view.sample.as_str(), "pv-<id>"))
                .collect::<Vec<_>>()
        };
        let ordinary = view
            .target
            .iter()
            .find(|spec| spec.name == *name)
            .map(lines)
            .unwrap_or_default();
        for line in &ordinary {
            crate::ui::stream(format_args!("    {line}"));
        }
        // The first deploy signs its own spec, so what it adds shows apart.
        if let Some(setup) = view.setup.iter().find(|setup| setup.name == *name) {
            crate::ui::stream("    setup     runs once, on each preview's first deploy, with");
            for line in lines(setup).iter().filter(|line| !ordinary.contains(line)) {
                crate::ui::stream(format_args!("      {line}"));
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// GitHub as these tests see it: `alice` is 1109914 and `bob` 2207431.
    async fn github(named: &Named) -> Result<Account, Lookup> {
        let (id, login) = match named {
            Named::Id(1_109_914) => (1_109_914, "alice"),
            Named::Login(login) if login.eq_ignore_ascii_case("alice") => (1_109_914, "alice"),
            Named::Id(2_207_431) => (2_207_431, "bob"),
            Named::Login(login) if login == "bob" => (2_207_431, "bob"),
            Named::Id(_) | Named::Login(_) => return Err(Lookup::NotFound),
        };
        Ok(Account {
            id,
            login: Some(login.to_owned()),
        })
    }

    #[tokio::test]
    async fn without_trust_the_rule_trusts_nobody_and_github_is_not_asked() {
        let asked = Cell::new(0);
        let accounts = trusted_accounts(&[], async |named| {
            asked.set(asked.get() + 1);
            github(named).await
        })
        .await
        .unwrap();
        assert!(accounts.is_empty());
        assert_eq!(asked.get(), 0);
        assert_eq!(
            trusted_line(&accounts),
            "nobody: every pull request needs a review"
        );
    }

    #[tokio::test]
    async fn trusted_accounts_show_as_logins_github_names_with_their_ids() {
        let named = ["bob", "1109914", "Alice"]
            .map(|text| Named::parse(text).unwrap())
            .to_vec();
        let accounts = trusted_accounts(&named, github).await.unwrap();
        assert_eq!(
            accounts
                .iter()
                .map(|account| account.id)
                .collect::<Vec<_>>(),
            [1_109_914, 2_207_431]
        );
        assert_eq!(trusted_line(&accounts), "alice (1109914), bob (2207431)");
    }

    #[tokio::test]
    async fn an_account_github_does_not_name_is_refused_before_signing() {
        let error = trusted_accounts(&[Named::Id(666)], github)
            .await
            .unwrap_err();
        assert_eq!(error.report().code, RpcErrorCode::NotFound);
        let error = trusted_accounts(&[Named::Id(1_109_914)], async |_| {
            Err(Lookup::Status(StatusCode::FORBIDDEN))
        })
        .await
        .unwrap_err();
        assert_eq!(error.report().code, RpcErrorCode::Unavailable);
    }

    #[tokio::test]
    async fn an_account_shown_only_keeps_its_id_when_github_does_not_answer() {
        let accounts = shown_accounts(&[1_109_914, 666], github).await;
        assert_eq!(
            trusted_line(&accounts),
            "alice (1109914), 666 (GitHub didn't name it)"
        );
    }

    #[test]
    fn trust_takes_a_login_or_an_id() {
        assert_eq!(Named::parse("1109914").unwrap(), Named::Id(1_109_914));
        assert_eq!(
            Named::parse("octo-cat").unwrap(),
            Named::Login("octo-cat".into())
        );
        for text in ["", "-octo", "octo cat", "octo/cat", &"a".repeat(40)] {
            assert!(Named::parse(text).is_err(), "{text:?}");
        }
    }
}
