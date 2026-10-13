use super::history::NotSetVariable;
use super::publish::DiscardTarget;
use super::sealed::{EverSealed, sealed};
use super::{NodeChange, Review};
use crate::scope::{Environment, EnvironmentSummary};
use crate::settings::{NodeName, ServiceSetting, SettingPath, Target, shown};
use crate::storage::Tx;
use crate::{error, scope};
use ployz_core::config::{
    At, AuthoredServiceConfig, BuildMethod, ChangeKind, CompiledNodeConfig, ConfigAttachment,
    EnvironmentNodeType, ReviewComparisonRole, ReviewLifecycleKind, ReviewNodeIdentity, RowId,
    SavedConfigFile, SavedConfigIntent, SavedEnvironmentIntent, SavedServiceIntent,
    SavedVariableIntent, SavedVariableValue, SavedVolumeIntent, ServiceBuildConfig,
    ServiceGitAccess, ServiceGitBranch, ServiceHealthcheck, ServiceManagedHostname,
    ServiceRestartPolicy, ServiceRoute, ServiceSettingChange, ServiceSource, ServiceTemplate,
    Setting, ValuePart, VolumeAttachment, VolumeKind, compare_service_settings,
    compile_environment_intent, render_variable_parts,
};
use ployz_core::{ConfigFileName, ConfigName, RpcError, ServiceName};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

struct Delta<T> {
    before: T,
    after: T,
}
impl<T: Clone + PartialEq> Delta<T> {
    fn between(before: &T, after: &T) -> Option<Self> {
        (before != after).then(|| Self {
            before: before.clone(),
            after: after.clone(),
        })
    }
}
enum Lifecycle<T> {
    Added(T),
    Removed(T),
}
impl<T> Lifecycle<T> {
    fn record(&self) -> &T {
        match self {
            Self::Added(value) | Self::Removed(value) => value,
        }
    }
    fn kind(&self) -> ReviewLifecycleKind {
        match self {
            Self::Added(_) => ReviewLifecycleKind::Create,
            Self::Removed(_) => ReviewLifecycleKind::Delete,
        }
    }
}
#[derive(Clone, PartialEq)]
struct VersionedValue {
    value: SavedVariableValue,
    fingerprint: String,
}
impl VersionedValue {
    fn of(variable: &SavedVariableIntent) -> Self {
        let SavedVariableIntent {
            id: _,
            key: _,
            description: _,
            exported: _,
            value_fingerprint,
            value,
        } = variable;
        Self {
            value: value.clone(),
            fingerprint: value_fingerprint.clone(),
        }
    }
    fn restore(&self, variable: &mut SavedVariableIntent) {
        (variable.value, variable.value_fingerprint) =
            (self.value.clone(), self.fingerprint.clone());
    }
    fn shown(&self, names: &BTreeMap<String, String>) -> Value {
        match &self.value {
            SavedVariableValue::Literal { value } => json!(render_variable_parts(
                &[ValuePart::Text {
                    value: value.clone()
                }],
                names
            )),
            SavedVariableValue::Template { parts } => json!(render_variable_parts(parts, names)),
            SavedVariableValue::Secret { encrypted_value: _ } => json!({ "secret": true }),
            SavedVariableValue::SecretWithoutValue => json!({ "secret": false }),
        }
    }
}

enum ServiceField {
    Name(Delta<String>),
    Source(Delta<ServiceSource>),
    GitSource(Delta<GitSelection>),
    Branch(Delta<ServiceGitBranch>),
    PreDeployCommand(Delta<Option<String>>),
    StartCommand(Delta<Option<String>>),
    Healthcheck(Delta<ServiceHealthcheck>),
    RestartPolicy(Delta<ServiceRestartPolicy>),
    MaxRetries(Delta<u8>),
    Replicas(Delta<u8>),
    CpuLimit(Delta<Option<f64>>),
    MemLimit(Delta<Option<f64>>),
    PrivateDns(Delta<ServiceName>),
    BuildMethod(Delta<BuildMethod>),
    DockerfilePath(Delta<Option<String>>),
    BuildCommand(Delta<Option<String>>),
    Template(Delta<Option<ServiceTemplate>>),
    ManagedHostnames(Delta<Vec<ServiceManagedHostname>>),
}
enum VariableField {
    Id(Delta<String>),
    Value(Delta<VersionedValue>),
    Exported(Delta<bool>),
    Description(Delta<Option<String>>),
}
enum VolumeField {
    Name(Delta<String>),
    Storage(Delta<VolumeKind>),
    SharedWrites(Delta<bool>),
}
enum ConfigField {
    Name(Delta<ConfigName>),
    File(ConfigFileName, Delta<Option<SavedConfigFile>>),
}
/// A name or path that one thing in an Environment holds at a time.
#[derive(Clone, PartialEq)]
enum Slot<'a> {
    /// A Service slug or private DNS name: they share one namespace.
    Service(&'a str),
    Variable(&'a str, &'a str),
    VariableId(&'a str),
    Volume(&'a str),
    Config(&'a str),
    /// A file path inside one Config.
    File(&'a str, &'a str),
    /// A container path one Service mounts a Volume or Config at.
    Mount(&'a str, String),
    /// The container path where one Service sees a mounted Config file.
    MountedFile(&'a str, String),
}
impl Slot<'_> {
    /// Whether holding `self` keeps `other` from being held: the same slot, or
    /// a Config file path that another needs as a directory.
    fn blocks(&self, other: &Self) -> bool {
        let inside = |path: &str, dir: &str| {
            path.strip_prefix(dir)
                .is_some_and(|rest| rest.starts_with('/'))
        };
        match (self, other) {
            (Self::File(config, path), Self::File(other_config, other_path)) => {
                config == other_config
                    && (path == other_path || inside(path, other_path) || inside(other_path, path))
            }
            (Self::MountedFile(service, path), Self::MountedFile(other_service, other_path)) => {
                service == other_service
                    && (path == other_path || inside(path, other_path) || inside(other_path, path))
            }
            (Self::Mount(service, at), Self::MountedFile(other_service, file))
            | (Self::MountedFile(other_service, file), Self::Mount(service, at)) => {
                service == other_service && (at == file || inside(at, file))
            }
            _ => self == other,
        }
    }
}
enum Effect {
    ServiceLifecycle(Box<Lifecycle<SavedServiceIntent>>),
    Service {
        id: String,
        field: ServiceField,
    },
    VariableLifecycle {
        service: String,
        key: String,
        change: Lifecycle<SavedVariableIntent>,
    },
    Variable {
        service: String,
        key: String,
        field: VariableField,
    },
    VolumeLifecycle(Lifecycle<SavedVolumeIntent>),
    Volume {
        id: String,
        field: VolumeField,
    },
    ConfigLifecycle(Lifecycle<SavedConfigIntent>),
    Config {
        id: String,
        field: ConfigField,
    },
    Route {
        service: String,
        id: String,
        delta: Delta<Option<ServiceRoute>>,
        before_order: Vec<String>,
    },
    VolumeMount {
        service: String,
        resource: String,
        delta: Delta<Option<VolumeAttachment>>,
    },
    ConfigMount {
        service: String,
        resource: String,
        delta: Delta<Option<ConfigAttachment>>,
    },
}

pub(super) struct Summary {
    pub(super) changes: Vec<NodeChange>,
    pub(super) total_count: usize,
    /// Row path to how many changes undoing that row alone reverses; rows that
    /// undo only themselves are absent.
    pub(super) linked: BTreeMap<String, usize>,
}
/// What one side of an effect depends on: the service lineages its values
/// reference and the resources it mounts.
#[derive(Default)]
struct Uses {
    lineages: BTreeSet<String>,
    volumes: BTreeSet<String>,
    configs: BTreeSet<String>,
}
impl Uses {
    fn of_service(service: &SavedServiceIntent) -> Self {
        Self {
            lineages: service
                .variables
                .iter()
                .flat_map(|variable| variable.value.referenced_lineages())
                .map(str::to_owned)
                .collect(),
            volumes: service
                .volume_attachments
                .iter()
                .map(|mount| mount.volume_resource_id.clone())
                .collect(),
            configs: service
                .config_attachments
                .iter()
                .map(|mount| mount.config_resource_id.clone())
                .collect(),
        }
    }
    fn of_config(config: &SavedConfigIntent) -> Self {
        Self {
            lineages: config
                .files
                .values()
                .flat_map(SavedConfigFile::referenced_lineages)
                .map(str::to_owned)
                .collect(),
            ..Self::default()
        }
    }
    fn of_value(value: &SavedVariableValue) -> Self {
        Self {
            lineages: value.referenced_lineages().map(str::to_owned).collect(),
            ..Self::default()
        }
    }
    fn of_file(file: Option<&SavedConfigFile>) -> Self {
        Self {
            lineages: file
                .into_iter()
                .flat_map(SavedConfigFile::referenced_lineages)
                .map(str::to_owned)
                .collect(),
            ..Self::default()
        }
    }
    fn names(&self, effect: &Effect) -> bool {
        match effect {
            Effect::ServiceLifecycle(change) => self.lineages.contains(&change.record().lineage_id),
            Effect::VolumeLifecycle(change) => self.volumes.contains(&change.record().resource_id),
            Effect::ConfigLifecycle(change) => self.configs.contains(&change.record().resource_id),
            Effect::Service { .. }
            | Effect::VariableLifecycle { .. }
            | Effect::Variable { .. }
            | Effect::Volume { .. }
            | Effect::Config { .. }
            | Effect::Route { .. }
            | Effect::VolumeMount { .. }
            | Effect::ConfigMount { .. } => false,
        }
    }
}
struct Row {
    node: ReviewNodeIdentity,
    path: String,
    effects: Vec<usize>,
}
struct Plan {
    effects: Vec<Effect>,
}
/// The saved versions a plan goes from and to.
type Sides<'a> = (&'a SavedEnvironmentIntent, &'a SavedEnvironmentIntent);

macro_rules! field {
    ($effects:expr, $id:expr, $before:expr, $after:expr, $variant:ident) => {
        if let Some(delta) = Delta::between($before, $after) {
            $effects.push(Effect::Service {
                id: $id.to_owned(),
                field: ServiceField::$variant(delta),
            });
        }
    };
}

impl Plan {
    fn variable_selection(
        target: Option<&Target>,
        node: &ReviewNodeIdentity,
        before: &SavedEnvironmentIntent,
        after: &SavedEnvironmentIntent,
    ) -> Option<Self> {
        let target = target?;
        let key = match target {
            Target::Variable(key) | Target::Exported(key) | Target::Description(key) => key,
            Target::Setting(_)
            | Target::Source
            | Target::Domains
            | Target::Mount(_)
            | Target::ConfigMount(_) => {
                return None;
            }
        };
        let find = |intent: &SavedEnvironmentIntent| {
            intent
                .services
                .iter()
                .find(|service| service.id == node.id)
                .and_then(|service| {
                    service
                        .variables
                        .iter()
                        .find(|variable| variable.key == key.as_str())
                })
                .cloned()
        };
        let mut before = find(before);
        let after = find(after);
        if before.is_none()
            && matches!(target, Target::Exported(_) | Target::Description(_))
            && let Some(variable) = &after
        {
            let mut defaults = variable.clone();
            defaults.exported = false;
            defaults.description = None;
            before = Some(defaults);
        }
        let mut effects = Vec::new();
        variable_changes(
            &mut effects,
            &node.id,
            key.as_str(),
            before.as_ref(),
            after.as_ref(),
        );
        effects.retain(|effect| match effect {
            Effect::VariableLifecycle { .. } => !matches!(target, Target::Exported(_)),
            Effect::Variable { field, .. } => matches!(
                (target, field),
                (
                    Target::Variable(_),
                    VariableField::Id(_) | VariableField::Value(_)
                ) | (Target::Exported(_), VariableField::Exported(_))
                    | (Target::Description(_), VariableField::Description(_))
            ),
            Effect::ServiceLifecycle(_)
            | Effect::Service { .. }
            | Effect::VolumeLifecycle(_)
            | Effect::Volume { .. }
            | Effect::ConfigLifecycle(_)
            | Effect::Config { .. }
            | Effect::Route { .. }
            | Effect::VolumeMount { .. }
            | Effect::ConfigMount { .. } => {
                unreachable!("variable traversal emits variable effects")
            }
        });
        Some(Self { effects })
    }
    fn between(
        before: &SavedEnvironmentIntent,
        after: &SavedEnvironmentIntent,
    ) -> Result<Self, RpcError> {
        let SavedEnvironmentIntent {
            version,
            environment_slug,
            services,
            volumes,
            configs,
        } = after;
        invariant(
            *version == before.version && *environment_slug == before.environment_slug,
            "History cannot change the Environment identity or document schema",
        )?;
        let mut effects = Vec::new();
        for id in keys(&before.services, services, |service| &service.id) {
            match (
                before.services.iter().find(|service| service.id == id),
                services.iter().find(|service| service.id == id),
            ) {
                (None, Some(after)) => effects.push(Effect::ServiceLifecycle(Box::new(
                    Lifecycle::Added(after.clone()),
                ))),
                (Some(before), None) => effects.push(Effect::ServiceLifecycle(Box::new(
                    Lifecycle::Removed(before.clone()),
                ))),
                (Some(before), Some(after)) => service_changes(&mut effects, before, after)?,
                (None, None) => unreachable!("union contains an owner"),
            }
        }
        for id in keys(&before.volumes, volumes, |volume| &volume.resource_id) {
            match (
                before
                    .volumes
                    .iter()
                    .find(|volume| volume.resource_id == id),
                volumes.iter().find(|volume| volume.resource_id == id),
            ) {
                (None, Some(after)) => {
                    effects.push(Effect::VolumeLifecycle(Lifecycle::Added(after.clone())))
                }
                (Some(before), None) => {
                    effects.push(Effect::VolumeLifecycle(Lifecycle::Removed(before.clone())))
                }
                (Some(before), Some(after)) => {
                    let SavedVolumeIntent {
                        resource_id,
                        resource_lineage_id,
                        name,
                        storage,
                        shared_writes,
                    } = after;
                    invariant(
                        *resource_lineage_id == before.resource_lineage_id,
                        "History cannot change a Volume's stable lineage",
                    )?;
                    if let Some(delta) = Delta::between(&before.name, name) {
                        effects.push(Effect::Volume {
                            id: resource_id.clone(),
                            field: VolumeField::Name(delta),
                        });
                    }
                    if let Some(delta) = Delta::between(&before.storage, storage) {
                        effects.push(Effect::Volume {
                            id: resource_id.clone(),
                            field: VolumeField::Storage(delta),
                        });
                    }
                    if let Some(delta) = Delta::between(&before.shared_writes, shared_writes) {
                        effects.push(Effect::Volume {
                            id: resource_id.clone(),
                            field: VolumeField::SharedWrites(delta),
                        });
                    }
                }
                (None, None) => unreachable!("union contains an owner"),
            }
        }
        for id in keys(&before.configs, configs, |config| &config.resource_id) {
            match (
                before
                    .configs
                    .iter()
                    .find(|config| config.resource_id == id),
                configs.iter().find(|config| config.resource_id == id),
            ) {
                (None, Some(after)) => {
                    effects.push(Effect::ConfigLifecycle(Lifecycle::Added(after.clone())))
                }
                (Some(before), None) => {
                    effects.push(Effect::ConfigLifecycle(Lifecycle::Removed(before.clone())))
                }
                (Some(before), Some(after)) => {
                    let SavedConfigIntent {
                        resource_id,
                        resource_lineage_id,
                        name,
                        files,
                    } = after;
                    invariant(
                        *resource_lineage_id == before.resource_lineage_id,
                        "History cannot change a Config's stable lineage",
                    )?;
                    if let Some(delta) = Delta::between(&before.name, name) {
                        effects.push(Effect::Config {
                            id: resource_id.clone(),
                            field: ConfigField::Name(delta),
                        });
                    }
                    for file in before
                        .files
                        .keys()
                        .chain(files.keys())
                        .collect::<BTreeSet<_>>()
                    {
                        if let Some(delta) = Delta::between(
                            &before.files.get(file).cloned(),
                            &files.get(file).cloned(),
                        ) {
                            effects.push(Effect::Config {
                                id: resource_id.clone(),
                                field: ConfigField::File(file.clone(), delta),
                            });
                        }
                    }
                }
                (None, None) => unreachable!("union contains an owner"),
            }
        }
        Ok(Self { effects })
    }
    fn reverse_into(
        &self,
        current: &mut SavedEnvironmentIntent,
        selected: &[usize],
    ) -> Result<(), RpcError> {
        let mut selected: BTreeSet<_> = selected.iter().copied().collect();
        let dependencies: Vec<_> = self
            .effects
            .iter()
            .enumerate()
            .filter(|(_, effect)| {
                matches!(
                    effect,
                    Effect::VolumeMount {
                        delta: Delta {
                            before: Some(_),
                            after: None
                        },
                        ..
                    } | Effect::ConfigMount {
                        delta: Delta {
                            before: Some(_),
                            after: None
                        },
                        ..
                    }
                ) && selected.iter().any(|index| {
                    let parent = self.effects.get(*index).expect("selection belongs to plan");
                    matches!(
                        parent,
                        Effect::VolumeLifecycle(Lifecycle::Removed(_))
                            | Effect::ConfigLifecycle(Lifecycle::Removed(_))
                    ) && parent.intersects(effect)
                })
            })
            .map(|(index, _)| index)
            .collect();
        selected.extend(dependencies);
        for index in &selected {
            self.effects
                .get(*index)
                .expect("selection belongs to plan")
                .reverse_into(current)?;
        }
        Ok(())
    }
    /// What undoing each effect must undo with it, once per plan.
    fn links(&self, rows: &[Row], sides: Sides<'_>) -> Links {
        let mut first_row = vec![None; self.effects.len()];
        let mut next = vec![BTreeSet::new(); self.effects.len()];
        for (at, row) in rows.iter().enumerate() {
            for effect in &row.effects {
                first_row
                    .get_mut(*effect)
                    .expect("row effects belong to plan")
                    .get_or_insert(at);
                next.get_mut(*effect)
                    .expect("row effects belong to plan")
                    .extend(row.effects.iter().filter(|other| *other != effect));
            }
        }
        for (index, effect) in self.effects.iter().enumerate() {
            for (other, candidate) in self.effects.iter().enumerate() {
                if other != index && effect.drags(candidate, sides) {
                    next.get_mut(index)
                        .expect("effect belongs to plan")
                        .insert(other);
                }
            }
        }
        Links { next, first_row }
    }
}

/// Which effects undoing one drags along: the rest of its row, and whatever the
/// same save linked to it.
struct Links {
    next: Vec<BTreeSet<usize>>,
    first_row: Vec<Option<usize>>,
}
impl Links {
    /// Every effect that undoing `seed` must undo with it.
    fn closure(&self, seed: &[usize]) -> BTreeSet<usize> {
        let mut closed: BTreeSet<usize> = seed.iter().copied().collect();
        let mut frontier: Vec<usize> = closed.iter().copied().collect();
        while let Some(index) = frontier.pop() {
            for other in self.next.get(index).expect("seed belongs to plan") {
                if closed.insert(*other) {
                    frontier.push(*other);
                }
            }
        }
        closed
    }
    /// How many listed changes `effects` span: one per node row for a lifecycle,
    /// one per settings row otherwise.
    fn units(&self, effects: &BTreeSet<usize>) -> usize {
        effects
            .iter()
            .filter_map(|effect| *self.first_row.get(*effect).expect("effect belongs to plan"))
            .collect::<BTreeSet<_>>()
            .len()
    }
}

#[derive(Clone, PartialEq, Eq)]
struct MovedMount {
    resource: String,
    attached: bool,
}
impl MovedMount {
    fn reversed(self) -> Self {
        Self {
            attached: !self.attached,
            ..self
        }
    }
}

fn keys<T>(before: &[T], after: &[T], key: impl Fn(&T) -> &String) -> BTreeSet<String> {
    before
        .iter()
        .chain(after)
        .map(|value| key(value).clone())
        .collect()
}
fn invariant(valid: bool, message: &str) -> Result<(), RpcError> {
    if valid {
        Ok(())
    } else {
        Err(error::conflict(message, json!({})))
    }
}
fn service_changes(
    effects: &mut Vec<Effect>,
    before: &SavedServiceIntent,
    after: &SavedServiceIntent,
) -> Result<(), RpcError> {
    let SavedServiceIntent {
        id,
        lineage_id,
        slug,
        config,
        variables,
        volume_attachments,
        config_attachments,
    } = after;
    invariant(
        *lineage_id == before.lineage_id,
        "History cannot change a Service's stable lineage",
    )?;
    field!(effects, id, &before.slug, slug, Name);
    let AuthoredServiceConfig {
        version,
        source,
        pre_deploy_command,
        start_command,
        healthcheck,
        restart_policy,
        max_retries,
        replicas,
        cpu_limit,
        mem_limit,
        private_dns,
        routes,
        managed_hostnames,
        build,
        template,
    } = config;
    invariant(
        *version == before.config.version,
        "History cannot change a Service's setting schema",
    )?;
    source_changes(effects, id, &before.config.source, source);
    field!(
        effects,
        id,
        &before.config.pre_deploy_command,
        pre_deploy_command,
        PreDeployCommand
    );
    field!(
        effects,
        id,
        &before.config.start_command,
        start_command,
        StartCommand
    );
    field!(
        effects,
        id,
        &before.config.healthcheck,
        healthcheck,
        Healthcheck
    );
    field!(
        effects,
        id,
        &before.config.restart_policy,
        restart_policy,
        RestartPolicy
    );
    field!(
        effects,
        id,
        &before.config.max_retries,
        max_retries,
        MaxRetries
    );
    field!(effects, id, &before.config.replicas, replicas, Replicas);
    field!(effects, id, &before.config.cpu_limit, cpu_limit, CpuLimit);
    field!(effects, id, &before.config.mem_limit, mem_limit, MemLimit);
    field!(
        effects,
        id,
        &before.config.private_dns,
        private_dns,
        PrivateDns
    );
    let ServiceBuildConfig {
        build_method,
        dockerfile_path,
        command,
    } = build;
    field!(
        effects,
        id,
        &before.config.build.build_method,
        build_method,
        BuildMethod
    );
    field!(
        effects,
        id,
        &before.config.build.dockerfile_path,
        dockerfile_path,
        DockerfilePath
    );
    field!(
        effects,
        id,
        &before.config.build.command,
        command,
        BuildCommand
    );
    field!(effects, id, &before.config.template, template, Template);
    for route in keys(&before.config.routes, routes, |route| &route.id) {
        if let Some(delta) = Delta::between(
            &before
                .config
                .routes
                .iter()
                .find(|value| value.id == route)
                .cloned(),
            &routes.iter().find(|value| value.id == route).cloned(),
        ) {
            effects.push(Effect::Route {
                service: id.clone(),
                id: route,
                delta,
                before_order: before
                    .config
                    .routes
                    .iter()
                    .map(|route| route.id.clone())
                    .collect(),
            });
        }
    }
    field!(
        effects,
        id,
        &before.config.managed_hostnames,
        managed_hostnames,
        ManagedHostnames
    );
    // By key, so a History page over a large Environment stays linear in its variables.
    fn by_key(variables: &[SavedVariableIntent]) -> BTreeMap<&str, &SavedVariableIntent> {
        variables
            .iter()
            .map(|variable| (variable.key.as_str(), variable))
            .collect()
    }
    let (earlier, later) = (by_key(&before.variables), by_key(variables));
    for key in earlier.keys().chain(later.keys()).collect::<BTreeSet<_>>() {
        variable_changes(
            effects,
            id,
            key,
            earlier.get(key).copied(),
            later.get(key).copied(),
        );
    }
    for resource in keys(&before.volume_attachments, volume_attachments, |mount| {
        &mount.volume_resource_id
    }) {
        if let Some(delta) = Delta::between(
            &before
                .volume_attachments
                .iter()
                .find(|mount| mount.volume_resource_id == resource)
                .cloned(),
            &volume_attachments
                .iter()
                .find(|mount| mount.volume_resource_id == resource)
                .cloned(),
        ) {
            effects.push(Effect::VolumeMount {
                service: id.clone(),
                resource,
                delta,
            });
        }
    }
    for resource in keys(&before.config_attachments, config_attachments, |mount| {
        &mount.config_resource_id
    }) {
        if let Some(delta) = Delta::between(
            &before
                .config_attachments
                .iter()
                .find(|mount| mount.config_resource_id == resource)
                .cloned(),
            &config_attachments
                .iter()
                .find(|mount| mount.config_resource_id == resource)
                .cloned(),
        ) {
            effects.push(Effect::ConfigMount {
                service: id.clone(),
                resource,
                delta,
            });
        }
    }
    Ok(())
}

fn variable_changes(
    effects: &mut Vec<Effect>,
    service: &str,
    key: &str,
    before: Option<&SavedVariableIntent>,
    after: Option<&SavedVariableIntent>,
) {
    match (before, after) {
        (None, Some(after)) => effects.push(Effect::VariableLifecycle {
            service: service.to_owned(),
            key: key.to_owned(),
            change: Lifecycle::Added(after.clone()),
        }),
        (Some(before), None) => effects.push(Effect::VariableLifecycle {
            service: service.to_owned(),
            key: key.to_owned(),
            change: Lifecycle::Removed(before.clone()),
        }),
        (Some(before), Some(after)) => {
            let SavedVariableIntent {
                id: variable_id,
                key,
                description,
                exported,
                value_fingerprint: _,
                value: _,
            } = after;
            let pair = VersionedValue::of(after);
            let mut add = |field| {
                effects.push(Effect::Variable {
                    service: service.to_owned(),
                    key: key.clone(),
                    field,
                })
            };
            if let Some(delta) = Delta::between(&before.id, variable_id) {
                add(VariableField::Id(delta));
            }
            if let Some(delta) = Delta::between(&VersionedValue::of(before), &pair) {
                add(VariableField::Value(delta));
            }
            if let Some(delta) = Delta::between(&before.description, description) {
                add(VariableField::Description(delta));
            }
            if let Some(delta) = Delta::between(&before.exported, exported) {
                add(VariableField::Exported(delta));
            }
        }
        (None, None) => {}
    }
}

fn source_changes(
    effects: &mut Vec<Effect>,
    id: &str,
    before: &ServiceSource,
    after: &ServiceSource,
) {
    let git = |source: &ServiceSource| match source {
        ServiceSource::Git {
            version,
            repository,
            repository_id,
            access,
            root_dir,
            branch,
        } => Some((
            GitSelection {
                version: *version,
                repository: repository.clone(),
                repository_id: *repository_id,
                access: access.clone(),
                root_dir: root_dir.clone(),
            },
            branch.clone(),
        )),
        ServiceSource::Empty { version, root_dir } => {
            let _ = (version, root_dir);
            None
        }
        ServiceSource::Image {
            version,
            image,
            credentials,
        } => {
            let _ = (version, image, credentials);
            None
        }
    };
    match (git(before), git(after)) {
        (Some((before, bb)), Some((after, ab))) => {
            if let Some(delta) = Delta::between(&before, &after) {
                effects.push(Effect::Service {
                    id: id.to_owned(),
                    field: ServiceField::GitSource(delta),
                });
            }
            field!(effects, id, &bb, &ab, Branch);
        }
        _ => {
            field!(effects, id, before, after, Source);
        }
    }
}

#[derive(Clone, PartialEq)]
struct GitSelection {
    version: u8,
    repository: String,
    repository_id: u64,
    access: ServiceGitAccess,
    root_dir: String,
}
impl GitSelection {
    fn restore(&self, source: &mut ServiceSource) -> Result<(), RpcError> {
        match source {
            ServiceSource::Git {
                version,
                repository,
                repository_id,
                access,
                root_dir,
                branch: _,
            } => {
                *version = self.version;
                repository.clone_from(&self.repository);
                *repository_id = self.repository_id;
                access.clone_from(&self.access);
                root_dir.clone_from(&self.root_dir);
                Ok(())
            }
            ServiceSource::Empty {
                version: _,
                root_dir: _,
            }
            | ServiceSource::Image {
                version: _,
                image: _,
                credentials: _,
            } => Err(error::conflict(
                "The Git source this change belongs to was replaced",
                json!({}),
            )),
        }
    }
    fn shown(&self) -> Value {
        json!({ "type": "git", "repository": self.repository, "rootDir": self.root_dir })
    }
}

impl Effect {
    fn node(&self) -> ReviewNodeIdentity {
        let (node_type, id) = match self {
            Self::ServiceLifecycle(change) => (EnvironmentNodeType::Service, &change.record().id),
            Self::Service { id, field: _ } => (EnvironmentNodeType::Service, id),
            Self::VariableLifecycle {
                service,
                key: _,
                change: _,
            }
            | Self::Variable {
                service,
                key: _,
                field: _,
            }
            | Self::Route {
                service,
                id: _,
                delta: _,
                before_order: _,
            }
            | Self::VolumeMount {
                service,
                resource: _,
                delta: _,
            }
            | Self::ConfigMount {
                service,
                resource: _,
                delta: _,
            } => (EnvironmentNodeType::Service, service),
            Self::VolumeLifecycle(change) => {
                (EnvironmentNodeType::Volume, &change.record().resource_id)
            }
            Self::Volume { id, field: _ } => (EnvironmentNodeType::Volume, id),
            Self::ConfigLifecycle(change) => {
                (EnvironmentNodeType::Config, &change.record().resource_id)
            }
            Self::Config { id, field: _ } => (EnvironmentNodeType::Config, id),
        };
        ReviewNodeIdentity {
            node_type,
            id: id.clone(),
        }
    }
    fn lifecycle(&self) -> Option<ReviewLifecycleKind> {
        match self {
            Self::ServiceLifecycle(change) => Some(change.kind()),
            Self::VolumeLifecycle(change) => Some(change.kind()),
            Self::ConfigLifecycle(change) => Some(change.kind()),
            Self::Service { .. }
            | Self::VariableLifecycle { .. }
            | Self::Variable { .. }
            | Self::Volume { .. }
            | Self::Config { .. }
            | Self::Route { .. }
            | Self::VolumeMount { .. }
            | Self::ConfigMount { .. } => None,
        }
    }
    /// What the saved side of this effect depends on.
    fn uses(&self) -> Uses {
        match self {
            Self::ServiceLifecycle(change) => match change.as_ref() {
                Lifecycle::Added(service) => Uses::of_service(service),
                Lifecycle::Removed(_) => Uses::default(),
            },
            Self::VariableLifecycle {
                change: Lifecycle::Added(variable),
                ..
            } => Uses::of_value(&variable.value),
            Self::Variable {
                field: VariableField::Value(delta),
                ..
            } => Uses::of_value(&delta.after.value),
            Self::ConfigLifecycle(Lifecycle::Added(config)) => Uses::of_config(config),
            Self::Config {
                field: ConfigField::File(_, delta),
                ..
            } => Uses::of_file(delta.after.as_ref()),
            Self::VolumeMount {
                resource,
                delta: Delta { after: Some(_), .. },
                ..
            } => Uses {
                volumes: BTreeSet::from([resource.clone()]),
                ..Uses::default()
            },
            Self::ConfigMount {
                resource,
                delta: Delta { after: Some(_), .. },
                ..
            } => Uses {
                configs: BTreeSet::from([resource.clone()]),
                ..Uses::default()
            },
            Self::VariableLifecycle { .. }
            | Self::Variable { .. }
            | Self::ConfigLifecycle(_)
            | Self::Config { .. }
            | Self::VolumeMount { .. }
            | Self::ConfigMount { .. }
            | Self::Service { .. }
            | Self::VolumeLifecycle(_)
            | Self::Volume { .. }
            | Self::Route { .. } => Uses::default(),
        }
    }
    /// What undoing this effect puts back, and so depends on again.
    fn restores(&self) -> Uses {
        match self {
            Self::ServiceLifecycle(change) => match change.as_ref() {
                Lifecycle::Removed(service) => Uses::of_service(service),
                Lifecycle::Added(_) => Uses::default(),
            },
            Self::VariableLifecycle {
                change: Lifecycle::Removed(variable),
                ..
            } => Uses::of_value(&variable.value),
            Self::Variable {
                field: VariableField::Value(delta),
                ..
            } => Uses::of_value(&delta.before.value),
            Self::ConfigLifecycle(Lifecycle::Removed(config)) => Uses::of_config(config),
            Self::Config {
                field: ConfigField::File(_, delta),
                ..
            } => Uses::of_file(delta.before.as_ref()),
            Self::VolumeMount {
                resource,
                delta: Delta {
                    before: Some(_), ..
                },
                ..
            } => Uses {
                volumes: BTreeSet::from([resource.clone()]),
                ..Uses::default()
            },
            Self::ConfigMount {
                resource,
                delta: Delta {
                    before: Some(_), ..
                },
                ..
            } => Uses {
                configs: BTreeSet::from([resource.clone()]),
                ..Uses::default()
            },
            Self::VariableLifecycle { .. }
            | Self::Variable { .. }
            | Self::ConfigLifecycle(_)
            | Self::Config { .. }
            | Self::VolumeMount { .. }
            | Self::ConfigMount { .. }
            | Self::Service { .. }
            | Self::VolumeLifecycle(_)
            | Self::Volume { .. }
            | Self::Route { .. } => Uses::default(),
        }
    }
    /// Whether undoing `self` alone would leave `other`'s side of the same save
    /// dangling, or split one authored change in two.
    fn drags(&self, other: &Self, sides: Sides<'_>) -> bool {
        let mounted = match (self, other) {
            (Self::VolumeLifecycle(change), Self::VolumeMount { resource, .. }) => {
                change.record().resource_id == *resource
            }
            (Self::ConfigLifecycle(change), Self::ConfigMount { resource, .. }) => {
                change.record().resource_id == *resource
            }
            _ => false,
        };
        if mounted {
            return true;
        }
        if let (
            Self::Service { id, field },
            Self::Service {
                id: other_id,
                field: other_field,
            },
        ) = (self, other)
            && id == other_id
        {
            let source = |field: &ServiceField| {
                matches!(
                    field,
                    ServiceField::Source(_) | ServiceField::GitSource(_) | ServiceField::Branch(_)
                )
            };
            if source(field) && source(other_field) {
                return true;
            }
        }
        if let Some(moved) = self.moved_mount()
            && other.moved_mount() == Some(moved.reversed())
        {
            return true;
        }
        let (before, after) = self.slots(sides);
        let (other_before, other_after) = other.slots(sides);
        let blocks = |taken: &[Slot<'_>], held: &[Slot<'_>]| {
            taken
                .iter()
                .any(|slot| held.iter().any(|other| slot.blocks(other)))
        };
        if blocks(&before, &other_after) || blocks(&other_before, &after) {
            return true;
        }
        if self.lifecycle() == Some(ReviewLifecycleKind::Create) && other.uses().names(self) {
            return true;
        }
        other.lifecycle() == Some(ReviewLifecycleKind::Delete) && self.restores().names(other)
    }
    /// The names and paths this change gives up and the ones it takes, each
    /// only on its own side: undoing it takes back the first and frees the second.
    fn slots<'a>(&'a self, (earlier, later): Sides<'a>) -> (Vec<Slot<'a>>, Vec<Slot<'a>>) {
        fn lifecycle<'a, T>(
            change: &'a Lifecycle<T>,
            slots: impl FnOnce(&'a T) -> Vec<Slot<'a>>,
        ) -> (Vec<Slot<'a>>, Vec<Slot<'a>>) {
            match change {
                Lifecycle::Added(record) => (Vec::new(), slots(record)),
                Lifecycle::Removed(record) => (slots(record), Vec::new()),
            }
        }
        fn delta<'a, T>(
            delta: &'a Delta<T>,
            slot: impl Fn(&'a T) -> Option<Slot<'a>>,
        ) -> (Vec<Slot<'a>>, Vec<Slot<'a>>) {
            (
                slot(&delta.before).into_iter().collect(),
                slot(&delta.after).into_iter().collect(),
            )
        }
        let (before, after) = match self {
            Self::ServiceLifecycle(change) => lifecycle(change, |service| {
                vec![
                    Slot::Service(&service.slug),
                    Slot::Service(service.config.private_dns.as_str()),
                ]
            }),
            Self::Service {
                field: ServiceField::Name(name),
                ..
            } => delta(name, |name| Some(Slot::Service(name))),
            Self::Service {
                field: ServiceField::PrivateDns(dns),
                ..
            } => delta(dns, |dns| Some(Slot::Service(dns.as_str()))),
            Self::VariableLifecycle {
                service,
                key,
                change,
            } => lifecycle(change, |variable| {
                vec![Slot::Variable(service, key), Slot::VariableId(&variable.id)]
            }),
            Self::Variable {
                field: VariableField::Id(id),
                ..
            } => delta(id, |id| Some(Slot::VariableId(id))),
            Self::VolumeLifecycle(change) => {
                lifecycle(change, |volume| vec![Slot::Volume(&volume.name)])
            }
            Self::Volume {
                field: VolumeField::Name(name),
                ..
            } => delta(name, |name| Some(Slot::Volume(name))),
            Self::ConfigLifecycle(change) => {
                lifecycle(change, |config| vec![Slot::Config(config.name.as_str())])
            }
            Self::Config {
                field: ConfigField::Name(name),
                ..
            } => delta(name, |name| Some(Slot::Config(name.as_str()))),
            Self::Config {
                id,
                field: ConfigField::File(file, change),
            } => {
                let side = |intent: &'a SavedEnvironmentIntent, held: &Option<SavedConfigFile>| {
                    let mut slots = Vec::new();
                    if held.is_some() {
                        slots.push(Slot::File(id, file.as_str()));
                        for service in &intent.services {
                            for mount in &service.config_attachments {
                                if mount.config_resource_id == *id {
                                    slots.push(Slot::MountedFile(
                                        &service.id,
                                        format!("{}/{file}", mount.mount_dir.as_str()),
                                    ));
                                }
                            }
                        }
                    }
                    slots
                };
                (side(earlier, &change.before), side(later, &change.after))
            }
            Self::VolumeMount {
                service,
                delta: change,
                ..
            } => delta(change, |mount| {
                mount
                    .as_ref()
                    .map(|mount| Slot::Mount(service, mount.container_path()))
            }),
            Self::ConfigMount {
                service,
                resource,
                delta: change,
            } => {
                let side = |intent: &'a SavedEnvironmentIntent,
                            mount: &Option<ConfigAttachment>| {
                    let Some(mount) = mount else {
                        return Vec::new();
                    };
                    let dir = mount.mount_dir.as_str();
                    let files = intent
                        .configs
                        .iter()
                        .filter(|config| config.resource_id == *resource)
                        .flat_map(|config| config.files.keys())
                        .map(|file| Slot::MountedFile(service, format!("{dir}/{file}")));
                    std::iter::once(Slot::Mount(service, dir.to_owned()))
                        .chain(files)
                        .collect()
                };
                (side(earlier, &change.before), side(later, &change.after))
            }
            Self::Service { .. }
            | Self::Variable { .. }
            | Self::Volume { .. }
            | Self::Route { .. } => (Vec::new(), Vec::new()),
        };
        (
            before
                .iter()
                .filter(|slot| !after.contains(slot))
                .cloned()
                .collect(),
            after
                .iter()
                .filter(|slot| !before.contains(slot))
                .cloned()
                .collect(),
        )
    }
    fn moved_mount(&self) -> Option<MovedMount> {
        let (resource, before, after) = match self {
            Self::VolumeMount {
                resource, delta, ..
            } => (resource, delta.before.is_some(), delta.after.is_some()),
            Self::ConfigMount {
                resource, delta, ..
            } => (resource, delta.before.is_some(), delta.after.is_some()),
            Self::ServiceLifecycle(_)
            | Self::Service { .. }
            | Self::VariableLifecycle { .. }
            | Self::Variable { .. }
            | Self::VolumeLifecycle(_)
            | Self::Volume { .. }
            | Self::ConfigLifecycle(_)
            | Self::Config { .. }
            | Self::Route { .. } => return None,
        };
        (before != after).then(|| MovedMount {
            resource: resource.clone(),
            attached: after,
        })
    }
    fn intersects(&self, other: &Self) -> bool {
        let resource_descendant = match self {
            Self::VolumeLifecycle(change) => {
                matches!(other, Self::VolumeMount { resource, .. } if change.record().resource_id == *resource)
            }
            Self::ConfigLifecycle(change) => {
                matches!(other, Self::ConfigMount { resource, .. } if change.record().resource_id == *resource)
            }
            Self::VolumeMount { resource, .. } => {
                matches!(other, Self::VolumeLifecycle(change) if change.record().resource_id == *resource)
            }
            Self::ConfigMount { resource, .. } => {
                matches!(other, Self::ConfigLifecycle(change) if change.record().resource_id == *resource)
            }
            Self::ServiceLifecycle(_)
            | Self::Service { .. }
            | Self::VariableLifecycle { .. }
            | Self::Variable { .. }
            | Self::Volume { .. }
            | Self::Config { .. }
            | Self::Route { .. } => false,
        };
        if resource_descendant {
            return true;
        }
        if self.node() != other.node() {
            return false;
        }
        if self.lifecycle().is_some() || other.lifecycle().is_some() {
            return true;
        }
        match self {
            Self::Service { field: left, .. } => match other {
                Self::Service { field: right, .. } => {
                    std::mem::discriminant(left) == std::mem::discriminant(right)
                        || matches!(
                            (left, right),
                            (
                                ServiceField::Source(_),
                                ServiceField::GitSource(_) | ServiceField::Branch(_)
                            ) | (
                                ServiceField::GitSource(_) | ServiceField::Branch(_),
                                ServiceField::Source(_)
                            )
                        )
                }
                Self::ServiceLifecycle(_)
                | Self::VariableLifecycle { .. }
                | Self::Variable { .. }
                | Self::VolumeLifecycle(_)
                | Self::Volume { .. }
                | Self::ConfigLifecycle(_)
                | Self::Config { .. }
                | Self::Route { .. }
                | Self::VolumeMount { .. }
                | Self::ConfigMount { .. } => false,
            },
            Self::VariableLifecycle { key: left, .. } => {
                matches!(other, Self::VariableLifecycle { key: right, .. } | Self::Variable { key: right, .. } if left == right)
            }
            Self::Variable {
                key: left,
                field: lf,
                ..
            } => match other {
                Self::VariableLifecycle { key: right, .. } => left == right,
                Self::Variable {
                    key: right,
                    field: rf,
                    ..
                } => left == right && std::mem::discriminant(lf) == std::mem::discriminant(rf),
                Self::ServiceLifecycle(_)
                | Self::Service { .. }
                | Self::VolumeLifecycle(_)
                | Self::Volume { .. }
                | Self::ConfigLifecycle(_)
                | Self::Config { .. }
                | Self::Route { .. }
                | Self::VolumeMount { .. }
                | Self::ConfigMount { .. } => false,
            },
            Self::Volume { field: left, .. } => {
                matches!(other, Self::Volume { field: right, .. } if std::mem::discriminant(left) == std::mem::discriminant(right))
            }
            Self::Config { field: left, .. } => match other {
                Self::Config { field: right, .. } => match left {
                    ConfigField::File(left, _) => {
                        matches!(right, ConfigField::File(right, _) if left == right)
                    }
                    ConfigField::Name(_) => {
                        std::mem::discriminant(left) == std::mem::discriminant(right)
                    }
                },
                Self::ServiceLifecycle(_)
                | Self::Service { .. }
                | Self::VariableLifecycle { .. }
                | Self::Variable { .. }
                | Self::VolumeLifecycle(_)
                | Self::Volume { .. }
                | Self::ConfigLifecycle(_)
                | Self::Route { .. }
                | Self::VolumeMount { .. }
                | Self::ConfigMount { .. } => false,
            },
            Self::Route { id: left, .. } => {
                matches!(other, Self::Route { id: right, .. } if left == right)
            }
            Self::VolumeMount { resource: left, .. } => {
                matches!(other, Self::VolumeMount { resource: right, .. } if left == right)
            }
            Self::ConfigMount { resource: left, .. } => {
                matches!(other, Self::ConfigMount { resource: right, .. } if left == right)
            }
            Self::ServiceLifecycle(_) | Self::VolumeLifecycle(_) | Self::ConfigLifecycle(_) => {
                unreachable!("node lifecycle dominates above")
            }
        }
    }

    fn reverse_into(&self, current: &mut SavedEnvironmentIntent) -> Result<(), RpcError> {
        match self {
            Self::ServiceLifecycle(change) => {
                let id = &change.record().id;
                current.services.retain(|service| &service.id != id);
                if let Lifecycle::Removed(record) = change.as_ref() {
                    current.services.push(record.clone());
                }
            }
            Self::VolumeLifecycle(change) => {
                let id = &change.record().resource_id;
                current.volumes.retain(|volume| &volume.resource_id != id);
                match change {
                    Lifecycle::Removed(record) => current.volumes.push(record.clone()),
                    Lifecycle::Added(_) => {
                        for service in &mut current.services {
                            service
                                .volume_attachments
                                .retain(|mount| &mount.volume_resource_id != id);
                        }
                    }
                }
            }
            Self::ConfigLifecycle(change) => {
                let id = &change.record().resource_id;
                current.configs.retain(|config| &config.resource_id != id);
                match change {
                    Lifecycle::Removed(record) => current.configs.push(record.clone()),
                    Lifecycle::Added(_) => {
                        for service in &mut current.services {
                            service
                                .config_attachments
                                .retain(|mount| &mount.config_resource_id != id);
                        }
                    }
                }
            }
            Self::Service { id, field } => field.reverse_into(service_mut(current, id)?)?,
            Self::VariableLifecycle {
                service,
                key,
                change,
            } => {
                let service = service_mut(current, service)?;
                service.variables.retain(|variable| &variable.key != key);
                if let Lifecycle::Removed(record) = change {
                    service.variables.push(record.clone());
                }
            }
            Self::Variable {
                service,
                key,
                field,
            } => {
                let variable = service_mut(current, service)?
                    .variables
                    .iter_mut()
                    .find(|variable| &variable.key == key)
                    .ok_or_else(|| {
                        error::conflict(
                            "The Variable this change belongs to was removed",
                            json!({ "removed": "variable", "id": service, "key": key }),
                        )
                    })?;
                match field {
                    VariableField::Id(delta) => variable.id.clone_from(&delta.before),
                    VariableField::Value(delta) => delta.before.restore(variable),
                    VariableField::Exported(delta) => variable.exported = delta.before,
                    VariableField::Description(delta) => {
                        variable.description.clone_from(&delta.before)
                    }
                }
            }
            Self::Volume { id, field } => {
                let volume = current
                    .volumes
                    .iter_mut()
                    .find(|volume| &volume.resource_id == id)
                    .ok_or_else(|| {
                        error::conflict(
                            "The Volume this change belongs to was removed",
                            json!({ "removed": "volume", "id": id }),
                        )
                    })?;
                match field {
                    VolumeField::Name(delta) => volume.name.clone_from(&delta.before),
                    VolumeField::Storage(delta) => volume.storage = delta.before,
                    VolumeField::SharedWrites(delta) => volume.shared_writes = delta.before,
                }
            }
            Self::Config { id, field } => {
                let config = current
                    .configs
                    .iter_mut()
                    .find(|config| &config.resource_id == id)
                    .ok_or_else(|| {
                        error::conflict(
                            "The Config this change belongs to was removed",
                            json!({ "removed": "config", "id": id }),
                        )
                    })?;
                match field {
                    ConfigField::Name(delta) => config.name.clone_from(&delta.before),
                    ConfigField::File(file, delta) => {
                        if let Some(value) = &delta.before {
                            config.files.insert(file.clone(), value.clone());
                        } else {
                            config.files.remove(file);
                        }
                    }
                }
            }
            Self::Route {
                service,
                id,
                delta,
                before_order,
            } => {
                let routes = &mut service_mut(current, service)?.config.routes;
                if let Some(before) = &delta.before {
                    if let Some(route) = routes.iter_mut().find(|route| &route.id == id) {
                        route.clone_from(before);
                    } else {
                        let index = before_order
                            .iter()
                            .position(|prior| prior == id)
                            .expect("baseline route has order");
                        let position = routes
                            .iter()
                            .position(|route| {
                                before_order
                                    .iter()
                                    .position(|prior| prior == &route.id)
                                    .is_none_or(|prior| prior > index)
                            })
                            .unwrap_or(routes.len());
                        routes.insert(position, before.clone());
                    }
                } else {
                    routes.retain(|route| &route.id != id);
                }
            }
            Self::VolumeMount {
                service,
                resource,
                delta,
            } => {
                let mounts = &mut service_mut(current, service)?.volume_attachments;
                mounts.retain(|mount| &mount.volume_resource_id != resource);
                if let Some(before) = &delta.before {
                    mounts.push(before.clone());
                }
            }
            Self::ConfigMount {
                service,
                resource,
                delta,
            } => {
                let mounts = &mut service_mut(current, service)?.config_attachments;
                mounts.retain(|mount| &mount.config_resource_id != resource);
                if let Some(before) = &delta.before {
                    mounts.push(before.clone());
                }
            }
        }
        Ok(())
    }
}
fn service_mut<'a>(
    current: &'a mut SavedEnvironmentIntent,
    id: &str,
) -> Result<&'a mut SavedServiceIntent, RpcError> {
    current
        .services
        .iter_mut()
        .find(|service| service.id == id)
        .ok_or_else(|| {
            error::conflict(
                "The Service this change belongs to was removed",
                json!({ "removed": "service", "id": id }),
            )
        })
}
impl ServiceField {
    fn reverse_into(&self, service: &mut SavedServiceIntent) -> Result<(), RpcError> {
        match self {
            Self::Name(delta) => service.slug.clone_from(&delta.before),
            Self::Source(delta) => service.config.source.clone_from(&delta.before),
            Self::GitSource(delta) => delta.before.restore(&mut service.config.source)?,
            Self::Branch(delta) => match &mut service.config.source {
                ServiceSource::Git { branch, .. } => branch.clone_from(&delta.before),
                ServiceSource::Empty {
                    version: _,
                    root_dir: _,
                }
                | ServiceSource::Image {
                    version: _,
                    image: _,
                    credentials: _,
                } => {
                    return Err(error::conflict(
                        "The Git source this branch belongs to was replaced",
                        json!({}),
                    ));
                }
            },
            Self::PreDeployCommand(delta) => {
                service.config.pre_deploy_command.clone_from(&delta.before)
            }
            Self::StartCommand(delta) => service.config.start_command.clone_from(&delta.before),
            Self::Healthcheck(delta) => service.config.healthcheck.clone_from(&delta.before),
            Self::RestartPolicy(delta) => service.config.restart_policy = delta.before,
            Self::MaxRetries(delta) => service.config.max_retries = delta.before,
            Self::Replicas(delta) => service.config.replicas = delta.before,
            Self::CpuLimit(delta) => service.config.cpu_limit = delta.before,
            Self::MemLimit(delta) => service.config.mem_limit = delta.before,
            Self::PrivateDns(delta) => service.config.private_dns.clone_from(&delta.before),
            Self::BuildMethod(delta) => service.config.build.build_method = delta.before,
            Self::DockerfilePath(delta) => service
                .config
                .build
                .dockerfile_path
                .clone_from(&delta.before),
            Self::BuildCommand(delta) => service.config.build.command.clone_from(&delta.before),
            Self::Template(delta) => service.config.template.clone_from(&delta.before),
            Self::ManagedHostnames(delta) => {
                service.config.managed_hostnames.clone_from(&delta.before)
            }
        }
        Ok(())
    }
}

impl Plan {
    fn render(
        &self,
        before: &SavedEnvironmentIntent,
        after: &SavedEnvironmentIntent,
    ) -> Result<(Summary, Vec<Row>), RpcError> {
        let mut changes: Vec<NodeChange> = Vec::new();
        let mut rows: Vec<Row> = Vec::new();
        let names = crate::config_item::names_in(&[after, before]);
        for (index, effect) in self.effects.iter().enumerate() {
            let node = effect.node();
            let (name, lineage) = node_info(&node, after, before)?;
            if !changes.iter().any(|change| change.node == node) {
                changes.push(NodeChange {
                    node: node.clone(),
                    name: name.clone(),
                    row: RowId::node(&lineage),
                    lifecycle: effect.lifecycle().unwrap_or(ReviewLifecycleKind::Update),
                    comparison: Some(ReviewComparisonRole::Saved),
                    settings: Vec::new(),
                    data: None,
                    restarts: Vec::new(),
                });
            }
            let change = changes
                .iter_mut()
                .find(|change| change.node == node)
                .expect("node was inserted");
            if effect.lifecycle().is_some() {
                change.settings = lifecycle_rows(effect, before, after, &name, &lineage, &names)?;
                rows.push(Row {
                    node: node.clone(),
                    path: node_path(change),
                    effects: vec![index],
                });
                rows.extend(change.settings.iter().map(|row| Row {
                    node: node.clone(),
                    path: row.path.clone(),
                    effects: vec![index],
                }));
                continue;
            }
            let (path, at, left, right, config_name) = match effect {
                Effect::Service { field, .. } => {
                    let (path, at, left, right) = field.render();
                    (
                        format!("{name}.{path}"),
                        at.clone(),
                        public_setting(path, at.as_ref(), left),
                        public_setting(path, at.as_ref(), right),
                        None,
                    )
                }
                Effect::VariableLifecycle { key, change, .. } => {
                    let (left, right) = match change {
                        Lifecycle::Added(variable) => {
                            (Value::Null, crate::variables::shown(variable, &names))
                        }
                        Lifecycle::Removed(variable) => {
                            (crate::variables::shown(variable, &names), Value::Null)
                        }
                    };
                    (
                        format!("{name}.env.{key}"),
                        Some(At::Variable(key.clone())),
                        left,
                        right,
                        None,
                    )
                }
                Effect::Variable {
                    key,
                    field,
                    service,
                } => {
                    let (suffix, left, right) = match field {
                        VariableField::Id(_) => {
                            let value = |intent: &SavedEnvironmentIntent| {
                                intent
                                    .services
                                    .iter()
                                    .find(|owner| &owner.id == service)
                                    .and_then(|owner| {
                                        owner.variables.iter().find(|variable| &variable.key == key)
                                    })
                                    .map_or(Value::Null, |variable| {
                                        crate::variables::shown(variable, &names)
                                    })
                            };
                            ("", value(before), value(after))
                        }
                        VariableField::Value(delta) => {
                            ("", delta.before.shown(&names), delta.after.shown(&names))
                        }
                        VariableField::Exported(delta) => {
                            (".exported", json!(delta.before), json!(delta.after))
                        }
                        VariableField::Description(delta) => {
                            (".description", json!(delta.before), json!(delta.after))
                        }
                    };
                    (
                        format!("{name}.env.{key}{suffix}"),
                        Some(At::Variable(key.clone())),
                        left,
                        right,
                        None,
                    )
                }
                Effect::Volume { field, .. } => {
                    let (path, at, left, right) = match field {
                        VolumeField::Name(delta) => (
                            "name",
                            Some(At::Name),
                            json!(delta.before),
                            json!(delta.after),
                        ),
                        VolumeField::Storage(delta) => (
                            "storage",
                            Some(At::Storage),
                            json!(delta.before),
                            json!(delta.after),
                        ),
                        VolumeField::SharedWrites(delta) => (
                            "sharedWrites",
                            None,
                            json!(delta.before),
                            json!(delta.after),
                        ),
                    };
                    (format!("volumes.{name}.{path}"), at, left, right, None)
                }
                Effect::Config { id, field } => match field {
                    ConfigField::Name(delta) => (
                        format!("configs.@{id}.name"),
                        Some(At::Name),
                        json!(delta.before),
                        json!(delta.after),
                        None,
                    ),
                    ConfigField::File(file, delta) => (
                        format!("configs.@{id}.files.{file}"),
                        Some(At::File(file.clone())),
                        crate::config_item::shown_file(json!(delta.before), &names),
                        crate::config_item::shown_file(json!(delta.after), &names),
                        None,
                    ),
                },
                Effect::Route { id, delta, .. } => (
                    format!("{name}.routes.{id}"),
                    Some(At::Setting(Setting::Routes)),
                    json!(delta.before),
                    json!(delta.after),
                    None,
                ),
                Effect::VolumeMount {
                    resource, delta, ..
                } => {
                    let volume = [after, before]
                        .into_iter()
                        .flat_map(|intent| &intent.volumes)
                        .find(|volume| &volume.resource_id == resource)
                        .ok_or_else(|| error::corrupt("Volume mount identity"))?;
                    (
                        format!("{name}.mounts.{}", volume.name),
                        Some(At::Mount(volume.resource_lineage_id.clone())),
                        json!(delta.before.as_ref().map(|mount| &mount.mount_path)),
                        json!(delta.after.as_ref().map(|mount| &mount.mount_path)),
                        None,
                    )
                }
                Effect::ConfigMount {
                    resource, delta, ..
                } => {
                    let config = [after, before]
                        .into_iter()
                        .flat_map(|intent| &intent.configs)
                        .find(|config| &config.resource_id == resource)
                        .ok_or_else(|| error::corrupt("Config mount identity"))?;
                    (
                        format!("{name}.configs.@{resource}"),
                        Some(At::ConfigMount(config.resource_lineage_id.clone())),
                        json!(delta.before.as_ref().map(|mount| &mount.mount_dir)),
                        json!(delta.after.as_ref().map(|mount| &mount.mount_dir)),
                        Some(config.name.clone()),
                    )
                }
                Effect::ServiceLifecycle(_)
                | Effect::VolumeLifecycle(_)
                | Effect::ConfigLifecycle(_) => unreachable!("lifecycle rendered above"),
            };
            if let Some(row) = rows
                .iter_mut()
                .find(|row| row.node == node && row.path == path)
            {
                row.effects.push(index);
                continue;
            }
            let kind = if left.is_null() {
                ChangeKind::Add
            } else if right.is_null() {
                ChangeKind::Remove
            } else {
                ChangeKind::Update
            };
            change.settings.push(ServiceSettingChange {
                path: path.clone(),
                config_name,
                kind,
                before: left,
                after: right,
                can_restore: SettingPath::parse(&path).is_ok(),
                hostnames: None,
                row: at.map(|at| RowId::of(&lineage, at)),
            });
            rows.push(Row {
                node,
                path,
                effects: vec![index],
            });
        }
        for change in &mut changes {
            change.data =
                super::data_effect(&change.node, change.lifecycle, &change.settings, before);
            change.restarts = super::restarts(&change.node, change.lifecycle, after, before)?;
        }
        let total_count = changes
            .iter()
            .map(|change| {
                if change.lifecycle == ReviewLifecycleKind::Update {
                    change.settings.len()
                } else {
                    1
                }
            })
            .sum();
        let mut seen = BTreeSet::new();
        let links = self.links(&rows, (before, after));
        let linked = rows
            .iter()
            .filter(|row| row.effects.iter().any(|effect| seen.insert(*effect)))
            .filter_map(|row| {
                let units = links.units(&links.closure(&row.effects));
                (units > 1).then(|| (row.path.clone(), units))
            })
            .collect();
        Ok((
            Summary {
                changes,
                total_count,
                linked,
            },
            rows,
        ))
    }
}
impl ServiceField {
    fn render(&self) -> (&'static str, Option<At>, Value, Value) {
        macro_rules! setting {
            ($path:literal, $setting:ident, $delta:expr) => {
                (
                    $path,
                    Some(At::Setting(Setting::$setting)),
                    json!($delta.before),
                    json!($delta.after),
                )
            };
        }
        match self {
            Self::Name(delta) => ("name", None, json!(delta.before), json!(delta.after)),
            Self::Source(delta) => (
                "source",
                Some(At::Setting(Setting::Source)),
                public_source(&delta.before),
                public_source(&delta.after),
            ),
            Self::GitSource(delta) => (
                "source",
                Some(At::Setting(Setting::Source)),
                delta.before.shown(),
                delta.after.shown(),
            ),
            Self::ManagedHostnames(delta) => setting!("managedHostnames", ManagedHostnames, delta),
            Self::Branch(delta) => setting!("branch", Branch, delta),
            Self::PreDeployCommand(delta) => setting!("preDeployCommand", PreDeployCommand, delta),
            Self::StartCommand(delta) => setting!("startCommand", StartCommand, delta),
            Self::Healthcheck(delta) => setting!("healthcheck", Healthcheck, delta),
            Self::RestartPolicy(delta) => setting!("restartPolicy", RestartPolicy, delta),
            Self::MaxRetries(delta) => setting!("maxRetries", MaxRetries, delta),
            Self::Replicas(delta) => setting!("replicas", Replicas, delta),
            Self::CpuLimit(delta) => setting!("cpuLimit", CpuLimit, delta),
            Self::MemLimit(delta) => setting!("memLimit", MemLimit, delta),
            Self::PrivateDns(delta) => setting!("privateDns", PrivateDns, delta),
            Self::BuildMethod(delta) => setting!("buildMethod", BuildMethod, delta),
            Self::DockerfilePath(delta) => setting!("dockerfilePath", DockerfilePath, delta),
            Self::BuildCommand(delta) => setting!("buildCommand", BuildCommand, delta),
            Self::Template(delta) => ("template", None, json!(delta.before), json!(delta.after)),
        }
    }
}
fn public_source(source: &ServiceSource) -> Value {
    match source {
        ServiceSource::Empty {
            version: _,
            root_dir,
        } => json!({ "type": "empty", "rootDir": root_dir }),
        ServiceSource::Git {
            version: _,
            repository,
            repository_id: _,
            access: _,
            root_dir,
            branch,
        } => {
            json!({ "type": "git", "repository": repository, "rootDir": root_dir, "branch": branch })
        }
        ServiceSource::Image {
            version: _,
            image,
            credentials,
        } => json!({ "type": "image", "image": image, "credentials": credentials }),
    }
}
fn node_info(
    node: &ReviewNodeIdentity,
    after: &SavedEnvironmentIntent,
    before: &SavedEnvironmentIntent,
) -> Result<(String, String), RpcError> {
    [after, before]
        .into_iter()
        .find_map(|intent| match node.node_type {
            EnvironmentNodeType::Service => intent
                .services
                .iter()
                .find(|owner| owner.id == node.id)
                .map(|owner| (owner.slug.clone(), owner.lineage_id.clone())),
            EnvironmentNodeType::Volume => intent
                .volumes
                .iter()
                .find(|owner| owner.resource_id == node.id)
                .map(|owner| (owner.name.clone(), owner.resource_lineage_id.clone())),
            EnvironmentNodeType::Config => intent
                .configs
                .iter()
                .find(|owner| owner.resource_id == node.id)
                .map(|owner| {
                    (
                        owner.name.as_str().to_owned(),
                        owner.resource_lineage_id.clone(),
                    )
                }),
        })
        .ok_or_else(|| error::corrupt("Authored owner"))
}
fn node_path(change: &NodeChange) -> String {
    match change.node.node_type {
        EnvironmentNodeType::Service => change.name.clone(),
        EnvironmentNodeType::Volume => format!("volumes.{}", change.name),
        EnvironmentNodeType::Config => format!("configs.@{}", change.node.id),
    }
}
pub(super) fn compare(
    before: &SavedEnvironmentIntent,
    after: &SavedEnvironmentIntent,
) -> Result<Summary, RpcError> {
    Ok(Plan::between(before, after)?.render(before, after)?.0)
}

pub(super) struct Prepared {
    preview: Summary,
    overwritten: Vec<String>,
    not_set: Vec<NotSetVariable>,
    state: PreparedState,
}
enum PreparedState {
    Unchanged(EnvironmentSummary),
    Changed {
        before: SavedEnvironmentIntent,
        working: Box<scope::ValidatedWorking>,
    },
}
impl Prepared {
    pub(super) fn preview(&self) -> &Summary {
        &self.preview
    }
    pub(super) fn overwritten(&self) -> &[String] {
        &self.overwritten
    }
    pub(super) fn not_set(&self) -> &[NotSetVariable] {
        &self.not_set
    }
    pub(super) fn persist(self, tx: &mut dyn Tx) -> Result<(EnvironmentSummary, bool), RpcError> {
        match self.state {
            PreparedState::Unchanged(summary) => Ok((summary, false)),
            PreparedState::Changed { before, working } => {
                crate::branch::rewind(
                    tx,
                    &working.environment().summary.id,
                    (&before, &working.environment().working),
                )?;
                Ok(((*working).persist(tx)?, true))
            }
        }
    }
    fn from_target(
        tx: &mut dyn Tx,
        environment: &Environment,
        saved: Option<&SavedEnvironmentIntent>,
        target: SavedEnvironmentIntent,
        from: Option<&SavedEnvironmentIntent>,
    ) -> Result<Self, RpcError> {
        prevent_plaintext(&environment.working, &target)?;
        let working = scope::validated_working_from(
            tx,
            Environment {
                summary: environment.summary.clone(),
                working: target,
                live: environment.live.clone(),
            },
            from,
        )?;
        crate::volume::check_storage(tx, &environment.summary.id, &working.environment().working)?;
        let actual = Plan::between(&environment.working, &working.environment().working)?;
        let (preview, _) = actual.render(&environment.working, &working.environment().working)?;
        let mut overwritten = BTreeSet::new();
        if let Some(saved) = saved {
            let draft = Plan::between(saved, &environment.working)?;
            let (_, rows) = draft.render(saved, &environment.working)?;
            for row in rows {
                if row.effects.iter().any(|index| {
                    actual.effects.iter().any(|effect| {
                        effect.intersects(draft.effects.get(*index).expect("row belongs to plan"))
                    })
                }) {
                    overwritten.insert(row.path);
                }
            }
        }
        let state = if actual.effects.is_empty() {
            PreparedState::Unchanged(environment.summary.clone())
        } else {
            PreparedState::Changed {
                before: environment.working.clone(),
                working: Box::new(working),
            }
        };
        Ok(Self {
            preview,
            overwritten: overwritten.into_iter().collect(),
            not_set: Vec::new(),
            state,
        })
    }
}
pub(super) fn prepare_history(
    tx: &mut dyn Tx,
    environment: &Environment,
    ever: &EverSealed,
    saved: Option<&SavedEnvironmentIntent>,
    selected: &SavedEnvironmentIntent,
    previous: Option<&SavedEnvironmentIntent>,
    change: Option<&str>,
) -> Result<Prepared, RpcError> {
    let mut target = if let Some(previous) = previous {
        let plan = Plan::between(previous, selected)?;
        let selection: Vec<usize> = match change {
            Some(path) => {
                let (_, rows) = plan.render(previous, selected)?;
                let row = rows.iter().find(|row| row.path == path).ok_or_else(|| {
                    error::not_found(
                        "This saved version has no change at this path",
                        json!({ "path": path }),
                    )
                })?;
                plan.links(&rows, (previous, selected))
                    .closure(&row.effects)
                    .into_iter()
                    .collect()
            }
            None => (0..plan.effects.len()).collect(),
        };
        if change.is_none()
            || selection
                .iter()
                .any(|index| matches!(plan.effects.get(*index), Some(Effect::Route { .. })))
        {
            keeps_route_order(previous, selected)?;
        }
        let mut target = environment.working.clone();
        plan.reverse_into(&mut target, &selection)?;
        target
    } else {
        selected.clone()
    };
    let not_set = keep_sealed(environment, ever, &mut target)?;
    crate::names::refuse_new_taken(tx, environment, &target)?;
    let mut prepared = Prepared::from_target(tx, environment, saved, target, None)?;
    prepared.not_set = not_set;
    Ok(prepared)
}
/// History never brings a sealed value back: a key sealed now keeps its value,
/// and a sealed key History must bring back, deleted since or in a Service that
/// comes back, comes back sealed without a value. A key once sealed never comes
/// back as plain text. Returns the keys that came back without a value.
fn keep_sealed(
    environment: &Environment,
    ever: &EverSealed,
    target: &mut SavedEnvironmentIntent,
) -> Result<Vec<NotSetVariable>, RpcError> {
    let mut not_set = Vec::new();
    for service in &mut target.services {
        let current = environment
            .working
            .services
            .iter()
            .find(|current| current.id == service.id);
        for variable in &mut service.variables {
            let now = current.and_then(|current| {
                current
                    .variables
                    .iter()
                    .find(|now| now.key == variable.key)
                    .or_else(|| current.variables.iter().find(|now| now.id == variable.id))
            });
            match now {
                Some(now) if sealed(&now.value) || sealed(&variable.value) => {
                    variable.id.clone_from(&now.id);
                    variable
                        .value_fingerprint
                        .clone_from(&now.value_fingerprint);
                    variable.value = now.value.clone();
                }
                Some(now) if now.value == variable.value => {}
                Some(_) if ever.holds(&service.id, variable) => {
                    let key = &variable.key;
                    return Err(error::conflict(
                        format!("{key} was sealed. Set it again."),
                        json!({ "path": format!("{}.env.{key}", service.slug) }),
                    ));
                }
                None if sealed(&variable.value) || ever.holds(&service.id, variable) => {
                    variable.value = SavedVariableValue::SecretWithoutValue;
                    variable.value_fingerprint.clear();
                    not_set.push(NotSetVariable {
                        service: ServiceName::parse(&service.slug)
                            .map_err(|_| error::corrupt("Service name"))?,
                        key: variable.key.clone(),
                    });
                }
                Some(_) | None => {}
            }
        }
        if let Some(current) = current {
            for now in &current.variables {
                if sealed(&now.value)
                    && !service
                        .variables
                        .iter()
                        .any(|kept| kept.key == now.key || kept.id == now.id)
                {
                    service.variables.push(now.clone());
                }
            }
        }
    }
    not_set.sort_by(|a, b| (&a.service, &a.key).cmp(&(&b.service, &b.key)));
    Ok(not_set)
}
/// An inverse puts a removed route back by its old neighbours, so undoing a
/// revision that reordered common routes would not restore their order.
fn keeps_route_order(
    before: &SavedEnvironmentIntent,
    after: &SavedEnvironmentIntent,
) -> Result<(), RpcError> {
    for service in &after.services {
        let Some(prior) = before.services.iter().find(|prior| prior.id == service.id) else {
            continue;
        };
        let common = |routes: &[ServiceRoute], other: &[ServiceRoute]| -> Vec<String> {
            routes
                .iter()
                .filter(|route| other.iter().any(|value| value.id == route.id))
                .map(|route| route.id.clone())
                .collect()
        };
        invariant(
            common(&prior.config.routes, &service.config.routes)
                == common(&service.config.routes, &prior.config.routes),
            "History cannot restore reordered custom routes",
        )?;
    }
    Ok(())
}
pub(super) fn prepare_discard(
    tx: &mut dyn Tx,
    environment: &Environment,
    reviewed: &Review,
    target: DiscardTarget,
    path: Option<&SettingPath>,
) -> Result<Prepared, RpcError> {
    let saved = reviewed.saved.as_ref().map(|saved| &saved.intent);
    let mut working = environment.working.clone();
    let mut baseline = match target {
        DiscardTarget::Saved => saved.unwrap_or(&environment.working).clone(),
        DiscardTarget::Head | DiscardTarget::Review => reviewed.head.intent.clone(),
    };
    if target == DiscardTarget::Review {
        if path.is_some() {
            return Err(error::invalid(
                "Review discard takes the whole reviewed list",
                json!({}),
            ));
        }
        let introductions = super::introductions(tx, environment)?;
        for change in &reviewed.view.changes {
            if change.lifecycle == ReviewLifecycleKind::Update && !has_node(&baseline, &change.node)
            {
                copy_node(&mut baseline, &introductions, &change.node);
            }
        }
        let runtime = Plan::between(&baseline, &environment.working)?;
        let (_, rows) = runtime.render(&baseline, &environment.working)?;
        let mut selected = BTreeSet::new();
        for change in &reviewed.view.changes {
            for row in &rows {
                if row.node == change.node
                    && (change.lifecycle != ReviewLifecycleKind::Update
                        || change
                            .settings
                            .iter()
                            .any(|display| display.path == row.path))
                {
                    selected.extend(&row.effects);
                }
            }
        }
        let selected: Vec<usize> = selected.into_iter().collect();
        runtime.reverse_into(&mut working, &selected)?;
        if let Some(saved) = saved {
            let draft = Plan::between(saved, &environment.working)?;
            let chosen: Vec<_> = draft
                .effects
                .iter()
                .enumerate()
                .filter(|(_, effect)| {
                    !selected.iter().any(|index| {
                        effect.intersects(
                            runtime
                                .effects
                                .get(*index)
                                .expect("runtime selection belongs to plan"),
                        )
                    })
                })
                .map(|(index, _)| index)
                .collect();
            draft.reverse_into(&mut working, &chosen)?;
        }
    } else if let Some(path) = path {
        if let (Some(Target::Domains), Some(service)) = (path.target(), path.service()) {
            return Err(crate::settings::domains_are_read_only(path, service));
        }
        let node = resolve_node(path, environment, &baseline)?;
        if target == DiscardTarget::Head
            && (path.target().is_some() || path.node_field().is_some())
            && !has_node(&baseline, &node)
        {
            copy_node(
                &mut baseline,
                &super::introductions(tx, environment)?,
                &node,
            );
        }
        let plan = if let Some(plan) =
            Plan::variable_selection(path.target(), &node, &baseline, &environment.working)
        {
            plan
        } else {
            Plan::between(&baseline, &environment.working)?
        };
        let (_, rows) = plan.render(&baseline, &environment.working)?;
        let whole = path.target().is_none() && path.node_field().is_none();
        let selected_path = selector_path(path, &node, &environment.working, &baseline)?;
        if !whole
            && rows.iter().any(|row| {
                row.node == node
                    && row.path == selected_path
                    && row.effects.iter().any(|index| {
                        plan.effects
                            .get(*index)
                            .expect("row belongs to plan")
                            .lifecycle()
                            .is_some()
                    })
            })
        {
            return Err(error::conflict(
                "The authored baseline for this field is unavailable",
                json!({ "path": path }),
            ));
        }
        let selected: Vec<_> = rows
            .iter()
            .filter(|row| {
                row.node == node
                    && (whole
                        || row.path == selected_path
                            && row.effects.iter().all(|index| {
                                plan.effects
                                    .get(*index)
                                    .expect("row belongs to plan")
                                    .lifecycle()
                                    .is_none()
                            }))
            })
            .flat_map(|row| row.effects.iter().copied())
            .collect();
        plan.reverse_into(&mut working, &selected)?;
    } else {
        working = baseline;
    }
    Prepared::from_target(tx, environment, saved, working, Some(&reviewed.head.intent)).map_err(
        |failure| {
            if failure.code == ployz_core::RpcErrorCode::InvalidArgument {
                error::conflict(
                    format!(
                        "Discard would leave an invalid Environment: {}",
                        failure.message
                    ),
                    failure.details,
                )
            } else {
                failure
            }
        },
    )
}
fn has_node(intent: &SavedEnvironmentIntent, node: &ReviewNodeIdentity) -> bool {
    match node.node_type {
        EnvironmentNodeType::Service => intent.services.iter().any(|owner| owner.id == node.id),
        EnvironmentNodeType::Volume => intent
            .volumes
            .iter()
            .any(|owner| owner.resource_id == node.id),
        EnvironmentNodeType::Config => intent
            .configs
            .iter()
            .any(|owner| owner.resource_id == node.id),
    }
}
fn copy_node(
    target: &mut SavedEnvironmentIntent,
    source: &SavedEnvironmentIntent,
    node: &ReviewNodeIdentity,
) {
    match node.node_type {
        EnvironmentNodeType::Service => {
            if let Some(owner) = source.services.iter().find(|owner| owner.id == node.id) {
                target.services.push(owner.clone());
            }
        }
        EnvironmentNodeType::Volume => {
            if let Some(owner) = source
                .volumes
                .iter()
                .find(|owner| owner.resource_id == node.id)
            {
                target.volumes.push(owner.clone());
            }
        }
        EnvironmentNodeType::Config => {
            if let Some(owner) = source
                .configs
                .iter()
                .find(|owner| owner.resource_id == node.id)
            {
                target.configs.push(owner.clone());
            }
        }
    }
}
fn resolve_node(
    path: &SettingPath,
    environment: &Environment,
    baseline: &SavedEnvironmentIntent,
) -> Result<ReviewNodeIdentity, RpcError> {
    let working = &environment.working;
    let (node_type, id) = match path.node() {
        NodeName::Service(name) => (
            EnvironmentNodeType::Service,
            [working, baseline]
                .into_iter()
                .flat_map(|intent| &intent.services)
                .find(|owner| owner.slug == name.as_str())
                .map(|owner| owner.id.clone())
                .ok_or_else(|| scope::no_service(&name, &environment.summary.name, working))?,
        ),
        NodeName::Volume(name) => (
            EnvironmentNodeType::Volume,
            [working, baseline]
                .into_iter()
                .flat_map(|intent| &intent.volumes)
                .find(|owner| owner.name == name.as_str())
                .map(|owner| owner.resource_id.clone())
                .ok_or_else(|| {
                    environment
                        .volume(&name)
                        .err()
                        .unwrap_or_else(|| error::corrupt("Volume"))
                })?,
        ),
        NodeName::Config(selector) => (
            EnvironmentNodeType::Config,
            selector.resolve([working, baseline])?.resource_id.clone(),
        ),
    };
    Ok(ReviewNodeIdentity { node_type, id })
}
fn selector_path(
    path: &SettingPath,
    node: &ReviewNodeIdentity,
    working: &SavedEnvironmentIntent,
    baseline: &SavedEnvironmentIntent,
) -> Result<String, RpcError> {
    let (name, _) = node_info(node, working, baseline)?;
    if let Some(field) = path.node_field() {
        return Ok(match field {
            crate::settings::NodeField::Volume(field) => format!("volumes.{name}.{}", field.name()),
            crate::settings::NodeField::Config(crate::settings::ConfigField::Name) => {
                format!("configs.@{}.name", node.id)
            }
            crate::settings::NodeField::Config(crate::settings::ConfigField::File(file)) => {
                format!("configs.@{}.files.{file}", node.id)
            }
        });
    }
    Ok(match path.target() {
        Some(Target::Setting(setting)) => match setting.covering() {
            Target::Source => format!("{name}.source"),
            Target::Setting(setting) => format!("{name}.{}", setting.name()),
            Target::Variable(_)
            | Target::Exported(_)
            | Target::Description(_)
            | Target::Domains
            | Target::Mount(_)
            | Target::ConfigMount(_) => unreachable!("setting covers a setting or source"),
        },
        Some(Target::Source) => format!("{name}.source"),
        Some(Target::Domains) => format!("{name}.domains"),
        Some(Target::Variable(key)) => format!("{name}.env.{key}"),
        Some(Target::Exported(key)) => format!("{name}.env.{key}.exported"),
        Some(Target::Description(key)) => format!("{name}.env.{key}.description"),
        Some(Target::Mount(volume)) => format!("{name}.mounts.{volume}"),
        Some(Target::ConfigMount(config)) => format!(
            "{name}.configs.@{}",
            config.resolve([working, baseline])?.resource_id
        ),
        None => path.to_string(),
    })
}
fn prevent_plaintext(
    current: &SavedEnvironmentIntent,
    target: &SavedEnvironmentIntent,
) -> Result<(), RpcError> {
    for service in &current.services {
        if let Some(restored) = target.services.iter().find(|owner| owner.id == service.id) {
            for variable in &service.variables {
                let secret = |value: &SavedVariableValue| match value {
                    SavedVariableValue::Secret { encrypted_value: _ }
                    | SavedVariableValue::SecretWithoutValue => true,
                    SavedVariableValue::Literal { value: _ }
                    | SavedVariableValue::Template { parts: _ } => false,
                };
                if secret(&variable.value)
                    && restored
                        .variables
                        .iter()
                        .any(|next| next.key == variable.key && !secret(&next.value))
                {
                    return Err(error::conflict(
                        "A secret cannot become plain text through History",
                        json!({ "path": format!("{}.env.{}", restored.slug, variable.key) }),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn public_setting(path: &str, at: Option<&At>, value: Value) -> Value {
    if let Some(At::Setting(setting)) = at
        && let Some(setting) = ServiceSetting::of(*setting)
    {
        return setting.shown(value);
    }
    shown(path, value)
}
fn lifecycle_rows(
    effect: &Effect,
    before: &SavedEnvironmentIntent,
    after: &SavedEnvironmentIntent,
    name: &str,
    lineage: &str,
    names: &BTreeMap<String, String>,
) -> Result<Vec<ServiceSettingChange>, RpcError> {
    let removed = effect.lifecycle() == Some(ReviewLifecycleKind::Delete);
    let mut rows = match effect {
        Effect::ServiceLifecycle(change) => {
            let record = change.record();
            let intent = if removed { before } else { after };
            let compiled = compile_environment_intent("", intent.clone());
            let config = compiled
                .node_snapshots
                .into_iter()
                .find(|node| node.node_id == record.id)
                .ok_or_else(|| error::corrupt("Service lifecycle projection"))?;
            let CompiledNodeConfig::Service(config) = config.snapshot.0 else {
                return Err(error::corrupt("Service lifecycle type"));
            };
            compare_service_settings(&config, None)
                .into_iter()
                .filter_map(|(mut row, at)| {
                    if let Some(At::Variable(key)) = &at
                        && !record.variables.iter().any(|variable| &variable.key == key)
                    {
                        return None;
                    }
                    let at = match row.path.split_once('.') {
                        Some(("mounts", id)) => intent
                            .volumes
                            .iter()
                            .find(|volume| volume.resource_id == id)
                            .map(|volume| At::Mount(volume.resource_lineage_id.clone())),
                        Some(("configs", id)) => intent
                            .configs
                            .iter()
                            .find(|config| config.resource_id == id)
                            .map(|config| At::ConfigMount(config.resource_lineage_id.clone())),
                        _ => at,
                    };
                    let path = if let Some(At::Setting(setting)) = &at
                        && let Some(setting) = ServiceSetting::of(*setting)
                    {
                        format!("{name}.{}", setting.name())
                    } else {
                        SettingPath::from_core(name, &row.path, |family, id| {
                            if family == "configs" {
                                super::config_name(&[after, before], id)
                            } else {
                                super::volume_name(&[after, before], id)
                            }
                            .unwrap_or_default()
                        })
                    };
                    row.after = if row.path.starts_with("mounts.") {
                        row.after.get("mountPath").cloned().unwrap_or_default()
                    } else if row.path.starts_with("configs.") {
                        row.after.get("mountDir").cloned().unwrap_or_default()
                    } else {
                        public_setting(&row.path, at.as_ref(), row.after)
                    };
                    row.row = at.map(|at| RowId::of(lineage, at));
                    row.config_name = row
                        .path
                        .strip_prefix("configs.")
                        .and_then(|id| super::config_name(&[after, before], id))
                        .and_then(|name| ConfigName::parse(name).ok());
                    row.path = path;
                    row.can_restore = false;
                    Some(row)
                })
                .collect()
        }
        Effect::ConfigLifecycle(change) => change
            .record()
            .files
            .iter()
            .map(|(file, value)| ServiceSettingChange {
                path: format!("configs.@{}.files.{file}", change.record().resource_id),
                config_name: None,
                kind: ChangeKind::Add,
                before: Value::Null,
                after: crate::config_item::shown_file(json!(value), names),
                can_restore: false,
                hostnames: None,
                row: Some(RowId::of(lineage, At::File(file.clone()))),
            })
            .collect(),
        Effect::VolumeLifecycle(change) => {
            let volume = change.record();
            vec![ServiceSettingChange {
                path: format!("volumes.{name}.storage"),
                config_name: None,
                kind: ChangeKind::Add,
                before: Value::Null,
                after: json!(volume.storage),
                can_restore: false,
                hostnames: None,
                row: Some(RowId::of(lineage, At::Storage)),
            }]
        }
        Effect::Service { .. }
        | Effect::VariableLifecycle { .. }
        | Effect::Variable { .. }
        | Effect::Volume { .. }
        | Effect::Config { .. }
        | Effect::Route { .. }
        | Effect::VolumeMount { .. }
        | Effect::ConfigMount { .. } => unreachable!("lifecycle renderer takes a lifecycle"),
    };
    if removed {
        for row in &mut rows {
            std::mem::swap(&mut row.before, &mut row.after);
            row.kind = ChangeKind::Remove;
        }
    }
    Ok(rows)
}
