//! Tools on one Service: read it, create it, change its Settings, rename it and
//! remove it.

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::domain::row;
use super::environment::{REPOSITORY, source};
use super::machine::{Cx, Stop, Tool, at, written};
use super::refusal::{Call, Refusal, Writes, from_store, not_setting, not_setting_sentence};
use super::schema;
use crate::catalog::{NODE_NAME, VARIABLE_KEY};
use crate::{
    BatchCommand, BranchName, Change, CreateGitService, CreateService, DomainsQuery, Edit,
    EnvironmentName, ProjectName, RemoveService, RenameService, RepositoryName, ServiceQuery,
    ServicesQuery, SettingPath, Written,
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
    const DESCRIPTION: &'static str = "Everything about one Service: Settings with values and defaults, variables, mounts, Configs, domains with status, and what is Staged. Use it before changing a Service.";
    const READ_ONLY: bool = true;

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
            .map(|change| own_path(name, &change.path))
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
    /// Write only if the place is still at this revision. Omit unless you just read it.
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
    const DESCRIPTION: &'static str = "Create a Service. Give image to run a container image, or repository to build a GitHub repository. Staged: it runs after the next Deploy. Add domains with domain_add and Settings with service_update.";
    const READ_ONLY: bool = false;

    fn example() -> Value {
        json!({ "name": "whoami", "image": "traefik/whoami" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let staged = match (self.image, self.repository) {
            (Some(_), Some(_)) | (None, None) => {
                return Err(Refusal::invalid("Give image or repository, not both", "image").into());
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
    /// Write only if the place is still at this revision. Omit unless you just read it.
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
            "A Service's Settings, in the shape service_inspect returns as values.".into(),
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

impl Tool for ServiceUpdate {
    const NAME: &'static str = "service_update";
    const DESCRIPTION: &'static str = "Change one Service's Settings and variables in one step. set changes the Settings it names and keeps the rest; env inside set sets the variables it names. unset returns Settings to their default, deletes variables (env.KEY), or unmounts (mounts.VOLUME, configs.CONFIG). rename_env renames variables and keeps their values, secret or not. Domains are not Settings: use domain_add, domain_update, domain_remove. Most changes are Staged: Review lists them, Discard undoes them, so do not ask the member to confirm. template, autoDeploy, waitForCi, watchPaths and preferredBuilder take effect at once and Discard does not undo them; the result lists them under immediate. Secrets and registry credentials cannot be set here: the member sets them in the dashboard or terminal.";
    const READ_ONLY: bool = false;

    fn example() -> Value {
        json!({ "service": "web", "set": { "env": { "LOG_LEVEL": "info" } }, "unset": ["env.OLD_FLAG"] })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let changes = self.changes(cx.call())?;
        let edited = cx.write_expecting(
            &Edit {
                environment: scope,
                expect: self.expect_revision.map(crate::Revision),
                changes,
            },
            self.expect_revision,
            refuse_referenced_rename,
        )?;
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
                return Err(Refusal::invalid(
                    "A new variable name is text",
                    &format!("rename_env.{from}"),
                )
                .into());
            };
            changes.push(Change::Rename {
                path: path(&format!("rename_env.{from}"), &format!("env.{from}"))?,
                to: to.to_owned(),
                update_references: self.update_references,
            });
        }
        if changes.is_empty() {
            return Err(Refusal::invalid("Give set, unset or rename_env", "set").into());
        }
        Ok(changes)
    }
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
    /// Write only if the place is still at this revision. Omit unless you just read it.
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
    const READ_ONLY: bool = false;

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
    /// Write only if the place is still at this revision. Omit unless you just read it.
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
    const DESCRIPTION: &'static str = "Remove a Service. Staged: Review lists it and Discard undoes it; the next Publish or Deploy asks a human to approve.";
    const READ_ONLY: bool = false;

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
        }))
    }
}
