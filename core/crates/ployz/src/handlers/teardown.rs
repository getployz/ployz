//! Removing what ran: the typed confirmation, what goes, and the removal Deployment
//! that takes an Environment off the Servers first. `env rm` and `project rm` share it.

use clap::ArgMatches;
use ployz_core::{RpcErrorCode, ServiceName};
use std::collections::BTreeMap;

use ployz_store::{
    Admit, AdmitRemoval, DeploymentId, DeploymentStatus, DeploymentSummary, DeploymentView,
    DestructiveEffect, DestructiveKind, DueQuery, EnvironmentName, EnvironmentRef,
    EnvironmentSummary, EnvironmentsQuery, Left, ProjectName, ProjectsQuery, Removal,
    RemovalsQuery, Remove, RemovedQuery, ServicesQuery, ShutDown, VolumeName, VolumesQuery,
};
use serde_json::json;

use super::Error;
use super::deploy;
use super::store::{self, Store, mint};
use crate::cloud_account::StoreCallError;
use crate::ui::{self, Hint, Tree};

/// `--confirm` typed `name`, or a person typed it at the prompt after seeing
/// `unconfirmed`'s loss tree. Anywhere else its refusal is the error; a
/// different `--confirm` is a usage error naming `retry`.
pub(super) fn confirm(
    matches: &ArgMatches,
    name: &str,
    what: &str,
    retry: String,
    unconfirmed: impl FnOnce() -> Result<(Error, Tree), Error>,
) -> Result<(), Error> {
    match matches.get_one::<String>("confirm") {
        Some(typed) if typed == name => Ok(()),
        Some(typed) => Err(Error::usage(format!(
            "--confirm {} does not match {what} {name}. No changes made.",
            typed.escape_debug()
        ))
        .hint(Hint::Retry(retry))),
        None => {
            let (refusal, loss) = unconfirmed()?;
            if ui::can_prompt() {
                ui::note_inline(&loss);
            }
            ui::confirm_name(name, || refusal, "Cancelled. Nothing was removed.")
        }
    }
}

pub(super) fn inventory(store: &Store, at: &EnvironmentRef) -> Result<Inventory, Error> {
    try_inventory(store, at).map_err(|error| store.fail(error))
}

fn try_inventory(store: &Store, at: &EnvironmentRef) -> Result<Inventory, StoreCallError> {
    let services = store.try_read(&ServicesQuery {
        environment: at.clone(),
    })?;
    let volumes = store.try_read(&VolumesQuery {
        environment: at.clone(),
    })?;
    Ok(Inventory {
        environment: services.environment,
        services: services
            .services
            .into_iter()
            .map(|listing| listing.service.name)
            .collect(),
        volumes: volumes
            .volumes
            .iter()
            .map(|listing| json!({ "name": listing.volume.name, "deployed": listing.deployed }))
            .collect(),
    })
}

/// What `project rm` or `env rm` would list at its prompt now, read as it read it.
fn listed(
    store: &Store,
    project: &ProjectName,
    environment: Option<&EnvironmentName>,
) -> Result<Shown, StoreCallError> {
    let names = match environment {
        Some(name) => vec![name.clone()],
        None => store
            .try_read(&ProjectsQuery {})?
            .projects
            .into_iter()
            .find(|listing| &listing.name == project)
            .map(|listing| listing.environments)
            .unwrap_or_default(),
    };
    let inventories = names
        .into_iter()
        .map(|name| {
            try_inventory(
                store,
                &EnvironmentRef {
                    project: Some(project.clone()),
                    environment: Some(name),
                },
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Shown::of(&inventories))
}

/// Every `--accept-volume-loss` name.
pub(super) fn accepted(matches: &ArgMatches) -> Result<Vec<VolumeName>, Error> {
    store::volume_names(matches, "accept-volume-loss")
}

/// `--accept-volume-loss` for each of `accept`, the words a retry repeats.
pub(super) fn with_accepted(accept: &[VolumeName]) -> Vec<&str> {
    accept
        .iter()
        .flat_map(|name| ["--accept-volume-loss", name.as_str()])
        .collect()
}

/// Remove `environment` of `project`, or the whole Project, in one write: each
/// `--accept-volume-loss` name goes to the members whose removal deletes it, and
/// `--expect-version` binds the review its refusal handed back. Then run each
/// removal Deployment it admitted. In process, this CLI also moves the removal on
/// (sweep, admit what is due, run it) until the Store deleted the target; over
/// HTTPS, Cloud carries on after the first ones and this waits until it deleted the
/// target, or stops at once when a Server didn't answer the review. `None` once a
/// removal Deployment that didn't apply was reported (running the command again
/// finishes it).
pub(super) fn remove(
    matches: &ArgMatches,
    store: &Store,
    (project, environment): (&ProjectName, Option<&EnvironmentName>),
    shown: Option<&Shown>,
    mut events: Option<std::io::BufWriter<std::fs::File>>,
) -> Result<Option<Removed>, Error> {
    deploy::refuse_local_detach(matches, store)?;
    let accept = accepted(matches)?;
    let again = store.again(&with_accepted(&accept));
    let members = match environment {
        Some(name) => vec![name.clone()],
        None => store
            .read(&EnvironmentsQuery {
                project: Some(project.clone()),
            })?
            .environments
            .into_iter()
            .map(|listing| listing.name)
            .collect(),
    };
    let at = |name: &EnvironmentName| EnvironmentRef {
        project: Some(project.clone()),
        environment: Some(name.clone()),
    };
    let mut accept_volume_loss = BTreeMap::new();
    for name in &members {
        let deletes = store.read(&RemovalsQuery {
            environment: at(name),
            remove: true,
            deployment: None,
        })?;
        let accepted: Vec<_> = accept
            .iter()
            .filter(|accepted| {
                deletes
                    .volumes
                    .iter()
                    .any(|volume| &volume.name == *accepted)
            })
            .cloned()
            .collect();
        if !accepted.is_empty() {
            accept_volume_loss.insert(name.clone(), accepted);
        }
    }
    let mut request = Remove {
        project: project.clone(),
        environment: environment.cloned(),
        version: matches.get_one::<String>("expect-version").cloned(),
        accept_volume_loss,
        every_copy: BTreeMap::new(),
    };
    let refs: Vec<_> = members.iter().map(at).collect();
    let what = target(project, environment);
    let prompted = shown.map(|shown| (shown, || listed(store, project, environment)));
    let (removal, effects) = bound(&mut request, &what, prompted, |request| {
        store.remove(request, &refs)
    })
    .map_err(|unbound| {
        let Unbound { error, effects } = *unbound;
        let first = goes_first(&error, project);
        let goes = first.is_some();
        let error = store.accepting(store::with_next(
            error,
            |_| goes,
            || first.unwrap_or_default(),
        ));
        bound_to(error, &effects)
    })?;
    let mut ran = Vec::new();
    let result = (|| {
        let (id, left, waiting_on) = match removal {
            Removal::Removed { residuals } => {
                return Ok(Some((Removal::Removed { residuals }, Vec::new())));
            }
            Removal::Removing {
                id,
                left,
                waiting_on,
            } => (id, left, waiting_on),
        };
        // Cloud may admit a removal Deployment, or sweep one, before this follows
        // it, so the finished mark says what was left on old servers.
        let removed = |ran: &mut Vec<DeploymentSummary>| {
            let residuals = store.read(&RemovedQuery { id: id.clone() })?.residuals;
            Ok::<_, Error>(Some((Removal::Removed { residuals }, std::mem::take(ran))))
        };
        let mut admitted: Vec<DeploymentSummary> = left
            .iter()
            .filter_map(|left| left.deployment.clone())
            .collect();
        loop {
            for deployment in std::mem::take(&mut admitted) {
                let writer = events
                    .as_mut()
                    .map(|writer| writer.get_ref().try_clone())
                    .transpose()?
                    .map(std::io::BufWriter::new);
                let deploy::Shipped {
                    view, ran: outcome, ..
                } = match deploy::execute(matches, store, &deployment, None, writer) {
                    Err(error) if swept(&error, || gone(store, project, environment)) => {
                        return removed(&mut ran);
                    }
                    shipped => shipped?,
                };
                if view.deployment.status != DeploymentStatus::Applied {
                    let reported = unfinished(matches, (&view, &effects), &ran, outcome, &again);
                    ran.clear();
                    reported?;
                    return Ok(None);
                }
                ran.push(view.deployment);
            }
            if store.local().is_none() {
                if matches.get_flag("detach") {
                    return Ok(Some((
                        Removal::Removing {
                            id,
                            left,
                            waiting_on,
                        },
                        std::mem::take(&mut ran),
                    )));
                }
                // The review is the only word on `waiting_on`, and it skips an
                // Environment once its removal applied, so it stays the freshest.
                // An Environment with no removal admitted waits its turn or stalled,
                // and Cloud carries it on alone; otherwise only Cloud's sweep is left.
                let sweep_next = left.iter().all(|left| left.deployment.is_some());
                if waiting_on.is_empty() && sweep_next {
                    swept_by_cloud(store, project, environment)?;
                } else if !gone(store, project, environment)? {
                    let left = applied(left, &ran);
                    return Ok(Some((
                        Removal::Removing {
                            id,
                            left,
                            waiting_on,
                        },
                        std::mem::take(&mut ran),
                    )));
                }
                return removed(&mut ran);
            }
            store.sweep().map_err(|error| store.fail(error))?;
            if gone(store, project, environment)? {
                return removed(&mut ran);
            }
            for due in store.read(&DueQuery {})? {
                let summary = due.environment;
                if summary.project != *project
                    || environment.is_some_and(|name| *name != summary.name)
                {
                    continue;
                }
                admitted.push(
                    store
                        .admit(&Admit::Remove(AdmitRemoval {
                            id: DeploymentId::parse(mint())?,
                            environment: at(&summary.name),
                        }))
                        .map_err(|error| store.fail(error))?,
                );
            }
            if admitted.is_empty() {
                return Err(stalled(project, environment, &left).hint(Hint::Next(again.clone())));
            }
        }
    })();
    match result {
        Ok(Some((removal, ran))) => Ok(Some(Removed {
            removal,
            effects,
            ran,
        })),
        Ok(None) => Ok(None),
        Err(error) if !ran.is_empty() => {
            for deployment in &ran {
                crate::ui::stream(format_args!("Deployment {} applied", deployment.label()));
            }
            let mut committed = json!({ "applied": ran });
            if let (false, Some(object)) = (effects.is_empty(), committed.as_object_mut()) {
                object.insert("effects".to_owned(), json!(effects));
            }
            crate::ui::emit_committed(committed, Err(error)).map(|()| None)
        }
        Err(error) => Err(bound_to(error, &effects)),
    }
}

/// `error` after the note named what the removal was bound to: its `--json`
/// carries those `effects` too.
fn bound_to(error: Error, effects: &[DestructiveEffect]) -> Error {
    match effects.is_empty() {
        true => error,
        false => error.with_detail("effects", json!(effects)),
    }
}

/// Send `request` removing `what`. The typed name confirms it, so when it carries
/// no `--expect-version` and the Store refuses it only for a version, show what goes
/// and send it once more bound to the version that refusal reviewed. A refusal then
/// never asks for a version: it names what goes, and a lost Volume nobody accepted.
///
/// A name typed at the prompt confirms only what the prompt showed, in `prompted`
/// with the read that lists it again. Nothing is sent when the list read again now
/// differs. Nothing is sent again, and the refusal names what changed since, when
/// the refusal removes a Service or Volume the prompt didn't list under its
/// Environment, or when the list read again after the refusal differs.
/// `--confirm` showed nothing, so the name confirms the plan as it is. A plan that changed between the two sends refuses the second,
/// saying so. Returns the effects the second send was bound to, none without one,
/// and with an error that isn't the Store's refusal, which names what goes itself.
/// A send that failed, with what it was bound to when it failed after binding.
struct Unbound {
    error: StoreCallError,
    effects: Vec<DestructiveEffect>,
}

fn bound(
    request: &mut Remove,
    what: &str,
    prompted: Option<(&Shown, impl Fn() -> Result<Shown, StoreCallError>)>,
    mut send: impl FnMut(&Remove) -> Result<Removal, StoreCallError>,
) -> Result<(Removal, Vec<DestructiveEffect>), Box<Unbound>> {
    let unbound = |error| {
        Box::new(Unbound {
            error,
            effects: Vec::new(),
        })
    };
    if let Some((shown, listed)) = &prompted {
        let since = shown.changes(&listed().map_err(unbound)?);
        if !since.is_empty() {
            return Err(unbound(StoreCallError::Refused(ployz_core::RpcError {
                code: RpcErrorCode::ConfirmationRequired,
                message: format!(
                    "What this removes changed since you confirmed: {}. Nothing was \
                         removed. Run it again to go ahead",
                    since.join(", ")
                ),
                details: json!({}),
                cause: Vec::new(),
            })));
        }
    }
    if request.version.is_some() {
        return send(request)
            .map(|removal| (removal, Vec::new()))
            .map_err(unbound);
    }
    let refused = match send(request) {
        Err(StoreCallError::Refused(refused)) => refused,
        sent => return sent.map(|removal| (removal, Vec::new())).map_err(unbound),
    };
    let Some(Reviewed {
        version,
        effects,
        by_environment,
        unaccepted: false,
    }) = Reviewed::of(&refused, &request.accept_volume_loss)
    else {
        return Err(unbound(StoreCallError::Refused(ask(
            refused,
            request,
            what,
            &Changed::No,
        ))));
    };
    if let Some((shown, listed)) = prompted {
        let unlisted = shown.unlisted(&effects, by_environment.as_ref());
        let since = match unlisted.is_empty() {
            false => format!("it now {}", said(&unlisted)),
            true => shown.changes(&listed().map_err(unbound)?).join(", "),
        };
        if !since.is_empty() {
            let since = Changed::SinceConfirmed(since);
            return Err(unbound(StoreCallError::Refused(ask(
                refused, request, what, &since,
            ))));
        }
    }
    ui::note(format_args!("Removing {what} {}.", said(&effects)));
    request.version = Some(version);
    let removal = send(request).map_err(|error| match error {
        StoreCallError::Refused(refused) => unbound(StoreCallError::Refused(ask(
            refused,
            request,
            what,
            &Changed::WhileRunning,
        ))),
        error @ (StoreCallError::Cloud(_) | StoreCallError::Stopped(_)) => Box::new(Unbound {
            error,
            effects: effects.clone(),
        }),
    })?;
    Ok((removal, effects))
}

/// What a `confirmation_required` removal refusal reviewed.
struct Reviewed {
    version: String,
    effects: Vec<DestructiveEffect>,
    /// The same effects by the Environment they remove from, when the Store says.
    by_environment: Option<BTreeMap<EnvironmentName, Vec<DestructiveEffect>>>,
    /// Whether it loses a Volume `accepted` doesn't name.
    unaccepted: bool,
}

impl Reviewed {
    fn of(
        refused: &ployz_core::RpcError,
        accepted: &BTreeMap<EnvironmentName, Vec<VolumeName>>,
    ) -> Option<Self> {
        if refused.code != RpcErrorCode::ConfirmationRequired {
            return None;
        }
        let details = &refused.details;
        let lost: BTreeMap<EnvironmentName, Vec<VolumeName>> = match details.get("accept") {
            Some(accept) => serde_json::from_value(accept.clone()).ok()?,
            None => BTreeMap::new(),
        };
        let unaccepted = lost.iter().any(|(environment, volumes)| {
            volumes.iter().any(|volume| {
                !accepted
                    .get(environment)
                    .is_some_and(|names| names.contains(volume))
            })
        });
        Some(Self {
            version: details.get("version")?.as_str()?.to_owned(),
            effects: serde_json::from_value(details.get("effects")?.clone()).ok()?,
            by_environment: details
                .get("effects_by_environment")
                .and_then(|by| serde_json::from_value(by.clone()).ok()),
            unaccepted,
        })
    }
}

/// How what a removal deletes moved away from what this command confirmed.
enum Changed {
    No,
    /// Between the prompt and the first send, in a clause.
    SinceConfirmed(String),
    /// Between the first send and the one bound to its version.
    WhileRunning,
}

/// The Services and Volumes the prompt listed before a person typed the name, by
/// Environment.
#[derive(Debug, Default)]
pub(super) struct Shown(BTreeMap<EnvironmentName, Listed>);

#[derive(Debug, Default)]
struct Listed {
    services: std::collections::BTreeSet<String>,
    volumes: std::collections::BTreeSet<String>,
}

impl Shown {
    /// What the prompt listed for each of `inventories`.
    pub(super) fn of<'a>(inventories: impl IntoIterator<Item = &'a Inventory>) -> Self {
        let mut shown = Self::default();
        for inventory in inventories {
            let listed = shown
                .0
                .entry(inventory.environment.name.clone())
                .or_default();
            listed
                .services
                .extend(inventory.services.iter().map(ToString::to_string));
            listed.volumes.extend(
                inventory
                    .volumes
                    .iter()
                    .filter_map(|volume| Some(volume.get("name")?.as_str()?.to_owned())),
            );
        }
        shown
    }

    /// The effects that remove a Service or a Volume the prompt didn't list under
    /// the Environment it goes from. Other kinds, such as a domain, go with the
    /// Services it listed. Without `by_environment` nothing can be matched, so every
    /// effect counts as unlisted.
    fn unlisted(
        &self,
        effects: &[DestructiveEffect],
        by_environment: Option<&BTreeMap<EnvironmentName, Vec<DestructiveEffect>>>,
    ) -> Vec<DestructiveEffect> {
        let Some(by_environment) = by_environment else {
            return effects.to_vec();
        };
        let none = Listed::default();
        by_environment
            .iter()
            .flat_map(|(environment, effects)| {
                let listed = self.0.get(environment).unwrap_or(&none);
                effects.iter().filter(move |effect| !listed.has(effect))
            })
            .cloned()
            .collect()
    }
}

impl Shown {
    /// How `now` differs from what the prompt listed, each as a clause.
    fn changes(&self, now: &Self) -> Vec<String> {
        let none = Listed::default();
        let environments: std::collections::BTreeSet<&EnvironmentName> =
            self.0.keys().chain(now.0.keys()).collect();
        let mut changes = Vec::new();
        for environment in environments {
            match (
                self.0.contains_key(environment),
                now.0.contains_key(environment),
            ) {
                (false, true) => changes.push(format!("Environment {environment} is new")),
                (true, false) => changes.push(format!("Environment {environment} is gone")),
                (true, true) | (false, false) => {}
            }
            let shown = self.0.get(environment).unwrap_or(&none);
            let listed = now.0.get(environment).unwrap_or(&none);
            for (kind, shown, listed) in [
                ("Service", &shown.services, &listed.services),
                ("Volume", &shown.volumes, &listed.volumes),
            ] {
                for name in listed.difference(shown) {
                    changes.push(format!("{environment} now has {kind} {name}"));
                }
                for name in shown.difference(listed) {
                    changes.push(format!("{environment} no longer has {kind} {name}"));
                }
            }
        }
        changes
    }
}

impl Listed {
    fn has(&self, effect: &DestructiveEffect) -> bool {
        match effect.kind {
            DestructiveKind::RemovesService => self.services.contains(&effect.name),
            DestructiveKind::DeletesVolume | DestructiveKind::DetachesVolume => {
                self.volumes.contains(&effect.name)
            }
            DestructiveKind::RemovesDomain
            | DestructiveKind::RemovesServer
            | DestructiveKind::RemovesMember
            | DestructiveKind::ChangesRole
            | DestructiveKind::RevokesInvite
            | DestructiveKind::StopsService
            | DestructiveKind::RestartsService
            | DestructiveKind::UnprotectsEnvironment
            | DestructiveKind::RemovesOrganization => true,
        }
    }
}

/// `refused` as a person who typed the name reads it: what goes, without the
/// version, and whether it `changed` since this command showed it.
fn ask(
    mut refused: ployz_core::RpcError,
    request: &Remove,
    what: &str,
    changed: &Changed,
) -> ployz_core::RpcError {
    let Some(Reviewed {
        effects,
        unaccepted,
        ..
    }) = Reviewed::of(&refused, &request.accept_volume_loss)
    else {
        return refused;
    };
    if let Some(details) = refused.details.as_object_mut() {
        details.remove("version");
    }
    let changed = match changed {
        Changed::No => String::new(),
        Changed::SinceConfirmed(since) => {
            format!("What this removes changed since you confirmed: {since}. Nothing was removed. ")
        }
        Changed::WhileRunning => {
            "What this removes changed while it ran, so nothing was removed. ".to_owned()
        }
    };
    let go = match unaccepted {
        true => "Accept each lost Volume by name to go ahead",
        false => "Run it again to go ahead",
    };
    refused.message = format!("{changed}Removing {what} {}. {go}", said(&effects));
    refused
}

/// Each effect as a person reads it, in one clause.
fn said(effects: &[DestructiveEffect]) -> String {
    effects
        .iter()
        .map(crate::approval::line)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Where a removal ended, and the removal Deployments this command ran.
#[derive(serde::Serialize)]
pub(super) struct Removed {
    #[serde(flatten)]
    pub(super) removal: Removal,
    /// What the typed name confirmed, as the Store reviewed it: what the removal
    /// was bound to when this command bound it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) effects: Vec<DestructiveEffect>,
    #[serde(rename = "deployments")]
    pub(super) ran: Vec<DeploymentSummary>,
}

impl Removed {
    /// Each line a person reads about it, after the ones `target` names.
    pub(super) fn say(&self, target: &str) {
        for deployment in &self.ran {
            crate::ui::stream(format_args!(
                "Took an Environment off the Servers (Deployment {}).",
                deployment.label()
            ));
        }
        match &self.removal {
            Removal::Removed { residuals } => {
                for residual in residuals {
                    crate::ui::stream(format_args!(
                        "No Server was left to take {} off; if a Server still runs it, it keeps running until you remove it there.",
                        residual.environment
                    ));
                }
                crate::ui::stream(format_args!("Removed {target}."));
            }
            Removal::Removing {
                left, waiting_on, ..
            } => {
                for left in left {
                    let why = left
                        .why
                        .as_deref()
                        .map(|why| format!(": {why}"))
                        .unwrap_or_default();
                    crate::ui::stream(format_args!(
                        "{} {}{why}",
                        left.environment.name,
                        store::word(&left.status)
                    ));
                }
                for waiting in waiting_on {
                    crate::ui::stream(format_args!(
                        "Waiting on {}: {}",
                        waiting.server, waiting.why
                    ));
                }
                crate::ui::stream(format_args!("Cloud goes on removing {target}."));
            }
        }
    }

    /// Exit 3 when a Server didn't answer, unless `--detach` asked not to wait.
    pub(super) fn ended(&self, matches: &ArgMatches) -> Result<(), Error> {
        match (&self.removal, matches.get_flag("detach")) {
            (Removal::Removing { waiting_on, .. }, false) if !waiting_on.is_empty() => {
                Err(Error::partial())
            }
            _ => Ok(()),
        }
    }
}

/// `left` with each admitted Deployment as it ended in `ran`.
fn applied(mut left: Vec<Left>, ran: &[DeploymentSummary]) -> Vec<Left> {
    for left in &mut left {
        let Some(admitted) = left.deployment.as_mut() else {
            continue;
        };
        if let Some(ended) = ran.iter().find(|ended| ended.id == admitted.id) {
            admitted.clone_from(ended);
        }
    }
    left
}

/// Whether following a removal Deployment failed only because Cloud already swept
/// it with its Environment: the Deployment is `not_found` and the target is `gone`.
fn swept(error: &Error, gone: impl FnOnce() -> Result<bool, Error>) -> bool {
    error.report().code == RpcErrorCode::NotFound && matches!(gone(), Ok(true))
}

/// Wait until Cloud deleted the target once its removal Deployments applied: it
/// sweeps after other cleanup, with no deadline.
fn swept_by_cloud(
    store: &Store,
    project: &ProjectName,
    environment: Option<&EnvironmentName>,
) -> Result<(), Error> {
    let signal = crate::cancellation::interrupted()?;
    let runtime = super::runtime()?;
    let mut slow = Some(std::time::Instant::now() + SLOW);
    while !gone(store, project, environment)? {
        if slow.is_some_and(|after| std::time::Instant::now() >= after) {
            slow = None;
            ui::note(format_args!(
                "Cloud is still removing {}",
                target(project, environment)
            ));
        }
        runtime.block_on(async {
            tokio::select! {
                () = signal.cancelled() => {},
                () = tokio::time::sleep(deploy::FOLLOW_POLL) => {},
            }
        });
        if signal.is_cancelled() {
            return Err(Error::cancelled());
        }
    }
    Ok(())
}

/// How long `swept_by_cloud` waits before saying Cloud is still at it.
const SLOW: std::time::Duration = std::time::Duration::from_secs(30);

/// What goes before an Environment of `project` the Store won't remove alone: its
/// first Branch, its protection, or for a Default Environment another Default.
fn goes_first(error: &StoreCallError, project: &ProjectName) -> Option<String> {
    let StoreCallError::Refused(refusal) = error else {
        return None;
    };
    if refusal.code != RpcErrorCode::Unsupported {
        return None;
    }
    let details = &refusal.details;
    let branch = details
        .get("branches")
        .and_then(|branches| branches.get(0))
        .and_then(serde_json::Value::as_str);
    let protected = details.get("protected").and_then(serde_json::Value::as_str);
    let words: &[&str] = match (branch, protected) {
        (Some(branch), _) => &["env", "rm", branch],
        (None, Some(protected)) => &["env", "protect", protected, "--off"],
        (None, None) => &["env", "default", "ENV"],
    };
    let project = ["--project", project.as_str()];
    Some(shell_words::join(
        ["ployz"]
            .into_iter()
            .chain(words.iter().copied())
            .chain(project),
    ))
}

/// Whether the Store deleted `environment` of `project`, or the whole Project.
fn gone(
    store: &Store,
    project: &ProjectName,
    environment: Option<&EnvironmentName>,
) -> Result<bool, Error> {
    let projects = store.read(&ProjectsQuery {})?.projects;
    let Some(listing) = projects.iter().find(|listing| &listing.name == project) else {
        return Ok(true);
    };
    Ok(environment.is_some_and(|name| !listing.environments.contains(name)))
}

/// `PROJECT/ENV`, or the Project alone when the whole Project goes.
fn target(project: &ProjectName, environment: Option<&EnvironmentName>) -> String {
    match environment {
        Some(name) => format!("{project}/{name}"),
        None => project.to_string(),
    }
}

/// A removal nothing more of can come off now.
fn stalled(project: &ProjectName, environment: Option<&EnvironmentName>, left: &[Left]) -> Error {
    let target = target(project, environment);
    let why = left
        .iter()
        .find_map(|left| {
            Some(format!(
                ": {} {}",
                left.environment.name,
                left.why.as_deref()?
            ))
        })
        .unwrap_or_default();
    Error::detailed(
        RpcErrorCode::Conflict,
        format!("Nothing more of {target} can come off the Servers now{why}"),
        json!({ "left": left }),
    )
}

/// Whether removal Deployment `deployment` found no Server left to take it off, so
/// whatever ran is still on the Servers that ran it: "Left on old servers".
pub(super) fn left_on_old_servers(deployment: &DeploymentSummary) -> bool {
    matches!(deployment.outcome, Some(ployz_store::Outcome::Forgotten))
}

/// Take Environment `at` off the Servers: admit a removal Deployment under the
/// destructive review, accepting the loss of `accept` as reviewed at `version`, then run or follow it as
/// `deploy` does. A volume-loss refusal names this command again, accepting.
pub(super) fn take_off(
    matches: &ArgMatches,
    store: &Store,
    at: &EnvironmentRef,
    (accept, version): (&[VolumeName], Option<String>),
    events: Option<std::io::BufWriter<std::fs::File>>,
) -> Result<(DeploymentView, Result<(), Error>), Error> {
    deploy::refuse_local_detach(matches, store)?;
    let admitted = store
        .admit(&Admit::ShutDown(ShutDown {
            id: DeploymentId::parse(mint())?,
            environment: at.clone(),
            version,
            accept_volume_loss: accept.to_vec(),
        }))
        .map_err(|error| store.accepting(error))?;
    let deploy::Shipped { view, ran, .. } =
        deploy::execute(matches, store, &admitted, None, events)?;
    Ok((view, ran))
}

/// Report a removal Deployment that didn't apply (yet), after the `applied` ones
/// before it, with the `effects` its removal was bound to, naming `again`, the
/// command that finishes it: exit 3, or 0 when `--detach` asked not to wait.
pub(super) fn unfinished(
    matches: &ArgMatches,
    (view, effects): (&DeploymentView, &[DestructiveEffect]),
    applied: &[DeploymentSummary],
    ran: Result<(), Error>,
    again: &str,
) -> Result<(), Error> {
    #[derive(serde::Serialize)]
    struct Unfinished<'a> {
        #[serde(flatten)]
        view: &'a DeploymentView,
        #[serde(skip_serializing_if = "<[_]>::is_empty")]
        applied: &'a [DeploymentSummary],
        #[serde(skip_serializing_if = "<[_]>::is_empty")]
        effects: &'a [DestructiveEffect],
    }
    let hint = Some(again.to_owned());
    crate::ui::finish(
        &store::Next::new(
            &Unfinished {
                view,
                applied,
                effects,
            },
            hint,
        ),
        || {
            for deployment in applied {
                crate::ui::stream(format_args!("Deployment {} applied", deployment.label()));
            }
            deploy::say_view(view);
        },
    )?;
    ran.map_err(|error| error.hint(Hint::Retry(again.to_owned())))
        .and_then(|()| match matches.get_flag("detach") {
            true => Ok(()),
            false => Err(Error::partial()),
        })
}

/// What removing an Environment deletes: its Services and Volumes, by name.
#[derive(serde::Serialize)]
pub(super) struct Inventory {
    pub(super) environment: EnvironmentSummary,
    pub(super) services: Vec<ServiceName>,
    pub(super) volumes: Vec<serde_json::Value>,
}

impl Inventory {
    /// A line for its Services and one for its Volumes, each only when it has some.
    pub(super) fn branches(&self) -> Vec<Tree> {
        let volumes = self
            .volumes
            .iter()
            .filter_map(|volume| volume.get("name")?.as_str())
            .collect::<Vec<_>>();
        [
            ("Services", super::joined(&self.services)),
            ("Volumes", volumes.join(", ")),
        ]
        .into_iter()
        .filter(|(_, names)| !names.is_empty())
        .map(|(kind, names)| Tree::leaf(format!("{kind}: {names}")))
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asks(version: &str, service: &str) -> StoreCallError {
        asks_in(version, "production", service)
    }

    fn asks_in(version: &str, environment: &str, service: &str) -> StoreCallError {
        let effect =
            json!({ "kind": "removes_service", "name": service, "node": "n", "path": "p" });
        StoreCallError::Refused(ployz_core::RpcError {
            code: RpcErrorCode::ConfirmationRequired,
            message: format!("Removing this will remove Service {service}. Pass the version"),
            details: json!({
                "volumes": {},
                "accept": {},
                "effects": [effect],
                "effects_by_environment": { environment: [effect] },
                "version": version,
            }),
            cause: Vec::new(),
        })
    }

    fn request() -> Remove {
        Remove {
            project: ProjectName::parse("shop").unwrap(),
            environment: None,
            version: None,
            accept_volume_loss: BTreeMap::new(),
            every_copy: BTreeMap::new(),
        }
    }

    #[test]
    fn a_removal_binds_the_version_it_was_refused_with_and_sends_once_more() {
        let mut sent = Vec::new();
        let removed = bound(&mut request(), "shop", unprompted(), |request| {
            sent.push(request.version.clone());
            match request.version.as_deref() {
                Some("v1") => Ok(Removal::Removed {
                    residuals: Vec::new(),
                }),
                _ => Err(asks("v1", "web")),
            }
        });
        let Ok((Removal::Removed { .. }, effects)) = removed else {
            panic!("the bound send didn't remove it");
        };
        assert_eq!(
            effects.first().map(|effect| effect.name.as_str()),
            Some("web")
        );
        assert_eq!(sent, [None, Some("v1".to_owned())]);
    }

    #[test]
    fn a_plan_that_changed_before_the_bound_send_is_refused_without_a_version() {
        let mut sent = 0;
        let refused = bound(&mut request(), "shop", unprompted(), |_| {
            sent += 1;
            Err(asks(if sent == 1 { "v1" } else { "v2" }, "api"))
        });
        let Some(refused) = refusal(refused) else {
            panic!("a changed plan goes ahead");
        };
        assert_eq!(sent, 2, "it does not loop");
        assert_eq!(refused.code, RpcErrorCode::ConfirmationRequired);
        assert_eq!(
            refused.message,
            "What this removes changed while it ran, so nothing was removed. Removing shop \
             removes Service api. Run it again to go ahead"
        );
        assert_eq!(refused.details.get("version"), None);
    }

    type Reread = fn() -> Result<Shown, StoreCallError>;

    fn unprompted() -> Option<(&'static Shown, Reread)> {
        None
    }

    /// The refusal a bound send ended with, if it ended refused.
    fn refusal(
        bound: Result<(Removal, Vec<DestructiveEffect>), Box<Unbound>>,
    ) -> Option<ployz_core::RpcError> {
        match bound.err()?.error {
            StoreCallError::Refused(refused) => Some(refused),
            StoreCallError::Cloud(_) | StoreCallError::Stopped(_) => None,
        }
    }

    #[test]
    fn a_service_staged_while_the_prompt_was_open_is_not_removed_without_effects() {
        // Nothing ran, so the Store removes without asking for a version; a `worker`
        // staged since the prompt listed `web` would go with it.
        let prompt = shown(&["web"]);
        let mut sent = 0;
        let again = || Ok(shown(&["web", "worker"]));
        let refused = bound(&mut request(), "shop", Some((&prompt, again)), |_| {
            sent += 1;
            Ok(Removal::Removed {
                residuals: Vec::new(),
            })
        });
        assert_eq!(sent, 0, "it sends nothing the person didn't see");
        let Some(refused) = refusal(refused) else {
            panic!("a staged worker the person never saw goes");
        };
        assert_eq!(refused.code, RpcErrorCode::ConfirmationRequired);
        assert_eq!(
            refused.message,
            "What this removes changed since you confirmed: production now has Service \
             worker. Nothing was removed. Run it again to go ahead"
        );
    }

    #[test]
    fn an_empty_environment_added_with_the_first_refusal_is_not_removed() {
        // An Environment that never deployed is not in the Store's review, so its
        // version doesn't change when one is added while the first send is refused.
        let prompt = shown(&["web"]);
        let reads = std::cell::Cell::new(0);
        let again = || {
            reads.set(reads.get() + 1);
            Ok(match reads.get() {
                1 => shown(&["web"]),
                _ => shown_in(&[("production", &["web"]), ("staging", &[])]),
            })
        };
        let mut sent = 0;
        let refused = bound(&mut request(), "shop", Some((&prompt, again)), |_| {
            sent += 1;
            Err(asks("v1", "web"))
        });
        let Some(refused) = refusal(refused) else {
            panic!("an empty staging the person never saw goes");
        };
        assert_eq!(
            sent, 1,
            "it sends nothing bound to what the person didn't see"
        );
        assert!(
            refused.message.starts_with(
                "What this removes changed since you confirmed: Environment staging is new."
            ),
            "{}",
            refused.message
        );
        assert_eq!(refused.details.get("version"), None);
    }

    fn shown(services: &[&str]) -> Shown {
        shown_in(&[("production", services)])
    }

    /// What a prompt listed: each Environment with its Services.
    fn shown_in(environments: &[(&str, &[&str])]) -> Shown {
        Shown(
            environments
                .iter()
                .map(|(environment, services)| {
                    (
                        EnvironmentName::parse(*environment).unwrap(),
                        Listed {
                            services: services.iter().map(|name| (*name).to_owned()).collect(),
                            volumes: std::collections::BTreeSet::new(),
                        },
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn a_service_shown_in_another_environment_is_refused_without_sending_again() {
        // The prompt listed production/web and an empty staging; staging/web was
        // deployed after the list was read again, before the first send.
        let listed = shown_in(&[("production", &["web"]), ("staging", &[])]);
        let again = || Ok(shown_in(&[("production", &["web"]), ("staging", &[])]));
        let mut sent = 0;
        let refused = bound(&mut request(), "shop", Some((&listed, again)), |_| {
            sent += 1;
            Err(asks_in("v1", "staging", "web"))
        });
        let Some(refused) = refusal(refused) else {
            panic!("staging/web goes though the person never saw it");
        };
        assert_eq!(
            sent, 1,
            "it sends nothing bound to what the person didn't see"
        );
        assert!(
            refused.message.starts_with(
                "What this removes changed since you confirmed: it now removes Service web."
            ),
            "{}",
            refused.message
        );
        assert_eq!(refused.details.get("version"), None);
    }

    #[test]
    fn a_refusal_without_effects_by_environment_is_never_sent_again_after_a_prompt() {
        let prompt = shown(&["web"]);
        let mut sent = 0;
        let refused = bound(
            &mut request(),
            "shop",
            Some((&prompt, || Ok(shown(&["web"])))),
            |_| {
                sent += 1;
                let mut asked = asks("v1", "web");
                if let StoreCallError::Refused(refused) = &mut asked
                    && let Some(details) = refused.details.as_object_mut()
                {
                    details.insert("effects_by_environment".to_owned(), serde_json::Value::Null);
                }
                Err(asked)
            },
        );
        assert!(refusal(refused).is_some());
        assert_eq!(sent, 1);
    }

    #[test]
    fn a_service_the_prompt_did_not_list_is_refused_without_sending_again() {
        let prompt = shown(&["web"]);
        let mut sent = 0;
        let refused = bound(
            &mut request(),
            "shop",
            Some((&prompt, || Ok(shown(&["web"])))),
            |_| {
                sent += 1;
                Err(asks("v1", "api"))
            },
        );
        let Some(refused) = refusal(refused) else {
            panic!("a Service the person never saw goes");
        };
        assert_eq!(
            sent, 1,
            "it sends nothing bound to what the person didn't see"
        );
        assert_eq!(refused.code, RpcErrorCode::ConfirmationRequired);
        assert_eq!(
            refused.message,
            "What this removes changed since you confirmed: it now removes Service api. \
             Nothing was removed. Removing shop removes Service api. Run it again to go ahead"
        );
        assert_eq!(refused.details.get("version"), None);
    }

    #[test]
    fn what_the_prompt_listed_is_removed_bound_to_its_version() {
        let prompt = shown(&["web"]);
        let mut sent = Vec::new();
        let removed = bound(
            &mut request(),
            "shop",
            Some((&prompt, || Ok(shown(&["web"])))),
            |request| {
                sent.push(request.version.clone());
                match request.version.as_deref() {
                    Some("v1") => Ok(Removal::Removed {
                        residuals: Vec::new(),
                    }),
                    _ => Err(asks("v1", "web")),
                }
            },
        );
        let Ok((Removal::Removed { .. }, effects)) = removed else {
            panic!("the bound send didn't remove it");
        };
        assert_eq!(
            effects.first().map(|effect| effect.name.as_str()),
            Some("web")
        );
        assert_eq!(sent, [None, Some("v1".to_owned())]);
    }

    #[test]
    fn a_bound_send_that_fails_still_reports_what_it_was_bound_to() {
        let mut sent = 0;
        let failed = bound(&mut request(), "shop", unprompted(), |_| {
            sent += 1;
            match sent {
                1 => Err(asks("v1", "web")),
                _ => Err(StoreCallError::Stopped(Error::unavailable(
                    "Cloud is down.",
                ))),
            }
        });
        let Err(failed) = failed else {
            panic!("the bound send didn't fail as Cloud did");
        };
        let Unbound {
            error: StoreCallError::Stopped(_),
            effects,
        } = *failed
        else {
            panic!("the bound send didn't fail as Cloud did");
        };
        let report = bound_to(Error::unavailable("Cloud is down."), &effects).report();
        assert_eq!(
            report.details.pointer("/effects/0/name"),
            Some(&json!("web")),
            "{report:?}"
        );
        assert_eq!(report.code, RpcErrorCode::Unavailable);
    }

    #[test]
    fn a_passed_version_is_sent_as_typed_and_only_once() {
        let mut request = Remove {
            version: Some("typed".to_owned()),
            ..request()
        };
        let mut sent = 0;
        let refused = bound(&mut request, "shop", unprompted(), |_| {
            sent += 1;
            Err(asks("v1", "web"))
        });
        assert_eq!(sent, 1);
        let Some(refused) = refusal(refused) else {
            panic!("a stale version goes ahead");
        };
        assert_eq!(refused.details.get("version"), Some(&json!("v1")));
    }

    #[test]
    fn a_missing_deployment_is_swept_only_once_its_target_is_gone() {
        let missing = || Error::not_found("No Deployment d1.");
        assert!(swept(&missing(), || Ok(true)));
        assert!(!swept(&missing(), || Ok(false)));
        assert!(!swept(&missing(), || Err(Error::unavailable(
            "Cloud is down."
        ))));
        assert!(!swept(&Error::unavailable("Cloud is down."), || Ok(true)));
    }
}
