//! `ployz key rotate`: replace a CI or agent key with a new one holding the same grant.
//! Every Member takes the new key only over the grant this CLI read for the old one there.
//! The versions the old key signed that still run are carried: each checked here under
//! the old key, shown, signed again under the new key and redeployed, so nothing stays
//! admitted under the old key. The old key is removed only once each redeploy applied
//! with no gaps, a read again finds nothing more the old key alone signed, and a fresh
//! read finds both grants as this run left them. The new key
//! waits beside the config until then, with the Services its redeploys applied, so a run
//! that stops resumes with it and redeploys the rest.

use std::collections::{BTreeMap, BTreeSet};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use clap::{ArgMatches, Command};
use ployz_core::authority::{
    Approver, KeyChange, KeyKind, KeyMember, PayloadDigest, Prior, ServiceVersion, Signed,
    SignerKey, SigningPublicKey, Timestamp, TrustedGrant, TrustedKeys,
};
use ployz_core::{DeviceCode, DeviceKey, MachineId, Namespace, Retired, ServiceName};
use ployz_store::{
    Admit, Deploy, DeploymentId, EnvironmentRef, NamespaceQuery, NamespacesQuery, PlanQuery,
    PostedVersion, Running, RunningQuery, TagPin,
};
use serde::{Deserialize, Serialize};

use super::super::store::{Store, mint, store};
use super::super::{Error, config_path, deploy, leaf_matches, route, runtime};
use super::members::{Bundle, Member, Roster};
use super::{
    Contents, LEAKED, Removal, code_arg, context, grant_moved, new_key, own_signing,
    remove_everywhere, results, typed_code,
};
use crate::cli::base;
use crate::device::{DeviceSigning, key};
use crate::ui;

pub(crate) fn rotate_command(command: Command) -> Command {
    base(
        command,
        "Replace a CI or agent key, and redeploy what it signed",
    )
    .after_help(format!(
        "{LEAKED}\n\n\
         It follows each redeploy to the end, and removes the old key only once every \
         one applied. If it stops before that, run the same command again. Put the new \
         PLOYZ_KEY where CI deploys from as soon as it prints: a deploy with the old key \
         keeps the old key trusted.\n\n\
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
    // The Store reads and the prompt below can outlast the connection's idle timeout,
    // so the trust write and the retirement each open their own.
    drop(client);
    let cluster = roster.cluster()?;
    let seed = pending_key(&pending)?;
    let new = SigningPublicKey::from(key::public(&seed));
    let old = source(&roster, code)?;
    let missing = replacement(&roster, &old, new)?;
    let applied_path = applied_path(&pending, new);
    let mut applied = Applied::load(&applied_path)?;
    let carried = carried(&store, &old.grant, &applied)?;
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
        // Each step still carries the prior read above, so a Member whose grant moved
        // since refuses it.
        let mut client = runtime.block_on(route::connect_full(leaf, context(leaf)))?;
        let reached = runtime.block_on(Roster::read(&mut client))?;
        reached.require_all()?;
        let (answers, gaps) = runtime.block_on(bundle.send(&client, &reached.reach));
        if !gaps.is_complete() {
            let moved = gaps.failures.iter().any(grant_moved);
            let servers = super::answered(&reached.reach, answers);
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
    let carry = carry(
        carried,
        &mut applied,
        &applied_path,
        |applied| self::carried(&store, &old.grant, applied),
        |environment| {
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
            redeploy(leaf, &store, environment, &resigner, events)
        },
    )?;
    let deployments = carry.deployments;
    if !carry.left.is_empty() {
        let unretired = Unretired {
            old: code,
            new: new.code(),
            deployments,
            landing: carry
                .left
                .iter()
                .flat_map(|environment| {
                    environment.services.iter().map(|service| Landing {
                        service: service.authored.clone(),
                        namespace: environment.namespace.clone(),
                        digest: service.version.digest,
                    })
                })
                .collect(),
        };
        ui::finish(&BTreeMap::from([("rotation", &unretired)]), || {
            ui::warn(format!(
                "A deploy signed with key {code} keeps landing, so the old key stays trusted."
            ));
            for landing in &unretired.landing {
                ui::note(format_args!(
                    "  {} in {}: digest {}",
                    landing.service, landing.namespace, landing.digest
                ));
            }
            ui::note(format_args!(
                "Something still deploys with the old key, likely CI. Put the new PLOYZ_KEY in that secret, then run ployz key rotate {code} again."
            ));
        })?;
        return Err(Error::partial());
    }
    let still_works = |error: Error| {
        error
            .context(format!(
                "Couldn't remove key {code} once its redeploys applied, so the old key still works."
            ))
            .hint(crate::cli::Rerun::again(root))
    };
    let mut client = runtime
        .block_on(route::connect_full(leaf, context(leaf)))
        .map_err(still_works)?;
    let now = runtime
        .block_on(Roster::read(&mut client))
        .map_err(still_works)?;
    now.require_all().map_err(still_works)?;
    let retired = unmoved(&now, &old, new)?;
    let (removed, gaps) = runtime.block_on(remove_everywhere(
        &client,
        &now.reach,
        old.grant.grant.key,
        &Removal::Rotated(retired),
    ));
    let moved = gaps.failures.iter().any(grant_moved);
    let rotated = Rotated {
        old: code,
        new: new.code(),
        deployments,
        removed: super::answered(&now.reach, removed),
    };
    if gaps.is_complete() {
        forget_pending(&pending)?;
    }
    ui::finish_fanout("rotated", &rotated, &gaps, || {
        ui::stream(format_args!("Rotated key {code} to {}.", rotated.new));
        if moved {
            ui::note(format_args!(
                "Key {code} or {} changed on a server while this ran, so that server still trusts the old key. Check ployz key ls, match the new key's scope with ployz key scope {}, then run ployz key rm {code}.",
                rotated.new, rotated.new
            ));
        } else if !gaps.is_complete() {
            ui::note(format_args!(
                "The old key still works on those servers. Run ployz key rm {code} once they answer."
            ));
        }
    })
}

/// A rotation that left the old key trusted, since a deploy under it kept landing.
#[derive(Serialize)]
struct Unretired {
    old: DeviceCode,
    new: DeviceCode,
    deployments: Vec<u64>,
    /// What the old key alone signed after the last redeploy.
    landing: Vec<Landing>,
}

#[derive(Serialize)]
struct Landing {
    service: ServiceName,
    namespace: Namespace,
    digest: PayloadDigest,
}

#[derive(Serialize)]
struct Rotated {
    old: DeviceCode,
    new: DeviceCode,
    /// The numbers of the redeploys that carried what the old key signed.
    deployments: Vec<u64>,
    removed: Vec<super::Answer>,
}

/// The new key and applied list a stopped rotation of `code` left on this computer,
/// once the old key is removed and they carry nothing more.
///
/// # Errors
/// Returns the read or delete that failed.
pub(super) fn forget(config: &Path, code: DeviceCode) -> Result<(), Error> {
    forget_pending(&pending_path(config, code))
}

fn forget_pending(pending: &Path) -> Result<(), Error> {
    let Some(seed) = key::load(pending)? else {
        return Ok(());
    };
    let new = SigningPublicKey::from(key::public(&seed));
    for path in [applied_path(pending, new), pending.to_owned()] {
        match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(key::KeyError::Write {
                    path,
                    source: error,
                }
                .into());
            }
            _ => {}
        }
    }
    Ok(())
}

/// Where a rotation to `new` keeps the Services its redeploys applied.
fn applied_path(pending: &Path, new: SigningPublicKey) -> PathBuf {
    pending.with_extension(format!("{}.applied", new.code()))
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

/// The Members that don't hold the new key yet, each with when its copy of the old grant
/// was issued. A Member that holds a key under the new code holds the new key itself,
/// with what the old one gives, or the rotation stops.
///
/// # Errors
/// Refuses when a Member holds another key under the new code, or the new key with
/// another kind or scope.
fn replacement(
    roster: &Roster,
    old: &Source,
    new: SigningPublicKey,
) -> Result<Vec<(KeyMember, Timestamp)>, Error> {
    let mut missing = Vec::new();
    for member in &roster.members {
        match member.grant(new.code()) {
            None => missing.extend(old.read.get(&member.key.id).map(|read| (member.key, *read))),
            Some(held) if replaces(held, &old.grant, new) => {}
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
    Ok(missing)
}

/// Whether `held` is the new key `new` with what `old` gives.
fn replaces(held: &TrustedGrant, old: &TrustedGrant, new: SigningPublicKey) -> bool {
    held.grant.key == new
        && held.grant.kind == old.grant.kind
        && Contents::of(&held.grant) == Contents::of(&old.grant)
}

/// Refuse to remove the old key unless every Member still holds it as this run read it,
/// and holds the new key with what it gives: a scope change made while this ran would
/// otherwise be lost, or the new key would keep a grant the old one no longer has. Each
/// Member's removal carries both grants as read here, so one that moves after this read
/// is refused there.
///
/// # Errors
/// Returns `conflict` naming the first Member where either grant moved.
fn unmoved(
    now: &Roster,
    old: &Source,
    new: SigningPublicKey,
) -> Result<BTreeMap<MachineId, Retired>, Error> {
    let code = old.grant.grant.key.code();
    let mut retired = BTreeMap::new();
    for member in &now.members {
        let kept = member.grant(code).filter(|held| {
            held.grant.key == old.grant.grant.key
                && old.read.get(&member.key.id) == Some(&held.grant.issued_at)
                && Contents::of(&held.grant) == Contents::of(&old.grant.grant)
        });
        let taken = member
            .grant(new.code())
            .filter(|held| replaces(held, &old.grant, new));
        let (Some(kept), Some(taken)) = (kept, taken) else {
            return Err(moved(member, code));
        };
        retired.insert(
            member.key.id,
            Retired {
                issued_at: kept.grant.issued_at,
                by: new,
                by_issued_at: taken.grant.issued_at,
            },
        );
    }
    Ok(retired)
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
    pub(super) environment: EnvironmentRef,
    pub(super) namespace: Namespace,
    pub(super) services: Vec<CarriedVersion>,
}

#[derive(Debug)]
pub(super) struct CarriedVersion {
    /// As Setting paths address it.
    pub(super) authored: ServiceName,
    pub(super) version: ServiceVersion,
    pub(super) image: String,
    pub(super) pin: Option<TagPin>,
}

/// What the old key signed that runs now, in each place its grant names, checked here.
/// A Service this rotation's own redeploy `applied` at the spec it runs now is carried
/// already.
///
/// # Errors
/// Returns the Store's refusal, and refuses the whole rotation when any version naming
/// the old key doesn't check under it.
fn carried(store: &Store, old: &TrustedGrant, applied: &Applied) -> Result<Vec<Carried>, Error> {
    let owned = store.read(&NamespacesQuery {})?.namespaces;
    let mut wanted: BTreeMap<Namespace, Option<BTreeSet<ServiceName>>> = BTreeMap::new();
    match &old.grant.scope.places {
        ployz_core::authority::KeyPlaces::Unscoped => {
            return Err(Error::conflict(format!(
                "Key {} names no environment, so no server runs what it signs. Make a new key with ployz key add --ci --env, then remove this one with ployz key rm. Nothing changed.",
                old.grant.key.code()
            )));
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
            let Some(version) = verified((old, applied), &running.namespace, runtime, ran)? else {
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
/// this rotation's redeploy `applied` that spec already. A version the new key signed
/// proves nothing: a redeploy posts it before it runs, so a failed one leaves it too.
///
/// # Errors
/// Refuses when a version naming the old key doesn't check.
pub(super) fn verified(
    (old, applied): (&TrustedGrant, &Applied),
    namespace: &Namespace,
    runtime: &ServiceName,
    ran: &Running,
) -> Result<Option<ServiceVersion>, Error> {
    let digest = PayloadDigest::of(&ran.spec, namespace).ok();
    if digest.is_some_and(|digest| applied.holds(namespace, runtime, digest)) {
        return Ok(None);
    }
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

/// The Services this rotation's own redeploys applied on every server with no gaps, each
/// at the digest it ran. Kept beside the new key, under its code, so a run again with
/// the same key skips only these.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct Applied {
    services: Vec<(Namespace, ServiceName, PayloadDigest)>,
}

impl Applied {
    /// What `path` keeps, or nothing. One that doesn't read only makes this run redeploy
    /// again.
    fn load(path: &Path) -> Result<Self, Error> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).unwrap_or_default()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(key::KeyError::Read {
                path: path.to_owned(),
                source,
            }
            .into()),
        }
    }

    fn save(&self, path: &Path) -> Result<(), Error> {
        let bytes = serde_json::to_vec(self).expect("names and digests serialize");
        std::fs::write(path, bytes).map_err(|source| {
            key::KeyError::Write {
                path: path.to_owned(),
                source,
            }
            .into()
        })
    }

    /// Keep that a redeploy of `version` applied.
    pub(super) fn add(&mut self, version: &ServiceVersion) {
        self.services.push((
            version.namespace.clone(),
            version.service.clone(),
            version.digest,
        ));
    }

    pub(super) fn holds(
        &self,
        namespace: &Namespace,
        service: &ServiceName,
        digest: PayloadDigest,
    ) -> bool {
        self.services
            .iter()
            .any(|(kept, named, ran)| kept == namespace && named == service && *ran == digest)
    }
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
    list(carried);
    ui::note("The old key is removed once every redeploy applied on every server.");
}

fn list(carried: &[Carried]) {
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
}

/// How many times a rotation reads what the old key alone signed before it removes it.
const PASSES: usize = 3;

pub(super) struct Carry {
    /// The numbers of the redeploys that applied.
    pub(super) deployments: Vec<u64>,
    /// What the old key alone signed at the last read. The old key stays while any is left.
    pub(super) left: Vec<Carried>,
}

/// Redeploy each of `first`, then `read` what the old key alone signed again and redeploy
/// that, up to [`PASSES`] reads. While the old key holds its grant, anything holding the
/// key can still deploy with it, so what one read found isn't all that needs it by the
/// time it's removed. Each applied redeploy is kept in `applied` at `path` at once.
///
/// # Errors
/// Returns the first redeploy or read that failed.
pub(super) fn carry(
    first: Vec<Carried>,
    applied: &mut Applied,
    path: &Path,
    mut read: impl FnMut(&Applied) -> Result<Vec<Carried>, Error>,
    mut redeploy: impl FnMut(&Carried) -> Result<Option<u64>, Error>,
) -> Result<Carry, Error> {
    let mut deployments = Vec::new();
    let mut next = first;
    for pass in 0..PASSES {
        if pass > 0 {
            ui::note("Deployed with the old key while this ran, so redeployed too:");
            list(&next);
        }
        for environment in &next {
            deployments.extend(redeploy(environment)?);
            for service in &environment.services {
                applied.add(&service.version);
            }
            applied.save(path)?;
        }
        next = read(applied)?;
        if next.is_empty() {
            break;
        }
    }
    Ok(Carry {
        deployments,
        left: next,
    })
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
            copies: Vec::new(),
            recreate: carried
                .services
                .iter()
                .map(|service| service.version.service.clone())
                .collect(),
            rule: None,
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
