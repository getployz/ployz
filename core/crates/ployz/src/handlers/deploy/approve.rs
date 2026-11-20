//! A Device's approval of one Deploy: when a Server of the Cluster enforces Device
//! approval, `ployz deploy` lowers the plan as Cloud's runner will, signs a version of
//! each Service with the digest of its spec, and admits the Deploy with them. Each
//! Server then creates only the specs a trusted Device signed.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use clap::ArgMatches;
use ployz_core::authority::{
    ApprovedChange, Mounts, ServiceVersion, SignerKey, Timestamp, birth, deploy::content_addressed,
};
use ployz_core::{
    Claim, Hint, MachineId, MachineObservation, MachineTarget, Namespace, RequestedServiceSpec,
    RpcErrorCode, SIGNED_VERSION_CAPABILITY, SealingPublicKey, ServiceName, cluster_sealing_key,
};
use ployz_store::{EnvironmentRef, PlanQuery, PostedVersion, TagPin};
use serde_json::json;

use super::super::device::DeviceSigning;
use super::super::store::Store;
use super::super::{Error, route};
use crate::connect::Client;
use crate::image::{PinError, pin_public};

/// A signed version of each Service with the image it pinned, the plan version it
/// approves, and what it approved.
pub(in crate::handlers) struct DeviceApproval {
    pub(in crate::handlers) versions: Vec<PostedVersion>,
    pub(in crate::handlers) version: String,
    pub(in crate::handlers) approved: Vec<ApprovedChange>,
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

/// How long `ployz deploy` waits to learn whether a Server enforces Device approval.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Sign the versions a Deploy ships with this CLI's Device key, when it holds one and a
/// Server of the Cluster enforces Device approval. None otherwise: the Deploy runs as
/// before, and a Server that enforces refuses what nobody approved. The Servers are
/// asked while the Store plans, so a Cluster that doesn't enforce waits for neither.
///
/// # Errors
/// Refuses before admission when the person declines the lines it shows, or no terminal
/// can ask and `--yes` wasn't given, when a Service it ships builds from source, when a tag image
/// can't be pinned (its registry wants credentials, can't be reached, or has no such tag),
/// when a Server enforces but none reports the Cluster's key, and when the Store can't
/// lower the plan; returns the plan's or the key's error.
pub(in crate::handlers) fn sign(
    matches: &ArgMatches,
    store: &Store,
    (environment, services): (&EnvironmentRef, &[ServiceName]),
) -> Result<Option<DeviceApproval>, Error> {
    let key = super::super::device::key::path(&super::super::config_path(matches)?);
    let Ok(Some(signing)) = DeviceSigning::load(&key) else {
        return Ok(None);
    };
    let (probe, plan) = std::thread::scope(|scope| {
        let probe = scope.spawn(|| probe(matches));
        let plan = store.read(&PlanQuery {
            environment: environment.clone(),
            services: services.to_vec(),
            target: true,
            deployment: None,
        });
        (probe.join().unwrap_or(Probe::Unknown), plan)
    });
    let enforcing = match probe {
        Probe::Off => return Ok(None),
        Probe::Unknown => {
            crate::ui::warn(
                "Couldn't ask your servers whether they enforce device approval, so this \
                 deploy isn't signed. Servers that enforce will refuse it.",
            );
            return Ok(None);
        }
        Probe::NoClusterKey => {
            return Err(Error::coded(
                RpcErrorCode::Unavailable,
                "Your servers enforce device approval, but none reported the cluster's key, \
                 so this device can't sign the deploy. Run ployz status, then deploy again.",
            ));
        }
        Probe::Enforcing(enforcing) => enforcing,
    };
    if !enforcing.behind.outdated.is_empty() {
        return Err(outdated(matches, &enforcing.behind.outdated));
    }
    if !enforcing.behind.unverified.is_empty() {
        return Err(unverified(matches, &enforcing.behind.unverified));
    }
    let plan = plan?;
    let mut target = signable(plan.target)?;
    let pins = pin_tags(matches, &target)?;
    let namespace = plan.namespace;
    let signed_at = Timestamp::now();
    let seed = birth::seed();
    let minted = birth::mint(
        &mut target,
        (&namespace, &plan.volumes),
        signed_at,
        |name| birth::nonce(&seed, name),
    )
    .map_err(|error| {
        Error::caused(
            RpcErrorCode::InvalidArgument,
            "Can't sign this deploy",
            error,
        )
    })?;
    let mounts = Mounts {
        namespace: &namespace,
        volumes: &plan.volumes,
        minted: &minted,
        now: signed_at,
    };
    let approved =
        ApprovedChange::of(&target, &namespace, &pins, &mounts).map_err(Error::command)?;
    for line in approved.iter().flat_map(ApprovedChange::lines) {
        crate::ui::note(format_args!("{line}"));
    }
    let yes = matches
        .try_get_one::<bool>("yes")
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false);
    consent(
        (&namespace, &approved),
        yes,
        |question, refusal, declined| crate::ui::confirm(question, || refusal, declined),
    )?;
    let signer = SignerKey::Device(signing.device_key());
    let versions = approved
        .iter()
        .map(|change| {
            let statement = signing.sign(&ServiceVersion {
                cluster: enforcing.cluster,
                namespace: namespace.clone(),
                service: change.service.clone(),
                digest: change.digest,
                signed_at,
                signer,
                proposal: None,
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
    Ok(Some(DeviceApproval {
        versions,
        version: plan.version,
        approved,
    }))
}

/// What a person says to signing `approved` in `namespace`, asked after its lines show:
/// Cloud relays the plan, so a setting it planted, or another Environment's Namespace, must
/// be refusable before this Device signs it. `--yes`
/// answers for a run nobody watches; without it, a run that can't ask refuses before
/// signing, and a person who says no deploys nothing.
fn consent(
    (namespace, approved): (&Namespace, &[ApprovedChange]),
    yes: bool,
    confirm: impl FnOnce(&str, Error, &'static str) -> Result<(), Error>,
) -> Result<(), Error> {
    if yes {
        return Ok(());
    }
    let services: Vec<&str> = approved
        .iter()
        .map(|change| change.service.as_str())
        .collect();
    let refusal = Error::detailed(
        RpcErrorCode::ConfirmationRequired,
        "Your servers enforce device approval, so signing this deploy needs your yes, and \
         there's no terminal to ask in. Read the lines above, then add --yes to sign them. \
         Nothing deployed.",
        json!({ "services": services }),
    )
    .hint(Hint::Retry(super::super::typed_with(&["--yes"])));
    confirm(
        &format!("Approve {} in namespace {namespace}?", services.join(", ")),
        refusal,
        "Cancelled. Nothing signed or deployed.",
    )
}

/// The plan's target when a Device can sign every spec it ships, once its tags are pinned.
fn signable(target: Option<Vec<RequestedServiceSpec>>) -> Result<Vec<RequestedServiceSpec>, Error> {
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

/// What the Servers say about Device approval.
#[derive(Debug, PartialEq)]
pub(in crate::handlers) enum Probe {
    /// None enforces.
    Off,
    Enforcing(Enforcing),
    /// One enforces, and none reported the Cluster's Sealing key.
    NoClusterKey,
    /// No Server answered in time.
    Unknown,
}

pub(in crate::handlers) fn probe(matches: &ArgMatches) -> Probe {
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
        let answered = asked(&client, &unclaimed(&observed)).await;
        Some((observed, answered))
    });
    observed.map_or(Probe::Unknown, |(observed, answered)| {
        enforced(&observed, &answered)
    })
}

/// Each Server that enforces and was seen with no capabilities. An empty list claims
/// nothing: an older entry relays a newer peer's record without one.
fn unclaimed(observed: &[MachineObservation]) -> BTreeSet<MachineId> {
    observed
        .iter()
        .map(|observed| &observed.machine)
        .filter(|machine| {
            machine.runtime.device_enforcement == Some(true)
                && machine.runtime.claim(SIGNED_VERSION_CAPABILITY) == Claim::Unclaimed
        })
        .map(|machine| machine.id)
        .collect()
}

/// Whether each of `machines`, asked itself, checks a signed version. One that doesn't
/// answer in time is left out.
async fn asked(client: &Client, machines: &BTreeSet<MachineId>) -> BTreeMap<MachineId, bool> {
    let asks = machines.iter().map(|machine| async move {
        let target = MachineTarget::from(machine);
        let ask = client.supports(Some(&target), SIGNED_VERSION_CAPABILITY);
        let answer = tokio::time::timeout(PROBE_TIMEOUT, ask).await.ok()?.ok()?;
        Some((*machine, answer))
    });
    futures_util::future::join_all(asks)
        .await
        .into_iter()
        .flatten()
        .collect()
}

/// What `observed` says about Device approval, with `answered` saying, for each Server
/// seen with no capabilities that was asked and answered, whether it checks a signed
/// version.
fn enforced(observed: &[MachineObservation], answered: &BTreeMap<MachineId, bool>) -> Probe {
    if !observed
        .iter()
        .any(|observed| observed.machine.runtime.device_enforcement == Some(true))
    {
        return Probe::Off;
    }
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
        if machine.runtime.device_enforcement != Some(true) {
            continue;
        }
        let checks = match machine.runtime.claim(SIGNED_VERSION_CAPABILITY) {
            Claim::Advertises => Some(true),
            Claim::Lacks => Some(false),
            Claim::Unclaimed => answered.get(&machine.id).copied(),
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
    Probe::Enforcing(Enforcing {
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

/// The refusal for Servers that enforce and can't check a signed version, before
/// anything is signed or admitted.
fn outdated(matches: &ArgMatches, servers: &[String]) -> Error {
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
            "Upgrade {named} first: {each} enforces device approval with an older ployz that \
             can't check this device's signature, so {each} would refuse this deploy. Nothing \
             signed or deployed.",
            each = if servers.len() == 1 { "it" } else { "each" },
        ),
        json!({
            "servers": servers,
            "capability": SIGNED_VERSION_CAPABILITY,
        }),
    )
    .hint(Hint::Next(super::super::rerun(matches, &upgrade)))
}

/// The refusal for Servers that enforce and didn't say whether they can check a signed
/// version, before anything is signed or admitted.
fn unverified(matches: &ArgMatches, servers: &[String]) -> Error {
    Error::detailed(
        RpcErrorCode::Unavailable,
        format!(
            "Couldn't confirm {} can check this device's signature: {} enforces device \
             approval and didn't answer. Nothing signed or deployed.",
            listed(servers),
            if servers.len() == 1 { "it" } else { "each" },
        ),
        json!({
            "servers": servers,
            "capability": SIGNED_VERSION_CAPABILITY,
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
