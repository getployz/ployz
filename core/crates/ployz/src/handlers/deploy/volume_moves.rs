//! The Volumes a Deploy moves into their Service's region before it starts the Service:
//! what Cloud lists of them, and, on Servers that enforce Device approval, this Device's
//! approval of each, which Cloud keeps for the move that Deploy runs.

use clap::ArgMatches;
use ployz_core::authority::{Signed, VolumeApproval};
use ployz_core::{
    Machine, MachineName, Namespace, RequestedServiceSpec, ResolvedServiceSpec, ServiceName,
    ServicePlacementEligibility,
};
use ployz_store::{EnvironmentRef, EnvironmentSummary, VolumeName, VolumeQuery};
use serde_json::json;

use super::super::Error;
use super::super::store::{Backend, Store};
use super::super::volume::approve::{MoveApproval, MoveDraft, draft_move};
use super::approve::DeviceApproval;
use crate::cloud_account;
use crate::ui;

/// One Volume Cloud moves, with the Server it leaves and the one it lands on, by name.
#[derive(serde::Deserialize, serde::Serialize)]
pub(super) struct VolumeMove {
    pub(super) volume: String,
    pub(super) from: String,
    pub(super) to: String,
}

/// The moves Cloud makes for a Deploy of `services`. None without Cloud, from an older
/// Cloud, or when it doesn't answer.
pub(super) fn listed(
    store: &Store,
    environment: &EnvironmentSummary,
    services: &[ServiceName],
) -> Vec<VolumeMove> {
    #[derive(serde::Deserialize)]
    struct Moves {
        moves: Vec<VolumeMove>,
    }
    let Backend::Cloud(runtime, credential) = store.backend() else {
        return Vec::new();
    };
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query
        .append_pair("project", environment.project.as_str())
        .append_pair("environment", environment.name.as_str());
    for service in services {
        query.append_pair("service", service.as_str());
    }
    runtime
        .block_on(cloud_account::refusable::<Moves>(
            credential,
            reqwest::Method::GET,
            &format!("volume-moves?{}", query.finish()),
            None,
        ))
        .map(|answer| answer.moves)
        .unwrap_or_default()
}

/// A move as Cloud keeps this Device's approval of it.
#[derive(serde::Serialize)]
struct Approved {
    volume: String,
    to: String,
    approval: Signed<VolumeApproval>,
    service: ResolvedServiceSpec,
}

/// Sign each move Cloud lists for the Deploy `device` approved, and hand the approvals to
/// Cloud before that Deploy is admitted. A move this Device can't sign says why and stays
/// unsigned: the Servers refuse it, and its Service is not started.
///
/// # Errors
/// Refuses when the person declines a move or can't be asked without `--yes`; returns
/// Cloud's refusal to keep an approval, and the key's error.
pub(super) fn approve(
    matches: &ArgMatches,
    store: &Store,
    device: &DeviceApproval,
    (environment, services): (&EnvironmentRef, &[ServiceName]),
) -> Result<(), Error> {
    let Backend::Cloud(runtime, credential) = store.backend() else {
        return Ok(());
    };
    let mut approvals = Vec::new();
    for VolumeMove { volume, from, to } in listed(store, &device.environment, services) {
        let (name, draft) = match drafted(matches, store, device, environment, (&volume, &to)) {
            Ok(Some(drafted)) => drafted,
            Ok(None) => continue,
            Err(error) => {
                ui::warn(format!("{volume} stays on {from}. {error}"));
                continue;
            }
        };
        let MoveApproval {
            signed, service, ..
        } = draft.sign(matches, &name)?;
        approvals.push(Approved {
            volume,
            to,
            approval: signed,
            service,
        });
    }
    if approvals.is_empty() {
        return Ok(());
    }
    let body = json!({
        "environment": {
            "project": device.environment.project,
            "environment": device.environment.name,
        },
        "approvals": approvals,
    });
    runtime
        .block_on(cloud_account::refusable_operation::<serde::de::IgnoredAny>(
            credential,
            reqwest::Method::POST,
            "volume-moves",
            Some(&body),
        ))
        .map_err(|error| store.fail(error))?;
    Ok(())
}

/// The move of `volume` onto `to` as this Device drafts it, when the Deploy it signed calls
/// for it, with the Volume's name parsed. None when there is nothing to sign.
fn drafted(
    matches: &ArgMatches,
    store: &Store,
    device: &DeviceApproval,
    environment: &EnvironmentRef,
    (volume, to): (&str, &str),
) -> Result<Option<(VolumeName, MoveDraft)>, Error> {
    let name = VolumeName::parse(volume).map_err(Error::command)?;
    let server = MachineName::parse(to).map_err(Error::command)?;
    let view = store.read(&VolumeQuery {
        environment: environment.clone(),
        volume: name.clone(),
    })?;
    let Some(draft) = draft_move(
        matches,
        store,
        (&name, &view.volume.volume.id, &view.environment),
        &server,
        view.volume.recorded_storage,
    )?
    else {
        return Ok(None);
    };
    called_for(
        &device.specs,
        &device.namespace,
        &name,
        &draft.service.name,
        (&draft.writer, &draft.target),
    )?;
    Ok(Some((name, draft)))
}

/// Whether the Deploy that ships `specs` calls for moving `volume`, which `service` writes,
/// off `writer` onto `target`: it places `service` on `target` and not on `writer`. Cloud
/// names the Volume and the target, so this Device signs only what that holds for.
///
/// # Errors
/// Refuses a Service the Deploy doesn't ship, a target it leaves out, and a Writer it takes.
fn called_for(
    specs: &[RequestedServiceSpec],
    namespace: &Namespace,
    volume: &VolumeName,
    service: &ServiceName,
    (writer, target): (&Machine, &Machine),
) -> Result<(), Error> {
    let Some(spec) = specs.iter().find(|spec| spec.name == *service) else {
        return Err(Error::conflict(format!(
            "This deploy doesn't include {service}, which uses {volume}."
        )));
    };
    let takes = |machine: &Machine| {
        !matches!(
            spec.placement_eligibility_in_namespace(namespace, machine, None),
            ServicePlacementEligibility::Ineligible(_)
        )
    };
    if !takes(target) {
        return Err(Error::conflict(format!(
            "This deploy doesn't place {service} on {}.",
            target.name
        )));
    }
    if takes(writer) {
        return Err(Error::conflict(format!(
            "This deploy can run {service} on {}, where {volume} is.",
            writer.name
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "volume_moves_tests.rs"]
mod tests;
