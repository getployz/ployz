//! How two Environments compare: which base, what each side sends, how a variable
//! lands by default, and landing. Save, Update, Own Copy, a new Branch, the pull
//! request page's counts and Conditional Save all compare through here; nothing
//! else builds a comparison.

use super::*;

/// One move between a Branch and an Environment it comes from: `from`'s changes
/// over `base`, into the other side.
pub(crate) struct Moving {
    /// The Environment `from` belongs to: what arriving nodes carry comes from it.
    pub(crate) source: EnvironmentId,
    /// The Branch whose base advances by what lands.
    pub(crate) branch: EnvironmentId,
    /// Why nothing moves.
    pub(crate) nothing: String,
    pub(crate) from: SavedEnvironmentIntent,
    pub(crate) base: SavedEnvironmentIntent,
    /// Lineages the receiver uses live.
    provided: Vec<String>,
    pub(crate) hostnames: BranchHostnames,
    way: Way,
}

/// Which way a move goes, with what only that way needs.
#[derive(Clone, Debug)]
enum Way {
    /// Into a Branch: every variable lands with `from`'s value by default.
    Update,
    /// Into a Parent, or a PR Environment's Destination: nothing there is deleted.
    Save {
        /// The Parent's deployed values, offered for a variable; none when it
        /// deployed nothing.
        parent: Option<SavedEnvironmentIntent>,
        /// Whether `from` is a kept Branch: its secrets then stay by default.
        from_kept: bool,
    },
}

/// `id`'s Branch row: every move has a Branch side.
fn row_of(tx: &mut dyn Tx, id: &EnvironmentId) -> Result<Row, RpcError> {
    row(tx, id)?.ok_or_else(|| error::corrupt("Branch"))
}

impl Moving {
    /// What `parent` deployed (its Applied State) into `branch`, over `branch`'s base.
    pub(super) fn update(
        tx: &mut dyn Tx,
        parent: &Environment,
        branch: &Environment,
    ) -> Result<Self, RpcError> {
        Ok(Self {
            source: parent.summary.id.clone(),
            branch: branch.summary.id.clone(),
            nothing: format!("Nothing new in {}", parent.summary.name),
            from: deployment::head(tx, parent)?.applied,
            base: row_of(tx, &branch.summary.id)?.base,
            provided: used_live(&branch.working).into_keys().collect(),
            hostnames: BranchHostnames {
                from: suffix(tx, parent)?,
                into: suffix(tx, branch)?,
            },
            way: Way::Update,
        })
    }

    /// An Own Copy of the `copied` lineages from `owner` into `branch`: an Update in
    /// which they are neither in the base nor used live, so they arrive as new.
    pub(super) fn copy(
        tx: &mut dyn Tx,
        owner: &Environment,
        branch: &Environment,
        copied: &BTreeSet<String>,
    ) -> Result<Self, RpcError> {
        let mut moving = Self::update(tx, owner, branch)?;
        moving.nothing = format!("Nothing to copy from {}", owner.summary.name);
        moving
            .base
            .services
            .retain(|service| !copied.contains(&service.lineage_id));
        moving
            .base
            .volumes
            .retain(|volume| !copied.contains(&volume.resource_lineage_id));
        moving.provided.retain(|lineage| !copied.contains(lineage));
        Ok(moving)
    }

    /// Branch `from`'s Working State into `into` over `from`'s base, with what its
    /// Parent deployed on offer.
    pub(crate) fn save(
        tx: &mut dyn Tx,
        from: &Environment,
        into: &Environment,
    ) -> Result<Self, RpcError> {
        let row = row_of(tx, &from.summary.id)?;
        let parent = scope::load_by_id(tx, &row.parent)?;
        let parent = deployment::head(tx, &parent)?.applied;
        let deployed = !(parent.services.is_empty() && parent.volumes.is_empty());
        Ok(Self {
            source: from.summary.id.clone(),
            branch: from.summary.id.clone(),
            nothing: format!("Nothing to save into {}", into.summary.name),
            from: from.working.clone(),
            base: row.base,
            provided: used_live(&into.working).into_keys().collect(),
            hostnames: BranchHostnames {
                from: suffix(tx, from)?,
                into: suffix(tx, into)?,
            },
            way: Way::Save {
                parent: deployed.then_some(parent),
                from_kept: row.kept,
            },
        })
    }

    /// A saved move landing later: `from`'s changes over `base` from Branch
    /// `branch`, with `parent`'s deployed values on offer, into `into`.
    pub(crate) fn landed(
        branch: &EnvironmentId,
        (from, base): (&SavedEnvironmentIntent, &SavedEnvironmentIntent),
        hostnames: &BranchHostnames,
        parent: Option<&SavedEnvironmentIntent>,
        into: &SavedEnvironmentIntent,
    ) -> Self {
        Self {
            source: branch.clone(),
            branch: branch.clone(),
            nothing: "Nothing left to land".to_owned(),
            from: from.clone(),
            base: base.clone(),
            provided: used_live(into).into_keys().collect(),
            hostnames: hostnames.clone(),
            way: Way::Save {
                parent: parent.cloned(),
                from_kept: false,
            },
        }
    }

    /// A Save's deployed Parent values on offer; none for an Update.
    pub(crate) fn parent(&self) -> Option<&SavedEnvironmentIntent> {
        match &self.way {
            Way::Save { parent, .. } => parent.as_ref(),
            Way::Update => None,
        }
    }

    pub(crate) fn compare(
        &self,
        into: &SavedEnvironmentIntent,
        picks: Option<Vec<BranchPick>>,
    ) -> Result<BranchChanges, RpcError> {
        let (parent, from_kept) = match &self.way {
            Way::Update => (None, false),
            Way::Save { parent, from_kept } => (parent.as_ref(), *from_kept),
        };
        compare(Comparing {
            base: Some(&self.base),
            from: &self.from,
            into,
            parent,
            provided: &self.provided,
            hostnames: &self.hostnames,
            from_kept,
            picks,
        })
    }

    /// How a variable lands unless picked otherwise.
    pub(super) fn default(&self, offered: &BranchChoice) -> BranchOption {
        match self.way {
            Way::Update => BranchOption::From,
            Way::Save { .. } => offered.default,
        }
    }

    /// Row `key` as `NODE[.path]` in the catalog's words (`web.image`,
    /// `web.env.KEY`), by the node's name where it comes from, else in `into`.
    pub(crate) fn name(&self, into: &SavedEnvironmentIntent, key: &str) -> String {
        let (lineage, path) = split(key);
        let name_in = |lineage: &str| {
            name_of(&self.from, lineage)
                .or_else(|| name_of(into, lineage))
                .unwrap_or_else(|| lineage.to_owned())
        };
        let node = name_in(lineage);
        if path == "node" {
            return node;
        }
        SettingPath::from_core(&node, path, name_in)
    }

    /// Land `picks` in `into` and advance the Branch's base by exactly what landed.
    /// Into a Parent, nothing there is deleted.
    pub(super) fn apply(
        &self,
        tx: &mut dyn Tx,
        who: &Actor,
        into: &mut Environment,
        picks: Vec<BranchPick>,
    ) -> Result<Vec<NodeName>, RpcError> {
        let moved = self.compare(&into.working, Some(picks.clone()))?;
        let base = moved
            .base
            .ok_or_else(|| error::internal("Core returned no base for a move"))?;
        let ids = |intent: &SavedEnvironmentIntent| -> BTreeSet<String> {
            let services = intent.services.iter().map(|service| service.id.clone());
            let volumes = intent
                .volumes
                .iter()
                .map(|volume| volume.resource_id.clone());
            services.chain(volumes).collect()
        };
        let save = matches!(self.way, Way::Save { .. });
        if save && !ids(&into.working).is_subset(&ids(&moved.next)) {
            return Err(error::internal("A Save would delete from the Parent"));
        }
        let carried = Carried::of(tx, &self.source, &self.from)?;
        let staged = land(tx, who, into, (&self.from, &carried), moved.next, &picks)?;
        tx.execute(
            "UPDATE config_environment_branch SET base = ?1 WHERE environment_id = ?2",
            &[document(&base).as_str().into(), self.branch.as_str().into()],
        )?;
        Ok(staged)
    }
}

/// A row key's lineage and path.
pub(super) fn split(key: &str) -> (&str, &str) {
    key.split_once(':').unwrap_or((key, ""))
}

/// What the Services of one side of a move carry besides their configuration, by
/// Service ID: their sealed registry credentials and their Deployment Policies.
/// Captured when the move is made, so landing never reads a side that may be gone.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Carried {
    credentials: BTreeMap<String, ployz_core::config::EncryptedSecretValue>,
    policies: BTreeMap<String, Policy>,
}

impl Carried {
    /// What the Services of `from`, as Environment `source` holds them, carry.
    pub(crate) fn of(
        tx: &mut dyn Tx,
        source: &EnvironmentId,
        from: &SavedEnvironmentIntent,
    ) -> Result<Self, RpcError> {
        let mut carried = Self::default();
        for service in &from.services {
            if let Some(sealed) = registry::sealed(tx, source, &service.id)? {
                carried.credentials.insert(service.id.clone(), sealed);
            }
            if let Some(policy) = policy::stored(tx, source, &service.id)? {
                carried.policies.insert(service.id.clone(), policy);
            }
        }
        Ok(carried)
    }
}

/// Land core's `next` as `branch`'s Working State: the nodes arriving from `from`
/// bring their registry credentials and Deployment Policies (`carried`) and get
/// their Node Introductions, and a picked credential row brings its credential to
/// the Service already there. Returns the nodes staged.
pub(crate) fn land(
    tx: &mut dyn Tx,
    who: &Actor,
    branch: &mut Environment,
    (from, carried): (&SavedEnvironmentIntent, &Carried),
    next: SavedEnvironmentIntent,
    picks: &[BranchPick],
) -> Result<Vec<NodeName>, RpcError> {
    let before = std::mem::replace(&mut branch.working, next);
    crate::volume::check_storage(tx, &branch.summary.id, &branch.working)?;
    // What arrives from `from` may bring a rule it already broke; that's not new.
    scope::save_working_from(tx, branch, Some(from))?;
    let id = branch.summary.id.clone();
    let source_of = |lineage: &str| {
        from.services
            .iter()
            .find(|service| service.lineage_id == lineage)
            .map(|service| service.id.clone())
    };
    let mut staged = Vec::new();
    for service in &branch.working.services {
        let old = before.services.iter().find(|old| old.id == service.id);
        if old == Some(service) {
            continue;
        }
        staged.push(NodeName::Service(
            ServiceName::parse(service.slug.as_str())
                .map_err(|_| error::corrupt("Service name"))?,
        ));
        if old.is_some() {
            continue;
        }
        let credentials = matches!(
            service.config.source,
            ServiceSource::Image {
                credentials: ServiceImageCredentials::Configured { .. },
                ..
            }
        );
        if let Some(source) = source_of(&service.lineage_id) {
            if credentials && let Some(sealed) = carried.credentials.get(&source) {
                registry::store(tx, who, &id, &service.id, sealed)?;
            }
            if let Some(policy) = carried.policies.get(&source) {
                policy::store(tx, who, &id, &service.id, policy)?;
            }
        }
        scope::introduce(tx, who, &id, scope::Node::Service(service))?;
    }
    for pick in picks {
        let Some(lineage) = pick.key.strip_suffix(":source.credentials") else {
            continue;
        };
        let receiver = before.services.iter().find(|old| old.lineage_id == lineage);
        let sealed = source_of(lineage).and_then(|source| carried.credentials.get(&source));
        if let (Some(receiver), Some(sealed)) = (receiver, sealed) {
            registry::store(tx, who, &id, &receiver.id, sealed)?;
        }
    }
    for volume in &branch.working.volumes {
        if !before
            .volumes
            .iter()
            .any(|old| old.resource_id == volume.resource_id)
        {
            staged.push(NodeName::Volume(
                VolumeName::parse(volume.name.as_str())
                    .map_err(|_| error::corrupt("Volume name"))?,
            ));
            scope::introduce(tx, who, &id, scope::Node::Volume(volume))?;
        }
    }
    staged.sort();
    Ok(staged)
}

/// A new Branch: `from` arriving in its empty Working State `into`, but for what it
/// uses `live`, moving `picks`. There is no base yet: the comparison's new base is
/// what the Branch is made with.
pub(super) fn creating(
    from: &SavedEnvironmentIntent,
    into: &SavedEnvironmentIntent,
    live: &[String],
    hostnames: &BranchHostnames,
    picks: Vec<BranchPick>,
) -> Result<BranchChanges, RpcError> {
    compare(Comparing {
        base: None,
        from,
        into,
        parent: None,
        provided: live,
        hostnames,
        from_kept: false,
        picks: Some(picks),
    })
}

/// How many rows a Save of Branch `from` into `into` offers.
pub(crate) fn changes_into(
    tx: &mut dyn Tx,
    from: &Environment,
    into: &Environment,
) -> Result<usize, RpcError> {
    Ok(Moving::save(tx, from, into)?
        .compare(&into.working, None)?
        .rows
        .iter()
        .filter(|row| matches!(row.role, BranchRole::Move { .. }))
        .count())
}

/// The sides of core's comparison: `from` and `into` over `base`, moving `picks` when given.
struct Comparing<'a> {
    base: Option<&'a SavedEnvironmentIntent>,
    from: &'a SavedEnvironmentIntent,
    into: &'a SavedEnvironmentIntent,
    parent: Option<&'a SavedEnvironmentIntent>,
    provided: &'a [String],
    hostnames: &'a BranchHostnames,
    from_kept: bool,
    picks: Option<Vec<BranchPick>>,
}

fn compare(sides: Comparing<'_>) -> Result<BranchChanges, RpcError> {
    let value = |intent: &SavedEnvironmentIntent| {
        serde_json::to_value(intent).expect("Working State is JSON")
    };
    branch_changes(BranchChangesInput {
        base: sides.base.map(value),
        from: value(sides.from),
        into: value(sides.into),
        parent: sides.parent.map(value),
        provided: sides.provided.to_vec(),
        hostnames: sides.hostnames.clone(),
        from_kept: sides.from_kept,
        picks: sides.picks,
    })
    .map_err(config)
}
