//! What `--env`, `--service` and `--volume` name, resolved to what a grant holds. The
//! Store resolves names to IDs and Namespaces, and the person checks them in the prompt.
//! A Volume entry comes only from a version a person signed, checked here against the
//! keys a Server trusts, never from Cloud's word alone.

use clap::ArgMatches;
use ployz_core::authority::{
    Approved, Cap, CapSet, GrantVolume, KeyName, KeyPlace, KeyPlaces, KeyScope, PlaceService,
    TrustedKeys,
};
use ployz_core::{
    DockerVolumeName, Namespace, SealingPublicKey, ServiceName, ServiceVolumeReference,
    VolumeOrigin,
};
use ployz_store::{
    EnvironmentName, EnvironmentRef, NamespaceQuery, Parent, ParentQuery, ProjectName,
    ServicesQuery, VolumesQuery,
};
use uuid::Uuid;

use super::super::Error;
use super::super::store::Store;

/// The flags as typed, before anything is read.
pub(super) struct Typed {
    pub(super) project: Option<ProjectName>,
    pub(super) envs: Vec<EnvironmentName>,
    pub(super) service: Option<ServiceName>,
    pub(super) volumes: Vec<(ServiceName, ServiceVolumeReference)>,
    pub(super) name: Option<KeyName>,
}

impl Typed {
    /// # Errors
    /// Returns a usage error for a malformed name, no `--env`, and `--service` without
    /// exactly one.
    pub(super) fn of(matches: &ArgMatches) -> Result<Self, Error> {
        let envs = strings(matches, "env")
            .map(|name| EnvironmentName::parse(name.to_owned()))
            .collect::<Result<Vec<_>, _>>()?;
        if envs.is_empty() {
            return Err(Error::usage(
                "Name at least one environment the key may deploy to with --env. Nothing changed.",
            ));
        }
        let service = matches
            .get_one::<String>("service")
            .map(ServiceName::parse)
            .transpose()?;
        if service.is_some() && envs.len() != 1 {
            return Err(Error::usage("--service needs exactly one --env."));
        }
        let volumes = strings(matches, "volume")
            .map(volume)
            .collect::<Result<Vec<_>, _>>()?;
        let name = matches
            .get_one::<String>("name")
            .map(|name| KeyName::parse(name))
            .transpose()?;
        Ok(Self {
            project: super::super::store::project(matches)?,
            envs,
            service,
            volumes,
            name,
        })
    }

    fn at(&self, environment: &EnvironmentName) -> EnvironmentRef {
        EnvironmentRef {
            project: self.project.clone(),
            environment: Some(environment.clone()),
        }
    }
}

fn strings<'a>(matches: &'a ArgMatches, id: &str) -> impl Iterator<Item = &'a str> {
    matches
        .get_many::<String>(id)
        .into_iter()
        .flatten()
        .map(String::as_str)
}

fn volume(typed: &str) -> Result<(ServiceName, ServiceVolumeReference), Error> {
    let (service, name) = typed.split_once('/').ok_or_else(|| {
        Error::usage(format!(
            "--volume {typed} must be SERVICE/VOLUME, such as db/data."
        ))
    })?;
    Ok((
        ServiceName::parse(service)?,
        ServiceVolumeReference::parse(name.to_owned())?,
    ))
}

/// One place the Store resolved, with how the prompt names it.
#[derive(Clone)]
pub(super) struct Resolved {
    pub(super) place: KeyPlace,
    pub(super) environment: EnvironmentRef,
    pub(super) label: String,
}

/// A grant's scope as the flags name it, resolved.
pub(super) struct Wanted {
    pub(super) scope: KeyScope,
    pub(super) volumes: Vec<GrantVolume>,
    pub(super) name: Option<KeyName>,
    /// What the prompt shows for each place and Volume.
    pub(super) shown: Vec<String>,
}

/// Each `--env`, and the `--service` in it, as the Store names them.
///
/// # Errors
/// Returns the Store's refusal, and `not_found` for a Service the Environment lacks.
pub(super) fn places(store: &Store, typed: &Typed) -> Result<Vec<Resolved>, Error> {
    typed
        .envs
        .iter()
        .map(|environment| {
            let at = typed.at(environment);
            let view = store.read(&NamespaceQuery {
                environment: at.clone(),
            })?;
            let environment_id = uuid(view.environment.id.as_str())?;
            let service = typed
                .service
                .as_ref()
                .map(|name| place_service(store, &at, &view.services, name))
                .transpose()?;
            let label = match &service {
                Some(service) => format!(
                    "service {} in {} ({})",
                    service.name, view.environment, view.namespace
                ),
                None => format!("{} ({})", view.environment, view.namespace),
            };
            Ok(Resolved {
                place: KeyPlace {
                    environment: environment_id,
                    namespace: view.namespace,
                    service,
                },
                environment: at,
                label,
            })
        })
        .collect()
}

fn place_service(
    store: &Store,
    at: &EnvironmentRef,
    runtime: &std::collections::BTreeMap<ServiceName, ServiceName>,
    name: &ServiceName,
) -> Result<PlaceService, Error> {
    let listed = store.read(&ServicesQuery {
        environment: at.clone(),
    })?;
    let Some(listing) = listed
        .services
        .into_iter()
        .find(|listing| &listing.service.name == name)
    else {
        return Err(Error::not_found(format!(
            "No service {name} in {}.",
            listed.environment
        )));
    };
    Ok(PlaceService {
        id: uuid(listing.service.id.as_str())?,
        name: runtime
            .get(name)
            .cloned()
            .unwrap_or(listing.service.private_dns),
    })
}

fn uuid(id: &str) -> Result<Uuid, Error> {
    Uuid::parse_str(id).map_err(|_| {
        Error::conflict(format!(
            "Ployz Cloud named {id} by an ID that isn't a UUID. Nothing changed."
        ))
    })
}

/// The grant `typed` asks for, its places resolved in `places` and each Volume read from
/// the newest version a person signed of its Service, opened as a Server opens a parent:
/// under `trusted`, for Cluster `cluster`.
///
/// # Errors
/// Returns the Store's refusal, and refuses a Volume whose Service has no such version,
/// one whose signature or digest doesn't check, or a name its spec doesn't mount.
pub(super) fn wanted(
    store: &Store,
    typed: &Typed,
    places: Vec<Resolved>,
    (trusted, cluster): (&TrustedKeys, SealingPublicKey),
) -> Result<Wanted, Error> {
    let mut volumes = Vec::new();
    let mut shown: Vec<String> = places.iter().map(|place| place.label.clone()).collect();
    for resolved in &places {
        for (service, reference) in &typed.volumes {
            let entry = root_volume(store, resolved, (service, reference), (trusted, cluster))?;
            shown.push(format!(
                "volume {service}/{reference} in {} ({})",
                resolved.place.namespace, entry.name
            ));
            if !volumes.contains(&entry) {
                volumes.push(entry);
            }
        }
    }
    Ok(Wanted {
        scope: KeyScope {
            caps: CapSet::of(&[Cap::Spec]),
            places: KeyPlaces::Only(places.into_iter().map(|resolved| resolved.place).collect()),
        },
        volumes,
        name: typed.name.clone(),
        shown,
    })
}

fn root_volume(
    store: &Store,
    resolved: &Resolved,
    (service, reference): (&ServiceName, &ServiceVolumeReference),
    (trusted, cluster): (&TrustedKeys, SealingPublicKey),
) -> Result<GrantVolume, Error> {
    let namespace = &resolved.place.namespace;
    let runtime = store
        .read(&NamespaceQuery {
            environment: resolved.environment.clone(),
        })?
        .services
        .get(service)
        .cloned()
        .unwrap_or_else(|| service.clone());
    let parents = store.read(&ParentQuery {
        environment: resolved.environment.clone(),
        services: vec![runtime.clone()],
    })?;
    let unsigned = || {
        Error::conflict(format!(
            "{service} in {namespace} has no deploy a person approved yet. Deploy it from a terminal first. Nothing changed."
        ))
    };
    let Some(Parent::Signed {
        statement, spec, ..
    }) = parents.services.get(&runtime)
    else {
        return Err(unsigned());
    };
    let forged = || {
        Error::conflict(format!(
            "The version of {service} Ployz Cloud returned doesn't check against your servers' keys. Nothing changed."
        ))
    };
    let approved = Approved::verify(
        statement,
        (**spec).clone(),
        (trusted, cluster),
        (namespace, &runtime),
    )
    .ok_or_else(forged)?;
    let spec = approved.spec();
    if let Some(entry) = mounted(spec, namespace, reference) {
        return Ok(entry);
    }
    // A spec mounts a Volume by its ID, so a name as `volume ls` shows it resolves first.
    let named = store
        .read(&VolumesQuery {
            environment: resolved.environment.clone(),
        })?
        .volumes
        .into_iter()
        .find(|listing| listing.volume.name.as_str() == reference.as_str())
        .and_then(|listing| DockerVolumeName::for_resource(listing.volume.id.as_str()).ok())
        .and_then(|name| ServiceVolumeReference::parse(name.as_str().to_owned()).ok());
    named
        .and_then(|reference| mounted(spec, namespace, &reference))
        .ok_or_else(|| {
            Error::not_found(format!(
                "{service} in {namespace} mounts no volume {reference}."
            ))
        })
}

/// The Volume `spec` declares as `reference`, by the name it takes in `namespace`.
fn mounted(
    spec: &ployz_core::RequestedServiceSpec,
    namespace: &Namespace,
    reference: &ServiceVolumeReference,
) -> Option<GrantVolume> {
    let volume = spec
        .volume_graph()
        .mounted_volumes()
        .find(|volume| &volume.reference == reference)?;
    let mut source = volume.source.clone();
    source.scope_to_namespace(namespace);
    let name = source.managed_docker_volume_name()?.clone();
    let origin = VolumeOrigin::expected(source.birth(), &name).ok()?;
    Some(GrantVolume { origin, name })
}
