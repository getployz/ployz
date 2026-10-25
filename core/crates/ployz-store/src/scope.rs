//! Resolving which Project and Environment a request means, and loading and saving
//! an Environment's Working State document.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::config::{
    EnvironmentNodeType, SavedConfigIntent, SavedEnvironmentIntent, SavedServiceIntent,
    SavedVolumeIntent, parse_environment_intent,
};
use ployz_core::{ConfigName, RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::Actor;
use crate::error;
use crate::id::{EnvironmentId, EnvironmentName, ProjectId, ProjectName, Revision, VolumeName};
use crate::storage::Tx;

/// Which Environment a request addresses. An omitted Project means the
/// Organization's only Project; an omitted Environment means the Project's
/// Default Environment.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentRef {
    /// The Project, by name.
    #[serde(default)]
    pub project: Option<ProjectName>,
    /// The Environment, by name within the Project.
    #[serde(default)]
    pub environment: Option<EnvironmentName>,
}

/// An Environment as every result names it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct EnvironmentSummary {
    pub id: EnvironmentId,
    pub project: ProjectName,
    /// Its name within the Project.
    pub name: EnvironmentName,
    /// Working State's revision after the request.
    pub revision: Revision,
}

pub(crate) struct Project {
    pub(crate) id: ProjectId,
    pub(crate) name: ProjectName,
    pub(crate) default_environment: EnvironmentId,
}

/// An Environment with its Working State, loaded inside one transaction.
pub(crate) struct Environment {
    pub(crate) summary: EnvironmentSummary,
    pub(crate) working: SavedEnvironmentIntent,
    /// The name of each node a Branch uses live, by lineage: its variables
    /// reference them by these names.
    pub(crate) live: BTreeMap<String, String>,
    /// Every Service that ever left its Working State, by lineage. Only grows, so
    /// how a reference shows a removed Service never changes under an edit.
    pub(crate) removed: RemovedServices,
    /// Each Service it reads that left an ancestor and that `removed` lacks, as the
    /// nearest ancestor recording it has it.
    pub(crate) inherited: RemovedServices,
}

/// A Service that left an Environment's Working State: its name when it left, if
/// any record had one, its number among the removals shown by that name, and its
/// place among all the Environment's removals. None of them ever changes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Removed {
    pub(crate) name: Option<String>,
    pub(crate) nth: usize,
    pub(crate) order: u64,
}

impl Removed {
    /// How reads show the Service `lineage` it records, such as `<deleted redis#2>`,
    /// or by its lineage when no record named it, such as `<deleted #3f2a9c01>`.
    fn label(&self, lineage: &str) -> String {
        match &self.name {
            Some(name) => ployz_core::config::deleted_owner(name, self.nth),
            None => ployz_core::config::deleted_lineage(lineage),
        }
    }
}

/// The [`Removed`] Services of an Environment, by lineage.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct RemovedServices(BTreeMap<String, Removed>);

impl RemovedServices {
    /// Record that `lineage` left, called `name`. One recorded already stays as it
    /// was first recorded.
    pub(crate) fn record(&mut self, lineage: &str, name: Option<&str>) {
        self.record_beside(lineage, name, &Self::default());
    }

    /// [`Self::record`], numbered past each label `beside` shows other Services by.
    pub(crate) fn record_beside(&mut self, lineage: &str, name: Option<&str>, beside: &Self) {
        if !self.0.contains_key(lineage) {
            let mut removed = self.next(name);
            let taken: BTreeSet<String> = beside
                .labels()
                .filter(|(other, _)| *other != lineage)
                .map(|(_, label)| label)
                .collect();
            while name.is_some() && taken.contains(&removed.label(lineage)) {
                removed.nth += 1;
            }
            self.0.insert(lineage.to_owned(), removed);
        }
    }

    /// Append each of `other`'s removals this doesn't record, after its own, in
    /// `other`'s order. `keep` picks the lineages to take.
    pub(crate) fn append(&mut self, other: &Self, keep: impl Fn(&str) -> bool) {
        for (lineage, removed) in other.in_order() {
            if keep(lineage) {
                self.record(lineage, removed.name.as_deref());
            }
        }
    }

    /// Take each of `other`'s removals this doesn't record, with the label `other`
    /// shows it by, after its own. `other` is this record grown, so no two Services
    /// share a label.
    fn adopt(&mut self, other: &Self) {
        for (lineage, removed) in other.in_order() {
            if !self.0.contains_key(lineage) {
                let order = self.next(None).order;
                self.0.insert(
                    lineage.clone(),
                    Removed {
                        order,
                        ..removed.clone()
                    },
                );
            }
        }
    }

    /// Whether `lineage` is recorded.
    pub(crate) fn contains(&self, lineage: &str) -> bool {
        self.0.contains_key(lineage)
    }

    /// The last name of each removed Service that has one, by lineage.
    pub(crate) fn names(&self) -> impl Iterator<Item = (&String, &String)> {
        self.0
            .iter()
            .filter_map(|(lineage, removed)| Some((lineage, removed.name.as_ref()?)))
    }

    /// The next removal, called `name`: after every one recorded, and numbered
    /// after every one recorded by that name.
    fn next(&self, name: Option<&str>) -> Removed {
        let removals = self.0.values();
        let alike = removals
            .clone()
            .filter(|removed| removed.name.as_deref() == name);
        Removed {
            name: name.map(str::to_owned),
            nth: alike.map(|removed| removed.nth + 1).max().unwrap_or(1),
            order: removals.map(|removed| removed.order + 1).max().unwrap_or(1),
        }
    }

    fn in_order(&self) -> Vec<(&String, &Removed)> {
        let mut removals: Vec<_> = self.0.iter().collect();
        removals.sort_by_key(|(lineage, removed)| (removed.order, *lineage));
        removals
    }

    /// Each Service recorded, by lineage, with its label.
    fn labels(&self) -> impl Iterator<Item = (&String, String)> {
        self.0
            .iter()
            .map(|(lineage, removed)| (lineage, removed.label(lineage)))
    }

    /// `names`; then each Service this records, by the label it was recorded with,
    /// such as `<deleted redis#2>`; then each `inherited` records, by its label there
    /// unless another Service shows so; then the rest of `referenced`, by lineage,
    /// such as `<deleted #3f2a9c01>`. Every label comes from records that only grow,
    /// so a shown name always means the same Service.
    pub(crate) fn shown(
        &self,
        mut names: BTreeMap<String, String>,
        inherited: &Self,
        referenced: impl IntoIterator<Item = String>,
    ) -> BTreeMap<String, String> {
        let own: BTreeSet<String> = self.labels().map(|(_, label)| label).collect();
        let mut alike: BTreeMap<String, usize> = BTreeMap::new();
        for (_, label) in inherited.labels() {
            *alike.entry(label).or_default() += 1;
        }
        for (lineage, label) in self.labels() {
            names.entry(lineage.clone()).or_insert(label);
        }
        for (lineage, label) in inherited.labels() {
            let clashes = own.contains(&label) || alike.get(&label) > Some(&1);
            names.entry(lineage.clone()).or_insert_with(|| {
                if clashes {
                    ployz_core::config::deleted_lineage(lineage)
                } else {
                    label
                }
            });
        }
        for lineage in referenced {
            let label = ployz_core::config::deleted_lineage(&lineage);
            names.entry(lineage).or_insert(label);
        }
        names
    }
}

impl Environment {
    /// Every Service name a variable may reference, by lineage: its own Services
    /// and the nodes it uses live.
    pub(crate) fn names(&self) -> BTreeMap<String, String> {
        let mut names = self.live.clone();
        names.extend(
            self.working
                .services
                .iter()
                .map(|service| (service.lineage_id.clone(), service.slug.clone())),
        );
        names
    }

    /// [`Self::names`], and each removed Service as reads show it: one name per
    /// Service, so an edit reads each back as the Service it was.
    pub(crate) fn shown_names(&self) -> BTreeMap<String, String> {
        self.removed.shown(
            self.names(),
            &self.inherited,
            crate::rules::removed_lineages(&self.working, &self.live),
        )
    }

    /// The name each removed Service it records or inherits was last called, by
    /// lineage, where a record has one; its own record first.
    pub(crate) fn removed_names(&self) -> BTreeMap<String, String> {
        self.inherited
            .names()
            .chain(self.removed.names())
            .map(|(lineage, name)| (lineage.clone(), name.clone()))
            .collect()
    }

    /// Refuse with `conflict` unless Working State is still at `expect`.
    pub(crate) fn expect(&self, expect: Option<Revision>) -> Result<(), RpcError> {
        match expect {
            Some(expect) if expect != self.summary.revision => Err(error::conflict(
                format!(
                    "Working State moved from revision {expect} to {}",
                    self.summary.revision
                ),
                json!({ "revision": self.summary.revision }),
            )),
            _ => Ok(()),
        }
    }

    /// The Service named `name` in Working State.
    pub(crate) fn service(&self, name: &ServiceName) -> Result<&SavedServiceIntent, RpcError> {
        let services = &self.working.services;
        services
            .iter()
            .find(|service| service.slug == name.as_str())
            .ok_or_else(|| no_service(name, &self.summary.name, &self.working))
    }

    /// The Service named `name` in Working State, to change.
    pub(crate) fn service_mut(
        &mut self,
        name: &ServiceName,
    ) -> Result<&mut SavedServiceIntent, RpcError> {
        let index = self
            .working
            .services
            .iter()
            .position(|service| service.slug == name.as_str())
            .ok_or_else(|| no_service(name, &self.summary.name, &self.working))?;
        Ok(self
            .working
            .services
            .get_mut(index)
            .expect("position is in bounds"))
    }

    /// The Volume named `name` in Working State, to change.
    pub(crate) fn volume_mut(
        &mut self,
        name: &VolumeName,
    ) -> Result<&mut SavedVolumeIntent, RpcError> {
        let index = self
            .working
            .volumes
            .iter()
            .position(|volume| volume.name == name.as_str())
            .ok_or_else(|| no_volume(name, &self.summary.name, &self.working))?;
        Ok(self
            .working
            .volumes
            .get_mut(index)
            .expect("position is in bounds"))
    }

    /// The Volume named `name` in Working State.
    pub(crate) fn volume(&self, name: &VolumeName) -> Result<&SavedVolumeIntent, RpcError> {
        self.working
            .volumes
            .iter()
            .find(|volume| volume.name == name.as_str())
            .ok_or_else(|| no_volume(name, &self.summary.name, &self.working))
    }

    /// The Config named `name` in Working State, to change.
    pub(crate) fn config_mut(
        &mut self,
        name: &ConfigName,
    ) -> Result<&mut SavedConfigIntent, RpcError> {
        let index = self
            .working
            .configs
            .iter()
            .position(|config| config.name == *name)
            .ok_or_else(|| no_config(name, &self.summary.name, &self.working))?;
        Ok(self
            .working
            .configs
            .get_mut(index)
            .expect("found by position"))
    }

    /// The Config named `name` in Working State.
    pub(crate) fn config(&self, name: &ConfigName) -> Result<&SavedConfigIntent, RpcError> {
        self.working
            .configs
            .iter()
            .find(|config| config.name == *name)
            .ok_or_else(|| no_config(name, &self.summary.name, &self.working))
    }
}

/// `config` names no Config in Working State: list the ones it could mean.
pub(crate) fn no_config(
    config: &ConfigName,
    environment: &EnvironmentName,
    working: &SavedEnvironmentIntent,
) -> RpcError {
    let names = working
        .configs
        .iter()
        .map(|config| config.name.as_str())
        .collect::<Vec<_>>();
    error::choices(
        format!("No Config named {config} in Environment {environment}"),
        config.as_str(),
        names.iter().copied(),
    )
}

/// `volume` names no Volume in Working State: list the ones it could mean.
fn no_volume(
    volume: &VolumeName,
    environment: &EnvironmentName,
    working: &SavedEnvironmentIntent,
) -> RpcError {
    let names = working
        .volumes
        .iter()
        .map(|volume| volume.name.as_str())
        .collect::<Vec<_>>();
    error::choices(
        format!("No Volume named {volume} in Environment {environment}"),
        volume.as_str(),
        names.iter().copied(),
    )
}

/// `service` names no Service in Working State: list the ones it could mean.
pub(crate) fn no_service(
    service: &ServiceName,
    environment: &EnvironmentName,
    working: &SavedEnvironmentIntent,
) -> RpcError {
    let names = working.services.iter().map(|service| service.slug.as_str());
    no_service_among(service, environment, names)
}

/// No Service named `service` in `environment`, naming the close ones of `names`.
pub(crate) fn no_service_among<'name>(
    service: &ServiceName,
    environment: &EnvironmentName,
    names: impl Iterator<Item = &'name str> + Clone,
) -> RpcError {
    error::choices(
        format!("No Service named {service} in Environment {environment}"),
        service.as_str(),
        names,
    )
}

/// One node of an Environment, as its own rows store it beside the Environment.
#[derive(Clone, Copy)]
pub(crate) enum Node<'a> {
    Service(&'a SavedServiceIntent),
    Volume(&'a SavedVolumeIntent),
    Config(&'a SavedConfigIntent),
}

impl<'a> Node<'a> {
    pub(crate) const fn node_type(self) -> EnvironmentNodeType {
        match self {
            Self::Service(_) => EnvironmentNodeType::Service,
            Self::Volume(_) => EnvironmentNodeType::Volume,
            Self::Config(_) => EnvironmentNodeType::Config,
        }
    }

    pub(crate) fn lineage(self) -> &'a str {
        match self {
            Self::Service(service) => &service.lineage_id,
            Self::Volume(volume) => &volume.resource_lineage_id,
            Self::Config(config) => &config.resource_lineage_id,
        }
    }

    pub(crate) fn document(self) -> String {
        match self {
            Self::Service(service) => serde_json::to_string(service),
            Self::Volume(volume) => serde_json::to_string(volume),
            Self::Config(config) => serde_json::to_string(config),
        }
        .expect("a node is JSON")
    }
}

/// Capture `node`'s Node Introduction in `environment`: it never changes after.
pub(crate) fn introduce(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &EnvironmentId,
    node: Node<'_>,
) -> Result<(), RpcError> {
    let id = match node {
        Node::Service(service) => service.id.as_str(),
        Node::Volume(volume) => volume.resource_id.as_str(),
        Node::Config(config) => config.resource_id.as_str(),
    };
    tx.execute(
        "INSERT INTO config_node_introduction \
         (environment_id, node_id, organization_id, node_type, node) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        &[
            environment.as_str().into(),
            id.into(),
            who.organization.as_str().into(),
            crate::storage::name_of(node.node_type()).as_str().into(),
            node.document().as_str().into(),
        ],
    )?;
    Ok(())
}

/// Rows of `(node, node_type)`, as one document shaped like `like`: Node
/// Introductions, or Applied State (`what`, when one is unreadable).
pub(crate) fn nodes(
    rows: &[crate::storage::Row],
    like: &SavedEnvironmentIntent,
    what: &str,
) -> Result<SavedEnvironmentIntent, RpcError> {
    let mut intent = empty(&like.environment_slug);
    for row in rows {
        match row.variant(1, what)? {
            EnvironmentNodeType::Service => intent.services.push(row.json(0, what)?),
            EnvironmentNodeType::Volume => intent.volumes.push(row.json(0, what)?),
            EnvironmentNodeType::Config => intent.configs.push(row.json(0, what)?),
        }
    }
    Ok(intent)
}

pub(crate) fn project(
    tx: &mut dyn Tx,
    who: &Actor,
    name: Option<&ProjectName>,
) -> Result<Project, RpcError> {
    let organization = who.organization.as_str();
    let rows = match name {
        Some(name) => tx.query(
            "SELECT id, name, default_environment_id FROM config_project \
             WHERE organization_id = ?1 AND name = ?2",
            &[organization.into(), name.as_str().into()],
        )?,
        None => tx.query(
            "SELECT id, name, default_environment_id FROM config_project \
             WHERE organization_id = ?1 ORDER BY name",
            &[organization.into()],
        )?,
    };
    match rows.as_slice() {
        [row] => Ok(Project {
            id: row.parse::<ProjectId>(0, "identity")?,
            name: row.parse::<ProjectName>(1, "identity")?,
            default_environment: row.parse::<EnvironmentId>(2, "identity")?,
        }),
        [] => Err(match name {
            Some(name) => error::not_found(format!("No Project named {name}"), json!({})),
            None => error::not_found(
                "This Organization has no Project yet",
                json!({ "next": "ployz project new NAME" }),
            ),
        }),
        rows => Err(error::ambiguous(
            "Name a Project: this Organization has more than one",
            json!({ "projects": rows.iter().map(|row| row.text(1)).collect::<Result<Vec<_>, _>>()? }),
        )),
    }
}

/// The Project Environment `id` belongs to.
pub(crate) fn project_of(tx: &mut dyn Tx, id: &EnvironmentId) -> Result<Project, RpcError> {
    let rows = tx.query(
        "SELECT p.id, p.name, p.default_environment_id FROM config_project p \
         JOIN config_environment e ON e.project_id = p.id WHERE e.id = ?1",
        &[id.as_str().into()],
    )?;
    let row = rows.first().ok_or_else(|| error::corrupt("Environment"))?;
    Ok(Project {
        id: row.parse(0, "Project")?,
        name: row.parse(1, "Project")?,
        default_environment: row.parse(2, "Project")?,
    })
}

/// Resolve and load an Environment without locking it.
pub(crate) fn environment(
    tx: &mut dyn Tx,
    who: &Actor,
    at: &EnvironmentRef,
) -> Result<Environment, RpcError> {
    let (project, id) = resolve(tx, who, at)?;
    load(tx, project, &id)
}

/// Resolve, lock and load an Environment for a write. Concurrent writers to the same
/// Environment wait here until this transaction ends, so they apply in turn to the
/// latest Working State. The lock is a row update, portable to every adapter, taken
/// before the load so that under Postgres the load sees any writer that committed first.
pub(crate) fn lock(
    tx: &mut dyn Tx,
    who: &Actor,
    at: &EnvironmentRef,
) -> Result<Environment, RpcError> {
    let (project, id) = resolve(tx, who, at)?;
    lock_all(tx, [id.clone()])?;
    load(tx, project, &id)
}

/// Lock Environments `ids` for a write that touches several, strictly in ID order,
/// so writers that share any of them wait in the same order and never deadlock.
/// Locking one again later in the same transaction changes nothing.
pub(crate) fn lock_all(
    tx: &mut dyn Tx,
    ids: impl IntoIterator<Item = EnvironmentId>,
) -> Result<(), RpcError> {
    let ids: std::collections::BTreeSet<EnvironmentId> = ids.into_iter().collect();
    for id in ids {
        tx.execute(
            "UPDATE config_environment SET working_revision = working_revision WHERE id = ?1",
            &[id.as_str().into()],
        )?;
    }
    Ok(())
}

/// Lock the Project of Environment `id`: every write that touches more than one
/// Environment takes it first.
// ponytail: this serialises multi-Environment writes within a Project; go back to
// ID-ordered lock_all if throughput matters.
pub(crate) fn lock_project(tx: &mut dyn Tx, id: &EnvironmentId) -> Result<(), RpcError> {
    tx.execute(
        "UPDATE config_project SET name = name \
         WHERE id = (SELECT project_id FROM config_environment WHERE id = ?1)",
        &[id.as_str().into()],
    )?;
    Ok(())
}

/// Lock and load Environment `id` of `who`'s Organization, as [`lock`] does.
pub(crate) fn lock_id(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &EnvironmentId,
) -> Result<Environment, RpcError> {
    lock_all(tx, [id.clone()])?;
    owned(tx, who, id)
}

/// Load Environments `a` and `b` of `who`'s Organization, in that order; with
/// `lock`, lock both first, in ID order.
pub(crate) fn load_pair(
    tx: &mut dyn Tx,
    who: &Actor,
    (a, b): (&EnvironmentId, &EnvironmentId),
    lock: bool,
) -> Result<(Environment, Environment), RpcError> {
    if lock {
        lock_all(tx, [a.clone(), b.clone()])?;
    }
    Ok((owned(tx, who, a)?, owned(tx, who, b)?))
}

/// Load Environment `id` of `who`'s Organization without locking it.
fn owned(tx: &mut dyn Tx, who: &Actor, id: &EnvironmentId) -> Result<Environment, RpcError> {
    let rows = tx.query(
        "SELECT p.name FROM config_environment e JOIN config_project p ON p.id = e.project_id \
         WHERE e.id = ?1 AND e.organization_id = ?2",
        &[id.as_str().into(), who.organization.as_str().into()],
    )?;
    let project = rows
        .first()
        .ok_or_else(|| error::not_found("No such Environment", json!({})))?
        .parse::<ProjectName>(0, "identity")?;
    load(tx, project, id)
}

/// Which Environment `at` names, without reading its Working State.
pub(crate) fn resolve(
    tx: &mut dyn Tx,
    who: &Actor,
    at: &EnvironmentRef,
) -> Result<(ProjectName, EnvironmentId), RpcError> {
    let project = project(tx, who, at.project.as_ref())?;
    let Some(name) = &at.environment else {
        return Ok((project.name, project.default_environment));
    };
    let rows = tx.query(
        "SELECT id FROM config_environment WHERE project_id = ?1 AND name = ?2",
        &[project.id.as_str().into(), name.as_str().into()],
    )?;
    let Some(row) = rows.first() else {
        let rows = tx.query(
            "SELECT name FROM config_environment WHERE project_id = ?1 ORDER BY name",
            &[project.id.as_str().into()],
        )?;
        let names = rows
            .iter()
            .map(|row| row.text(0))
            .collect::<Result<Vec<_>, _>>()?;
        let mut details = error::suggest(name.as_str(), names.iter().copied());
        if let Some(details) = details.as_object_mut() {
            details.insert(
                "next".into(),
                format!("ployz env ls --project {}", project.name).into(),
            );
        }
        return Err(error::not_found(
            format!("No Environment named {name} in Project {}", project.name),
            details,
        ));
    };
    Ok((project.name, row.parse::<EnvironmentId>(0, "identity")?))
}

/// Whether Environment `id` is Protected.
pub(crate) fn protected(tx: &mut dyn Tx, id: &EnvironmentId) -> Result<bool, RpcError> {
    let rows = tx.query(
        "SELECT id FROM config_environment WHERE id = ?1 AND protected <> 0",
        &[id.as_str().into()],
    )?;
    Ok(!rows.is_empty())
}

/// Load an Environment by its ID, without locking it.
pub(crate) fn load_by_id(tx: &mut dyn Tx, id: &EnvironmentId) -> Result<Environment, RpcError> {
    let rows = tx.query(
        "SELECT p.name FROM config_environment e \
         JOIN config_project p ON p.id = e.project_id WHERE e.id = ?1",
        &[id.as_str().into()],
    )?;
    let project = rows
        .first()
        .ok_or_else(|| error::corrupt("Environment"))?
        .parse::<ProjectName>(0, "identity")?;
    load(tx, project, id)
}

/// The [`RemovedServices`] Environment `id` records.
pub(crate) fn removed_of(tx: &mut dyn Tx, id: &EnvironmentId) -> Result<RemovedServices, RpcError> {
    tx.query(
        "SELECT removed FROM config_environment WHERE id = ?1",
        &[id.as_str().into()],
    )?
    .first()
    .ok_or_else(|| error::corrupt("Environment"))?
    .json(0, "removed Services")
}

fn load(
    tx: &mut dyn Tx,
    project: ProjectName,
    id: &EnvironmentId,
) -> Result<Environment, RpcError> {
    let rows = tx.query(
        "SELECT name, working_revision, working, removed FROM config_environment WHERE id = ?1",
        &[id.as_str().into()],
    )?;
    let row = rows.first().ok_or_else(|| error::corrupt("Environment"))?;
    let working = row.intent(2, "Working State")?;
    let live = crate::branch::live_names(tx, id, &working)?;
    let removed = row.json(3, "removed Services")?;
    let inherited = inherited(tx, id, &working, &live, &removed)?;
    Ok(Environment {
        summary: EnvironmentSummary {
            id: id.clone(),
            project,
            name: row.parse::<EnvironmentName>(0, "identity")?,
            revision: Revision(row.number(1, "revision")?),
        },
        working,
        live,
        removed,
        inherited,
    })
}

/// The removal of each Service `working` reads that is neither in it, nor in `live`,
/// nor recorded in `removed`, as the nearest ancestor of `id` recording it has it.
fn inherited(
    tx: &mut dyn Tx,
    id: &EnvironmentId,
    working: &SavedEnvironmentIntent,
    live: &BTreeMap<String, String>,
    removed: &RemovedServices,
) -> Result<RemovedServices, RpcError> {
    let mut wanted = crate::rules::removed_lineages(working, live);
    wanted.retain(|lineage| !removed.contains(lineage));
    let mut inherited = RemovedServices::default();
    for ancestor in crate::branch::ancestors(tx, id)? {
        if wanted.is_empty() {
            break;
        }
        let theirs = removed_of(tx, &ancestor)?;
        wanted.retain(|lineage| match theirs.0.get(lineage) {
            Some(removal) => {
                inherited.0.insert(lineage.clone(), removal.clone());
                false
            }
            None => true,
        });
    }
    Ok(inherited)
}

/// Persist changed Working State as the next revision. The document is validated
/// whole first, so the Store never holds one it cannot read back, and must break no
/// rule of [`crate::rules`] that the Working State it replaces didn't. Refreshes
/// `environment.live` for the new Working State.
pub(crate) fn save_working(tx: &mut dyn Tx, environment: &mut Environment) -> Result<(), RpcError> {
    save_working_from(tx, environment, None)
}

/// [`save_working`] for Working State copied or restored from `from`: what `from`
/// already breaks is not new.
pub(crate) fn save_working_from(
    tx: &mut dyn Tx,
    environment: &mut Environment,
    from: Option<&SavedEnvironmentIntent>,
) -> Result<(), RpcError> {
    validate_and_refresh_working_from(tx, environment, from)?;
    persist_working(tx, environment)
}

pub(crate) fn persist_working(
    tx: &mut dyn Tx,
    environment: &mut Environment,
) -> Result<(), RpcError> {
    let rows = tx.query(
        "SELECT working, removed FROM config_environment WHERE id = ?1",
        &[environment.summary.id.as_str().into()],
    )?;
    let row = rows.first().ok_or_else(|| error::corrupt("Environment"))?;
    let before = row.intent(0, "Working State")?;
    let mut removed: RemovedServices = row.json(1, "removed Services")?;
    // A write that skipped validation still never stores a value it owes.
    crate::variables::make_generated(&mut environment.working)?;
    crate::storage::admit_protect(tx, |tx| {
        crate::review::touches_source(
            tx,
            &environment.summary.id,
            &before.services,
            &environment.working.services,
        )
    })?;
    removed.adopt(&environment.removed);
    let stays = |lineage: &str| {
        environment
            .working
            .services
            .iter()
            .any(|service| service.lineage_id == lineage)
    };
    for service in &before.services {
        if !stays(&service.lineage_id) {
            removed.record_beside(
                &service.lineage_id,
                Some(&service.slug),
                &environment.inherited,
            );
        }
    }
    environment.removed = removed;
    environment.summary.revision = environment.summary.revision.next();
    tx.execute(
        "UPDATE config_environment SET working_revision = ?1, working = ?2, removed = ?3 WHERE id = ?4",
        &[
            revision_param(environment.summary.revision)?.into(),
            serde_json::to_string(&environment.working)
                .expect("Working State is JSON")
                .as_str()
                .into(),
            serde_json::to_string(&environment.removed)
                .expect("names are JSON")
                .as_str()
                .into(),
            environment.summary.id.as_str().into(),
        ],
    )?;
    crate::review::sealed::record(tx, environment.summary.id.as_str(), &environment.working)?;
    // A row the draft changed is its own now, whatever proposal brought it.
    crate::branch::release_changed(tx, environment)
}

/// Persist the removals `environment` records, leaving its Working State as it is.
pub(crate) fn persist_removed(
    tx: &mut dyn Tx,
    environment: &mut Environment,
) -> Result<(), RpcError> {
    let mut removed = removed_of(tx, &environment.summary.id)?;
    removed.adopt(&environment.removed);
    environment.removed = removed;
    tx.execute(
        "UPDATE config_environment SET removed = ?1 WHERE id = ?2",
        &[
            serde_json::to_string(&environment.removed)
                .expect("names are JSON")
                .as_str()
                .into(),
            environment.summary.id.as_str().into(),
        ],
    )?;
    Ok(())
}

pub(crate) struct ValidatedWorking {
    environment: Environment,
}

impl ValidatedWorking {
    pub(crate) fn environment(&self) -> &Environment {
        &self.environment
    }
    pub(crate) fn persist(mut self, tx: &mut dyn Tx) -> Result<EnvironmentSummary, RpcError> {
        persist_working(tx, &mut self.environment)?;
        Ok(self.environment.summary)
    }
}

pub(crate) fn validated_working_from(
    tx: &mut dyn Tx,
    mut environment: Environment,
    from: Option<&SavedEnvironmentIntent>,
) -> Result<ValidatedWorking, RpcError> {
    validate_and_refresh_working_from(tx, &mut environment, from)?;
    environment.working = ployz_core::config::canonicalize_environment_intent(environment.working);
    Ok(ValidatedWorking { environment })
}

pub(crate) fn validate_and_refresh_working_from(
    tx: &mut dyn Tx,
    environment: &mut Environment,
    from: Option<&SavedEnvironmentIntent>,
) -> Result<(), RpcError> {
    // What is stored now is what this write replaces: the rules compare the two.
    let rows = tx.query(
        "SELECT working FROM config_environment WHERE id = ?1",
        &[environment.summary.id.as_str().into()],
    )?;
    let before = rows
        .first()
        .ok_or_else(|| error::corrupt("Environment"))?
        .intent(0, "Working State")?;
    // Every write of Working State, whole or not, comes through here.
    crate::variables::make_generated(&mut environment.working)?;
    crate::names::refuse_new_reserved(&before, &environment.working)?;
    crate::client_sealed::refuse_port(&environment.working)?;
    let (working, live) = checked(
        tx,
        &environment.summary,
        &before,
        from,
        &environment.working,
    )?;
    environment.working = working;
    environment.live = live;
    let read = crate::rules::removed_lineages(&environment.working, &environment.live);
    for service in from.into_iter().flat_map(|from| &from.services) {
        if read.contains(&service.lineage_id) {
            environment
                .removed
                .record(&service.lineage_id, Some(&service.slug));
        }
    }
    environment.inherited = inherited(
        tx,
        &environment.summary.id,
        &environment.working,
        &environment.live,
        &environment.removed,
    )?;
    Ok(())
}

/// `after`, parsed and checked against the rules as a write that replaces `before` in
/// `environment`, with the live names it gives. What `from` already breaks is not new.
pub(crate) fn checked(
    tx: &mut dyn Tx,
    environment: &EnvironmentSummary,
    before: &SavedEnvironmentIntent,
    from: Option<&SavedEnvironmentIntent>,
    after: &SavedEnvironmentIntent,
) -> Result<(SavedEnvironmentIntent, BTreeMap<String, String>), RpcError> {
    crate::volume::refuse_new_mounts(before, from, after)?;
    let document = serde_json::to_value(after).expect("an intent is JSON");
    let after = parse_environment_intent(document)
        .map_err(|error| error::invalid(error.message, json!({ "path": error.path })))?;
    let id = &environment.id;
    let setup = crate::branch::branch_setup(tx, id)?;
    let live_before = crate::branch::live_names(tx, id, before)?;
    let live = crate::branch::live_names(tx, id, &after)?;
    crate::rules::check_write(
        environment,
        crate::rules::Facts {
            working: before,
            live: &live_before,
            setup: &setup,
        },
        from,
        crate::rules::Facts {
            working: &after,
            live: &live,
            setup: &setup,
        },
    )?;
    Ok((after, live))
}

pub(crate) fn revision_param(revision: Revision) -> Result<i64, RpcError> {
    i64::try_from(revision.0).map_err(|_| error::internal("Working State revision overflowed"))
}

/// An Environment `name` with nothing in it.
pub(crate) fn empty(name: &str) -> SavedEnvironmentIntent {
    SavedEnvironmentIntent {
        version: 1,
        environment_slug: name.to_owned(),
        services: Vec::new(),
        volumes: Vec::new(),
        configs: Vec::new(),
    }
}
