//! `ployz key rotate`: replace a CI or agent key with a new one holding the same grant.
//! Every Member takes the new key only over the grant this CLI read for the old one there.
//! The versions the old key signed that still run are carried: each checked here under
//! the old key, shown, signed again under the new key and redeployed, so nothing stays
//! admitted under the old key. The old key is removed only once each redeploy applied
//! with no gaps and a fresh read finds both grants as this run left them. The new key
//! waits beside the config until then, so a run that stops resumes with it.

use std::collections::{BTreeMap, BTreeSet};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use clap::{ArgMatches, Command};
use ployz_core::authority::{
    Approver, KeyChange, KeyKind, KeyMember, PayloadDigest, Prior, ServiceVersion, Signed,
    SignerKey, SigningPublicKey, Timestamp, TrustedGrant, TrustedKeys,
};
use ployz_core::{DeviceCode, DeviceKey, MachineId, Namespace, ServiceName};
use ployz_store::{
    Admit, Deploy, DeploymentId, EnvironmentRef, NamespaceQuery, NamespacesQuery, PlanQuery,
    PostedVersion, Running, RunningQuery, TagPin,
};
use serde::Serialize;

use super::super::device::{DeviceSigning, key};
use super::super::store::{Store, mint, store};
use super::super::{Error, config_path, deploy, leaf_matches, route, runtime};
use super::members::{Bundle, Member, Roster};
use super::{
    Contents, LEAKED, code_arg, context, grant_moved, new_key, own_signing, remove_everywhere,
    results, typed_code,
};
use crate::cli::base;
use crate::ui;

pub(crate) fn rotate_command(command: Command) -> Command {
    base(
        command,
        "Replace a CI or agent key, and redeploy what it signed",
    )
    .after_help(format!(
        "{LEAKED}\n\n\
         It follows each redeploy to the end, and removes the old key only once every \
         one applied. If it stops before that, run the same command again.\n\n\
         Examples:\n  ployz key rotate mmg42klgyq"
    ))
    .arg(deploy::events_arg())
    .arg(code_arg())
}

pub(crate) fn rotate(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let code = typed_code(leaf)?;
    let signing = own_signing(leaf)?;
    let pending = pending_path(&config_path(leaf)?, code);
    let store = store(root)?;
    let events = deploy::open_events(leaf)?;
    let runtime = runtime()?;
    let mut client = runtime.block_on(route::connect_full(leaf, context(leaf)))?;
    let roster = runtime.block_on(Roster::read(&mut client))?;
    roster.require_all()?;
    let cluster = roster.cluster()?;
    let seed = pending_key(&pending)?;
    let new = SigningPublicKey::from(key::public(&seed));
    let old = source(&roster, code)?;
    let Replacement { missing, installed } = replacement(&roster, &old, new)?;
    let carried = carried(&store, &old.grant, installed.as_ref())?;
    show(&old.grant, new, &carried, &roster);
    ui::confirm(
        &format!("Rotate key {code} to {}?", new.code()),
        || {
            Error::coded(
                ployz_core::RpcErrorCode::ConfirmationRequired,
                "Rotating a key needs you at a terminal. Nothing changed.",
            )
        },
        "Nothing changed.",
    )?;
    if !missing.is_empty() {
        let grant = &old.grant.grant;
        let wanted = super::Wanted {
            scope: grant.scope.clone(),
            volumes: grant.volumes.clone(),
            name: grant.name.clone(),
            shown: Vec::new(),
        };
        // Each Member takes the new key over the old grant as it read there.
        let mut by_read: BTreeMap<u64, (Timestamp, Vec<_>)> = BTreeMap::new();
        for (member, read) in &missing {
            by_read
                .entry(read.as_millis())
                .or_insert_with(|| (*read, Vec::new()))
                .1
                .push(*member);
        }
        let changes = by_read
            .into_values()
            .map(|(issued_at, members)| KeyChange::Trust {
                key: new_key(grant.kind, new, &wanted),
                members,
                prior: Some(Prior::Replace {
                    key: grant.key,
                    issued_at,
                }),
            })
            .collect();
        let bundle = Bundle::sign(&signing, cluster, changes)?;
        let (answers, gaps) = runtime.block_on(bundle.send(&client, &roster.reach));
        if !gaps.is_complete() {
            let moved = gaps.failures.iter().any(grant_moved);
            let servers = super::answered(&roster.reach, answers);
            return ui::finish_fanout("servers", &servers, &gaps, || {
                ui::rows(&results(&servers));
                match moved {
                    true => ui::note(format_args!(
                        "Key {code} changed on a server while this ran. The old key still works. Check ployz key ls, then run ployz key rotate {code} again."
                    )),
                    false => ui::note(format_args!(
                        "The old key still works. Run ployz key rotate {code} again once those servers answer."
                    )),
                }
            });
        }
    }
    ui::stream(format_args!("PLOYZ_KEY={}", hex::encode(seed)));
    ui::note("Replace the old PLOYZ_KEY with this one where the key deploys from.");
    let resigner = DeviceSigning::from_seed(&seed);
    let mut deployments = Vec::new();
    for environment in &carried {
        let events = events
            .as_ref()
            .map(|events| events.get_ref().try_clone().map(BufWriter::new))
            .transpose()
            .map_err(|source| {
                Error::detailed_caused(
                    ployz_core::RpcErrorCode::Internal,
                    "Can't write the --events file for the next redeploy.",
                    serde_json::Value::Null,
                    Some(source),
                )
            })?;
        deployments.extend(redeploy(leaf, &store, environment, &resigner, events)?);
    }
    let now = runtime.block_on(Roster::read(&mut client))?;
    now.require_all()?;
    unmoved(&now, &old, new)?;
    let (removed, gaps) =
        runtime.block_on(remove_everywhere(&client, &now.reach, old.grant.grant.key));
    let rotated = Rotated {
        old: code,
        new: new.code(),
        deployments,
        removed: super::answered(&now.reach, removed),
    };
    if gaps.is_complete() {
        std::fs::remove_file(&pending).map_err(|source| key::KeyError::Write {
            path: pending.clone(),
            source,
        })?;
    }
    ui::finish_fanout("rotated", &rotated, &gaps, || {
        ui::stream(format_args!("Rotated key {code} to {}.", rotated.new));
        if !gaps.is_complete() {
            ui::note(format_args!(
                "Run ployz key rm {code} once those servers answer, so none of them trusts the old key."
            ));
        }
    })
}

#[derive(Serialize)]
struct Rotated {
    old: DeviceCode,
    new: DeviceCode,
    /// The numbers of the redeploys that carried what the old key signed.
    deployments: Vec<u64>,
    removed: Vec<super::Answer>,
}

/// Where a rotation keeps its new key until the old one is removed.
fn pending_path(config: &Path, code: DeviceCode) -> PathBuf {
    let device = key::path(config);
    device
        .parent()
        .unwrap_or(config)
        .join("rotating")
        .join(format!("{code}.key"))
}

/// The new key a stopped rotation left, else a new one written there first.
fn pending_key(path: &Path) -> Result<[u8; 32], Error> {
    if let Some(seed) = key::load(path)? {
        return Ok(seed);
    }
    let seed = key::generate();
    key::store(path, &seed)?;
    Ok(key::load(path)?.unwrap_or(seed))
}

/// The grant the rotation replaces, and when each Member's copy was issued.
pub(super) struct Source {
    pub(super) grant: TrustedGrant,
    read: BTreeMap<MachineId, Timestamp>,
}

/// The grant every Member holds for the key `code` names, the one the rotation replaces.
/// The copies may differ in when they were issued, as after a scope change that one
/// Member took later, but not in what they give.
///
/// # Errors
/// Refuses when a Member holds none, or Members hold different grants.
fn source(roster: &Roster, code: DeviceCode) -> Result<Source, Error> {
    let mut held = Vec::new();
    for member in &roster.members {
        let Some(grant) = member.grant(code) else {
            return Err(Error::conflict(format!(
                "{} doesn't hold key {code}, so it can't be rotated there. Check ployz key ls. Nothing changed.",
                member.name
            )));
        };
        held.push((member.key.id, grant));
    }
    let Some((_, first)) = held.first() else {
        return Err(Error::not_found("No servers answered. Nothing changed."));
    };
    let gives = |grant: &TrustedGrant| {
        (
            grant.grant.key,
            grant.grant.kind,
            Contents::of(&grant.grant),
        )
    };
    if held.iter().any(|(_, grant)| gives(grant) != gives(first)) {
        return Err(Error::conflict(format!(
            "Your servers hold different scopes for key {code}. Run ployz key scope {code} to settle it first. Nothing changed."
        )));
    }
    Ok(Source {
        grant: (*first).clone(),
        read: held
            .iter()
            .map(|(machine_id, grant)| (*machine_id, grant.grant.issued_at))
            .collect(),
    })
}

/// Where the new key stands on the Members.
struct Replacement {
    /// The Members that don't hold it yet, each with when its copy of the old grant was
    /// issued.
    missing: Vec<(KeyMember, Timestamp)>,
    /// Its grant, where a Member holds it.
    installed: Option<TrustedGrant>,
}

/// Where the new key stands on each Member.
/// A Member that holds a key under the new code holds the new key itself, with what the
/// old one gives, or the rotation stops.
///
/// # Errors
/// Refuses when a Member holds another key under the new code, or the new key with
/// another kind or scope.
fn replacement(
    roster: &Roster,
    old: &Source,
    new: SigningPublicKey,
) -> Result<Replacement, Error> {
    let mut missing = Vec::new();
    let mut installed = None;
    for member in &roster.members {
        match member.grant(new.code()) {
            None => missing.extend(old.read.get(&member.key.id).map(|read| (member.key, *read))),
            Some(held) if replaces(held, &old.grant, new) => {
                installed.get_or_insert_with(|| held.clone());
            }
            Some(_) => {
                return Err(Error::conflict(format!(
                    "{} holds the new key {} with another scope than key {}. Make them match with ployz key scope, then run this again. Nothing changed.",
                    member.name,
                    new.code(),
                    old.grant.grant.key.code()
                )));
            }
        }
    }
    Ok(Replacement { missing, installed })
}

/// Whether `held` is the new key `new` with what `old` gives.
fn replaces(held: &TrustedGrant, old: &TrustedGrant, new: SigningPublicKey) -> bool {
    held.grant.key == new
        && held.grant.kind == old.grant.kind
        && Contents::of(&held.grant) == Contents::of(&old.grant)
}

/// Refuse to remove the old key unless every Member still holds it as this run read it,
/// and holds the new key with what it gives: a scope change made while this ran would
/// otherwise be lost, or the new key would keep a grant the old one no longer has.
///
/// # Errors
/// Returns `conflict` naming the first Member where either grant moved.
fn unmoved(now: &Roster, old: &Source, new: SigningPublicKey) -> Result<(), Error> {
    let code = old.grant.grant.key.code();
    for member in &now.members {
        let kept = member.grant(code).is_some_and(|held| {
            held.grant.key == old.grant.grant.key
                && old.read.get(&member.key.id) == Some(&held.grant.issued_at)
                && Contents::of(&held.grant) == Contents::of(&old.grant.grant)
        });
        let taken = member
            .grant(new.code())
            .is_some_and(|held| replaces(held, &old.grant, new));
        if !kept || !taken {
            return Err(moved(member, code));
        }
    }
    Ok(())
}

fn moved(member: &Member, code: DeviceCode) -> Error {
    Error::conflict(format!(
        "Key {code} changed on {} while this ran, so the old key stays trusted. Check ployz key ls, then run ployz key rotate {code} again.",
        member.name
    ))
}

/// One Environment's Services the old key signed what runs.
#[derive(Debug)]
pub(super) struct Carried {
    environment: EnvironmentRef,
    namespace: Namespace,
    services: Vec<CarriedVersion>,
}

#[derive(Debug)]
pub(super) struct CarriedVersion {
    /// As Setting paths address it.
    authored: ServiceName,
    pub(super) version: ServiceVersion,
    image: String,
    pin: Option<TagPin>,
}

/// What the old key signed that runs now, in each place its grant names, checked here.
/// A Service whose running version the new key, as `installed` holds it, already signed
/// is carried already.
///
/// # Errors
/// Returns the Store's refusal, and refuses the whole rotation when any version naming
/// the old key doesn't check under it.
fn carried(
    store: &Store,
    old: &TrustedGrant,
    installed: Option<&TrustedGrant>,
) -> Result<Vec<Carried>, Error> {
    let owned = store.read(&NamespacesQuery {})?.namespaces;
    let mut wanted: BTreeMap<Namespace, Option<BTreeSet<ServiceName>>> = BTreeMap::new();
    match &old.grant.scope.places {
        ployz_core::authority::KeyPlaces::Cluster => {
            for owned in &owned {
                wanted.insert(owned.namespace.clone(), None);
            }
        }
        ployz_core::authority::KeyPlaces::Only(places) => {
            for place in places {
                let entry = wanted
                    .entry(place.namespace.clone())
                    .or_insert_with(|| Some(BTreeSet::new()));
                match (&place.service, entry.as_mut()) {
                    (Some(service), Some(names)) => {
                        names.insert(service.name.clone());
                    }
                    (None, _) => *entry = None,
                    (Some(_), None) => {}
                }
            }
        }
    }
    let mut carried = Vec::new();
    for (namespace, only) in wanted {
        let Some(owned) = owned.iter().find(|owned| owned.namespace == namespace) else {
            continue;
        };
        let environment = EnvironmentRef {
            project: Some(owned.project.clone()),
            environment: Some(owned.environment.clone()),
        };
        let runtime_names = store
            .read(&NamespaceQuery {
                environment: environment.clone(),
            })?
            .services;
        let services: Vec<ServiceName> = runtime_names
            .values()
            .filter(|runtime| only.as_ref().is_none_or(|only| only.contains(*runtime)))
            .cloned()
            .collect();
        if services.is_empty() {
            continue;
        }
        let running = store.read(&RunningQuery {
            environment: environment.clone(),
            services,
        })?;
        let mut versions = Vec::new();
        for (runtime, ran) in &running.services {
            let Some(version) = verified((old, installed), &running.namespace, runtime, ran)?
            else {
                continue;
            };
            let authored = runtime_names
                .iter()
                .find(|(_, named)| *named == runtime)
                .map_or_else(|| runtime.clone(), |(authored, _)| authored.clone());
            versions.push(CarriedVersion {
                authored,
                version,
                image: ran.spec.container.image.clone(),
                pin: ran.pin.clone(),
            });
        }
        if !versions.is_empty() {
            carried.push(Carried {
                environment,
                namespace: running.namespace,
                services: versions,
            });
        }
    }
    Ok(carried)
}

/// The newest version of `runtime` the old key signed for what runs, checked under the
/// old key alone and against the spec it runs; `None` when the old key signed none, or
/// the new key, as it is installed, signed it too.
///
/// # Errors
/// Refuses when a version naming the old key doesn't check.
pub(super) fn verified(
    (old, installed): (&TrustedGrant, Option<&TrustedGrant>),
    namespace: &Namespace,
    runtime: &ServiceName,
    ran: &Running,
) -> Result<Option<ServiceVersion>, Error> {
    let digest = PayloadDigest::of(&ran.spec, namespace).ok();
    let signed_by = |grant: &TrustedGrant, statement: &Signed<ServiceVersion>| {
        let only = TrustedKeys::new([], [], [grant.clone()]);
        statement.open().is_ok_and(|version| {
            version.signer == SignerKey::Device(DeviceKey::from(grant.grant.key))
                && &version.service == runtime
                && &version.namespace == namespace
                && Some(version.digest) == digest
        }) && statement
            .approvers(&only)
            .is_ok_and(|by| by == [Approver::Signing(grant.grant.key)])
    };
    if let Some(new) = installed
        && ran
            .versions
            .iter()
            .any(|statement| signed_by(new, statement))
    {
        return Ok(None);
    }
    let signer = SignerKey::Device(DeviceKey::from(old.grant.key));
    for statement in &ran.versions {
        let Ok(version) = statement.open() else {
            continue;
        };
        if version.signer != signer {
            continue;
        }
        if !signed_by(old, statement) {
            return Err(Error::conflict(format!(
                "A version of {runtime} that Ployz Cloud says key {} signed doesn't check against that key. Nothing changed.",
                old.grant.key.code()
            )));
        }
        return Ok(Some(version));
    }
    Ok(None)
}

/// Say what the rotation carries before the person confirms it.
fn show(old: &TrustedGrant, new: SigningPublicKey, carried: &[Carried], roster: &Roster) {
    let grant = &old.grant;
    ui::note(format_args!(
        "{} key {} becomes {}, with the same scope, trusted on {}.",
        KeyKind::from(grant.kind),
        grant.key.code(),
        DeviceKey::from(new).check_code(),
        roster.names()
    ));
    if carried.is_empty() {
        ui::note("Nothing it signed is running, so nothing is redeployed.");
        return;
    }
    ui::note("Redeploys under the new key:");
    for environment in carried {
        for service in &environment.services {
            ui::note(format_args!(
                "  {} in {}: {}, signed {}, digest {}",
                service.authored,
                environment.namespace,
                service.image,
                service.version.signed_at.utc(),
                service.version.digest
            ));
        }
    }
    ui::note("The old key is removed once every redeploy applied on every server.");
}

/// Sign each carried version again under the new key and deploy them, so every server
/// admits their Containers again under it, following the Deployment to its end. Its
/// number, once it applied with no gaps.
///
/// # Errors
/// Refuses when the plan for a carried Service no longer comes to the digest it runs,
/// and returns the Deploy's error, or the Deployment's failure when it didn't apply on
/// every server.
fn redeploy(
    leaf: &ArgMatches,
    store: &Store,
    carried: &Carried,
    resigner: &DeviceSigning,
    events: Option<BufWriter<std::fs::File>>,
) -> Result<Option<u64>, Error> {
    let services: Vec<ServiceName> = carried
        .services
        .iter()
        .map(|service| service.authored.clone())
        .collect();
    let plan = store.read(&PlanQuery {
        environment: carried.environment.clone(),
        services: services.clone(),
        target: true,
        deployment: None,
    })?;
    let target = plan.target.unwrap_or_default();
    let signer = SignerKey::Device(resigner.device_key());
    let mut versions = Vec::new();
    for service in &carried.services {
        let version = &service.version;
        let planned = target
            .iter()
            .find(|spec| spec.name == version.service)
            .and_then(|spec| {
                let mut spec = spec.clone();
                if let Some(pin) = &service.pin {
                    // The pin holds only for the tag it pinned: a staged image is a change.
                    if spec.container.image != pin.tag {
                        return None;
                    }
                    spec.container.image.clone_from(&pin.image);
                }
                Some(spec)
            });
        let same = planned
            .and_then(|spec| PayloadDigest::of(&spec, &plan.namespace).ok())
            .is_some_and(|digest| digest == version.digest);
        if !same {
            return Err(Error::conflict(format!(
                "{} in {} has staged changes. Deploy or discard them, then run this again. The old key still works.",
                service.authored, carried.namespace
            )));
        }
        let statement: Signed<ServiceVersion> = resigner.sign(&ServiceVersion {
            signer,
            proposal: None,
            forget_older: false,
            ..version.clone()
        })?;
        versions.push(PostedVersion {
            statement,
            pin: service.pin.clone(),
            births: BTreeMap::new(),
        });
    }
    let admitted = store
        .admit(&Admit::Deploy(Deploy {
            id: DeploymentId::parse(mint())?,
            environment: carried.environment.clone(),
            services,
            version: Some(plan.version),
            upload: None,
            accept_volume_loss: Vec::new(),
            message: Some("Key rotation".to_owned()),
            prune: false,
            no_prune: false,
            versions,
            recreate: true,
        }))
        .map_err(|error| store.fail(error))?;
    let shipped = deploy::execute(leaf, store, &admitted, None, events)?;
    if shipped.ran.is_ok() {
        return Ok(shipped.view.deployment.number);
    }
    ui::note(
        "The old key still works. Once a redeploy applies on every server, run ployz key rotate again.",
    );
    shipped.finish().and(Err(Error::partial()))
}
