//! Tools on one Service: read it, create it, change its Settings, rename it and
//! remove it.

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::domain::row;
use super::environment::{REPOSITORY, source};
use super::machine::{Cx, Effect, Stop, Tool, at, written};
use super::refusal::{Call, Refusal, Writes, from_store, not_setting, not_setting_sentence};
use super::schema;
use super::step::{DashboardPage, Fix};
use crate::catalog::{NODE_NAME, VARIABLE_KEY};
use crate::{
    BatchCommand, BranchName, Change, CreateGitService, CreateService, DomainsQuery, Edit,
    EnvironmentName, EnvironmentRef, ProjectName, RemoveService, RenameService, RepositoryName,
    ServiceQuery, ServicesQuery, SettingPath, Written,
};
use ployz_core::config::ReviewLifecycleKind;
use ployz_core::{RpcError, RpcErrorCode, ServiceName};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServiceInspect {
    #[schemars(with = "String", pattern(NODE_NAME))]
    service: ServiceName,
    /// Project name.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for ServiceInspect {
    const NAME: &'static str = "service_inspect";
    const DESCRIPTION: &'static str = "Everything about one Service: Settings with values and defaults, variables, mounts, Configs, domains with status, and what is Staged.";
    const EFFECT: Effect = Effect::Read;

    fn example() -> Value {
        json!({ "service": "web" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let environment = cx.scope(self.project.as_ref(), self.environment.as_ref());
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
            "at": at(&view.environment),
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

/// A Setting path without its Service: `env.KEY` for `web.env.KEY`.
fn own_path(service: &str, path: &str) -> String {
    path.strip_prefix(service)
        .and_then(|rest| rest.strip_prefix('.'))
        .unwrap_or(path)
        .to_owned()
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServiceAdd {
    /// New Service name, unique in the Environment.
    #[schemars(with = "String", pattern(NODE_NAME), example = &"whoami")]
    name: ServiceName,
    /// Container image to run. Give image or repository.
    #[serde(default)]
    #[schemars(length(min = 1), example = &"traefik/whoami")]
    image: Option<String>,
    /// GitHub repository owner/name to build. Give image or repository.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(REPOSITORY), example = &"acme/api")]
    repository: Option<RepositoryName>,
    /// Branch to build, only with repository. Default: the repository's default branch.
    #[serde(default)]
    #[schemars(with = "Option<String>", length(min = 1))]
    branch: Option<BranchName>,
    /// Writes only if the place is still at this revision.
    #[serde(default)]
    expect_revision: Option<u64>,
    /// Project name.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for ServiceAdd {
    const NAME: &'static str = "service_add";
    const DESCRIPTION: &'static str = "Create a Service. Give image to run a container image, or repository to build a GitHub repository. Staged: it runs after the next Deploy.";
    const EFFECT: Effect = Effect::Change;

    fn example() -> Value {
        json!({ "name": "whoami", "image": "traefik/whoami" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
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
            "at": at(&staged.environment),
            "service": staged.service.name,
            "source": source,
            "private_dns": staged.service.private_dns,
            "staged": true,
        }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServiceUpdate {
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
    /// With rename_env, also switch the variables that reference a renamed one to its new name.
    #[serde(default)]
    update_references: bool,
    #[serde(default)]
    #[schemars(schema_with = "generate_secret_schema")]
    generate_secret: Vec<String>,
    /// Writes only if the place is still at this revision.
    #[serde(default)]
    expect_revision: Option<u64>,
    /// Project name.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

/// The Service Settings, without the credential an agent cannot set.
fn set_schema(_: &mut SchemaGenerator) -> Schema {
    let mut set = schema::portable(crate::catalog::service(None));
    if let Some(object) = set.as_object_mut() {
        object.remove("title");
        object.insert(
            "description".into(),
            "A Service's Settings, in the shape a Service's inspected values take.".into(),
        );
        if let Some(Value::Object(properties)) = object.get_mut("properties") {
            properties.remove("registryCredential");
        }
    }
    Schema::try_from(set).unwrap_or_default()
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
        "description": "Settings to return to default, variables to delete (env.KEY), mounts to remove (mounts.VOLUME, configs.CONFIG).",
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
        "description": "Variables to rename, old name to new name. The value moves with it; a secret stays sealed.",
        "patternProperties": { VARIABLE_KEY: { "type": "string", "pattern": VARIABLE_KEY } },
        "examples": [{ "API_URL": "PUBLIC_API_URL" }],
    }))
    .unwrap_or_default()
}

fn generate_secret_schema(_: &mut SchemaGenerator) -> Schema {
    let variable = VARIABLE_KEY.trim_start_matches('^');
    Schema::try_from(json!({
        "type": "array",
        "description": "Variables to give a new random sealed secret, as env.KEY. No one sees its value.",
        "items": { "type": "string", "pattern": format!("^env\\.{variable}") },
    }))
    .unwrap_or_default()
}

impl Tool for ServiceUpdate {
    const NAME: &'static str = "service_update";
    const DESCRIPTION: &'static str = "Change one Service's Settings and variables in one step. set changes the Settings it names and keeps the rest; env inside set sets the variables it names. Domains are not Settings and are not changed here. Most changes are Staged until the next deploy and can be undone before it. template, autoDeploy, waitForCi, watchPaths and preferredBuilder take effect at once and cannot be undone that way; the result lists them under immediate. A secret's value never passes through here; generate_secret makes a random one.";
    const EFFECT: Effect = Effect::Change;

    fn example() -> Value {
        json!({ "service": "web", "set": { "env": { "LOG_LEVEL": "info" } }, "unset": ["env.OLD_FLAG"] })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        if let Some(path) = self.set.as_ref().and_then(secret_value) {
            return Err(self.plaintext_secret(cx, scope, &path));
        }
        let changes = self.changes(cx.call())?;
        let mut sealed = None;
        let edited = cx.write_expecting(
            &Edit {
                environment: scope.clone(),
                expect: self.expect_revision.map(crate::Revision),
                changes,
            },
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
            "at": at(&edited.environment),
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
                        .with_next(Call::new(tool, drop_nulls(next)))
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
                value: json!({ "secret": { "generate": true } }),
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
    let sealing = |value: &Value| {
        value.get("secret").is_some_and(Value::is_string)
            || value
                .get("value")
                .and_then(|value| value.get("secret"))
                .is_some_and(Value::is_string)
    };
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
    if let Value::Object(details) = &mut error.details {
        details.remove("next");
    }
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

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServiceRename {
    #[schemars(with = "String", pattern(NODE_NAME))]
    service: ServiceName,
    /// New name.
    #[schemars(with = "String", pattern(NODE_NAME))]
    to: ServiceName,
    /// Writes only if the place is still at this revision.
    #[serde(default)]
    expect_revision: Option<u64>,
    /// Project name.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for ServiceRename {
    const NAME: &'static str = "service_rename";
    const DESCRIPTION: &'static str =
        "Rename a Service. Private DNS names and references to it follow. Staged.";
    const EFFECT: Effect = Effect::Change;

    fn example() -> Value {
        json!({ "service": "web", "to": "frontend" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        cx.expect(&scope, self.expect_revision)?;
        let staged = cx.write(&RenameService {
            environment: scope,
            service: self.service.clone(),
            name: self.to,
        })?;
        Ok(json!({
            "at": at(&staged.environment),
            "service": staged.service.name,
            "renamed_from": self.service,
            "staged": true,
        }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServiceRemove {
    #[schemars(with = "String", pattern(NODE_NAME))]
    service: ServiceName,
    /// Writes only if the place is still at this revision.
    #[serde(default)]
    expect_revision: Option<u64>,
    /// Project name.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for ServiceRemove {
    const NAME: &'static str = "service_remove";
    const DESCRIPTION: &'static str = "Remove a Service, staged until a deploy a human approves. `broken`: references deploys skip.";
    const EFFECT: Effect = Effect::Change;

    fn example() -> Value {
        json!({ "service": "web" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let services = cx.one(&ServicesQuery {
            environment: scope.clone(),
        })?;
        if self
            .expect_revision
            .is_some_and(|expect| services.environment.revision.0 != expect)
        {
            return Err(super::machine::stale(cx.call()));
        }
        // A Service never deployed goes without asking anyone.
        let asks_approval = services
            .services
            .iter()
            .find(|listing| listing.service.name == self.service)
            .is_some_and(|listing| listing.change != Some(ReviewLifecycleKind::Create));
        let staged = cx.write(&RemoveService {
            environment: scope,
            service: self.service,
        })?;
        Ok(json!({
            "at": at(&staged.environment),
            "removed": staged.service.name,
            "asks_approval": asks_approval,
            "broken": staged.broken.iter().map(ToString::to_string).collect::<Vec<_>>(),
        }))
    }
}
