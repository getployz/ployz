//! Tools that remove an Environment or a Project: one Store `remove` each.

use std::collections::BTreeMap;

use ployz_core::{RpcError, RpcErrorCode};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::act::{server_show, servers};
use super::cloud::ServersAnswer;
use super::machine::{Cx, Effect, Stop, Tool, store};
use super::refusal::{Call, Refusal, Writes, from_store};
use super::step::{AcceptedLoss, DashboardPage, Fix, Gate, ServerName};
use crate::catalog::NODE_NAME;
use crate::{
    EnvironmentName, EnvironmentsQuery, Left, ProjectName, Removal, Remove, RemovedQuery,
    VolumeName, Waiting,
};

/// Volumes by Environment, as `remove` takes them.
type Volumes = BTreeMap<EnvironmentName, Vec<VolumeName>>;

/// What is removed, and how the tool's Volume lists name a Volume.
#[derive(Clone, Copy)]
enum Target {
    /// One Environment: a Volume by its name.
    Environment,
    /// A Project: a Volume as `environment/volume`.
    Project,
}

impl Target {
    const fn tool(self) -> &'static str {
        match self {
            Self::Environment => EnvironmentRemove::NAME,
            Self::Project => ProjectRemove::NAME,
        }
    }

    /// The tool's Volume list for `volumes`.
    fn list(self, volumes: &Volumes) -> Vec<String> {
        volumes
            .iter()
            .flat_map(|(environment, names)| {
                names.iter().map(move |volume| match self {
                    Self::Environment => volume.to_string(),
                    Self::Project => format!("{environment}/{volume}"),
                })
            })
            .collect()
    }
}

/// `environment/volume` lines as `remove` takes them.
fn by_environment(lines: &[String], field: &str) -> Result<Volumes, Stop> {
    let mut volumes = Volumes::new();
    for line in lines {
        let parsed = line.split_once('/').and_then(|(environment, volume)| {
            Some((
                EnvironmentName::parse(environment).ok()?,
                VolumeName::parse(volume).ok()?,
            ))
        });
        let Some((environment, volume)) = parsed else {
            return Err(Refusal::invalid("Name a Volume as environment/volume", field).into());
        };
        volumes.entry(environment).or_default().push(volume);
    }
    Ok(volumes)
}

/// The Volume map a Store refusal carries at `key`, as the tool's list.
fn listed(target: Target, details: &Value, key: &str) -> Value {
    let volumes = details
        .get(key)
        .cloned()
        .and_then(|volumes| serde_json::from_value::<Volumes>(volumes).ok())
        .unwrap_or_default();
    json!(target.list(&volumes))
}

/// The Store's fix for a waiting Server, named as the tools name it.
fn fix(fix: &crate::Fix, names: &BTreeMap<String, ServerName>) -> Option<Fix> {
    Some(match fix {
        crate::Fix::UseCloud => Fix::UseCloud,
        crate::Fix::ConnectServers => Fix::ConnectServers,
        crate::Fix::UpdateClient => Fix::UpdateClient,
        crate::Fix::OpenDashboard { page } => Fix::OpenDashboard {
            page: serde_json::from_value::<DashboardPage>(json!(page)).ok()?,
        },
        crate::Fix::ServerRemove { server } => Fix::ServerRemove {
            server: names.get(server)?.clone(),
        },
    })
}

fn waiting(waiting: &[Waiting], names: &BTreeMap<String, ServerName>) -> Vec<Value> {
    waiting
        .iter()
        .map(|waiting| {
            let server = names
                .get(&waiting.server)
                .map_or(waiting.server.as_str(), ServerName::as_str);
            let mut shown = json!({ "server": server, "why": waiting.why });
            if let (Some(fix), Value::Object(shown)) = (fix(&waiting.fix, names), &mut shown) {
                shown.insert("fix".into(), json!(fix));
            }
            shown
        })
        .collect()
}

/// The Servers `waiting` names by id, by their names. Only Cloud knows them.
fn names(cx: &mut Cx<'_>, waiting: &[Waiting]) -> Result<BTreeMap<String, ServerName>, Stop> {
    if waiting.is_empty() || cx.is_local() {
        return Ok(BTreeMap::new());
    }
    let mut answers = cx.read(vec![servers()])?.into_iter();
    let answer = cx.cloud::<ServersAnswer>(answers.next())?;
    Ok(answer
        .servers
        .into_iter()
        .map(|server| (server.machine.to_string(), server.name))
        .collect())
}

/// A Store refusal of `remove` in the tool's words: a confirmation's retry and
/// an every-copy offer list Volumes as the tool takes them.
fn refused(target: Target, error: RpcError, call: &Call, writes: Writes) -> Refusal {
    let details = error.details.clone();
    let code = error.code.clone();
    let mut refusal = from_store(error, call, writes);
    if code == RpcErrorCode::ConfirmationRequired
        && let Value::Object(input) = &mut refusal.retry.input
    {
        input.insert(
            "accept_volume_loss".into(),
            listed(target, &details, "accept"),
        );
    }
    if let Some(offer) = details
        .get("every_copy")
        .and_then(|offer| offer.get("input"))
    {
        // Deleting every copy is the member's choice: offered, never the retry.
        let mut input = call.input.as_object().cloned().unwrap_or_default();
        input.insert(
            "accept_volume_loss".into(),
            listed(target, offer, "accept_volume_loss"),
        );
        input.insert("every_copy".into(), listed(target, offer, "every_copy"));
        input.insert(
            "version".into(),
            offer.get("version").cloned().unwrap_or(Value::Null),
        );
        refusal = refusal.with_detail("every_copy", json!(Call::new(target.tool(), input.into())));
    }
    if let Some(waiting) = details.get("waiting_on") {
        refusal = refusal.with_detail("waiting_on", waiting.clone());
    }
    refusal
}

/// The waiting Servers a refusal carries, named, with the read that shows them.
fn named(cx: &mut Cx<'_>, mut refusal: Refusal) -> Result<Refusal, Stop> {
    let Some(raw) = refusal.details.get("waiting_on").cloned() else {
        return Ok(refusal);
    };
    let waiting_on = serde_json::from_value::<Vec<Waiting>>(raw).unwrap_or_default();
    let names = names(cx, &waiting_on)?;
    refusal = refusal.with_detail("waiting_on", json!(self::waiting(&waiting_on, &names)));
    if !cx.is_local() {
        refusal = refusal.with_next(server_show(None));
    }
    Ok(refusal)
}

fn left(left: &Left) -> Value {
    json!({
        "environment": left.environment.name,
        "deployment": left.deployment.as_ref().map(|deployment| deployment.number),
        "status": left.status,
        "why": left.why,
        "services": left.services,
    })
}

/// Run `remove`, follow it within `wait_seconds`, and say where it stands.
fn remove(
    cx: &mut Cx<'_>,
    target: Target,
    command: &Remove,
    wait_seconds: u32,
) -> Result<Map<String, Value>, Stop> {
    let removal = match cx.write_with(command, |error, call, writes| {
        refused(target, error, call, writes)
    }) {
        Ok(removal) => removal,
        Err(Stop::Refuse(refusal)) => return Err(named(cx, *refusal)?.into()),
        Err(step) => return Err(step),
    };
    let (id, mut left, waiting_on, mut residuals) = match removal {
        Removal::Removed { residuals } => (None, Vec::new(), Vec::new(), residuals),
        Removal::Removing {
            id,
            left,
            waiting_on,
        } => (Some(id), left, waiting_on, Vec::new()),
    };
    let query = EnvironmentsQuery {
        project: Some(command.project.clone()),
    };
    let mut attempt = 0;
    while !left.is_empty() {
        let Some(answers) = cx.poll(wait_seconds, attempt, vec![store(&query)?])? else {
            break;
        };
        let answer = answers.into_iter().next();
        if matches!(&answer, Some(Err(error)) if error.code == RpcErrorCode::NotFound) {
            left.clear();
            break;
        }
        let view = cx.view::<EnvironmentsQuery>(answer)?;
        left.retain(|left| {
            view.environments
                .iter()
                .any(|listed| listed.id == left.environment.id)
        });
        attempt += 1;
    }
    if let (Some(id), true) = (id, left.is_empty()) {
        residuals = cx.one(&RemovedQuery { id })?.residuals;
    }
    let names = names(cx, &waiting_on)?;
    let mut shown = Map::new();
    let removing = !left.is_empty();
    shown.insert(
        "state".into(),
        match removing {
            true => "removing",
            false => "removed",
        }
        .into(),
    );
    shown.insert("left".into(), left.iter().map(self::left).collect());
    shown.insert(
        "waiting_on".into(),
        match removing {
            true => waiting(&waiting_on, &names).into(),
            false => Value::Array(Vec::new()),
        },
    );
    shown.insert("residuals".into(), json!(residuals));
    if let Some(first) = left.first() {
        shown.insert(
            "next".into(),
            json!(Call::new(
                "deployment_show",
                json!({
                    "project": command.project,
                    "environment": first.environment.name,
                    "wait_seconds": 120,
                }),
            )),
        );
    }
    Ok(shown)
}

/// The Project a call names, else the one it is in.
fn project(cx: &Cx<'_>, named: Option<&ProjectName>) -> Result<ProjectName, Stop> {
    cx.scope(named, None)
        .project
        .ok_or_else(|| Refusal::invalid("Name the Project", "project").into())
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct EnvironmentRemove {
    #[schemars(with = "String", pattern(NODE_NAME))]
    environment: EnvironmentName,
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    #[serde(default)]
    #[schemars(with = "Vec<String>")]
    accept_volume_loss: Vec<VolumeName>,
    #[serde(default)]
    #[schemars(with = "Vec<String>")]
    every_copy: Vec<VolumeName>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    #[schemars(range(min = 0, max = 300))]
    wait_seconds: u32,
}

impl Tool for EnvironmentRemove {
    const NAME: &'static str = "environment_remove";
    const DESCRIPTION: &'static str = "Delete an Environment, its Volumes and their data, once no server runs it. A server that does not answer holds its part until it answers or is removed. The result says removed or removing.";
    const GATE: Gate = Gate::Store;
    const EFFECT: Effect = Effect::Remove;

    fn example() -> Value {
        json!({ "environment": "preview", "wait_seconds": 120 })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let project = project(cx, self.project.as_ref())?;
        AcceptedLoss::from_input(
            self.accept_volume_loss
                .iter()
                .map(ToString::to_string)
                .collect(),
            self.version.clone(),
        )?;
        let one = |volumes: Vec<VolumeName>| match volumes.is_empty() {
            true => Volumes::new(),
            false => Volumes::from([(self.environment.clone(), volumes)]),
        };
        let command = Remove {
            project,
            environment: Some(self.environment.clone()),
            version: self.version,
            accept_volume_loss: one(self.accept_volume_loss),
            every_copy: one(self.every_copy),
        };
        let mut shown = remove(cx, Target::Environment, &command, self.wait_seconds)?;
        shown.insert(
            "at".into(),
            format!("{}/{}", command.project, self.environment).into(),
        );
        Ok(shown.into())
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectRemove {
    #[schemars(with = "String", pattern(NODE_NAME))]
    project: ProjectName,
    #[serde(default)]
    accept_volume_loss: Vec<String>,
    #[serde(default)]
    every_copy: Vec<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    #[schemars(range(min = 0, max = 300))]
    wait_seconds: u32,
}

impl Tool for ProjectRemove {
    const NAME: &'static str = "project_remove";
    const DESCRIPTION: &'static str = "Delete a Project, every Environment and Volume and their data, the Default last. A server that does not answer holds its part until it answers or is removed. The result says removed or removing.";
    const GATE: Gate = Gate::Store;
    const EFFECT: Effect = Effect::Remove;

    fn example() -> Value {
        json!({ "project": "trial", "wait_seconds": 120 })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        AcceptedLoss::from_input(self.accept_volume_loss.clone(), self.version.clone())?;
        let command = Remove {
            project: self.project,
            environment: None,
            version: self.version,
            accept_volume_loss: by_environment(&self.accept_volume_loss, "accept_volume_loss")?,
            every_copy: by_environment(&self.every_copy, "every_copy")?,
        };
        let mut shown = remove(cx, Target::Project, &command, self.wait_seconds)?;
        shown.insert("at".into(), command.project.to_string().into());
        Ok(shown.into())
    }
}

#[cfg(test)]
#[path = "removal_tests.rs"]
mod tests;
