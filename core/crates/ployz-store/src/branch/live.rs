//! What a Branch uses live from its Parent: its plan, view and lowering inputs.

use super::*;

pub(crate) fn branch(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &BranchQuery,
) -> Result<BranchView, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    view(tx, &environment)
}

/// Plan a Branch of `query.from` by names, as creating it would.
pub(crate) fn branch_plan(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &BranchPlanQuery,
) -> Result<BranchPlanView, RpcError> {
    let from = scope::environment(tx, who, &query.from)?;
    let working = &from.working;
    let chain = chain(tx, &from.summary.id)?;
    let deployed = chain
        .first()
        .map(|own| lineages(&own.applied))
        .unwrap_or_default();
    let named = |names: &[NodeName]| -> Result<Vec<String>, RpcError> {
        names
            .iter()
            .map(|name| lineage_named(working, name))
            .collect()
    };
    let focus = named(&query.focus)?;
    let plan = |picks: &BranchPicks| plan_branch(working, &deployed, &focus, picks).map_err(config);
    let planned = plan(&match query.preset {
        Some(preset) => BranchPicks::Preset { preset },
        None => BranchPicks::Own {
            own: named(&query.copy)?,
        },
    })?;
    let own = |plan: &BranchPlan| -> BTreeSet<String> {
        plan.nodes
            .iter()
            .filter(|node| matches!(node.role, BranchNodeRole::Own { .. }))
            .map(|node| node.lineage_id.clone())
            .collect()
    };
    let owned = own(&planned);
    let only = own(&plan(&BranchPicks::Preset {
        preset: BranchPreset::Only,
    })?);
    let uses = own(&plan(&BranchPicks::Preset {
        preset: BranchPreset::Uses,
    })?);
    // "Plus what it uses" only when it adds something to "Only what changes".
    let presets = match uses == only {
        true => vec![BranchPreset::Only, BranchPreset::All],
        false => vec![BranchPreset::Only, BranchPreset::Uses, BranchPreset::All],
    };
    let role = |role: &BranchNodeRole| match role {
        BranchNodeRole::Own { .. } => PlannedRole::Own,
        BranchNodeRole::Live => PlannedRole::Live,
        BranchNodeRole::LeftOut => PlannedRole::LeftOut,
    };
    let names = from.names();
    let nodes = planned
        .nodes
        .iter()
        .map(|node| {
            let lineage = &node.lineage_id;
            let mut flipped = owned.clone();
            if !flipped.remove(lineage) {
                flipped.insert(lineage.clone());
            }
            let toggled = plan(&BranchPicks::Own {
                own: flipped.into_iter().collect(),
            })?
            .nodes
            .iter()
            .find(|other| other.lineage_id == *lineage)
            .map_or(PlannedRole::LeftOut, |other| role(&other.role));
            let name = node_of(working, lineage)
                .or_else(|| {
                    names
                        .get(lineage)
                        .and_then(|name| ServiceName::parse(name.as_str()).ok())
                        .map(NodeName::Service)
                })
                .ok_or_else(|| error::corrupt("Branch plan"))?;
            Ok(PlannedNode {
                name,
                kind: node.node_type,
                role: role(&node.role),
                because: match &node.role {
                    BranchNodeRole::Own { because } => Some(*because),
                    BranchNodeRole::Live | BranchNodeRole::LeftOut => None,
                },
                toggled,
                owner: chain
                    .iter()
                    .find(|ancestor| holds(&ancestor.applied, lineage))
                    .map(|ancestor| ancestor.environment.summary.name.clone()),
                data: holds_data(working, lineage),
            })
        })
        .collect::<Result<_, RpcError>>()?;
    Ok(BranchPlanView {
        from: from.summary.clone(),
        preset: planned.preset,
        presets,
        nodes,
    })
}

/// What deploying a Branch adds to lowering: the values of the Live Nodes it
/// reads, from where they run, and the Setup Commands of Own Copies never deployed.
#[derive(Default)]
pub(crate) struct Lowering {
    /// Producers of the Live Nodes' values, rescoped to their owners' Namespaces.
    pub(crate) live: Vec<SavedVariableProducer>,
    /// Setup Commands by Service ID.
    pub(crate) setup: BTreeMap<String, Vec<String>>,
}

/// [`Lowering`] for a Deployment of `saved` in Environment `id`, as things stand now.
pub(crate) fn lowering(
    tx: &mut dyn Tx,
    id: &EnvironmentId,
    saved: &SavedEnvironmentIntent,
) -> Result<Lowering, RpcError> {
    let Some(row) = row(tx, id)? else {
        return Ok(Lowering::default());
    };
    let deployed = tx
        .query(
            "SELECT node_id FROM config_applied WHERE environment_id = ?1",
            &[id.as_str().into()],
        )?
        .iter()
        .map(|row| row.text(0).map(str::to_owned))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let setup = saved
        .services
        .iter()
        .filter(|service| !deployed.contains(&service.id))
        .filter_map(|service| {
            let commands: Vec<String> = row
                .setup
                .iter()
                .filter(|setup| setup.lineage == service.lineage_id)
                .map(|setup| setup.command.clone())
                .collect();
            (!commands.is_empty()).then(|| (service.id.clone(), commands))
        })
        .collect();
    Ok(Lowering {
        live: live_producers(tx, &row.parent, &used_live(saved))?,
        setup,
    })
}

/// The name of each node `working` uses live, by lineage, from the nearest
/// Environment it comes from whose Working State names it. Empty for a root.
pub(crate) fn live_names(
    tx: &mut dyn Tx,
    id: &EnvironmentId,
    working: &SavedEnvironmentIntent,
) -> Result<BTreeMap<String, String>, RpcError> {
    let mut wanted: BTreeSet<String> = used_live(working).into_keys().collect();
    let mut names = BTreeMap::new();
    for ancestor in ancestors(tx, id)? {
        if wanted.is_empty() {
            break;
        }
        let rows = tx.query(
            "SELECT working FROM config_environment WHERE id = ?1",
            &[ancestor.as_str().into()],
        )?;
        let intent = rows
            .first()
            .ok_or_else(|| error::corrupt("Environment"))?
            .intent(0, "Branch")?;
        wanted.retain(|lineage| match name_of(&intent, lineage) {
            Some(name) => {
                names.insert(lineage.clone(), name);
                false
            }
            None => true,
        });
    }
    Ok(names)
}

/// A Branch's view: its Parent, its Live Nodes and their owners, what Update would
/// stage, and how many changes it would sync into its Parent.
pub(crate) fn view(tx: &mut dyn Tx, branch: &Environment) -> Result<BranchView, RpcError> {
    let row = branch_row(tx, branch)?;
    let chain = chain(tx, &row.parent)?;
    let parent = chain.first().ok_or_else(|| error::corrupt("Branch"))?;
    let uses = used_live(&branch.working);
    let live = uses
        .keys()
        .map(|lineage| {
            let owner = chain
                .iter()
                .find(|ancestor| holds(&ancestor.applied, lineage));
            LiveNode {
                name: branch
                    .live
                    .get(lineage)
                    .cloned()
                    .unwrap_or_else(|| lineage.clone()),
                owner: owner.map(|ancestor| ancestor.environment.summary.name.clone()),
                data: owner.is_some_and(|ancestor| holds_data(&ancestor.applied, lineage)),
                used_by: branch
                    .working
                    .services
                    .iter()
                    .filter(|service| references(service, lineage))
                    .filter_map(|service| ServiceName::parse(service.slug.as_str()).ok())
                    .collect(),
            }
        })
        .collect();
    let update = if parent.applied.services.is_empty() && parent.applied.volumes.is_empty() {
        Vec::new()
    } else {
        let moving = Moving::update(tx, &parent.environment, branch)?;
        moving
            .compare(&branch.working, None)?
            .rows
            .iter()
            .filter(|row| matches!(row.role, BranchRole::Move { .. }))
            .map(|row| moving.name(&branch.working, &row.key.to_string()))
            .collect()
    };
    let to_parent = Moving::sync(tx, branch, &parent.environment)?;
    let to_parent = to_parent
        .compare(&parent.environment.working, None)?
        .rows
        .iter()
        .filter(|row| to_parent.ticked(row))
        .count();
    let setup = row
        .setup
        .iter()
        .filter_map(|setup| {
            let service = branch
                .working
                .services
                .iter()
                .find(|service| service.lineage_id == setup.lineage)?;
            Some(SetupCommand {
                service: ServiceName::parse(service.slug.as_str()).ok()?,
                command: setup.command.clone(),
            })
        })
        .collect();
    Ok(BranchView {
        environment: branch.summary.clone(),
        parent: parent.environment.summary.name.clone(),
        kept: row.kept,
        setup,
        live,
        update,
        to_parent,
        pull_request: crate::pull_request::of(tx, &branch.summary.id)?,
    })
}

/// What a Branch's Live Nodes provide to the Services reading them, from the
/// nearest Environment `parent` or above that runs each: its nodes' values and,
/// when it is itself a Branch, the values it reads live in turn.
pub(super) fn live_producers(
    tx: &mut dyn Tx,
    parent: &EnvironmentId,
    uses: &BTreeMap<String, BTreeSet<String>>,
) -> Result<Vec<SavedVariableProducer>, RpcError> {
    if uses.is_empty() {
        return Ok(Vec::new());
    }
    let chain = chain(tx, parent)?;
    let mut producers = Vec::new();
    for owner in &chain {
        let lineages: Vec<LiveLineageUse> = uses
            .iter()
            .filter(|(lineage, _)| {
                chain
                    .iter()
                    .find(|ancestor| holds(&ancestor.applied, lineage))
                    .is_some_and(|found| {
                        found.environment.summary.id == owner.environment.summary.id
                    })
            })
            .map(|(lineage, keys)| LiveLineageUse {
                lineage_id: lineage.clone(),
                keys: keys.iter().cloned().collect(),
            })
            .collect();
        if lineages.is_empty() {
            continue;
        }
        let id = &owner.environment.summary.id;
        let rows = tx.query(
            "SELECT namespace FROM config_namespace WHERE environment_id = ?1",
            &[id.as_str().into()],
        )?;
        let Some(namespace) = rows.first() else {
            continue;
        };
        let namespace = namespace.parse::<Namespace>(0, "Namespace")?;
        let mut provided =
            compile_environment_intent(id.as_str(), owner.applied.clone()).variable_producers;
        if let Some(row) = row(tx, id)? {
            provided.extend(live_producers(tx, &row.parent, &used_live(&owner.applied))?);
        }
        let values = live_values(LiveValuesInput {
            owner: LiveValuesOwner {
                namespace,
                producers: provided,
            },
            lineages,
        });
        producers.extend(values.producers);
    }
    Ok(producers)
}

/// Whether any of `service`'s variables reads the Service of `lineage`.
pub(super) fn references(service: &SavedServiceIntent, lineage: &str) -> bool {
    service.variables.iter().any(|variable| {
        matches!(&variable.value, ployz_core::config::SavedVariableValue::Template { parts }
            if parts.iter().any(|part| matches!(part, ValuePart::Ref {
                owner: ValuePartOwner::Service { lineage_id }, ..
            } if lineage_id == lineage)))
    })
}

/// Each Service lineage `intent`'s variables reference but it doesn't own, with the
/// keys read from it: the nodes it uses live.
pub(crate) fn used_live(intent: &SavedEnvironmentIntent) -> BTreeMap<String, BTreeSet<String>> {
    let owned: BTreeSet<&str> = intent
        .services
        .iter()
        .map(|service| service.lineage_id.as_str())
        .collect();
    let mut uses: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for variable in intent
        .services
        .iter()
        .flat_map(|service| &service.variables)
    {
        let ployz_core::config::SavedVariableValue::Template { parts } = &variable.value else {
            continue;
        };
        for part in parts {
            if let ValuePart::Ref {
                owner: ValuePartOwner::Service { lineage_id },
                key,
            } = part
                && !owned.contains(lineage_id.as_str())
            {
                uses.entry(lineage_id.clone())
                    .or_default()
                    .insert(key.clone());
            }
        }
    }
    uses
}
