//! Tools on one Service: read it, create it, change its Settings, rename it and
//! remove it.

use schemars::{Schema, SchemaGenerator};
use serde_json::{Map, Value, json};

use super::ToolId;
use super::domain::row;
use super::input::tool_input;
use super::machine::{Cx, Effect, Stop, Tool, effects_ask, written};
use super::refusal::{Call, Refusal, Writes, from_store, not_setting, not_setting_sentence};
use super::schema::{self, REPOSITORY};
use super::step::{DashboardPage, Fix};
use crate::catalog::{NODE_NAME, VARIABLE_KEY};
use crate::followup::FollowUp;
use crate::{
    BatchCommand, BranchName, Change, CreateGitService, CreateService, DestructiveKind,
    DomainsQuery, Edit, EnvironmentRef, RemoveService, RenameService, RepositoryName, SecretInput,
    ServiceQuery, ServicesQuery, SettingPath, SourceKind, Written,
};
use ployz_core::{RpcError, RpcErrorCode, ServiceName};

tool_input! {
    pub(crate) struct ServiceInspect {
        /// The Service to show.
        #[schemars(with = "String", pattern(NODE_NAME))]
        service: ServiceName,
        @place(named),
    }
}

impl Tool for ServiceInspect {
    const NAME: &'static str = "service_inspect";
    const STATUS: ToolId = ToolId::ServiceState;
    const TITLE: &'static str = "Show one Service";
    const DESCRIPTION: &'static str = "Show one Service as saved: its source, Private DNS name, Settings, variables, mounts, public domains with any DNS records still to create, what is staged for the next deploy, and the Environment's revision. Secret values are never shown. To see if it is running, use `service_status`.";
    const EFFECT: Effect = Effect::Read;

    fn example() -> Value {
        json!({ "service": "web" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let environment = cx.scope(&self);
        let mut answers = cx
            .read(vec![
                super::machine::store(&ServiceQuery {
                    environment: environment.clone(),
                    service: self.service.clone(),
                })?,
                super::machine::store(&DomainsQuery {
                    environment,
                    service: Some(self.service),
                })?,
            ])?
            .into_iter();
        let view = cx.view::<ServiceQuery>(answers.next())?;
        let domains = cx.view::<DomainsQuery>(answers.next())?;
        let name = view.service.service.name.as_str();
        let mut settings = view.values;
        let mut take = |key: &str| settings.remove(key).unwrap_or_else(|| json!({}));
        let (env, mounts, configs) = (take("env"), take("mounts"), take("configs"));
        let staged = view
            .changes
            .iter()
            .map(|change| own_path(name, &change.shown_path(&view.changes)))
            .collect::<Vec<_>>();
        Ok(json!({
            "at": view.environment.to_string(),
            "revision": view.environment.revision.0,
            "service": name,
            "source": source(view.service.source),
            "private_dns": view.service.service.private_dns,
            "settings": settings,
            "env": env,
            "mounts": mounts,
            "configs": configs,
            "domains": domains.domains.iter().map(row).collect::<Vec<_>>(),
            "staged": staged,
        }))
    }
}

/// How a Service's source reads to an agent.
pub(crate) const fn source(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Empty => "empty",
        SourceKind::Uploaded => "uploaded",
        SourceKind::Git => "repository",
        SourceKind::Image => "image",
    }
}

/// A Setting path without its Service: `env.KEY` for `web.env.KEY`.
fn own_path(service: &str, path: &str) -> String {
    path.strip_prefix(service)
        .and_then(|rest| rest.strip_prefix('.'))
        .unwrap_or(path)
        .to_owned()
}

tool_input! {
    pub(crate) struct ServiceAdd {
        /// Name for the new Service, unique in the Environment. Its Private DNS name starts out the same.
        #[schemars(with = "String", pattern(NODE_NAME), example = &"whoami")]
        name: ServiceName,
        /// Image to run, e.g. `redis` or `ghcr.io/org/app:latest`. Give this or repository, not both.
        #[serde(default)]
        #[schemars(length(min = 1), example = &"traefik/whoami")]
        image: Option<String>,
        /// GitHub repository to build, e.g. `acme/api`. The Organization must be able to read it.
        #[serde(default)]
        #[schemars(with = "Option<String>", pattern(REPOSITORY), example = &"acme/api")]
        repository: Option<RepositoryName>,
        /// Branch to build, with repository only (default: the repository's default branch).
        #[serde(default)]
        #[schemars(with = "Option<String>", length(min = 1))]
        branch: Option<BranchName>,
        @revision,
        @place(named),
    }
}

impl Tool for ServiceAdd {
    const NAME: &'static str = "service_add";
    const STATUS: ToolId = ToolId::ServiceState;
    const TITLE: &'static str = "Create a Service";
    const DESCRIPTION: &'static str = "Create a Service from a container image or a GitHub repository. Nothing runs yet: it is staged and starts on the next deploy. Fails if the name is taken.";
    // The same input again finds the name taken and is refused.
    const EFFECT: Effect = Effect::Change { idempotent: true };
    // On Cloud, a repository is looked up on GitHub, and a branch checked there
    // (dashboard/src/modules/config-store/git-evidence.server.ts, gatherGitEvidence).
    const OPEN_WORLD: bool = true;

    fn example() -> Value {
        json!({ "name": "whoami", "image": "traefik/whoami" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(&self);
        let staged = match (self.image, self.repository) {
            (None, None) => {
                return Err(Refusal::invalid("Give an image or a repository", "image").into());
            }
            (Some(_), Some(_)) => {
                return Err(Refusal::invalid(
                    "Give an image or a repository, not both",
                    "repository",
                )
                .into());
            }
            (Some(image), None) => {
                if self.branch.is_some() {
                    return Err(
                        Refusal::invalid("A branch goes only with repository", "branch").into(),
                    );
                }
                let create = CreateService {
                    id: cx.mint(None)?,
                    environment: scope.clone(),
                    name: self.name,
                    image: Some(image.clone()),
                    template: None,
                };
                let results = cx.batch(
                    &scope,
                    self.expect_revision,
                    vec![BatchCommand::CreateService(create)],
                )?;
                let staged = written(results, |one| {
                    if let Written::Service(staged) = one {
                        Some(staged)
                    } else {
                        None
                    }
                })?;
                (staged, json!({ "image": image }))
            }
            (None, Some(repository)) => {
                cx.expect(&scope, self.expect_revision)?;
                let create = CreateGitService {
                    id: cx.mint(None)?,
                    environment: scope,
                    name: self.name,
                    repository: repository.clone(),
                    branch: self.branch.clone(),
                };
                let staged = cx.write(&create)?;
                let source = json!({ "repository": repository, "branch": self.branch });
                (staged, source)
            }
        };
        let (staged, source) = staged;
        Ok(json!({
            "at": staged.environment.to_string(),
            "service": staged.service.name,
            "source": source,
            "private_dns": staged.service.private_dns,
            "staged": true,
        }))
    }
}

tool_input! {
    pub(crate) struct ServiceUpdate {
        /// The Service to change.
        #[schemars(with = "String", pattern(NODE_NAME), example = &"web")]
        service: ServiceName,
        #[serde(default)]
        #[schemars(schema_with = "set_schema")]
        set: Option<Map<String, Value>>,
        #[serde(default)]
        #[schemars(schema_with = "unset_schema")]
        unset: Vec<String>,
        #[serde(default)]
        #[schemars(schema_with = "rename_schema")]
        rename_env: Map<String, Value>,
        /// With rename_env, also updates the variables that reference a renamed one. Without it, renaming a referenced variable fails.
        #[serde(default)]
        update_references: bool,
        #[serde(default)]
        #[schemars(schema_with = "generate_secret_schema")]
        generate_secret: Vec<String>,
        @revision,
        @place(named),
    }
}

/// The Service Settings, without the credential an agent cannot set.
fn set_schema(_: &mut SchemaGenerator) -> Schema {
    let mut set = schema::portable(crate::catalog::service(None));
    if let Some(object) = set.as_object_mut() {
        object.remove("title");
        object.insert(
            "description".into(),
            "Settings to change, in the shape they are shown in, e.g. `{\"replicas\": 2}`. Anything left out stays as it is.".into(),
        );
        if let Some(Value::Object(properties)) = object.get_mut("properties") {
            properties.remove("registryCredential");
            for (setting, texts) in NESTED {
                if let Some(nested) = properties.get_mut(*setting) {
                    describe(nested, texts);
                }
            }
        }
    }
    Schema::try_from(set).unwrap_or_default()
}

/// What the fields inside a Setting's value mean, where the settings catalog
/// describes only the Setting as a whole.
const NESTED: &[(&str, &[(&str, &str)])] = &[
    (
        "env",
        &[
            (
                "secret",
                "`true` keeps the secret the variable already holds. Fails if it holds none.",
            ),
            ("value", "The value. Leave it out to keep the current one."),
            (
                "exported",
                "`true` marks a variable other Services are meant to reference.",
            ),
        ],
    ),
    (
        "healthcheck",
        &[
            ("path", "HTTP path the check requests, e.g. `/health`."),
            (
                "command",
                "Shell command the check runs, e.g. `pg_isready`.",
            ),
            ("timeoutSeconds", "How many seconds the check may take."),
        ],
    ),
    (
        "template",
        &[
            ("id", "The Service Template's ID, e.g. `postgres`."),
            ("version", "The version of it the Service came from."),
        ],
    ),
];

/// Give each property of `schema` that `texts` names its description, at any
/// depth, keeping one it already has.
fn describe(schema: &mut Value, texts: &[(&str, &str)]) {
    match schema {
        Value::Object(object) => {
            if let Some(Value::Object(properties)) = object.get_mut("properties") {
                for (name, text) in texts {
                    if let Some(Value::Object(property)) = properties.get_mut(*name) {
                        property
                            .entry("description")
                            .or_insert_with(|| Value::from(*text));
                    }
                }
            }
            object.values_mut().for_each(|value| describe(value, texts));
        }
        Value::Array(items) => items.iter_mut().for_each(|item| describe(item, texts)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn unset_schema(_: &mut SchemaGenerator) -> Schema {
    let service = crate::catalog::service(None);
    let mut settings = service
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| {
            properties
                .keys()
                .filter(|key| {
                    !matches!(
                        key.as_str(),
                        "env" | "mounts" | "configs" | "registryCredential"
                    )
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    settings.sort();
    let variable = VARIABLE_KEY.trim_start_matches('^');
    let node = NODE_NAME.trim_start_matches('^');
    Schema::try_from(json!({
        "type": "array",
        "uniqueItems": true,
        "description": "Settings to reset to default, variables to delete (`env.KEY`) and mounts to remove (`mounts.VOLUME`, `configs.CONFIG`).",
        "items": { "anyOf": [
            { "type": "string", "enum": settings },
            { "type": "string", "pattern": format!("^env\\.{variable}") },
            { "type": "string", "pattern": format!("^(mounts|configs)\\.{node}") },
        ]},
        "examples": [["cpuLimit", "env.OLD_FLAG"]],
    }))
    .unwrap_or_default()
}

fn rename_schema(_: &mut SchemaGenerator) -> Schema {
    Schema::try_from(json!({
        "type": "object",
        "additionalProperties": false,
        "description": "Variables to rename, as old name to new name. The value moves with the name, and a secret stays sealed.",
        "patternProperties": { VARIABLE_KEY: {
            "type": "string",
            "pattern": VARIABLE_KEY,
            "description": "The variable's new name.",
        } },
        "examples": [{ "API_URL": "PUBLIC_API_URL" }],
    }))
    .unwrap_or_default()
}

fn generate_secret_schema(_: &mut SchemaGenerator) -> Schema {
    let variable = VARIABLE_KEY.trim_start_matches('^');
    Schema::try_from(json!({
        "type": "array",
        "description": "Variables to fill with a random sealed secret, e.g. `env.SESSION_SECRET`. Nobody sees the value. One that already holds a sealed secret keeps it.",
        "items": { "type": "string", "pattern": format!("^env\\.{variable}") },
    }))
    .unwrap_or_default()
}

impl Tool for ServiceUpdate {
    const NAME: &'static str = "service_update";
    const STATUS: ToolId = ToolId::ServiceState;
    const TITLE: &'static str = "Change a Service's Settings";
    const DESCRIPTION: &'static str = "Change a Service's Settings, variables and mounts. One call applies as a whole or not at all. Most changes are staged until the next deploy; template, autoDeploy, waitForCi, watchPaths and preferredBuilder apply right away and come back under `immediate`. A secret's value can't be sent here, but generate_secret makes a random one. For domains, use `domain_add`.";
    // Values set again change nothing, a generated secret is kept once sealed,
    // and a variable rename already done is left as it is.
    const EFFECT: Effect = Effect::Change { idempotent: true };
    // On Cloud, setting repository or branch looks them up on GitHub
    // (dashboard/src/modules/config-store/git-evidence.server.ts, gatherGitEvidence).
    const OPEN_WORLD: bool = true;

    fn example() -> Value {
        json!({ "service": "web", "set": { "env": { "LOG_LEVEL": "info" } }, "unset": ["env.OLD_FLAG"] })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(&self);
        if let Some(path) = self.set.as_ref().and_then(secret_value) {
            return Err(self.plaintext_secret(cx, scope, &path));
        }
        let edit = Edit {
            environment: scope.clone(),
            expect: self.expect_revision.map(crate::Revision),
            changes: self.changes(cx.call())?,
        };
        super::environment::refuse_unknown_regions(cx, &edit.regions())?;
        let mut sealed = None;
        let edited =
            cx.write_expecting(
                &edit,
                self.expect_revision,
                |error, call, writes| match self.sealed_key(&error) {
                    Some(key) => {
                        sealed = Some(key);
                        from_store(error, call, writes)
                    }
                    None => refuse_referenced_rename(error, call, writes),
                },
            );
        let edited = match (edited, sealed) {
            (Err(_), Some(key)) => {
                return Err(self.plaintext_secret(cx, scope, &format!("env.{key}")));
            }
            (edited, _) => edited?,
        };
        let name = self.service.as_str();
        let paths = |paths: &[SettingPath]| {
            paths
                .iter()
                .map(|path| own_path(name, &path.to_string()))
                .collect::<Vec<_>>()
        };
        let staged = paths(&edited.staged);
        let immediate = paths(&edited.immediate);
        let changed = staged.iter().chain(&immediate).cloned().collect::<Vec<_>>();
        Ok(json!({
            "at": edited.environment.to_string(),
            "service": name,
            "changed": changed,
            "immediate": immediate,
            "staged": !staged.is_empty(),
        }))
    }
}

impl ServiceUpdate {
    /// The edit's changes, refusing a key that lives in another tool.
    fn changes(&self, call: &Call) -> Result<Vec<Change>, Stop> {
        let service = self.service.as_str();
        let path = |field: &str, rest: &str| {
            let text = match rest.is_empty() {
                true => service.to_owned(),
                false => format!("{service}.{rest}"),
            };
            SettingPath::parse(&text)
                .map_err(|_| Stop::from(Refusal::invalid("That is not a Setting path", field)))
        };
        let mut changes = Vec::new();
        if let Some(set) = self.set.as_ref().filter(|set| !set.is_empty()) {
            if let Some((key, tool)) = set.keys().find_map(|key| Some((key, not_setting(key)?))) {
                let next = json!({
                    "service": service,
                    "project": call.input.get("project"),
                    "environment": call.input.get("environment"),
                });
                return Err(
                    Refusal::invalid(not_setting_sentence(tool), &format!("set.{key}"))
                        .with_next(Call::to(tool, drop_nulls(next)))
                        .into(),
                );
            }
            changes.push(Change::Patch {
                path: path("set", "")?,
                value: Value::Object(set.clone()),
            });
        }
        for (index, unset) in self.unset.iter().enumerate() {
            changes.push(Change::Unset {
                path: path(&format!("unset.{index}"), unset)?,
            });
        }
        for (from, to) in &self.rename_env {
            let Some(to) = to.as_str() else {
                return Err(Refusal::invalid("A new variable name is text", "rename_env").into());
            };
            changes.push(Change::Rename {
                path: path("rename_env", &format!("env.{from}"))?,
                to: to.to_owned(),
                update_references: self.update_references,
            });
        }
        let given = self.set.as_ref().and_then(|set| set.get("env"));
        for (index, generate) in self.generate_secret.iter().enumerate() {
            let field = format!("generate_secret.{index}");
            let Some(key) = generate.strip_prefix("env.") else {
                return Err(Refusal::invalid(
                    "A secret is generated for a variable, env.KEY",
                    &field,
                )
                .into());
            };
            if given.and_then(|env| env.get(key)).is_some() || self.unset.contains(generate) {
                return Err(Refusal::invalid(
                    format!("{generate} is also in set or unset; give each variable once"),
                    &field,
                )
                .into());
            }
            changes.push(Change::Set {
                path: path(&field, generate)?,
                value: SecretInput::Generate.to_value(),
            });
        }
        if changes.is_empty() {
            return Err(
                Refusal::invalid("Give set, unset, rename_env or generate_secret", "set").into(),
            );
        }
        Ok(changes)
    }

    /// The variable the Store refused text for because it holds a secret.
    fn sealed_key(&self, error: &RpcError) -> Option<String> {
        let key = match error.code == RpcErrorCode::Conflict {
            true => error.details.get("variable").and_then(Value::as_str)?,
            false => return None,
        };
        let given = self.set.as_ref().and_then(|set| set.get("env"));
        let renamed_onto = self.rename_env.values().any(|to| to.as_str() == Some(key));
        (given.and_then(|env| env.get(key)).is_some() && !renamed_onto).then(|| key.to_owned())
    }

    /// A secret's value sent by an agent: the member sets it, and this names how, in the
    /// Environment the call resolved to even when it named none.
    fn plaintext_secret(&self, cx: &mut Cx<'_>, scope: EnvironmentRef, path: &str) -> Stop {
        let view = match cx.one(&ServicesQuery { environment: scope }) {
            Ok(view) => view,
            Err(stop) => return stop,
        };
        let service = self.service.as_str();
        let (project, environment) = (&view.environment.project, &view.environment.name);
        let command = format!(
            "printf %s \"$VALUE\" | ployz set {service}.{path} --secret --env {environment} --project {project}"
        );
        let random = match path.starts_with("env.") {
            true => format!(" For a random one, send generate_secret: [\"{path}\"] instead."),
            false => String::new(),
        };
        Refusal {
            secret_sent: true,
            ..Refusal::invalid(
                format!(
                    "{service}.{path} is a secret, and its value never passes through an agent. The member sets it in the dashboard under Variables, or runs: {command}.{random}"
                ),
                &format!("set.{path}"),
            )
        }
        .with_fix(&Fix::OpenDashboard {
            page: DashboardPage::Variables,
        })
        .into()
    }
}

/// The Setting path, such as `env.KEY`, of the first secret value `set` gives.
fn secret_value(set: &Map<String, Value>) -> Option<String> {
    let seals = |value: &Value| {
        SecretInput::in_object(value)
            .as_ref()
            .is_some_and(SecretInput::seals)
    };
    let sealing = |value: &Value| seals(value) || value.get("value").is_some_and(seals);
    if set.get("registryCredential").is_some_and(sealing) {
        return Some("registryCredential".to_owned());
    }
    set.get("env")?
        .as_object()?
        .iter()
        .find(|(_, value)| sealing(value))
        .map(|(key, _)| format!("env.{key}"))
}

/// A rename the variables referencing it block, retried with `update_references`.
fn refuse_referenced_rename(mut error: RpcError, call: &Call, writes: Writes) -> Refusal {
    let referrers = match error.code == RpcErrorCode::Conflict {
        true => error.details.get("variables").cloned(),
        false => None,
    };
    let Some(referrers) = referrers else {
        return from_store(error, call, writes);
    };
    // The Store's next is the CLI form of the retry below.
    FollowUp::remove_from(&mut error.details);
    let mut retry = call.input.clone();
    if let Value::Object(fields) = &mut retry {
        fields.insert("update_references".into(), true.into());
    }
    from_store(error, call, writes)
        .with_detail("variables", referrers)
        .with_retry(Call::new(&call.tool, retry))
}

/// `value` with its top-level null fields removed.
pub(crate) fn drop_nulls(mut value: Value) -> Value {
    if let Value::Object(fields) = &mut value {
        fields.retain(|_, field| !field.is_null());
    }
    value
}

tool_input! {
    pub(crate) struct ServiceRename {
        /// The Service's current name.
        #[schemars(with = "String", pattern(NODE_NAME))]
        service: ServiceName,
        /// Its new name, unique in the Environment.
        #[schemars(with = "String", pattern(NODE_NAME))]
        to: ServiceName,
        @revision,
        @place(named),
    }
}

impl Tool for ServiceRename {
    const NAME: &'static str = "service_rename";
    const STATUS: ToolId = ToolId::ServiceState;
    const TITLE: &'static str = "Rename a Service";
    const DESCRIPTION: &'static str = "Rename a Service, staged until the next deploy. References to it follow the new name, but its Private DNS name stays the same. Fails if the name is taken.";
    // Once renamed, the same call finds no Service by the old name and is refused.
    const EFFECT: Effect = Effect::Change { idempotent: true };

    fn example() -> Value {
        json!({ "service": "web", "to": "frontend" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(&self);
        cx.expect(&scope, self.expect_revision)?;
        let staged = cx.write(&RenameService {
            environment: scope,
            service: self.service.clone(),
            name: self.to,
        })?;
        Ok(json!({
            "at": staged.environment.to_string(),
            "service": staged.service.name,
            "renamed_from": self.service,
            "staged": true,
        }))
    }
}

tool_input! {
    pub(crate) struct ServiceRemove {
        /// The Service to remove.
        #[schemars(with = "String", pattern(NODE_NAME))]
        service: ServiceName,
        @revision,
        @place(named),
    }
}

impl Tool for ServiceRemove {
    const NAME: &'static str = "service_remove";
    const STATUS: ToolId = ToolId::ServiceState;
    const TITLE: &'static str = "Remove a Service";
    const DESCRIPTION: &'static str = "Remove a Service, staged until the next deploy. That deploy asks a human to approve unless the Service has never deployed. Returns whether approval will be asked and the references this breaks; the deploy leaves out the variables that hold them.";
    // Once removed, the same call finds no Service by that name and is refused.
    const EFFECT: Effect = Effect::Change { idempotent: true };

    fn example() -> Value {
        json!({ "service": "web" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(&self);
        let services = cx.one(&ServicesQuery {
            environment: scope.clone(),
        })?;
        cx.fresh(&services.environment, self.expect_revision)?;
        let staged = cx.write(&RemoveService {
            environment: scope.clone(),
            service: self.service,
        })?;
        // A Service never deployed goes without asking anyone.
        let effects = cx.effects_after_write(scope)?;
        let asks_approval = effects_ask(
            &effects,
            &[
                DestructiveKind::RemovesService,
                DestructiveKind::StopsService,
            ],
            &staged.service.id.to_string(),
        );
        Ok(json!({
            "at": staged.environment.to_string(),
            "removed": staged.service.name,
            "asks_approval": asks_approval,
            "broken": staged.broken.iter().map(ToString::to_string).collect::<Vec<_>>(),
        }))
    }
}
