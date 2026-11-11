//! A Device's approval of one Deploy: when a Server of the Cluster enforces Device
//! approval, `ployz deploy` lowers the plan as Cloud's runner will, signs the digest of
//! each Service's spec with this Device's key, and admits the Deploy with it. Each
//! Server then creates only the specs this Device approved.

use std::collections::BTreeMap;
use std::time::Duration;

use clap::ArgMatches;
use ployz_core::authority::{
    ApprovedChange, DeployApproval, DeployTerms, Signed, Signer, SignerKey, Timestamp,
    deploy::content_addressed,
};
use ployz_core::{
    MachineObservation, OperationId, RequestedServiceSpec, RpcErrorCode, Sealing, SealingPublicKey,
    ServiceName,
};
use ployz_store::{DeploymentId, EnvironmentRef, PlanQuery};
use serde_json::json;

use super::super::store::Store;
use super::super::{Error, server};
use crate::image::{PinError, pin_public};
use crate::ui::Hint;

/// A signed approval, the plan version it approves, and what it approved.
pub(in crate::handlers) struct DeviceApproval {
    pub(in crate::handlers) signed: Signed<DeployApproval>,
    pub(in crate::handlers) version: String,
    pub(in crate::handlers) approved: Vec<ApprovedChange>,
}

/// The Cluster as this Device sees it: the Sealing key every Server holds and the
/// Servers, when any of them enforces Device approval.
#[derive(Debug, PartialEq)]
pub(in crate::handlers) struct Enforcing {
    pub(in crate::handlers) cluster: SealingPublicKey,
    pub(in crate::handlers) machines: Vec<ployz_core::MachineId>,
    /// Whether every Server listed checks a person's approval of a commit in place of
    /// its branch head. An older one refuses the approval as an unknown field.
    pub(in crate::handlers) commit_approval: bool,
}

/// How long `ployz deploy` waits to learn whether a Server enforces Device approval.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Sign Deployment `id`'s approval with this CLI's Device key, when it holds one and a
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
    id: &DeploymentId,
    (environment, services): (&EnvironmentRef, &[ServiceName]),
) -> Result<Option<DeviceApproval>, Error> {
    let key = super::super::device::key::path(&super::super::config_path(matches)?);
    let Ok(Some(secret)) = super::super::device::key::load(&key) else {
        return Ok(None);
    };
    let (probe, plan) = std::thread::scope(|scope| {
        let probe = scope.spawn(|| probe(matches));
        let plan = store.read(&PlanQuery {
            environment: environment.clone(),
            services: services.to_vec(),
            target: true,
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
    let plan = plan?;
    let target = signable(plan.target)?;
    let pins = pin_tags(matches, &target)?;
    let signer = Signer::from_seed(&secret);
    let issued_at = Timestamp::now();
    let terms = DeployTerms {
        cluster: enforcing.cluster,
        operation: OperationId::from_uuid(id.as_str()).map_err(Error::command)?,
        machines: enforcing.machines,
        issued_at,
        expires_at: issued_at.plus(ployz_core::authority::restart::MAX_LIFETIME_MILLIS),
        signer: SignerKey::Device(signer.public().into()),
        proposal: None,
    };
    let (body, approved) =
        DeployApproval::draft(terms, plan.namespace, &target, &pins).map_err(Error::command)?;
    for line in approved.iter().flat_map(ApprovedChange::lines) {
        crate::ui::note(format_args!("{line}"));
    }
    let yes = matches
        .try_get_one::<bool>("yes")
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false);
    consent(&approved, yes, |question, refusal, declined| {
        crate::ui::confirm(question, || refusal, declined)
    })?;
    let signed = Signed::sign(&body, &signer).map_err(Error::command)?;
    Ok(Some(DeviceApproval {
        signed,
        version: plan.version,
        approved,
    }))
}

/// What a person says to signing `approved`, asked after its lines show: Cloud relays the
/// plan, so a setting it planted must be refusable before this Device signs it. `--yes`
/// answers for a run nobody watches; without it, a run that can't ask refuses before
/// signing, and a person who says no deploys nothing.
fn consent(
    approved: &[ApprovedChange],
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
        &format!("Approve {}?", services.join(", ")),
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
        tokio::time::timeout(PROBE_TIMEOUT, async {
            let mut client = server::connect(matches, context).await.ok()?;
            client.machines().await.ok()
        })
        .await
        .ok()
        .flatten()
    });
    observed.map_or(Probe::Unknown, |observed| enforced(&observed))
}

fn enforced(observed: &[MachineObservation]) -> Probe {
    if !observed
        .iter()
        .any(|observed| observed.machine.runtime.device_enforcement == Some(true))
    {
        return Probe::Off;
    }
    let Some(cluster) =
        observed
            .iter()
            .find_map(|observed| match observed.machine.runtime.sealing {
                Some(Sealing::Held { public_key }) => Some(public_key),
                _ => None,
            })
    else {
        return Probe::NoClusterKey;
    };
    let mut machines: Vec<_> = observed
        .iter()
        .map(|observed| observed.machine.id)
        .collect();
    machines.sort_unstable();
    machines.dedup();
    let commit_approval = observed.iter().all(|observed| {
        observed
            .machine
            .runtime
            .capabilities
            .contains(ployz_core::COMMIT_APPROVAL_CAPABILITY)
    });
    Probe::Enforcing(Enforcing {
        cluster,
        machines,
        commit_approval,
    })
}

/// Whether `spec` runs the image a build or upload will make, which no Device can
/// name by digest before it exists.
fn built(spec: &RequestedServiceSpec) -> bool {
    spec.container.image.starts_with("ployz-build/")
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
                repository(&spec.container.image)
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

/// `image` without its tag or digest: `ghcr.io/acme/web` of `ghcr.io/acme/web:1.4`.
fn repository(image: &str) -> &str {
    let image = image.split_once('@').map_or(image, |(name, _)| name);
    match image.rsplit_once(':') {
        Some((name, tag)) if !tag.contains('/') => name,
        _ => image,
    }
}

#[cfg(test)]
#[path = "approve_tests.rs"]
mod tests;
