//! Deployments. Admission freezes what a Deployment ships: a Saved revision, its
//! target nodes and the Deploy Intent lowered from them. A runner `claim`s it and
//! `record`s its evidence, and each confirmed Node Outcome advances Applied State.
//! The Head that reviews compare against comes from here too.

use ployz_core::config::{
    CompiledNodeConfig, SavedEnvironmentIntent, canonicalize_environment_intent,
    compile_environment_intent, lower_deployment, parse_environment_intent,
    parse_runtime_preview, project_runtime_outcome,
};
use ployz_core::{
    DeployIntent, DeployOutcome, DeployPreview, ExecutionError, Namespace, RpcError, ServiceName,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Actor;
use crate::error;
use crate::id::{DeploymentId, EnvironmentId, Revision, RunnerId};
use crate::review::{self, Head};
use crate::scope::{self, Environment, EnvironmentRef, EnvironmentSummary, revision_param};
use crate::storage::{Row, Tx};

/// Where a Deployment is in its life.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentStatus {
    /// Admitted, waiting for a runner.
    Queued,
    /// A newer admission replaced it before any runner claimed it.
    Superseded,
    /// Its runner claimed it and has not recorded an outcome yet.
    Running,
    /// Every planned operation completed.
    Applied,
    /// It stopped before every operation completed. Confirmed Node Outcomes still count.
    Failed,
    /// Another runner took over before its own recorded an outcome, so what ran is unknown.
    Unknown,
}

impl DeploymentStatus {
    const ALL: [Self; 6] = [
        Self::Queued,
        Self::Superseded,
        Self::Running,
        Self::Applied,
        Self::Failed,
        Self::Unknown,
    ];

    const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Superseded => "superseded",
            Self::Running => "running",
            Self::Applied => "applied",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

/// A Deployment as lists show it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeploymentSummary {
    pub id: DeploymentId,
    /// Counts from 1 within its Environment.
    pub number: u64,
    pub status: DeploymentStatus,
    /// The Saved revision it ships.
    pub saved: Revision,
    /// The Services it targets; empty targets every Service.
    pub services: Vec<ServiceName>,
    /// The runner that claimed it.
    pub runner: Option<RunnerId>,
}

/// A Deployment with its recorded Deploy Preview and every Node Outcome.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeploymentView {
    #[serde(flatten)]
    pub deployment: DeploymentSummary,
    pub environment: EnvironmentSummary,
    pub namespace: Namespace,
    pub nodes: Vec<NodeOutcome>,
    /// The Deploy Preview its runner prepared, with environment values removed.
    pub preview: Option<Value>,
    pub outcome: Option<Outcome>,
}

/// What a Deployment did to one of its target nodes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodeOutcome {
    pub id: String,
    pub name: String,
    pub outcome: NodeStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    /// The Deployment has not recorded an outcome yet.
    Pending,
    /// Every planned operation for it completed: Applied State holds it now.
    Applied,
    /// Not every planned operation for it completed, so Applied State kept the old one.
    NotApplied,
    /// Its runner was replaced before recording what ran.
    Unknown,
}

/// What a Deployment's runner recorded at its end.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Outcome {
    /// Execution ran. `summary` counts operations, without inputs or provider messages;
    /// `confirmed` names the Services every planned operation of which completed.
    Executed {
        summary: Value,
        confirmed: Vec<ServiceName>,
    },
    /// Nothing executed: preparation failed first.
    NotExecuted { reason: String },
}

/// Evidence a runner records about the Deployment it claimed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "evidence", content = "value", rename_all = "snake_case")]
pub enum RunEvidence {
    /// The Deploy Preview preparation produced, recorded before it is confirmed.
    Prepared(DeployPreview),
    /// What executing that preview did.
    Executed(Box<DeployOutcome<ExecutionError>>),
    /// Preparation failed, so nothing executed. Users read the reason: it holds no secret.
    NotExecuted(String),
}

/// A claimed Deployment and the frozen Deploy Intent its runner executes.
#[derive(Clone, Debug, PartialEq)]
pub struct Claimed {
    pub deployment: DeploymentSummary,
    pub intent: DeployIntent,
}

/// One node a Deployment targets, as frozen at admission.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct TargetNode {
    id: String,
    name: String,
    /// The runtime Service it lowers to, which Node Outcomes are confirmed by.
    service: ServiceName,
}

/// What admission freezes.
pub(crate) struct Frozen {
    pub(crate) nodes: Vec<TargetNode>,
    pub(crate) intent: DeployIntent,
}

impl Frozen {
    /// Whether this Deployment targets node `id`.
    pub(crate) fn targets(&self, id: &str) -> bool {
        self.nodes.iter().any(|node| node.id == id)
    }
}

/// The runner's evidence so far, stored as one document.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Run {
    runner: Option<RunnerId>,
    preview: Option<Value>,
    outcome: Option<Outcome>,
}

struct Stored {
    summary: DeploymentSummary,
    environment: EnvironmentId,
    namespace: Namespace,
    nodes: Vec<TargetNode>,
    intent: String,
    run: Run,
}

const COLUMNS: &str = "id, environment_id, number, status, saved_revision, services, nodes, \
     namespace, intent, run";

/// Freeze a Deployment of `saved`: its target nodes and the Deploy Intent lowered from
/// them. `services` narrows it; none targets every Service, including the removal of
/// those Applied State holds and `saved` does not.
pub(crate) fn freeze(
    environment: &EnvironmentId,
    saved: &SavedEnvironmentIntent,
    applied: &SavedEnvironmentIntent,
    services: &[ServiceName],
    namespace: Namespace,
) -> Result<Frozen, RpcError> {
    let target = |service: &ployz_core::config::SavedServiceIntent| TargetNode {
        id: service.id.clone(),
        name: service.slug.clone(),
        service: service.config.private_dns.clone(),
    };
    let nodes = if services.is_empty() {
        saved
            .services
            .iter()
            .chain(
                applied
                    .services
                    .iter()
                    .filter(|old| !saved.services.iter().any(|new| new.id == old.id)),
            )
            .map(target)
            .collect()
    } else {
        services
            .iter()
            .map(|name| {
                saved
                    .services
                    .iter()
                    .find(|service| service.slug == name.as_str())
                    .map(target)
                    .ok_or_else(|| {
                        error::not_found(
                            format!("No Service named {name} to deploy"),
                            json!({ "services": saved.services.iter().map(|service| &service.slug).collect::<Vec<_>>() }),
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    let compiled = compile_environment_intent(environment.as_str(), saved.clone());
    let snapshots: Vec<Value> = compiled
        .node_snapshots
        .into_iter()
        .filter_map(|node| match node.snapshot.0 {
            CompiledNodeConfig::Service(config) => {
                Some(json!({ "serviceId": node.node_id, "config": config }))
            }
            CompiledNodeConfig::Volume(_) => None,
        })
        .collect();
    let lineages: serde_json::Map<String, Value> = compiled
        .variable_producers
        .into_iter()
        .map(|producer| (producer.owner_lineage_id, json!(producer.owner_id)))
        .collect();
    // Empty reconciles the whole Namespace; names deploy only those Services.
    let selected: Vec<Value> = if services.is_empty() {
        Vec::new()
    } else {
        nodes
            .iter()
            .map(|node| json!({ "name": node.service }))
            .collect()
    };
    // ponytail: no variables or secrets yet, so every Service's resolved env is empty.
    // Variables (6a) resolve here, and secrets only at `claim`.
    let input = json!({
        "namespace": namespace,
        "snapshots": snapshots,
        "lineages": lineages,
        "selected": selected,
    });
    let intent = lower_deployment(serde_json::from_value(input).expect("lowering input is valid"))
        .map_err(|error| {
            error::invalid(
                format!("This Environment can't deploy: {}", error.message),
                json!({ "path": error.path }),
            )
        })?;
    Ok(Frozen { nodes, intent })
}

/// Admit a frozen Deployment of Saved revision `saved`. A Deployment still queued is
/// superseded: the newest admission replaces the pending one.
pub(crate) fn admit(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &DeploymentId,
    environment: &EnvironmentId,
    saved: Revision,
    services: &[ServiceName],
    frozen: &Frozen,
) -> Result<DeploymentSummary, RpcError> {
    let environment_id = environment.as_str();
    tx.execute(
        "UPDATE config_deployment SET status = 'superseded' \
         WHERE environment_id = ?1 AND status = 'queued'",
        &[environment_id.into()],
    )?;
    let number = tx
        .query(
            "SELECT COALESCE(MAX(number), 0) FROM config_deployment WHERE environment_id = ?1",
            &[environment_id.into()],
        )?
        .first()
        .ok_or_else(|| error::corrupt("Deployment number"))?
        .int(0)?
        + 1;
    let summary = DeploymentSummary {
        id: id.clone(),
        number: u64::try_from(number).map_err(|_| error::corrupt("Deployment number"))?,
        status: DeploymentStatus::Queued,
        saved,
        services: services.to_vec(),
        runner: None,
    };
    tx.execute(
        "INSERT INTO config_deployment \
         (id, organization_id, environment_id, number, status, saved_revision, services, nodes, \
          namespace, intent, run) \
         VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?6, ?7, ?8, ?9, ?10)",
        &[
            id.as_str().into(),
            who.organization.as_str().into(),
            environment_id.into(),
            number.into(),
            revision_param(saved)?.into(),
            json_text(&summary.services).as_str().into(),
            json_text(&frozen.nodes).as_str().into(),
            frozen.intent.namespace.as_str().into(),
            json_text(&frozen.intent).as_str().into(),
            json_text(&Run::default()).as_str().into(),
        ],
    )?;
    Ok(summary)
}

/// The Namespace `environment` deploys into: fixed at its first admission, so renames
/// never move it. `{project}-{environment}` unless another Environment of the
/// Organization holds that, then suffixed with this Environment's ID. `reserve`
/// fixes a new one; a plan only looks.
pub(crate) fn namespace(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &EnvironmentSummary,
    reserve: bool,
) -> Result<Namespace, RpcError> {
    let rows = tx.query(
        "SELECT namespace FROM config_namespace WHERE environment_id = ?1",
        &[environment.id.as_str().into()],
    )?;
    if let Some(row) = rows.first() {
        return Namespace::parse(row.text(0)?).map_err(|_| error::corrupt("Namespace"));
    }
    let base = format!("{}-{}", environment.project, environment.name);
    let suffix: String = environment
        .id
        .as_str()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase();
    let short = base.get(..54).unwrap_or(&base).trim_end_matches('-');
    for candidate in [base.clone(), format!("{short}-{suffix}")] {
        let Ok(namespace) = Namespace::parse(candidate.as_str()) else {
            continue;
        };
        let taken = !tx
            .query(
                "SELECT environment_id FROM config_namespace \
                 WHERE organization_id = ?1 AND namespace = ?2",
                &[who.organization.as_str().into(), namespace.as_str().into()],
            )?
            .is_empty();
        if namespace.is_reserved() || taken {
            continue;
        }
        if reserve {
            tx.execute(
                "INSERT INTO config_namespace (organization_id, namespace, environment_id) \
                 VALUES (?1, ?2, ?3)",
                &[
                    who.organization.as_str().into(),
                    namespace.as_str().into(),
                    environment.id.as_str().into(),
                ],
            )?;
        }
        return Ok(namespace);
    }
    Err(error::conflict(
        "No free Namespace for this Environment: rename it",
        json!({}),
    ))
}

/// Bind a queued Deployment to `runner` and hand over its frozen Deploy Intent.
/// Claiming again as the same runner returns the same. A Deployment another runner
/// still holds without an outcome reads `unknown` from here on.
pub(crate) fn claim(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    runner: &RunnerId,
) -> Result<Claimed, RpcError> {
    let mut stored = locked(tx, id)?;
    match stored.summary.status {
        DeploymentStatus::Running if stored.run.runner.as_ref() == Some(runner) => {}
        DeploymentStatus::Running => return Err(owned_elsewhere(id)),
        DeploymentStatus::Superseded => {
            return Err(error::conflict(
                "A newer Deployment replaced this one before it started",
                json!({ "deployment": id }),
            ));
        }
        DeploymentStatus::Applied | DeploymentStatus::Failed | DeploymentStatus::Unknown => {
            return Err(ended(id));
        }
        DeploymentStatus::Queued => {
            tx.execute(
                "UPDATE config_deployment SET status = 'unknown' \
                 WHERE environment_id = ?1 AND status = 'running'",
                &[stored.environment.as_str().into()],
            )?;
            stored.summary.status = DeploymentStatus::Running;
            stored.run.runner = Some(runner.clone());
            stored.summary.runner = Some(runner.clone());
            save(tx, &stored)?;
        }
    }
    let intent = serde_json::from_str(&stored.intent).map_err(|_| error::corrupt("Deploy Intent"))?;
    Ok(Claimed {
        deployment: stored.summary,
        intent,
    })
}

/// Record `runner`'s evidence. Only the runner that claimed the Deployment may, and
/// recording the same evidence again changes nothing.
pub(crate) fn record(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    runner: &RunnerId,
    evidence: RunEvidence,
) -> Result<DeploymentSummary, RpcError> {
    let mut stored = locked(tx, id)?;
    if stored.run.runner.as_ref() != Some(runner) {
        return Err(owned_elsewhere(id));
    }
    match evidence {
        RunEvidence::Prepared(preview) => {
            let preview = serde_json::to_value(preview)
                .ok()
                .and_then(|preview| parse_runtime_preview(preview).ok())
                .ok_or_else(|| invalid_evidence("Deploy Preview"))?;
            let preview = serde_json::to_value(preview).expect("a Deploy Preview is JSON");
            match &stored.run.preview {
                Some(recorded) if *recorded == preview => return Ok(stored.summary),
                Some(_) => {
                    return Err(error::conflict(
                        "This Deployment already recorded a different Deploy Preview",
                        json!({ "deployment": id }),
                    ));
                }
                None => {}
            }
            running(&stored)?;
            stored.run.preview = Some(preview);
            save(tx, &stored)?;
            Ok(stored.summary)
        }
        RunEvidence::Executed(outcome) => {
            let Some(preview) = stored.run.preview.clone() else {
                return Err(error::conflict(
                    "Record the Deploy Preview before what executing it did",
                    json!({ "deployment": id }),
                ));
            };
            let success = matches!(*outcome, DeployOutcome::Success { .. });
            let projection =
                project_runtime_outcome(preview, json!({ "version": 1, "outcome": outcome }))
                    .map_err(|_| invalid_evidence("Deploy Outcome"))?;
            let outcome = Outcome::Executed {
                summary: serde_json::to_value(projection.summary).expect("a summary is JSON"),
                confirmed: projection.confirmed_services,
            };
            let status = if success {
                DeploymentStatus::Applied
            } else {
                DeploymentStatus::Failed
            };
            finish(tx, stored, outcome, status)
        }
        RunEvidence::NotExecuted(reason) => {
            let reason = reason.chars().take(500).collect();
            finish(
                tx,
                stored,
                Outcome::NotExecuted { reason },
                DeploymentStatus::Failed,
            )
        }
    }
}

fn finish(
    tx: &mut dyn Tx,
    mut stored: Stored,
    outcome: Outcome,
    status: DeploymentStatus,
) -> Result<DeploymentSummary, RpcError> {
    match &stored.run.outcome {
        Some(recorded) if *recorded == outcome => return Ok(stored.summary),
        Some(_) => {
            return Err(error::conflict(
                "This Deployment already recorded a different outcome",
                json!({ "deployment": stored.summary.id }),
            ));
        }
        None => running(&stored)?,
    }
    if let Outcome::Executed { confirmed, .. } = &outcome {
        let saved = saved_at(tx, &stored.environment, stored.summary.saved)?;
        for node in stored
            .nodes
            .iter()
            .filter(|node| confirmed.contains(&node.service))
        {
            match saved.services.iter().find(|service| service.id == node.id) {
                Some(service) => tx.execute(
                    "INSERT INTO config_applied \
                     (environment_id, node_id, organization_id, deployment_id, node) \
                     SELECT environment_id, ?2, organization_id, id, ?3 \
                     FROM config_deployment WHERE id = ?1 \
                     ON CONFLICT (environment_id, node_id) \
                     DO UPDATE SET deployment_id = excluded.deployment_id, node = excluded.node",
                    &[
                        stored.summary.id.as_str().into(),
                        node.id.as_str().into(),
                        json_text(service).as_str().into(),
                    ],
                )?,
                None => tx.execute(
                    "DELETE FROM config_applied WHERE environment_id = ?1 AND node_id = ?2",
                    &[stored.environment.as_str().into(), node.id.as_str().into()],
                )?,
            };
        }
    }
    stored.summary.status = status;
    stored.run.outcome = Some(outcome);
    save(tx, &stored)?;
    Ok(stored.summary)
}

/// What reviews compare Working State against: Applied State, overlaid with the
/// target nodes of the Deployment in flight, if any. Its token changes whenever a
/// Deployment is admitted or ends.
pub(crate) fn head(tx: &mut dyn Tx, environment: &Environment) -> Result<Head, RpcError> {
    let id = &environment.summary.id;
    let mut applied = review::empty(&environment.working);
    for row in tx.query(
        "SELECT node FROM config_applied WHERE environment_id = ?1 ORDER BY node_id",
        &[id.as_str().into()],
    )? {
        applied.services.push(
            serde_json::from_str(row.text(0)?).map_err(|_| error::corrupt("Applied State"))?,
        );
    }
    let ended = tx
        .query(
            "SELECT COUNT(*) FROM config_deployment \
             WHERE environment_id = ?1 AND status IN ('applied', 'failed', 'unknown')",
            &[id.as_str().into()],
        )?
        .first()
        .ok_or_else(|| error::corrupt("Deployment count"))?
        .int(0)?;
    let in_flight = tx.query(
        "SELECT number, saved_revision, nodes FROM config_deployment \
         WHERE environment_id = ?1 AND status IN ('queued', 'running') \
         ORDER BY number DESC LIMIT 1",
        &[id.as_str().into()],
    )?;
    let Some(row) = in_flight.first() else {
        return Ok(Head {
            token: format!("0.{ended}"),
            intent: applied.clone(),
            applied,
        });
    };
    let saved = saved_at(tx, id, revision(row.int(1)?)?)?;
    let nodes: Vec<TargetNode> =
        serde_json::from_str(row.text(2)?).map_err(|_| error::corrupt("Deployment"))?;
    let mut intent = applied.clone();
    for node in nodes {
        intent.services.retain(|service| service.id != node.id);
        intent.services.extend(
            saved
                .services
                .iter()
                .filter(|service| service.id == node.id)
                .cloned(),
        );
    }
    Ok(Head {
        token: format!("{}.{ended}", row.int(0)?),
        intent,
        applied,
    })
}

/// One Deployment in `who`'s Organization, with its Node Outcomes.
pub(crate) fn view(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &DeploymentId,
) -> Result<DeploymentView, RpcError> {
    let rows = tx.query(
        &format!(
            "SELECT {COLUMNS} FROM config_deployment WHERE id = ?1 AND organization_id = ?2"
        ),
        &[id.as_str().into(), who.organization.as_str().into()],
    )?;
    let stored = stored(rows.first().ok_or_else(|| missing(id))?)?;
    let names = tx.query(
        "SELECT p.name, e.name FROM config_environment e \
         JOIN config_project p ON p.id = e.project_id WHERE e.id = ?1",
        &[stored.environment.as_str().into()],
    )?;
    let names = names.first().ok_or_else(|| error::corrupt("Environment"))?;
    let environment = scope::environment(
        tx,
        who,
        &EnvironmentRef {
            project: Some(parse_stored(names.text(0)?)?),
            environment: Some(parse_stored(names.text(1)?)?),
        },
    )?;
    let nodes = stored
        .nodes
        .iter()
        .map(|node| NodeOutcome {
            id: node.id.clone(),
            name: node.name.clone(),
            outcome: match (&stored.run.outcome, stored.summary.status) {
                (Some(Outcome::Executed { confirmed, .. }), _) if confirmed.contains(&node.service) => {
                    NodeStatus::Applied
                }
                (Some(_), _) | (None, DeploymentStatus::Superseded) => NodeStatus::NotApplied,
                (None, DeploymentStatus::Unknown) => NodeStatus::Unknown,
                (None, _) => NodeStatus::Pending,
            },
        })
        .collect();
    Ok(DeploymentView {
        deployment: stored.summary,
        environment: environment.summary,
        namespace: stored.namespace,
        nodes,
        preview: stored.run.preview,
        outcome: stored.run.outcome,
    })
}

/// One page of `environment`'s Deployments, newest first, before `cursor`.
pub(crate) fn page(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    limit: usize,
    before: Option<u64>,
) -> Result<(Vec<DeploymentSummary>, Option<u64>), RpcError> {
    let before = i64::try_from(before.unwrap_or(u64::MAX >> 1)).unwrap_or(i64::MAX);
    let rows = tx.query(
        &format!(
            "SELECT {COLUMNS} FROM config_deployment WHERE environment_id = ?1 AND number < ?2 \
             ORDER BY number DESC LIMIT ?3"
        ),
        &[
            environment.as_str().into(),
            before.into(),
            i64::try_from(limit + 1).unwrap_or(i64::MAX).into(),
        ],
    )?;
    let mut page = rows
        .iter()
        .map(|row| stored(row).map(|stored| stored.summary))
        .collect::<Result<Vec<_>, _>>()?;
    let next = (page.len() > limit).then(|| {
        page.truncate(limit);
        page.last().map_or(0, |last| last.number)
    });
    Ok((page, next))
}

/// Load a Deployment with its Environment locked, so admissions, claims and records
/// of one Environment apply in turn.
fn locked(tx: &mut dyn Tx, id: &DeploymentId) -> Result<Stored, RpcError> {
    let load = |tx: &mut dyn Tx| {
        let rows = tx.query(
            &format!("SELECT {COLUMNS} FROM config_deployment WHERE id = ?1"),
            &[id.as_str().into()],
        )?;
        stored(rows.first().ok_or_else(|| missing(id))?)
    };
    let environment = load(tx)?.environment;
    tx.execute(
        "UPDATE config_environment SET working_revision = working_revision WHERE id = ?1",
        &[environment.as_str().into()],
    )?;
    load(tx)
}

fn stored(row: &Row) -> Result<Stored, RpcError> {
    let status = row.text(3)?;
    let json = |index: usize, what: &str| -> Result<Value, RpcError> {
        serde_json::from_str(row.text(index)?).map_err(|_| error::corrupt(what))
    };
    Ok(Stored {
        summary: DeploymentSummary {
            id: parse_stored(row.text(0)?)?,
            number: u64::try_from(row.int(2)?).map_err(|_| error::corrupt("Deployment"))?,
            status: DeploymentStatus::ALL
                .into_iter()
                .find(|known| known.as_str() == status)
                .ok_or_else(|| error::corrupt("Deployment status"))?,
            saved: revision(row.int(4)?)?,
            services: serde_json::from_value(json(5, "Deployment")?)
                .map_err(|_| error::corrupt("Deployment"))?,
            runner: None,
        },
        environment: parse_stored(row.text(1)?)?,
        nodes: serde_json::from_value(json(6, "Deployment")?)
            .map_err(|_| error::corrupt("Deployment"))?,
        namespace: Namespace::parse(row.text(7)?).map_err(|_| error::corrupt("Namespace"))?,
        intent: row.text(8)?.to_owned(),
        run: serde_json::from_value(json(9, "Deployment run")?)
            .map_err(|_| error::corrupt("Deployment run"))?,
    })
    .map(|mut stored| {
        stored.summary.runner.clone_from(&stored.run.runner);
        stored
    })
}

fn save(tx: &mut dyn Tx, stored: &Stored) -> Result<(), RpcError> {
    tx.execute(
        "UPDATE config_deployment SET status = ?1, run = ?2 WHERE id = ?3",
        &[
            stored.summary.status.as_str().into(),
            json_text(&stored.run).as_str().into(),
            stored.summary.id.as_str().into(),
        ],
    )?;
    Ok(())
}

fn running(stored: &Stored) -> Result<(), RpcError> {
    match stored.summary.status {
        DeploymentStatus::Running => Ok(()),
        DeploymentStatus::Unknown => Err(error::conflict(
            "Another runner took over this Deployment, so its outcome stays unknown. \
             Start a new Deployment",
            json!({ "deployment": stored.summary.id }),
        )),
        DeploymentStatus::Applied | DeploymentStatus::Failed => Err(ended(&stored.summary.id)),
        DeploymentStatus::Queued | DeploymentStatus::Superseded => Err(error::conflict(
            "Claim this Deployment before recording what it did",
            json!({ "deployment": stored.summary.id }),
        )),
    }
}

fn saved_at(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    revision: Revision,
) -> Result<SavedEnvironmentIntent, RpcError> {
    let rows = tx.query(
        "SELECT intent FROM config_saved WHERE environment_id = ?1 AND revision = ?2",
        &[environment.as_str().into(), revision_param(revision)?.into()],
    )?;
    rows.first()
        .and_then(|row| row.text(0).ok())
        .and_then(|text| serde_json::from_str(text).ok())
        .and_then(|value| parse_environment_intent(value).ok())
        .map(canonicalize_environment_intent)
        .ok_or_else(|| error::corrupt("Saved State"))
}

fn revision(value: i64) -> Result<Revision, RpcError> {
    u64::try_from(value)
        .map(Revision)
        .map_err(|_| error::corrupt("revision"))
}

fn parse_stored<T: TryFrom<String, Error = RpcError>>(value: &str) -> Result<T, RpcError> {
    T::try_from(value.to_owned()).map_err(|_| error::corrupt("identity"))
}

fn json_text(value: &impl Serialize) -> String {
    serde_json::to_string(value).expect("stored documents are JSON")
}

fn missing(id: &DeploymentId) -> RpcError {
    error::not_found(format!("No Deployment {id}"), json!({}))
}

fn owned_elsewhere(id: &DeploymentId) -> RpcError {
    error::conflict(
        "Another runner owns this Deployment",
        json!({ "deployment": id }),
    )
}

fn ended(id: &DeploymentId) -> RpcError {
    error::conflict("This Deployment already ended", json!({ "deployment": id }))
}

fn invalid_evidence(what: &str) -> RpcError {
    error::invalid(
        format!("The runner's {what} does not match this Deployment"),
        json!({}),
    )
}
