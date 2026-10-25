//! Edits: `set`, `unset` and `rename` of Settings and variables in an Environment's
//! Working State.
//!
//! An edit is blind by default: it applies to the latest Working State under the
//! Environment's lock, so edits to different Settings never overwrite each other.
//! A caller may pass the revision it read; the edit then refuses if Working State moved.

use ployz_core::config::ClientSealedValue;
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use crate::error;
use crate::git::GitSetting;
use crate::id::{ConfigId, Revision, VolumeName};
use crate::policy::{self, Policy};
use crate::registry;
use crate::scope::{self, Environment, EnvironmentRef, EnvironmentSummary};
use crate::sealing::SealingKey;
use crate::settings::{self, Apply, ServiceSetting, SettingPath, Target};
use crate::storage::Tx;
use crate::typed_address::{Instead, Typed};
use crate::variables::{self, VariableKey};
use crate::{Actor, Trusted};

/// Apply every change to one Environment, all or none.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Refuse with `conflict` unless Working State is still at this revision.
    #[serde(default)]
    pub expect: Option<Revision>,
    /// The changes, applied in order, except that image and repository changes go first.
    /// Every Setting set must apply to the source the whole edit leaves, and no path is
    /// changed twice.
    pub changes: Vec<Change>,
}

impl Edit {
    /// Whether this edit carries a new secret's plain text, as the Store reads the
    /// edit, or holds `{"secret": "…"}` in an edit the Store refuses.
    #[must_use]
    pub fn carries_plaintext_secret(&self) -> bool {
        !matches!(self.plaintext_secrets(), Ok(found) if found.is_empty())
    }

    /// Each new secret's plain text this edit carries, as
    /// [`Self::carries_plaintext_secret`] finds it, replaced where the edit holds it
    /// by what `seal` makes of it; `None` leaves the text for the Store to seal.
    ///
    /// # Errors
    /// What `seal` refuses, and, through `refuse`, the Store's refusal of an edit or
    /// value that holds a secret's plain text, so the text is never sent.
    pub fn map_plaintext_secrets<E>(
        &mut self,
        refuse: impl Fn(RpcError) -> E,
        mut seal: impl FnMut(PlaintextSecret<'_>) -> Result<Option<ClientSealedValue>, E>,
    ) -> Result<(), E> {
        for found in self.plaintext_secrets().map_err(&refuse)? {
            let secret = PlaintextSecret {
                service: &found.service,
                key: &found.key,
                text: found.secret.text(),
            };
            let Some(sealed) = seal(secret)? else {
                continue;
            };
            let Some(slot) = found.origin.slot(&mut self.changes) else {
                return Err(refuse(error::internal(
                    "A secret's place in the edit moved",
                )));
            };
            *slot = found.secret.sealed(&sealed);
        }
        Ok(())
    }

    /// The new secrets' plain text the Store reads from this edit: each variable a
    /// step of [`expand`] sets, read as [`variables::set`] reads it.
    ///
    /// # Errors
    /// What the Store refuses in an edit that holds `{"secret": "…"}`, or in a
    /// variable's value that holds one.
    fn plaintext_secrets(&self) -> Result<Vec<Found>, RpcError> {
        let holds = self.changes.iter().any(|change| match change {
            Change::Set { value, .. } | Change::Patch { value, .. } => {
                variables::holds_secret_text(value)
            }
            Change::Unset { .. } | Change::Rename { .. } => false,
        });
        if !holds {
            return Ok(Vec::new());
        }
        let mut found = Vec::new();
        for (path, step, origin) in expand_from(&self.changes)? {
            let (Some(Target::Variable(key)), Step::Set(value), Some(origin)) =
                (path.target(), &step, origin)
            else {
                continue;
            };
            if !variables::holds_secret_text(value) {
                continue;
            }
            let Some(secret) = variables::new_secret(key, value)? else {
                continue;
            };
            found.push(Found {
                service: path.settings_of()?.clone(),
                key: key.clone(),
                secret,
                origin,
            });
        }
        Ok(found)
    }
}

/// A new secret's plain text the Store reads from an edit, and where the edit holds it.
struct Found {
    service: ServiceName,
    key: VariableKey,
    secret: variables::NewSecret,
    origin: Origin,
}

/// Where an edit holds the value one of its variable steps sets.
enum Origin {
    /// Change `n`, a `set`.
    Set(usize),
    /// Variable `key`, as change `n`, a patch, names it under `env`.
    Env(usize, String),
}

impl Origin {
    fn slot<'changes>(&self, changes: &'changes mut [Change]) -> Option<&'changes mut Value> {
        match self {
            Self::Set(change) => match changes.get_mut(*change)? {
                Change::Set { value, .. } => Some(value),
                Change::Patch { .. } | Change::Unset { .. } | Change::Rename { .. } => None,
            },
            Self::Env(change, key) => match changes.get_mut(*change)? {
                Change::Patch { value, .. } => value.get_mut("env")?.get_mut(key),
                Change::Set { .. } | Change::Unset { .. } | Change::Rename { .. } => None,
            },
        }
    }
}

/// A new secret's plain text in an [`Edit`], for the Service and variable it lands
/// in. The text is readable only through [`Self::admit`], so whoever seals it runs
/// the Store's own checks first.
pub struct PlaintextSecret<'edit> {
    service: &'edit ServiceName,
    key: &'edit VariableKey,
    text: &'edit str,
}

impl PlaintextSecret<'_> {
    #[must_use]
    pub fn service(&self) -> &ServiceName {
        self.service
    }

    #[must_use]
    pub fn key(&self) -> &str {
        self.key.as_str()
    }

    /// The plain text, once the Store would seal it.
    ///
    /// # Errors
    /// What the Store refuses in a new secret's plain text, as it refuses it.
    pub fn admit(&self) -> Result<&str, RpcError> {
        variables::admit_secret(self.key, self.text)?;
        Ok(self.text)
    }
}

/// One change, addressed as `SERVICE.SETTING`, or as `SERVICE` for a patch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Change {
    /// Set a value. Text is accepted for any Setting type. A variable
    /// (`SERVICE.env.KEY`) takes text, `{"secret": true}` to keep its secret,
    /// `{"secret": "…"}` to seal a new one, or `{"secret": {"generate": true}}` to
    /// seal a random one the Store makes unless it holds a sealed one already.
    Set {
        /// The Setting.
        path: SettingPath,
        value: Value,
    },
    /// Return a Setting to its default, or delete a variable.
    Unset {
        /// The Setting.
        path: SettingPath,
    },
    /// Set several Settings of one Service from an object shaped like `get`'s
    /// `values`. Omitted Settings stay as they are; `null` never clears one.
    Patch {
        /// The Service, as `SERVICE`.
        path: SettingPath,
        /// Its Settings by name.
        value: Value,
    },
    /// Rename a variable. Its value moves with it, and a secret stays sealed.
    /// Already renamed is no change, so a retried rename succeeds. A variable other
    /// variables reference is renamed only with `update_references`.
    Rename {
        /// The variable, as `SERVICE.env.KEY`.
        path: SettingPath,
        /// Its new name.
        to: String,
        /// Switch the variables of this Environment that reference it to the new
        /// name in the same edit. Without it, a referenced variable refuses with
        /// `conflict`, naming them.
        // An older Store refuses the field, so it is sent only when set.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[ts(as = "Option<bool>", optional)]
        update_references: bool,
    },
}

/// One expanded change: a value to set, a Setting to unset, or a variable's new name.
enum Step {
    Set(Value),
    Unset,
    Rename {
        to: VariableKey,
        update_references: bool,
    },
}

/// The Environment after an edit, and which Setting paths it changed: staged until
/// a Deploy, or applied immediately. An edit that changed nothing keeps the revision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct Edited {
    /// The Environment, at its revision after the edit.
    pub environment: EnvironmentSummary,
    /// Settings changed in Working State, waiting for a Deploy.
    pub staged: Vec<SettingPath>,
    /// Settings that took effect at once.
    pub immediate: Vec<SettingPath>,
    /// Variables this edit set to a value with Typed Addresses, each with what to set
    /// instead.
    // An older Cloud doesn't send it.
    #[serde(default)]
    pub typed_addresses: Vec<TypedAddresses>,
    /// Settings this edit set whose value may not act, each with why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<SettingNote>>", optional)]
    pub notes: Vec<SettingNote>,
}

/// Why a Setting an edit set may not act.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SettingNote {
    /// The Setting, as SERVICE.SETTING.
    pub path: SettingPath,
    pub note: String,
}

/// The Typed Addresses in one variable an edit set: whose they are, and what to set
/// instead.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct TypedAddresses {
    /// The variable, as SERVICE.env.KEY.
    pub path: SettingPath,
    /// The Services whose private addresses it types.
    pub services: Vec<ServiceName>,
    pub instead: Instead,
}

pub(crate) fn edit(
    tx: &mut dyn Tx,
    who: &Actor,
    sealing: &SealingKey,
    edit: &Edit,
    trusted: &Trusted,
) -> Result<Edited, RpcError> {
    if edit.changes.is_empty() {
        return Err(error::invalid(
            "An edit needs at least one change",
            json!({}),
        ));
    }
    let mut steps = expand(&edit.changes)?;
    let environment = scope::lock(tx, who, &edit.environment)?;
    environment.expect(edit.expect)?;
    refuse_twice(&steps, &environment)?;
    let before = environment.working.clone();
    // Each Setting set, judged against the source the whole edit leaves.
    let mut set = Vec::new();
    for (path, step) in &steps {
        if let (Step::Set(_), Some(Target::Setting(setting))) = (step, path.target()) {
            set.push((path.settings_of()?.clone(), *setting));
        }
    }
    for (service, setting) in &set {
        if *setting == ServiceSetting::Image
            && set.contains(&(service.clone(), ServiceSetting::Git(GitSetting::Repository)))
        {
            return Err(error::invalid(
                format!("Set {service}.image or {service}.repository, not both"),
                json!({ "service": service }),
            ));
        }
    }
    // Each Volume mount set, judged against the mounts the whole edit leaves.
    let mut mounted = Vec::new();
    for (path, step) in &steps {
        if let (Step::Set(_), Some(Target::Mount(volume))) = (step, path.target()) {
            mounted.push((path.settings_of()?.clone(), volume.clone()));
        }
    }
    // Each Service a variable is set on, judged by the environment the whole edit leaves.
    let mut grown = Vec::new();
    for (path, step) in &steps {
        if let (Step::Set(_), Some(Target::Variable(_))) = (step, path.target()) {
            let service = path.settings_of()?;
            if !grown.contains(service) {
                grown.push(service.clone());
            }
        }
    }
    // The source changes go first, unsets before sets, so the order of keys never matters.
    steps.sort_by_key(|(path, step)| match (path.target(), step) {
        (Some(Target::Setting(setting)), step) if is_source(*setting) => {
            u8::from(matches!(step, Step::Set(_)))
        }
        _ => 2,
    });
    let others = steps.iter().position(
        |(path, _)| !matches!(path.target(), Some(Target::Setting(setting)) if is_source(*setting)),
    );
    let others = steps.split_off(others.unwrap_or(steps.len()));
    let mut editing = Editing {
        tx,
        who,
        sealing,
        trusted,
        environment,
        staged: Vec::new(),
        immediate: Vec::new(),
        typed_addresses: Vec::new(),
    };
    for (path, step) in steps {
        editing.apply(path, step)?;
    }
    for (path, step) in others {
        editing.apply(path, step)?;
    }
    // Every Setting set must mean something to the source the edit leaves; unsetting never
    // has to, and an empty Service takes build settings for the uploads it builds. Checked
    // last, so a value of the wrong shape is named for its shape first.
    for (service, volume) in &mounted {
        crate::volume::refuse_shared_path(&editing.environment, service, volume)?;
    }
    let mut notes = Vec::<SettingNote>::new();
    // Each Service a variable was set on, by the environment the whole edit leaves.
    for service in &grown {
        if let Some(note) = variables::environment_note(&editing.environment, service)? {
            notes.push(SettingNote {
                path: SettingPath::whole(service),
                note,
            });
        }
    }
    for (service, setting) in set {
        let config = &editing.environment.service(&service)?.config;
        if !setting.applies(config) {
            return Err(setting.not_for(&service, &config.source));
        }
        let path = SettingPath::at(&service, Target::Setting(setting));
        if let Some(note) = setting.note(config)
            && !notes.iter().any(|noted| noted.path == path)
        {
            notes.push(SettingNote { path, note });
        }
    }
    let Editing {
        tx,
        mut environment,
        staged,
        immediate,
        typed_addresses,
        ..
    } = editing;
    crate::git::check_sources(&before, &environment.working, trusted)?;
    if environment.working != before {
        scope::save_working(tx, &mut environment)?;
    }
    Ok(Edited {
        environment: environment.summary,
        staged,
        immediate,
        typed_addresses,
        notes,
    })
}

/// Two changes to one path: only the last would land, without a word. A rename writes
/// where it moves from and where it moves to, a variable set with `exported` writes
/// its `.exported` too, and a Config by name and by ID is one Config.
fn refuse_twice(steps: &[(SettingPath, Step)], environment: &Environment) -> Result<(), RpcError> {
    let mut written: Vec<SettingPath> = Vec::new();
    for (path, step) in steps {
        let mut writes = vec![(identity(path, environment), path.clone())];
        let also = match (step, path.target()) {
            (Step::Rename { to, .. }, _) => Some(Target::Variable(to.clone())),
            (Step::Set(Value::Object(fields)), Some(Target::Variable(key)))
                if fields.contains_key("exported") =>
            {
                Some(Target::Exported(key.clone()))
            }
            _ => None,
        };
        if let Some(also) = also {
            let also = SettingPath::at(path.settings_of()?, also);
            if also != *path {
                writes.push((also.clone(), also));
            }
        }
        for (identity, named) in writes {
            if written.contains(&identity) {
                return Err(error::invalid(
                    format!("{named} is changed twice in one edit; change it once"),
                    json!({ "path": named }),
                ));
            }
            written.push(identity);
        }
    }
    Ok(())
}

/// `path`, with a Config it mounts named by ID. A Config no ID matches stays as named;
/// applying the change refuses it.
fn identity(path: &SettingPath, environment: &Environment) -> SettingPath {
    let (Some(service), Some(Target::ConfigMount(config))) = (path.service(), path.target()) else {
        return path.clone();
    };
    config
        .resolve([&environment.working])
        .ok()
        .and_then(|config| ConfigId::parse(&config.resource_id).ok())
        .map_or_else(
            || path.clone(),
            |id| SettingPath::at(service, Target::ConfigMount(id.into())),
        )
}

/// Whether `setting` chooses the Service's source.
const fn is_source(setting: ServiceSetting) -> bool {
    matches!(
        setting,
        ServiceSetting::Image | ServiceSetting::Git(GitSetting::Repository)
    )
}

/// One Environment mid-edit, and the Setting paths changed so far.
struct Editing<'a> {
    tx: &'a mut dyn Tx,
    who: &'a Actor,
    sealing: &'a SealingKey,
    trusted: &'a Trusted,
    environment: Environment,
    staged: Vec<SettingPath>,
    immediate: Vec<SettingPath>,
    typed_addresses: Vec<TypedAddresses>,
}

impl Editing<'_> {
    fn apply(&mut self, path: SettingPath, step: Step) -> Result<(), RpcError> {
        let service = path.settings_of()?;
        let value = match step {
            Step::Set(value) => Some(value),
            Step::Unset => None,
            Step::Rename {
                to,
                update_references,
            } => {
                let Some(Target::Variable(key)) = path.target() else {
                    return Err(error::invalid(
                        format!("{path}: only a variable is renamed"),
                        json!({ "example": "web.env.API_URL" }),
                    ));
                };
                let renamed = SettingPath::at(service, Target::Variable(to.clone()));
                let rewrote =
                    variables::rename(&mut self.environment, service, key, &to, update_references)?;
                if let Some(rewrote) = rewrote {
                    let rewrote = rewrote
                        .into_iter()
                        .map(|(service, key)| SettingPath::at(&service, Target::Variable(key)));
                    for changed in [renamed, path].into_iter().chain(rewrote) {
                        if !self.staged.contains(&changed) {
                            self.staged.push(changed);
                        }
                    }
                }
                return Ok(());
            }
        };
        // A new credential applies at once; turning one on or off is staged.
        if let Some(Target::Setting(setting @ ServiceSetting::RegistryCredential)) = path.target() {
            let changed = match &value {
                Some(value) => registry::set(
                    &mut *self.tx,
                    self.who,
                    &mut self.environment,
                    service,
                    value,
                    self.sealing,
                )?,
                None => {
                    let config = &mut self.environment.service_mut(service)?.config;
                    let was = setting.value(config, &Policy::default());
                    setting.unset(config)?;
                    registry::Changed {
                        staged: setting.value(config, &Policy::default()) != was,
                        rotated: false,
                    }
                }
            };
            for (changed, list) in [
                (changed.staged, &mut self.staged),
                (changed.rotated, &mut self.immediate),
            ] {
                if changed && !list.contains(&path) {
                    list.push(path.clone());
                }
            }
            return Ok(());
        }
        if let Some(Target::Setting(ServiceSetting::Policy(setting))) = path.target() {
            let node = self.environment.service(service)?;
            let id = self.environment.summary.id.clone();
            if policy::set(&mut *self.tx, self.who, &id, node, *setting, value)?
                && !self.immediate.contains(&path)
            {
                self.immediate.push(path.clone());
            }
            return Ok(());
        }
        let environment = &mut self.environment;
        let (changed, apply) = match (path.target(), value) {
            // A Private DNS name addresses one Service: none other has it as a name.
            (Some(Target::Setting(setting @ ServiceSetting::PrivateDns)), value) => {
                let name = value
                    .unwrap_or_else(|| setting.default(Some(service)))
                    .as_str()
                    .and_then(|text| ployz_core::ServiceName::parse(text.trim()).ok())
                    .ok_or_else(|| setting.invalid("expected a lowercase DNS label"))?;
                let id = environment.service(service)?.id.clone();
                crate::names::refuse_taken(
                    &mut *self.tx,
                    environment,
                    crate::names::Name::Service(name.as_str()),
                    Some(&id),
                )?;
                let config = &mut environment.service_mut(service)?.config;
                let changed = config.private_dns != name;
                config.private_dns = name;
                (changed, Apply::Staged)
            }
            (Some(Target::Setting(ServiceSetting::Healthcheck)), Some(value))
                if settings::is_healthcheck_text(&value) =>
            {
                return Err(settings::healthcheck_text(service, &environment.summary));
            }
            (Some(Target::Setting(setting)), value) => {
                let config = &mut environment.service_mut(service)?.config;
                let was = setting.value(config, &Policy::default());
                match value {
                    Some(value) => setting.set(config, value, self.trusted)?,
                    None => setting.unset(config)?,
                }
                (
                    setting.value(config, &Policy::default()) != was,
                    setting.apply(),
                )
            }
            (Some(Target::Domains), _) => {
                return Err(super::domains_are_read_only(&path, service));
            }
            (Some(Target::Setup), value) => (
                crate::branch::set_own(&mut *self.tx, environment, service, value)?,
                Apply::Immediate,
            ),
            (Some(Target::Description(_)), _) => {
                return Err(error::invalid(
                    "Variable descriptions are template metadata; use Discard to restore a description",
                    json!({ "path": path }),
                ));
            }
            (Some(Target::Source), _) => {
                return Err(error::invalid(
                    format!("{path}: set image or repository instead"),
                    json!({ "example": { "image": "nginx:1.27" } }),
                ));
            }
            (Some(Target::Variable(key)), Some(value)) => {
                let guard = crate::old_cli::Guard::from(self.trusted);
                let sealer = crate::client_sealed::Sealer {
                    organization: &self.who.organization,
                    recipient: self.trusted.sealing.as_ref(),
                };
                let (changed, typed) =
                    variables::set(environment, service, key, value, guard, sealer)?;
                self.typed_addresses
                    .extend(typed.map(|Typed { services, instead }| TypedAddresses {
                        path: path.clone(),
                        services,
                        instead,
                    }));
                (changed, Apply::Staged)
            }
            (Some(Target::Exported(key)), Some(value)) => (
                variables::set_exported(environment, service, key, value)?,
                Apply::Staged,
            ),
            (Some(Target::Variable(key)), None) => (
                variables::unset(environment, service, key, false)?,
                Apply::Staged,
            ),
            (Some(Target::Exported(key)), None) => (
                variables::unset(environment, service, key, true)?,
                Apply::Staged,
            ),
            (Some(Target::Mount(volume)), Some(value)) => {
                let Some(mount) = value.as_str() else {
                    return Err(error::invalid(
                        format!("{path}: expected an absolute path"),
                        json!({ "example": "/var/lib/data" }),
                    ));
                };
                (
                    crate::volume::attach(environment, service, volume, mount)?,
                    Apply::Staged,
                )
            }
            (Some(Target::Mount(volume)), None) => (
                crate::volume::detach(environment, service, volume)?,
                Apply::Staged,
            ),
            (Some(Target::ConfigMount(config)), Some(value)) => {
                let Some(dir) = value.as_str() else {
                    return Err(error::invalid(
                        format!("{path}: expected an absolute directory"),
                        json!({ "example": "/etc/app" }),
                    ));
                };
                let name = config.resolve([&environment.working])?.name.clone();
                (
                    crate::config_item::attach(environment, service, &name, dir)?,
                    Apply::Staged,
                )
            }
            (Some(Target::ConfigMount(config)), None) => {
                let name = config.resolve([&environment.working])?.name.clone();
                (
                    crate::config_item::detach(environment, service, &name)?,
                    Apply::Staged,
                )
            }
            (None, _) => return Err(crate::settings::name_a_setting(path.node())),
        };
        if !changed {
            return Ok(());
        }
        let list = match apply {
            Apply::Staged => &mut self.staged,
            Apply::Immediate => &mut self.immediate,
        };
        if !list.contains(&path) {
            list.push(path);
        }
        Ok(())
    }
}

/// Every change as one path to set (with its value), unset, or rename.
fn expand(changes: &[Change]) -> Result<Vec<(SettingPath, Step)>, RpcError> {
    let expanded = expand_from(changes)?;
    Ok(expanded
        .into_iter()
        .map(|(path, step, _)| (path, step))
        .collect())
}

/// [`expand`], with where the edit holds each value a `set` or a patch's `env` sets.
fn expand_from(changes: &[Change]) -> Result<Vec<(SettingPath, Step, Option<Origin>)>, RpcError> {
    let mut expanded = Vec::new();
    for (index, change) in changes.iter().enumerate() {
        match change {
            Change::Set { path, value } => expanded.push((
                path.clone(),
                Step::Set(value.clone()),
                Some(Origin::Set(index)),
            )),
            Change::Unset { path } => expanded.push((path.clone(), Step::Unset, None)),
            Change::Rename {
                path,
                to,
                update_references,
            } => {
                let step = Step::Rename {
                    to: VariableKey::parse(to)?,
                    update_references: *update_references,
                };
                expanded.push((path.clone(), step, None));
            }
            Change::Patch { path, value } => {
                let service = path.settings_of()?;
                if path.target().is_some() {
                    return Err(error::invalid(
                        "A patch addresses a Service, not one of its Settings",
                        json!({ "example": service }),
                    ));
                }
                let Some(object) = value.as_object() else {
                    return Err(error::invalid(
                        "A patch is a JSON object of Settings",
                        json!({ "example": { "replicas": 3 } }),
                    ));
                };
                for (name, value) in object {
                    if name == "env" {
                        let Some(variables) = value.as_object() else {
                            return Err(error::invalid(
                                "env is a JSON object of variables",
                                json!({ "example": { "env": { "LOG_LEVEL": "info" } } }),
                            ));
                        };
                        for (key, value) in variables {
                            let origin = Origin::Env(index, key.clone());
                            let key = VariableKey::parse(key)?;
                            let path = SettingPath::at(service, Target::Variable(key));
                            expanded.push((path, Step::Set(value.clone()), Some(origin)));
                        }
                        continue;
                    }
                    if name == "mounts" {
                        let Some(mounts) = value.as_object() else {
                            return Err(error::invalid(
                                "mounts is a JSON object of paths by Volume name",
                                json!({ "example": { "mounts": { "data": "/var/lib/data" } } }),
                            ));
                        };
                        for (volume, value) in mounts {
                            let volume = VolumeName::parse(volume.as_str())?;
                            let path = SettingPath::at(service, Target::Mount(volume));
                            expanded.push((path, Step::Set(value.clone()), None));
                        }
                        continue;
                    }
                    if name == "configs" {
                        let Some(mounts) = value.as_object() else {
                            return Err(error::invalid(
                                "configs is a JSON object of directories by Config name",
                                json!({ "example": { "configs": { "sentry": "/etc/sentry" } } }),
                            ));
                        };
                        for (config, value) in mounts {
                            let config = crate::settings::config_name(config)?;
                            let path = SettingPath::at(service, Target::ConfigMount(config.into()));
                            expanded.push((path, Step::Set(value.clone()), None));
                        }
                        continue;
                    }
                    let setting = ServiceSetting::parse(name)?;
                    let path = SettingPath::of(service, setting);
                    expanded.push((path, Step::Set(value.clone()), None));
                }
            }
        }
    }
    Ok(expanded)
}
