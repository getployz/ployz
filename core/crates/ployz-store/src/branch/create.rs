//! Creating a Branch, and copying or keeping one.

use super::*;

pub(crate) fn create_branch(
    tx: &mut dyn Tx,
    who: &Actor,
    create: &CreateBranch,
) -> Result<Branched, RpcError> {
    let parent = scope::lock(tx, who, &create.from)?;
    if let Some(removal) = crate::teardown::removing(tx, &parent.summary.id)? {
        return Err(crate::teardown::being_removed(&parent, &removal));
    }
    let project = scope::project(tx, who, Some(&parent.summary.project))?;
    let taken = tx.query(
        "SELECT id FROM config_environment WHERE project_id = ?1 AND name = ?2",
        &[project.id.as_str().into(), create.name.as_str().into()],
    )?;
    if !taken.is_empty() {
        return Err(error::conflict(
            format!("{} is taken in Project {}", create.name, project.name),
            json!({ "environment": create.name }),
        ));
    }
    let applied = deployment::head(tx, &parent)?.applied;
    let deployed = lineages(&applied);
    let failed = match &create.fix {
        Some(id) => failed_services(tx, who, &parent, &applied, id)?,
        None => Vec::new(),
    };
    let working = &parent.working;
    let mut copy = create
        .copy
        .iter()
        .map(|name| lineage_named(working, name))
        .collect::<Result<Vec<_>, _>>()?;
    if copy.is_empty() {
        copy = failed
            .iter()
            .map(|service| service.lineage_id.clone())
            .filter(|lineage| holds(working, lineage))
            .collect();
    }
    let picks = BranchPicks::Own { own: copy.clone() };
    let plan = plan_branch(working, &deployed, &copy, &picks).map_err(config)?;
    let (mut own, mut live) = (BTreeSet::new(), Vec::new());
    for node in &plan.nodes {
        match node.role {
            BranchNodeRole::Own { .. } => {
                own.insert(node.lineage_id.clone());
            }
            BranchNodeRole::Live => live.push(node.lineage_id.clone()),
            BranchNodeRole::LeftOut => {}
        }
    }
    if own.is_empty() {
        return Err(error::invalid(
            "Pick something to copy",
            json!({ "valid_children": names(working) }),
        ));
    }
    for name in &create.live {
        let lineage = lineage_named(working, name)?;
        if !live.contains(&lineage) {
            let why = if own.contains(&lineage) {
                "the Branch copies it: nothing running can lend it"
            } else {
                "nothing the Branch copies uses it"
            };
            return Err(error::invalid(
                format!("{name} can't be used live: {why}"),
                json!({ "node": name }),
            ));
        }
    }
    // None named: the Parent's defaults, each where the Branch copies its Service.
    let defaults = match create.setup.is_empty() {
        true => crate::teardown::branch_setup(tx, &parent.summary.id)?
            .into_iter()
            .filter(|setup| {
                working.services.iter().any(|service| {
                    service.slug == setup.service.as_str() && own.contains(&service.lineage_id)
                })
            })
            .collect(),
        false => Vec::new(),
    };
    let setup = create
        .setup
        .iter()
        .chain(&defaults)
        .map(|setup| {
            let service = working
                .services
                .iter()
                .find(|service| service.slug == setup.service.as_str())
                .filter(|service| own.contains(&service.lineage_id))
                .ok_or_else(|| {
                    error::invalid(
                        format!(
                            "{}: a Setup Command runs in one of the Branch's own Services",
                            setup.service
                        ),
                        json!({ "service": setup.service }),
                    )
                })?;
            Ok(Setup {
                lineage: service.lineage_id.clone(),
                command: setup_command(setup)?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    // A fix: each failed Service it copies stands in for the Parent's; the Parent stays as it is.
    let fixed: Vec<SavedServiceIntent> = failed
        .into_iter()
        .filter(|service| own.contains(&service.lineage_id) && holds(working, &service.lineage_id))
        .collect();
    if create.fix.is_some() && fixed.is_empty() {
        return Err(error::invalid(
            "Copy a Service this Deployment failed to apply",
            json!({ "fix": create.fix }),
        ));
    }
    let mut from = working.clone();
    for failed in fixed {
        for service in &mut from.services {
            if service.lineage_id == failed.lineage_id {
                *service = failed.clone();
            }
        }
    }

    let into = crate::review::empty(create.name.as_str());
    let hostnames = BranchHostnames {
        from: suffix(tx, &parent)?,
        into: format!("-{}", create.name),
    };
    let node_picks = own
        .iter()
        .map(|lineage| BranchPick {
            key: format!("{lineage}:node"),
            choice: None,
        })
        .collect();
    let creating = |from, picks| {
        compare(Comparing {
            base: None,
            from,
            into: &into,
            parent: None,
            provided: &live,
            hostnames: &hostnames,
            from_kept: false,
            picks: Some(picks),
        })
    };
    let changes = creating(&from, node_picks)?;
    // A fix's base is what the Parent runs, so the failed change shows as staged.
    let base = match create.fix {
        Some(_) => creating(&applied, Vec::new())?.base,
        None => changes.base,
    }
    .ok_or_else(|| error::internal("Core returned no base for a new Branch"))?;

    let summary = insert_environment(tx, who, &project, &create.id, create.name.clone())?;
    let mut branch = Environment {
        summary,
        working: into,
        live: BTreeMap::new(),
    };
    let carried = Carried::of(tx, &parent.summary.id, &from)?;
    let staged = land(tx, who, &mut branch, (&from, &carried), changes.next, &[])?;
    tx.execute(
        "INSERT INTO config_environment_branch (environment_id, organization_id, parent_id, kept, base, setup) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        &[
            create.id.as_str().into(),
            who.organization.as_str().into(),
            parent.summary.id.as_str().into(),
            i64::from(create.keep).into(),
            document(&base).as_str().into(),
            serde_json::to_string(&setup)
                .expect("Setup Commands are JSON")
                .as_str()
                .into(),
        ],
    )?;
    branch.live = live_names(tx, &create.id, &branch.working)?;
    Ok(Branched {
        branch: view(tx, &branch)?,
        staged,
        immediate: Vec::new(),
    })
}

/// Variable row `name`'s own value in the receiver: sealed for a secret, else text
/// referencing the receiver's Services by `names`.
pub(super) fn fresh_value(
    name: &str,
    secret: bool,
    text: &str,
    names: &BTreeMap<String, String>,
    sealing: &SealingKey,
) -> Result<BranchNewValue, RpcError> {
    let key = crate::variables::VariableKey::parse(name.rsplit('.').next().unwrap_or(name))?;
    let (value, value_fingerprint) = match secret {
        true => (
            SavedVariableValue::Secret {
                encrypted_value: Some(sealing.seal(text)),
            },
            sealing.fingerprint(text),
        ),
        false => crate::variables::text_value(&key, text, names)?,
    };
    Ok(BranchNewValue {
        value,
        value_fingerprint,
    })
}

pub(crate) fn copy_node(
    tx: &mut dyn Tx,
    who: &Actor,
    copy: &CopyNode,
) -> Result<Branched, RpcError> {
    let mut branch = scope::lock(tx, who, &copy.environment)?;
    branch.expect(copy.expect)?;
    let row = branch_row(tx, &branch)?;
    if branch.service(&copy.node).is_ok() {
        return Err(error::conflict(
            format!("This Branch already has its own copy of {}", copy.node),
            json!({ "node": copy.node }),
        ));
    }
    let uses = used_live(&branch.working);
    let lineage = uses
        .keys()
        .find(|lineage| branch.live.get(*lineage).map(String::as_str) == Some(copy.node.as_str()))
        .cloned()
        .ok_or_else(|| {
            let names: Vec<&str> = branch.live.values().map(String::as_str).collect();
            error::choices(
                format!("This Branch uses no node named {} live", copy.node),
                copy.node.as_str(),
                names.iter().copied(),
            )
        })?;
    settled(tx, &branch)?;
    let owner = chain(tx, &row.parent)?
        .into_iter()
        .find(|ancestor| holds(&ancestor.applied, &lineage))
        .ok_or_else(|| {
            error::conflict(
                format!("Nothing runs {}, so there is nothing to copy", copy.node),
                json!({ "node": copy.node }),
            )
        })?;
    // An Own Copy is an introduction: neither it nor the Volumes it mounts that the
    // Branch lacks are in the base or provided live.
    let mut copied = BTreeSet::from([lineage]);
    if let Some(service) = owner
        .applied
        .services
        .iter()
        .find(|service| copied.contains(&service.lineage_id))
    {
        for mount in &service.volume_attachments {
            let volume = owner
                .applied
                .volumes
                .iter()
                .find(|volume| volume.resource_id == mount.volume_resource_id);
            if let Some(volume) = volume
                && !holds(&branch.working, &volume.resource_lineage_id)
            {
                copied.insert(volume.resource_lineage_id.clone());
            }
        }
    }
    let mut base = row.base;
    base.services
        .retain(|service| !copied.contains(&service.lineage_id));
    base.volumes
        .retain(|volume| !copied.contains(&volume.resource_lineage_id));
    let provided = uses
        .into_keys()
        .filter(|lineage| !copied.contains(lineage))
        .collect();
    let mut moving = Moving::update(tx, &owner.environment, owner.applied, &branch, base)?;
    moving.nothing = format!("Nothing to copy from {}", owner.environment.summary.name);
    moving.provided = provided;
    // Every change of the copy, variables with the owner's values.
    let picks: Vec<BranchPick> = moving
        .compare(&branch.working, None)?
        .rows
        .iter()
        .filter_map(|row| {
            let BranchRole::Move { choice, .. } = &row.role else {
                return None;
            };
            let key = row.key.to_string();
            copied.contains(split(&key).0).then(|| BranchPick {
                choice: choice.as_ref().map(|_| BranchPickChoice::From),
                key,
            })
        })
        .collect();
    if picks.is_empty() {
        return Err(error::conflict(moving.nothing, json!({})));
    }
    let staged = moving.apply(tx, who, &mut branch, picks)?;
    Ok(Branched {
        branch: view(tx, &branch)?,
        staged,
        immediate: Vec::new(),
    })
}

pub(crate) fn keep_branch(
    tx: &mut dyn Tx,
    who: &Actor,
    keep: &KeepBranch,
) -> Result<Branched, RpcError> {
    let branch = scope::lock(tx, who, &keep.environment)?;
    let row = branch_row(tx, &branch)?;
    let mut immediate = Vec::new();
    if row.kept != keep.kept {
        tx.execute(
            "UPDATE config_environment_branch SET kept = ?1 WHERE environment_id = ?2",
            &[
                i64::from(keep.kept).into(),
                branch.summary.id.as_str().into(),
            ],
        )?;
        immediate.push("kept".to_owned());
    }
    Ok(Branched {
        branch: view(tx, &branch)?,
        staged: Vec::new(),
        immediate,
    })
}

/// The changed Services `parent`'s failed Deployment `id` didn't apply, as it
/// configured them: those it would have changed from what `parent` runs.
pub(super) fn failed_services(
    tx: &mut dyn Tx,
    who: &Actor,
    parent: &Environment,
    applied: &SavedEnvironmentIntent,
    id: &DeploymentId,
) -> Result<Vec<SavedServiceIntent>, RpcError> {
    let view = deployment::view(tx, who, id)?;
    if view.environment.id != parent.summary.id {
        return Err(error::invalid(
            format!("Deployment {id} is not one of {}", parent.summary.name),
            json!({ "fix": id }),
        ));
    }
    if view.deployment.status != DeploymentStatus::Failed {
        return Err(error::invalid(
            "Only a failed Deployment is fixed on a Branch",
            json!({ "fix": id }),
        ));
    }
    let saved = deployment::saved_at(tx, &parent.summary.id, view.deployment.saved)?;
    let applied = canonicalize_environment_intent(applied.clone());
    Ok(saved
        .services
        .into_iter()
        .filter(|service| {
            view.nodes
                .iter()
                .any(|node| node.node.id() == service.id && !node.outcome.advances())
                && !applied.services.contains(service)
        })
        .collect())
}

/// A Setup Command's command, trimmed, or why it is refused.
pub(crate) fn setup_command(setup: &SetupCommand) -> Result<String, RpcError> {
    let command = setup.command.trim();
    if command.is_empty() || command.chars().count() > COMMAND_MAX {
        return Err(error::invalid(
            format!(
                "{}: a Setup Command has 1-{COMMAND_MAX} characters",
                setup.service
            ),
            json!({ "service": setup.service }),
        ));
    }
    Ok(command.to_owned())
}
