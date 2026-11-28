//! A Device's approval of one Deploy: when a Server of the Cluster enforces Device
//! approval, `ployz deploy` lowers the plan as Cloud's runner will, signs a version of
//! each Service with the digest of its spec, and admits the Deploy with them. Each
//! Server then creates only the specs a trusted Device signed.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use clap::ArgMatches;
use ployz_core::authority::{
    Against, Approved, ApprovedChange, AutoDeployRule, Before, Births, Held, Mounts, RuleId,
    RuleSummary, RuleTerms, ServiceVersion, SignerKey, Timestamp, Unknown, birth,
    deploy::{ChangeError, content_addressed},
    rule::Builder,
    rule_draft::{RuleSourceView, renewed_platform},
    version_draft::{SourceNeed, source_need, source_rule},
};
use ployz_core::{
    Claim, ContainerObservation, EnvironmentValues, Hint, InspectRequest, MachineId,
    MachineObservation, MachineTarget, Namespace, PARENT_VERSIONS_CAPABILITY, RequestedServiceSpec,
    RpcErrorCode, SCOPED_KEYS_CAPABILITY, SIGNED_VERSION_CAPABILITY, SealingPublicKey, ServiceName,
    cluster_sealing_key, op,
};
use ployz_store::{
    EnvironmentRef, Parent, ParentQuery, PlanQuery, PostedVersion, RulePrior, RuleQuery, SavedRule,
    TagPin,
};
use serde_json::json;

use super::super::key::members::Member;
use super::super::store::Store;
use super::super::{Error, route};
use super::silent::{self, ImageEdit};
use crate::cli::Rerun;
use crate::cluster::Call;
use crate::connect::Client;
use crate::device::DeviceSigning;
use crate::image::{PinError, pin_public};

/// A signed version of each Service with the image it pinned, the plan version it
/// approves, and what it approved.
pub(in crate::handlers) struct DeviceApproval {
    pub(in crate::handlers) versions: Vec<PostedVersion>,
    pub(in crate::handlers) version: String,
    pub(in crate::handlers) approved: Vec<ApprovedChange>,
    /// The automatic deploy rule this Device signed with the versions, for what the Deploy
    /// saves.
    pub(in crate::handlers) rule: Option<SavedRule>,
}

/// The Cluster as this Device sees it: the Sealing key every Server holds and the
/// Servers, when any of them enforces Device approval.
#[derive(Debug, PartialEq)]
pub(in crate::handlers) struct Enforcing {
    pub(in crate::handlers) cluster: SealingPublicKey,
    /// Every Server, which an automatic deploy rule's approval names.
    pub(in crate::handlers) machines: Vec<ployz_core::MachineId>,
    /// The name each Server reports, which a person reads its signed lines by.
    pub(in crate::handlers) names: BTreeMap<ployz_core::MachineId, String>,
    /// Each Server that enforces and can't be shown to check a signed version. It would
    /// refuse every Service this Device signs.
    pub(in crate::handlers) behind: Behind,
}

/// Servers that enforce Device approval and that this Device can't sign for yet, by name.
#[derive(Debug, Default, PartialEq)]
pub(in crate::handlers) struct Behind {
    /// Each runs a ployz too old to check a signed version.
    pub(in crate::handlers) outdated: Vec<String>,
    /// Each was seen with no capabilities, as through a relay, and didn't answer when
    /// asked itself.
    pub(in crate::handlers) unverified: Vec<String>,
}

/// Whether this CLI holds a key that signs: `PLOYZ_KEY`, or this computer's Device key.
/// A deploy from one never waits on a person's passkey, even one it left unsigned, as an
/// upload or an unanswered probe leaves it.
pub(in crate::handlers) fn keyed(matches: &ArgMatches) -> bool {
    std::env::var_os(crate::env::KEY).is_some()
        || super::super::config_path(matches)
            .is_ok_and(|config| crate::device::key::path(&config).exists())
}

/// How long `ployz deploy` waits to learn whether a Server enforces Device approval.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Sign the versions a Deploy ships, when this CLI holds a key. None otherwise: Cloud holds
/// the Deploy for a person to approve, and a Server refuses what nobody approved. The
/// Servers are asked while the Store plans.
///
/// A CI or agent key in `PLOYZ_KEY` always signs, and so does `--yes`: each signs with no
/// person to ask, only an edit of the last version a person approved, as
/// [`silent::sign`] says. Otherwise this computer's Device key signs Cloud's plan once the
/// person says yes to the lines it shows.
///
/// # Errors
/// Refuses before admission when the person declines the lines it shows, or no terminal
/// can ask, when a Service it ships builds from source and no automatic deploy rule
/// builds it, when a tag image can't be pinned
/// (its registry wants credentials, can't be reached, or has no such tag), when no Server
/// answers or reports the Cluster's key, when a Server can't check what this CLI signs, and
/// when the Store can't lower the plan; returns the plan's or the key's error.
///
/// [`silent::sign`]: super::silent::sign
pub(in crate::handlers) fn sign(
    matches: &ArgMatches,
    store: &Store,
    (environment, services): (&EnvironmentRef, &[ServiceName]),
    edits: &[ImageEdit],
) -> Result<Option<DeviceApproval>, Error> {
    let plan_query = PlanQuery {
        environment: environment.clone(),
        services: services.to_vec(),
        target: true,
        deployment: None,
    };
    if let Some(key) = silent::key()? {
        silent::refuse_forget(matches, store.again())?;
        silent::servers_ready(matches)?;
        let plan = store.read(&plan_query)?;
        return silent::sign(matches, store, (environment, plan), edits, &key).map(Some);
    }
    let silent = silent::silent(matches);
    let key = crate::device::key::path(&super::super::config_path(matches)?);
    let Ok(Some(signing)) = DeviceSigning::load(&key) else {
        return Ok(None);
    };
    if silent {
        silent::refuse_forget(matches, store.again())?;
    }
    let needs = match silent {
        true => Needs::Silent,
        false => Needs::Person,
    };
    let (probe, evidence, plan) = std::thread::scope(|scope| {
        let probe = scope.spawn(|| probe(matches, needs));
        let evidence = (!silent).then(|| scope.spawn(|| evidence(matches)));
        let plan = store.read(&plan_query);
        let evidence = evidence.and_then(|read| read.join().ok().flatten());
        (probe.join().unwrap_or(Probe::Unknown), evidence, plan)
    });
    let enforcing = match probe {
        Probe::Unknown => {
            return Err(Error::coded(
                RpcErrorCode::Unavailable,
                "Couldn't reach your servers, so this device can't sign the deploy. \
                 Nothing deployed.",
            )
            .hint(Hint::Inspect(super::super::rerun(matches, &["status"]))));
        }
        Probe::NoClusterKey => {
            return Err(Error::coded(
                RpcErrorCode::Unavailable,
                "None of your servers reported the cluster's key, so this device can't sign \
                 the deploy. Run ployz status, then deploy again.",
            ));
        }
        Probe::Signing(enforcing) => enforcing,
    };
    let capability = needs.named();
    if !enforcing.behind.outdated.is_empty() {
        return Err(outdated(matches, &enforcing.behind.outdated, capability));
    }
    if !enforcing.behind.unverified.is_empty() {
        return Err(unverified(
            matches,
            &enforcing.behind.unverified,
            capability,
        ));
    }
    let plan = plan?;
    if silent {
        return silent::sign(matches, store, (environment, plan), edits, &signing).map(Some);
    }
    let now = Timestamp::now();
    let seed = birth::seed();
    let signer = SignerKey::Device(signing.device_key());
    let ruled = source(
        matches,
        store,
        (environment, &plan.version),
        &enforcing,
        (signer, &seed, now),
    );
    let mut target = signable(ruled.building(plan.target)?)?;
    let pins = pin_tags(matches, &target)?;
    let namespace = plan.namespace;
    let minted = birth::mint(&mut target, (&namespace, &plan.volumes), now, |name| {
        birth::nonce(&seed, name)
    })
    .map_err(|error| {
        Error::caused(
            RpcErrorCode::InvalidArgument,
            "Can't sign this deploy",
            error,
        )
    })?;
    let evidence = evidence.filter(|evidence| evidence.entry.cluster == Some(enforcing.cluster));
    if evidence.is_none() && target.iter().any(named_mounts) {
        crate::ui::warn(
            "Couldn't read your servers' keys and containers over a route that answers for \
             itself, so every volume shows in full and data from before births is refused. \
             Retry with --connect ssh://<server> or a direct context.",
        );
    }
    let before = bases(
        store,
        environment,
        (&target, &namespace),
        &evidence,
        enforcing.cluster,
    );
    let held = evidence.map(|evidence| {
        let unanswered = unanswered(&enforcing, &evidence);
        Held::of(&namespace, &evidence.containers, unanswered)
    });
    let mounts = Mounts {
        namespace: &namespace,
        volumes: &plan.volumes,
        minted: &minted,
        now,
        against: Some(Against {
            before: &before,
            held: held.as_ref(),
        }),
    };
    let approved =
        ApprovedChange::of(&target, &namespace, &pins, &mounts).map_err(|error| match error {
            ChangeError::Digest(error) => Error::command(error),
            ChangeError::Mounts(error) => Error::caused(
                RpcErrorCode::InvalidArgument,
                "Can't sign this deploy",
                error,
            ),
        })?;
    for line in approved.iter().flat_map(ApprovedChange::lines) {
        crate::ui::note(format_args!("{line}"));
    }
    let forget_older = flag(matches, "forget-older");
    if forget_older {
        crate::ui::note(forgetting(&approved));
    }
    let source = ruled.drafted;
    if approved.is_empty() && source.is_none() {
        return Ok(Some(DeviceApproval {
            versions: Vec::new(),
            version: plan.version,
            approved,
            rule: None,
        }));
    }
    if let Some(drafted) = &source {
        super::super::github_rule::show(plan.environment.name.as_str(), &drafted.summary);
    }
    let signed_at = consent(
        (
            &namespace,
            &approved,
            source.as_ref().map(|drafted| &drafted.summary),
        ),
        without_terminal(matches, store.again()),
        |question, refusal, declined| crate::ui::confirm(question, || refusal, declined),
    )?;
    let versions = approved
        .iter()
        .map(|change| {
            let statement = signing.sign(&ServiceVersion {
                forget_older,
                cluster: enforcing.cluster,
                namespace: namespace.clone(),
                service: change.service.clone(),
                digest: change.digest,
                signed_at,
                signer,
                proposal: None,
                parent: None,
            })?;
            let births = target
                .iter()
                .find(|spec| spec.name == change.service)
                .map(|spec| birth::carried(spec, &minted))
                .unwrap_or_default();
            Ok(PostedVersion {
                statement,
                pin: change.tag.clone().map(|tag| TagPin {
                    tag,
                    image: change.image.clone(),
                }),
                births,
            })
        })
        .collect::<Result<_, Error>>()?;
    let rule = source
        .map(|drafted| {
            Ok::<_, Error>(SavedRule {
                rule: Box::new(signing.sign(&drafted.rule)?),
                births: drafted.births,
                prior: drafted.prior,
            })
        })
        .transpose()?;
    Ok(Some(DeviceApproval {
        versions,
        version: plan.version,
        approved,
        rule,
    }))
}

/// An automatic deploy rule drafted for this Device to sign with a Deploy's versions.
struct Drafted {
    rule: AutoDeployRule,
    births: Births,
    prior: Option<RulePrior>,
    summary: RuleSummary,
}

/// The automatic deploy rule a Deploy's Git Services build under, as this review reads it:
/// the one the Environment keeps, and the one this Device signs with the versions, which
/// replaces it.
#[derive(Default)]
struct Ruled {
    kept: Option<AutoDeployRule>,
    drafted: Option<Drafted>,
    /// Where each Saved Git Service builds from, as this review stands.
    sources: Vec<RuleSourceView>,
    /// The Services the kept rule covers whose settings it no longer approves.
    changed: Vec<ServiceName>,
    /// Why the rule couldn't be read for this review, when it couldn't.
    unread: Option<&'static str>,
}

impl Ruled {
    /// The specs of `target` this Device signs a version of: all but the Git Services that
    /// build under the rule that stands once the Deploy is admitted. Any other Git Service
    /// is refused before anything is signed: no Server would build it.
    fn building(
        &self,
        target: Option<Vec<RequestedServiceSpec>>,
    ) -> Result<Option<Vec<RequestedServiceSpec>>, Error> {
        let Some(target) = target else {
            return Ok(None);
        };
        let (built, signed): (Vec<_>, Vec<_>) = target.into_iter().partition(built);
        let standing = match &self.drafted {
            Some(drafted) => Some((&drafted.rule, true)),
            None => self.kept.as_ref().map(|kept| (kept, false)),
        };
        for spec in &built {
            let name = &spec.name;
            let message = match (self.unread, standing) {
                (Some(why), _) => {
                    format!("{why}, so {name} can't build under it. Nothing deployed.")
                }
                (None, Some((rule, drafted))) if self.approves(rule, drafted, name) => continue,
                (None, Some((rule, _))) if rule.ended => format!(
                    "Automatic deploys are off for {name}. Turn on Deploy every push to deploy it."
                ),
                (None, _) => format!(
                    "No automatic deploy rule covers {name} as it stands, so your servers won't \
                     build it. Turn on Deploy every push, then deploy again."
                ),
            };
            return Err(Error::detailed(
                RpcErrorCode::InvalidArgument,
                message,
                json!({ "service": name }),
            ));
        }
        Ok(Some(signed))
    }

    /// Whether `rule` builds `name` as this review stands: it is on, covers the Service, and
    /// the Service still builds from its repository and branch. A `drafted` rule is drafted
    /// from this review; the kept one must also still approve the Service's settings.
    fn approves(&self, rule: &AutoDeployRule, drafted: bool, name: &ServiceName) -> bool {
        let Some(github) = rule.source.github() else {
            return false;
        };
        let tracks = self.sources.iter().any(|source| {
            source.service == *name
                && source.repository_id == github.repository_id
                && source.branch.as_deref() == Some(github.branch.as_str())
        });
        !rule.ended
            && rule.service(name).is_some()
            && tracks
            && (drafted || !self.changed.contains(name))
    }
}

/// The rule the Environment keeps, and the one a Deploy at review `version` signs with its
/// versions when what it saves isn't what the kept rule approves, drafted as the
/// dashboard's Deploy tap drafts it. No draft when it needs none, or after a warning when
/// it can't be read or drafted: the Deploy goes on without it, and pushes wait. A rule the
/// person ended stays ended.
fn source(
    matches: &ArgMatches,
    store: &Store,
    (environment, version): (&EnvironmentRef, &str),
    enforcing: &Enforcing,
    (signer, seed, issued_at): (SignerKey, &[u8; 16], Timestamp),
) -> Ruled {
    let without = |error: &(dyn std::error::Error + 'static)| {
        crate::ui::warn_cause(
            "Didn't sign automatic deploys, so pushes may wait for approval",
            error,
        );
    };
    let view = match store.read(&RuleQuery {
        environment: environment.clone(),
        staged: true,
    }) {
        Ok(view) => view,
        Err(error) => {
            without(&error);
            return Ruled {
                unread: Some("Couldn't read the automatic deploy rule"),
                ..Ruled::default()
            };
        }
    };
    // Another review: the Store refuses this Deploy as stale.
    if view.version.as_deref() != Some(version) {
        return Ruled {
            unread: Some("Your changes moved since this review"),
            ..Ruled::default()
        };
    }
    let mut basis = view.basis();
    let kept = basis.kept();
    let ruled = |drafted| Ruled {
        kept: kept.clone(),
        drafted,
        sources: view.sources.clone(),
        changed: view.changed.clone(),
        unread: None,
    };
    // An owner or admin decides this Deploy and signs the rule with it on its approval
    // page, so this Device signs none. An older Store says only whether the Environment is
    // Protected.
    if view
        .protector_decides
        .unwrap_or(view.protected != Some(false))
    {
        return ruled(None);
    }
    // A registry rule is approved in the dashboard alone; this Deploy signs versions.
    if source_need(&basis) == SourceNeed::None
        || kept
            .as_ref()
            .is_some_and(|kept| kept.source.registry().is_some())
    {
        return ruled(None);
    }
    let terms = RuleTerms {
        cluster: enforcing.cluster,
        machines: enforcing.machines.clone(),
        owner_id: 0,
        builder: (kept.as_ref())
            .and_then(|kept| kept.source.github())
            .map_or_else(Builder::pinned, |kept| kept.builder.clone()),
        platform: renewed_platform(kept.as_ref()),
        // The births the versions mint come from the same seed, so a Volume both mount is
        // born once.
        new_rule: RuleId::from_bytes(*seed),
        issued_at,
        signer,
    };
    let mut rule = match source_rule(&mut basis, terms, None) {
        Ok(Some(rule)) => rule,
        Ok(None) => return ruled(None),
        Err(error) => {
            without(&error);
            return ruled(None);
        }
    };
    let ployz_core::authority::RuleSource::Github(github) = &mut rule.source else {
        return ruled(None);
    };
    match super::super::github_rule::owner(
        matches,
        (&github.repository, github.repository_id),
        "deploy again",
    ) {
        Ok(owner) => github.owner_id = owner,
        Err(error) => {
            without(&error);
            return ruled(None);
        }
    }
    let summary = rule.summary(&basis);
    ruled(Some(Drafted {
        prior: kept.as_ref().map(|kept| RulePrior {
            rule: kept.rule,
            generation: kept.generation,
        }),
        births: basis.minted,
        rule,
        summary,
    }))
}

/// Whether `name`, a switch some commands that sign lack, was given.
pub(super) fn flag(matches: &ArgMatches, name: &str) -> bool {
    matches
        .try_get_one::<bool>(name)
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false)
}

/// What `--forget-older` does to the Services of `approved`, said before the person signs.
fn forgetting(approved: &[ApprovedChange]) -> String {
    let services: Vec<&str> = approved
        .iter()
        .map(|change| change.service.as_str())
        .collect();
    format!(
        "This also forgets every older version of {}: your servers refuse a new container \
         of one that opens a secret, mounts a volume, pulls its image with a login or reaches \
         its server. Running containers keep running. An automatic deploy rule for these \
         services stops, and Ployz Cloud can't move or drain the containers it built, until \
         you approve again under Automatic deploys in Ployz Cloud, which also approves \
         the commit now at the head of its branch.",
        services.join(", ")
    )
}

/// What a deploy that can't ask runs as without a terminal: itself with `--yes`, which
/// ships only the images `--image` names. With `--forget-older` nothing does: `--yes`
/// never forgets.
fn without_terminal(matches: &ArgMatches, again: Rerun) -> Option<Rerun> {
    if flag(matches, "forget-older") {
        return None;
    }
    let images = matches!(matches.try_get_many::<String>("image"), Ok(Some(_)));
    let again = if images {
        again
    } else {
        again.add("image", "SERVICE=IMAGE")
    };
    Some(again.switch("yes"))
}

/// What a person says to signing `approved` in `namespace`, asked after its lines show:
/// Cloud relays the plan, so a setting it planted, or another Environment's Namespace, must
/// be refusable before this Device signs it. A run that can't ask refuses before signing,
/// and a person who says no deploys nothing. A yes answers with the time it was given,
/// which every version signed for it carries, so a forget reaches each version signed
/// before the yes, not only those signed before the prompt.
fn consent(
    (namespace, approved, rule): (&Namespace, &[ApprovedChange], Option<&RuleSummary>),
    silent: Option<Rerun>,
    mut confirm: impl FnMut(&str, Error, &'static str) -> Result<(), Error>,
) -> Result<Timestamp, Error> {
    // A Deploy whose only signature is the rule asks about the Services it builds.
    let services: Vec<&str> = match (approved, rule) {
        ([], Some(rule)) => (rule.services.iter())
            .map(|service| service.name.as_str())
            .collect(),
        _ => (approved.iter())
            .map(|change| change.service.as_str())
            .collect(),
    };
    let mut marked: Vec<String> = approved
        .iter()
        .flat_map(|change| {
            let service = change.service.as_str();
            change
                .confirm
                .iter()
                .map(move |line| format!("{service}: {line}"))
        })
        .collect();
    if let Some(rule) = rule {
        marked.extend(rule.services.iter().flat_map(|service| {
            let name = service.name.as_str();
            (service.confirm.iter()).map(move |line| format!("{name}: {line}"))
        }));
        if let ployz_core::authority::SourceSummary::Github {
            pinned: false,
            builder,
            ..
        } = &rule.source
        {
            marked.push(format!("builder {builder}, not Ployz's own release"));
        }
    }
    let refusal = Error::detailed(
        RpcErrorCode::ConfirmationRequired,
        "This deploy uses data that needs your yes, and there's no terminal to ask in. Run it \
         at a terminal. Nothing deployed.",
        json!({ "services": services, "confirm": marked }),
    );
    ask_data(
        &marked,
        (refusal, "Cancelled. Nothing signed or deployed."),
        &mut confirm,
    )?;
    let refusal = Error::detailed(
        RpcErrorCode::ConfirmationRequired,
        "Signing this deploy needs your yes, and there's no terminal to ask in. Run it at a \
         terminal, or ship only new images. Nothing deployed.",
        json!({ "services": services }),
    )
    .hint(silent.map(Hint::from));
    let pushes = rule.map_or_else(String::new, |rule| match &rule.source {
        ployz_core::authority::SourceSummary::Github { branch, .. } => {
            format!(", and deploy every push to {branch}")
        }
        ployz_core::authority::SourceSummary::Registry { repository } => {
            format!(", and deploy every image pushed to {repository}")
        }
    });
    confirm(
        &format!(
            "Approve {} in namespace {namespace}{pushes}?",
            services.join(", ")
        ),
        refusal,
        "Cancelled. Nothing signed or deployed.",
    )?;
    Ok(Timestamp::now())
}

/// Lists `marked`, the mount lines a person must say yes to, each after its Service, and
/// asks about them all at once. Nothing is asked when none is marked.
pub(in crate::handlers) fn ask_data(
    marked: &[String],
    (refusal, declined): (Error, &'static str),
    confirm: &mut impl FnMut(&str, Error, &'static str) -> Result<(), Error>,
) -> Result<(), Error> {
    if marked.is_empty() {
        return Ok(());
    }
    crate::ui::note("Needs your yes:");
    for line in marked {
        crate::ui::note(format_args!("  {line}"));
    }
    confirm("Use the data marked above?", refusal, declined)
}

/// What this Device read over a Full route whose entry answers for itself: the keys and
/// Cluster key the entry holds, and every Server's Containers.
struct Evidence {
    entry: Member,
    containers: Vec<ContainerObservation>,
    /// The Servers whose listing came back.
    answered: BTreeSet<MachineId>,
    /// Each Server this route knew of, by the name it reports.
    names: BTreeMap<MachineId, String>,
}

/// The [`Evidence`] a mount's line is checked against, or `None` when no Full route
/// answered, or its entry doesn't answer for itself, as through Ployz Cloud's Server Access.
fn evidence(matches: &ArgMatches) -> Option<Evidence> {
    let context = matches
        .try_get_one::<String>("context")
        .ok()
        .flatten()
        .map(String::as_str);
    let runtime = super::super::runtime().ok()?;
    runtime.block_on(async {
        let (client, machines, details) = tokio::time::timeout(PROBE_TIMEOUT, async {
            let mut client = route::connect_full(matches, context).await.ok()?;
            // The entry the dial confirmed, which every read below goes through.
            let entry = client.entry_machine()?;
            if !client.answers_for_itself(&entry) {
                return None;
            }
            let to = MachineTarget::from(&entry);
            let inspect = Call::once().to(&to);
            let details = client
                .call_shared::<op::Inspect>(InspectRequest::default(), inspect)
                .await
                .ok()?;
            let machines = client.machines().await.ok()?;
            Some((client, machines, details))
        })
        .await
        .ok()
        .flatten()?;
        // Each Server's listing ends on its own deadline, so one that doesn't answer is
        // kept as unanswered beside the others' replies.
        let live = client
            .live_services_from(&machines, EnvironmentValues::Redacted)
            .await
            .ok()?
            .containers;
        Some(Evidence {
            entry: Member::of(details),
            answered: live
                .successes
                .iter()
                .map(|success| success.machine_id)
                .collect(),
            names: (machines.into_iter())
                .map(|seen| (seen.machine.id, seen.machine.name.to_string()))
                .collect(),
            containers: live
                .successes
                .into_iter()
                .flat_map(|success| success.value)
                .collect(),
        })
    })
}

/// Each Service of `target`'s last approved version, checked against `evidence`'s keys
/// for `cluster`. Without evidence every one is unknown, whatever Cloud says.
fn bases(
    store: &Store,
    environment: &EnvironmentRef,
    (target, namespace): (&[RequestedServiceSpec], &Namespace),
    evidence: &Option<Evidence>,
    cluster: SealingPublicKey,
) -> BTreeMap<ServiceName, Before> {
    let services: Vec<ServiceName> = target.iter().map(|spec| spec.name.clone()).collect();
    let Some(evidence) = evidence else {
        let unread = || Before::Unknown(Unknown::Unread);
        return services
            .into_iter()
            .map(|service| (service, unread()))
            .collect();
    };
    // A Config Store too old to answer leaves every Service unknown.
    let parents = store
        .read(&ParentQuery {
            environment: environment.clone(),
            services,
        })
        .map(|parents| parents.services)
        .unwrap_or_default();
    parents
        .into_iter()
        .map(|(service, parent)| {
            let before = match parent {
                Parent::Missing { deployed: false } => Before::New,
                Parent::Missing { deployed: true } => Before::Unknown(Unknown::NoRecord),
                Parent::Signed {
                    statement, spec, ..
                } => Approved::verify(
                    &statement,
                    *spec,
                    (&evidence.entry.trusted, cluster),
                    (namespace, &service),
                )
                .map_or(Before::Unknown(Unknown::Unchecked), Before::Approved),
            };
            (service, before)
        })
        .collect()
}

/// Each Server the probe or `evidence`'s route knew of whose listing didn't come back, by
/// name. The two may enter through different Servers, so either roster may miss one.
fn unanswered(enforcing: &Enforcing, evidence: &Evidence) -> Vec<String> {
    let known: BTreeSet<&MachineId> = (enforcing.machines.iter())
        .chain(evidence.names.keys())
        .collect();
    known
        .into_iter()
        .filter(|id| !evidence.answered.contains(id))
        .map(|id| {
            (enforcing.names.get(id))
                .or_else(|| evidence.names.get(id))
                .map_or_else(|| id.to_string(), Clone::clone)
        })
        .collect()
}

/// Whether `spec` mounts a named volume, which [`Mounts::lines`] checks.
fn named_mounts(spec: &RequestedServiceSpec) -> bool {
    spec.volume_graph()
        .mounted_volumes()
        .any(|volume| volume.source.docker_volume_name().is_some())
}

/// The plan's target when a Device can sign every spec it ships, once its tags are pinned.
pub(super) fn signable(
    target: Option<Vec<RequestedServiceSpec>>,
) -> Result<Vec<RequestedServiceSpec>, Error> {
    let Some(target) = target else {
        return Err(Error::coded(
            RpcErrorCode::Unsupported,
            "This Config Store can't plan a deploy for a device to approve. Upgrade it, then \
             deploy again.",
        ));
    };
    if let Some(built) = target.iter().find(|spec| built(spec)) {
        return Err(Error::detailed(
            RpcErrorCode::InvalidArgument,
            format!(
                "{} builds from source, and servers that enforce device approval can't run \
                 a build yet. Deploy it from an image pinned by digest.",
                built.name
            ),
            json!({ "service": built.name }),
        ));
    }
    Ok(target)
}

/// The digest each tag image of `target` has on its registry, asked of the registry
/// itself, all at once. An image written by digest needs none.
fn pin_tags(
    matches: &ArgMatches,
    target: &[RequestedServiceSpec],
) -> Result<BTreeMap<ServiceName, String>, Error> {
    let tagged: Vec<_> = target
        .iter()
        .filter(|spec| !content_addressed(&spec.container.image))
        .collect();
    if tagged.is_empty() {
        return Ok(BTreeMap::new());
    }
    let lookups = tagged.iter().map(|spec| pin_public(&spec.container.image));
    let found = super::super::runtime()?.block_on(futures_util::future::join_all(lookups));
    tagged
        .into_iter()
        .zip(found)
        .map(|(spec, found)| match found {
            Ok(pin) => Ok((spec.name.clone(), pin.as_str().to_owned())),
            Err(error) => Err(unpinnable(matches, spec, error)),
        })
        .collect()
}

/// What the Servers say about checking what this device signs.
#[derive(Debug, PartialEq)]
pub(in crate::handlers) enum Probe {
    Signing(Enforcing),
    /// None reported the Cluster's Sealing key.
    NoClusterKey,
    /// No Server answered in time.
    Unknown,
}

/// What a deploy's signature needs of the Servers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::handlers) enum Needs {
    /// A person's: each Server checks a signed version.
    Person,
    /// One with no person to ask, which signs an edit of the last version a person
    /// approved: each Server must hold every capability an edit needs.
    Silent,
}

impl Needs {
    fn capabilities(self) -> &'static [&'static str] {
        match self {
            Self::Person => &[SIGNED_VERSION_CAPABILITY],
            Self::Silent => &[
                SIGNED_VERSION_CAPABILITY,
                SCOPED_KEYS_CAPABILITY,
                PARENT_VERSIONS_CAPABILITY,
            ],
        }
    }

    /// The capability a refusal names.
    fn named(self) -> &'static str {
        match self {
            Self::Person => SIGNED_VERSION_CAPABILITY,
            Self::Silent => PARENT_VERSIONS_CAPABILITY,
        }
    }
}

/// What the Servers say about checking what this device signs, each asked for each
/// capability `needs` names.
pub(in crate::handlers) fn probe(matches: &ArgMatches, needs: Needs) -> Probe {
    let context = matches
        .try_get_one::<String>("context")
        .ok()
        .flatten()
        .map(String::as_str);
    let Ok(runtime) = super::super::runtime() else {
        return Probe::Unknown;
    };
    let observed = runtime.block_on(async {
        let (client, observed) = tokio::time::timeout(PROBE_TIMEOUT, async {
            let mut client = route::connect(matches, context).await.ok()?;
            let observed = client.machines().await.ok()?;
            Some((client, observed))
        })
        .await
        .ok()
        .flatten()?;
        let answered = asked(&client, &unclaimed(&observed, needs), needs).await;
        Some((observed, answered))
    });
    observed.map_or(Probe::Unknown, |(observed, answered)| {
        enforced(&observed, &answered, needs)
    })
}

/// Each Server seen with no capabilities. An empty list claims nothing: an older entry
/// relays a newer peer's record without one.
fn unclaimed(observed: &[MachineObservation], needs: Needs) -> BTreeSet<MachineId> {
    observed
        .iter()
        .map(|observed| &observed.machine)
        .filter(|machine| {
            needs
                .capabilities()
                .iter()
                .any(|needed| machine.runtime.claim(needed) == Claim::Unclaimed)
        })
        .map(|machine| machine.id)
        .collect()
}

/// Whether each of `machines`, asked itself, has every one of `needs`. One that doesn't
/// answer in time is left out.
async fn asked(
    client: &Client,
    machines: &BTreeSet<MachineId>,
    needs: Needs,
) -> BTreeMap<MachineId, bool> {
    let asks = machines.iter().map(|machine| async move {
        let target = MachineTarget::from(machine);
        let mut holds = true;
        for needed in needs.capabilities() {
            let ask = client.supports(Some(&target), needed);
            holds &= tokio::time::timeout(PROBE_TIMEOUT, ask).await.ok()?.ok()?;
        }
        Some((*machine, holds))
    });
    futures_util::future::join_all(asks)
        .await
        .into_iter()
        .flatten()
        .collect()
}

/// What `observed` says about checking what this device signs, with `answered` saying,
/// for each Server seen with no capabilities that was asked and answered, whether it has
/// every capability `needs` asks for. A Server that says it doesn't enforce runs a Ployz
/// from before every Server checked signatures, so it is behind.
fn enforced(
    observed: &[MachineObservation],
    answered: &BTreeMap<MachineId, bool>,
    needs: Needs,
) -> Probe {
    let Some(cluster) = cluster_sealing_key(observed.iter().map(|observed| &observed.machine))
    else {
        return Probe::NoClusterKey;
    };
    let mut machines: Vec<_> = observed
        .iter()
        .map(|observed| observed.machine.id)
        .collect();
    machines.sort_unstable();
    machines.dedup();
    let names = observed
        .iter()
        .map(|observed| (observed.machine.id, observed.machine.name.to_string()))
        .collect();
    let mut behind = Behind::default();
    for machine in observed.iter().map(|observed| &observed.machine) {
        match machine.runtime.device_enforcement {
            None => {
                behind.unverified.push(machine.name.to_string());
                continue;
            }
            Some(false) => {
                behind.outdated.push(machine.name.to_string());
                continue;
            }
            Some(true) => {}
        }
        let claims: Vec<Claim> = needs
            .capabilities()
            .iter()
            .map(|needed| machine.runtime.claim(needed))
            .collect();
        let checks = if claims.contains(&Claim::Lacks) {
            Some(false)
        } else if claims.contains(&Claim::Unclaimed) {
            answered.get(&machine.id).copied()
        } else {
            Some(true)
        };
        match checks {
            Some(true) => {}
            Some(false) => behind.outdated.push(machine.name.to_string()),
            None => behind.unverified.push(machine.name.to_string()),
        }
    }
    for names in [&mut behind.outdated, &mut behind.unverified] {
        names.sort_unstable();
        names.dedup();
    }
    Probe::Signing(Enforcing {
        cluster,
        machines,
        names,
        behind,
    })
}

/// `servers` joined for a sentence: `a`, `a and b`, `a, b and c`.
fn listed(servers: &[String]) -> String {
    match servers {
        [rest @ .., last] if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => servers.join(""),
    }
}

/// The refusal for Servers that lack `capability`, which what this CLI signs needs, before
/// anything is signed or admitted: a Server on Ployz 0.3 among them.
pub(super) fn outdated(matches: &ArgMatches, servers: &[String], capability: &str) -> Error {
    let named = listed(servers);
    // This CLI's own channel, which carries a ployz that checks what it signs.
    let channel = match ployz_core::MachineVersion::parse(env!("CARGO_PKG_VERSION")) {
        Ok(version) if !version.is_prerelease() => "stable",
        _ => "beta",
    };
    let mut upgrade = vec!["server", "upgrade", channel];
    upgrade.extend(servers.iter().map(String::as_str));
    Error::detailed(
        RpcErrorCode::Unsupported,
        format!(
            "Upgrade {named} first: {each} runs an older ployz that can't check this \
             device's signature, so {each} would refuse this deploy. Nothing signed or \
             deployed.",
            each = if servers.len() == 1 { "it" } else { "each" },
        ),
        json!({
            "servers": servers,
            "capability": capability,
        }),
    )
    .hint(Hint::Next(super::super::rerun(matches, &upgrade)))
}

/// The refusal for Servers that didn't say whether they have `capability`, before anything
/// is signed or admitted.
pub(super) fn unverified(matches: &ArgMatches, servers: &[String], capability: &str) -> Error {
    Error::detailed(
        RpcErrorCode::Unavailable,
        format!(
            "Couldn't confirm {} can check this device's signature: {} didn't answer. \
             Nothing signed or deployed.",
            listed(servers),
            if servers.len() == 1 { "it" } else { "each" },
        ),
        json!({
            "servers": servers,
            "capability": capability,
        }),
    )
    .hint(Hint::Inspect(super::super::rerun(matches, &["status"])))
}

/// Whether `spec` runs the image a build or upload will make, which no Device can
/// name by digest before it exists.
fn built(spec: &RequestedServiceSpec) -> bool {
    ployz_core::is_built_image(&spec.container.image)
}

/// Why `spec`'s tag has no pin, and the command that writes one by hand.
fn unpinnable(matches: &ArgMatches, spec: &RequestedServiceSpec, error: PinError) -> Error {
    let next = super::super::store::next(
        matches,
        &[
            "set",
            &format!(
                "{}.image={}@sha256:…",
                spec.name,
                ployz_core::image_repository(&spec.container.image)
            ),
        ],
    );
    let details = json!({ "service": spec.name, "image": spec.container.image });
    let failure = match &error {
        PinError::NeedsCredentials { registry, .. } => Error::detailed(
            RpcErrorCode::InvalidArgument,
            format!(
                "{registry} wants credentials to read {}'s image, so ployz can't pin it. \
                 Write its digest.",
                spec.name
            ),
            details,
        ),
        PinError::NotATag { .. } => Error::detailed(
            RpcErrorCode::InvalidArgument,
            format!(
                "Pin {}'s image by digest to deploy to servers that enforce device approval.",
                spec.name
            ),
            details,
        ),
        PinError::TimedOut { registry } | PinError::Unread { registry, .. } => {
            Error::detailed_caused(
                RpcErrorCode::Unavailable,
                format!("Couldn't ask {registry} for {}'s image digest.", spec.name),
                details,
                Some(error),
            )
        }
    };
    failure.hint(Hint::Next(next))
}

#[cfg(test)]
#[path = "approve_tests.rs"]
mod tests;
