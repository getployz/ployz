//! `ployz key rotate`: replace a CI or agent key with a new one holding the same grant.
//! Every Member takes the new key only over the grant this CLI read for the old one. The
//! versions the old key signed that still run are carried: each checked here under the
//! old key, shown, signed again under the new key and redeployed, so nothing stays
//! admitted under the old key. The old key is removed only once all of that succeeded.
//! The new key waits beside the config until then, so a run that stops resumes with it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use clap::{ArgMatches, Command};
use ployz_core::authority::{
    Approver, KeyChange, KeyKind, KeyRef, PayloadDigest, Prior, ServiceVersion, Signed, SignerKey,
    SigningPublicKey, TrustedGrant, TrustedKeys,
};
use ployz_core::{
    DeviceCode, DeviceKey, DeviceSet, MachineId, MachineTarget, Namespace, RpcError, ServiceName,
    SetDeviceRequest, op,
};
use ployz_store::{
    Admit, Deploy, DeploymentId, EnvironmentRef, NamespaceQuery, NamespacesQuery, PlanQuery,
    PostedVersion, Running, RunningQuery, TagPin,
};
use serde::Serialize;

use super::super::device::{DeviceSigning, key};
use super::super::store::{Store, mint, store};
use super::super::{Error, config_path, deploy, leaf_matches, route, runtime};
use super::members::{Bundle, Roster};
use super::{LEAKED, code_arg, context, new_key, own_signing, results, typed_code};
use crate::cli::base;
use crate::cluster::Call;
use crate::connect::Client;
use crate::ui::{self, Gaps};

pub(crate) fn rotate_command(command: Command) -> Command {
    deploy::following(base(
        command,
        "Replace a CI or agent key, and redeploy what it signed",
    ))
    .after_help(format!(
        "{LEAKED}\n\n\
         If a server misses the new key, run the same command again.\n\n\
         Examples:\n  ployz key rotate mmg42klgyq"
    ))
    .arg(code_arg())
}

pub(crate) fn rotate(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let code = typed_code(leaf)?;
    let signing = own_signing(leaf)?;
    let pending = pending_path(&config_path(leaf)?, code);
    let store = store(root)?;
    let runtime = runtime()?;
    let mut client = runtime.block_on(route::connect_full(leaf, context(leaf)))?;
    let roster = runtime.block_on(Roster::read(&mut client))?;
    roster.require_all()?;
    let cluster = roster.cluster()?;
    let seed = pending_key(&pending)?;
    let new = SigningPublicKey::from(key::public(&seed));
    let old = source(&roster, code)?;
    let carried = carried(&store, &old)?;
    show(&old, new, &carried, &roster);
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
    let missing: Vec<_> = roster
        .members
        .iter()
        .filter(|member| member.grant(new.code()).is_none())
        .map(|member| member.key)
        .collect();
    if !missing.is_empty() {
        let grant = &old.grant;
        let wanted = super::Wanted {
            scope: grant.scope.clone(),
            volumes: grant.volumes.clone(),
            name: grant.name.clone(),
            shown: Vec::new(),
        };
        let change = KeyChange::Trust {
            key: new_key(grant.kind, new, &wanted),
            members: missing,
            prior: Some(Prior::Replace {
                key: grant.key,
                issued_at: grant.issued_at,
            }),
        };
        let bundle = Bundle::sign(&signing, cluster, vec![change])?;
        let (answers, gaps) = runtime.block_on(bundle.send(&client, &roster.reach));
        if !gaps.is_complete() {
            let servers = super::answered(&roster.reach, answers);
            return ui::finish_fanout("servers", &servers, &gaps, || {
                ui::rows(&results(&servers));
                ui::note(format_args!(
                    "The old key still works. Run ployz key rotate {code} again once those servers answer."
                ));
            });
        }
    }
    ui::stream(format_args!("PLOYZ_KEY={}", hex::encode(seed)));
    ui::note("Replace the old PLOYZ_KEY with this one where the key deploys from.");
    let resigner = DeviceSigning::from_seed(&seed);
    for environment in &carried {
        redeploy(leaf, &store, environment, &resigner)?;
    }
    let (removed, gaps) = runtime.block_on(remove_everywhere(&client, &roster, old.grant.key));
    let rotated = Rotated {
        old: code,
        new: new.code(),
        removed: super::answered(&roster.reach, removed),
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

/// The grant every Member holds for the key `code` names, the one the rotation replaces.
///
/// # Errors
/// Refuses when a Member holds none, or Members hold different grants.
fn source(roster: &Roster, code: DeviceCode) -> Result<TrustedGrant, Error> {
    let mut held = Vec::new();
    for member in &roster.members {
        let Some(grant) = member.grant(code) else {
            return Err(Error::conflict(format!(
                "{} doesn't hold key {code}, so it can't be rotated there. Check ployz key ls. Nothing changed.",
                member.name
            )));
        };
        held.push(grant);
    }
    let Some(first) = held.first() else {
        return Err(Error::not_found("No servers answered. Nothing changed."));
    };
    if held.iter().any(|grant| grant.grant != first.grant) {
        return Err(Error::conflict(format!(
            "Your servers hold different scopes for key {code}. Run ployz key scope {code} to settle it first. Nothing changed."
        )));
    }
    Ok((*first).clone())
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
///
/// # Errors
/// Returns the Store's refusal, and refuses the whole rotation when any version naming
/// the old key doesn't check under it.
fn carried(store: &Store, old: &TrustedGrant) -> Result<Vec<Carried>, Error> {
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
            let Some(version) = verified(old, &running.namespace, runtime, ran)? else {
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
/// old key alone and against the spec it runs; `None` when the old key signed none.
///
/// # Errors
/// Refuses when a version naming the old key doesn't check.
pub(super) fn verified(
    old: &TrustedGrant,
    namespace: &Namespace,
    runtime: &ServiceName,
    ran: &Running,
) -> Result<Option<ServiceVersion>, Error> {
    let signer = SignerKey::Device(DeviceKey::from(old.grant.key));
    let only_old = TrustedKeys::new([], [], [old.clone()]);
    let digest = PayloadDigest::of(&ran.spec, namespace).ok();
    for statement in &ran.versions {
        let Ok(version) = statement.open() else {
            continue;
        };
        if version.signer != signer {
            continue;
        }
        let checks = statement
            .approvers(&only_old)
            .is_ok_and(|by| by == [Approver::Signing(old.grant.key)])
            && &version.service == runtime
            && &version.namespace == namespace
            && Some(version.digest) == digest;
        if !checks {
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
    ui::note("The old key is removed once every server runs them.");
}

/// Sign each carried version again under the new key and deploy them, so every server
/// admits their Containers again under it.
///
/// # Errors
/// Refuses when the plan for a carried Service no longer comes to the digest it runs,
/// and returns the Deploy's error.
fn redeploy(
    leaf: &ArgMatches,
    store: &Store,
    carried: &Carried,
    resigner: &DeviceSigning,
) -> Result<(), Error> {
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
            .map(|spec| {
                let mut spec = spec.clone();
                if let Some(pin) = &service.pin {
                    spec.container.image.clone_from(&pin.image);
                }
                spec
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
    deploy::execute(leaf, store, &admitted, None, None)?.finish()
}

async fn remove_everywhere(
    client: &Client,
    roster: &Roster,
    old: SigningPublicKey,
) -> (Vec<(MachineId, DeviceSet)>, Gaps) {
    let (answers, gaps) = roster
        .reach
        .fan_out(|machine_id| remove_on(client.clone(), machine_id, old))
        .await;
    (answers, roster.reach.named(gaps))
}

async fn remove_on(
    client: Client,
    machine_id: MachineId,
    old: SigningPublicKey,
) -> Result<Option<DeviceSet>, RpcError> {
    client
        .call_shared::<op::SetDevice>(
            SetDeviceRequest::RemoveKey {
                key: KeyRef::Signing { key: old.into() },
            },
            Call::once().to(&MachineTarget::from(&machine_id)),
        )
        .await
        .map_err(crate::connect::rpc_error)
        .map(Some)
}
