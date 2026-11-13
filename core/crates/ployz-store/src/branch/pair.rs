//! How changes move between two Environments: which base, which rules, and the ledger
//! of what arrived. Sync, Follow, a take and Own Copy all move through [`Move`];
//! nothing else lands one Environment's rows in another.
//!
//! A pair's base is what the two last shared (`config_sync_base`, keyed by the pair's
//! IDs in order), advanced by what lands. Each landed row is an arrival
//! (`config_sync_arrival`): until the receiver deploys it, a discard puts the base's
//! prior cell back so it is offered again, and Undo puts the receiver's own back.

use super::*;
use crate::Trusted;
use crate::digest::short_digest;
use crate::settings::ServiceSetting;

/// What a write checks the receiver against before anything lands. Only the Store
/// builds one: a client always sends the version it reviewed.
pub(crate) enum Guard<'version> {
    /// The Sync view's version.
    Sync(&'version str),
    /// The receiver's diff version.
    Diff(&'version str),
    /// The receiver's Working State revision.
    Revision(Option<Revision>),
    /// A Parent's deploy following into its Branch: nobody reviewed it.
    Deploy,
}

/// One way changes move into one receiver, ready to plan against its Working State.
pub(crate) struct Move {
    /// The Environment `from` belongs to: what arriving nodes carry comes from it.
    source: EnvironmentId,
    /// The receiver's side of the pair whose base this plans over.
    other: EnvironmentId,
    /// The receiver: where the rows land.
    receiver: EnvironmentId,
    /// Why nothing moves.
    nothing: String,
    pub(crate) from: SavedEnvironmentIntent,
    pub(crate) base: SavedEnvironmentIntent,
    pub(crate) hostnames: Hostnames,
    pub(crate) rules: Rules,
}

/// Who owns what a [`Checked::apply`] lands.
#[derive(Clone, Copy)]
pub(crate) enum Owner<'a> {
    /// Nobody: a Follow, a take of Use hints, and Own Copy. A row a Sync landed keeps
    /// that Sync's claim under one (see [`ledger::landing`]).
    None,
    /// Proposal `id`, included by Sync `sync`: a Sync.
    Proposal {
        id: &'a ProposalId,
        sync: &'a SyncId,
    },
}

/// A plan of a [`Move`] whose guard held, ready to land.
pub(crate) struct Checked<'way> {
    of: &'way Move,
    plan: Plan,
    version: String,
}

impl Move {
    /// What `parent` deployed (its Applied State) into `branch`, over the base `branch`
    /// shares with its Parent.
    pub(super) fn follow(
        tx: &mut dyn Tx,
        parent: &Environment,
        branch: &Environment,
    ) -> Result<Self, RpcError> {
        let nothing = format!("Nothing new in {}", parent.summary.name);
        Self::deployed(tx, (parent, branch), Way::Follow, nothing, &BTreeSet::new())
    }

    /// What `owner` deployed into `branch`, over the base `branch` shares with its
    /// Parent, but for the `copied` lineages, which are neither in the base nor used
    /// live.
    fn deployed(
        tx: &mut dyn Tx,
        (owner, branch): (&Environment, &Environment),
        way: Way,
        nothing: String,
        copied: &BTreeSet<String>,
    ) -> Result<Self, RpcError> {
        let other = row(tx, &branch.summary.id)?
            .ok_or_else(|| error::corrupt("Branch"))?
            .parent;
        let mut base = base_of(tx, (&branch.summary.id, &other))?;
        base.services
            .retain(|service| !copied.contains(&service.lineage_id));
        base.volumes
            .retain(|volume| !copied.contains(&volume.resource_lineage_id));
        base.configs
            .retain(|config| !copied.contains(&config.resource_lineage_id));
        let (from_marks, into_marks) = marks(tx, &owner.summary.id, &branch.summary.id)?;
        Ok(Self {
            source: owner.summary.id.clone(),
            receiver: branch.summary.id.clone(),
            nothing,
            from: deployment::head(tx, owner)?.applied,
            base,
            other,
            hostnames: Hostnames {
                from: suffix(tx, owner)?,
                into: suffix(tx, branch)?,
            },
            rules: Rules {
                way,
                from_marks,
                into_marks,
                live: used_live(&branch.working)
                    .into_keys()
                    .filter(|lineage| !copied.contains(lineage))
                    .collect(),
                own: None,
                held: BTreeSet::new(),
                accepted: BTreeMap::new(),
            },
        })
    }

    /// A Sync, now or at a pull request's merge: `from`'s Working State into `into`,
    /// over what the two last shared. A pair that never synced compares over where
    /// `from` was made, else where `into` was made, else `into` itself.
    pub(crate) fn sync(
        tx: &mut dyn Tx,
        from: &Environment,
        into: &Environment,
    ) -> Result<Self, RpcError> {
        let pair = (&from.summary.id, &into.summary.id);
        let base = match stored(tx, pair)? {
            Some(base) => base,
            None => match rows::made_with(tx, &from.summary.id)? {
                Some(base) => base,
                None => {
                    rows::made_with(tx, &into.summary.id)?.unwrap_or_else(|| into.working.clone())
                }
            },
        };
        // Syncing anywhere but into its Parent, a Branch's own changes are those since
        // it last shared with its Parent; the rest it only inherited. A change still
        // proposed in the Parent's draft is not shared until the Parent saves it.
        let own = match row(tx, &from.summary.id)? {
            Some(row) if row.parent != into.summary.id => {
                let parent = base_of(tx, (&from.summary.id, &row.parent))?;
                let mut own = changed(&parent, &from.working);
                own.extend(
                    arrived(tx, &row.parent, Which::From(&from.summary.id))?
                        .into_iter()
                        .filter(|arrived| {
                            matches!(
                                arrived.arrival,
                                Arrival::Pending {
                                    claim: Claim::Owned { .. },
                                    ..
                                }
                            )
                        })
                        .map(|arrived| arrived.row),
                );
                Some(own)
            }
            _ => None,
        };
        let (from_marks, into_marks) = marks(tx, &from.summary.id, &into.summary.id)?;
        Ok(Self {
            source: from.summary.id.clone(),
            other: from.summary.id.clone(),
            receiver: into.summary.id.clone(),
            nothing: format!("Nothing to sync into {}", into.summary.name),
            from: from.working.clone(),
            base,
            hostnames: Hostnames {
                from: suffix(tx, from)?,
                into: suffix(tx, into)?,
            },
            rules: Rules {
                way: Way::Sync,
                from_marks,
                into_marks,
                live: used_live(&into.working).into_keys().collect(),
                own,
                held: BTreeSet::new(),
                accepted: BTreeMap::new(),
            },
        })
    }

    /// What a pull request's merge lands from `from` into `into`: a Sync whose
    /// removals move too and whose rows land even where `into` changed them since.
    pub(crate) fn merge(
        tx: &mut dyn Tx,
        from: &Environment,
        into: &Environment,
    ) -> Result<Self, RpcError> {
        let mut way = Self::sync(tx, from, into)?;
        way.rules.way = Way::Merge;
        way.nothing = format!("Nothing lands in {}", into.summary.name);
        // The base holds every node `from` was made with, copied or not. Only a node
        // `from` introduced and no longer has is one it removed.
        let introduced = review::introductions(tx, from)?;
        let stays: BTreeSet<&str> = [&way.from, &introduced]
            .into_iter()
            .flat_map(|intent| {
                let services = intent.services.iter().map(|s| s.lineage_id.as_str());
                let volumes = intent
                    .volumes
                    .iter()
                    .map(|v| v.resource_lineage_id.as_str());
                let configs = intent
                    .configs
                    .iter()
                    .map(|c| c.resource_lineage_id.as_str());
                services.chain(volumes).chain(configs)
            })
            .collect();
        let base = &mut way.base;
        base.services
            .retain(|s| stays.contains(s.lineage_id.as_str()));
        base.volumes
            .retain(|v| stays.contains(v.resource_lineage_id.as_str()));
        base.configs
            .retain(|c| stays.contains(c.resource_lineage_id.as_str()));
        Ok(way)
    }

    /// A Sync now of `from` into `into` that includes it: the rows other proposals
    /// own there are held, and the rows `from`'s own proposal owns compare against
    /// what it accepted and count as its own.
    pub(crate) fn include(
        tx: &mut dyn Tx,
        from: &Environment,
        into: &Environment,
        owners: &Owners,
    ) -> Result<Self, RpcError> {
        let mut way = Self::sync(tx, from, into)?;
        way.rules.held = owners.held.keys().cloned().collect();
        way.rules.accepted = owners.accepted.clone();
        if let Some(own) = &mut way.rules.own {
            own.extend(owners.owned.iter().cloned());
        }
        Ok(way)
    }

    /// An Own Copy of the `copied` lineages from `owner` into `branch`: what `owner`
    /// runs, with them neither in the base nor used live, so they arrive as new.
    pub(super) fn copy(
        tx: &mut dyn Tx,
        owner: &Environment,
        branch: &Environment,
        copied: &BTreeSet<String>,
    ) -> Result<Self, RpcError> {
        let nothing = format!("Nothing to copy from {}", owner.summary.name);
        let mut way = Self::deployed(tx, (owner, branch), Way::Copy, nothing, copied)?;
        review::sealed::seal_running(tx, owner, &mut way.from, copied)?;
        Ok(way)
    }

    /// The rows from `from` into `into`.
    pub(crate) fn plan(&self, into: &SavedEnvironmentIntent) -> Plan {
        let plan = plan(
            Sides {
                base: Some(&self.base),
                from: &self.from,
                into,
                hostnames: self.hostnames.clone(),
            },
            &self.rules,
        );
        landing_in(plan, &self.receiver)
    }

    /// Plan into `into` and refuse unless `guard` holds.
    pub(crate) fn check(
        &self,
        tx: &mut dyn Tx,
        into: &Environment,
        guard: Guard,
    ) -> Result<Checked<'_>, RpcError> {
        let plan = self.plan(&into.working);
        let version = version(into, &plan);
        match guard {
            Guard::Sync(asked) if asked != version => {
                return Err(error::conflict(
                    "Changed since you reviewed: review the sync again",
                    json!({ "version": version }),
                ));
            }
            Guard::Diff(asked) => review::check(&review::review(tx, into)?, Some(asked))?,
            Guard::Revision(expect) => into.expect(expect)?,
            Guard::Sync(_) | Guard::Deploy => {}
        }
        Ok(Checked {
            of: self,
            plan,
            version,
        })
    }
}

impl Checked<'_> {
    pub(crate) fn rows(&self) -> &[PlannedRow] {
        self.plan.rows()
    }

    pub(crate) fn plan(&self) -> &Plan {
        &self.plan
    }

    /// Land `picks` in `into`, `values` filling the secrets that need one, and advance
    /// the pair's base by exactly what landed, recording each row's arrival for
    /// `owner` over what arrived there from that side before ([`ledger::landing`]).
    /// Arrivals are recorded before the rows land, so the release that saving the
    /// Working State runs sees them as they landed. Returns the nodes staged.
    pub(crate) fn apply(
        &self,
        tx: &mut dyn Tx,
        who: &Actor,
        into: &mut Environment,
        picks: &BTreeSet<RowId>,
        values: &BTreeMap<RowId, SealedCell>,
        (owner, trusted): (Owner<'_>, &Trusted),
    ) -> Result<Vec<NodeName>, RpcError> {
        let of = self.of;
        if picks.is_empty() {
            return Err(error::conflict(
                of.nothing.clone(),
                json!({ "version": self.version }),
            ));
        }
        crate::domain::refuse_long_subdomains(&self.plan, picks)?;
        let applied = self.plan.apply(picks, values).map_err(config)?;
        let carried = Carried::of(tx, &of.source, &of.from)?;
        let how = match of.rules.way {
            Way::Follow => How::Follow,
            Way::Sync | Way::Merge | Way::Copy => How::Sync,
        };
        let stored: BTreeMap<RowId, Arrival> =
            arrived(tx, &into.summary.id, Which::From(&of.other))?
                .into_iter()
                .map(|arrived| (arrived.row, arrived.arrival))
                .collect();
        for landed in applied.landed {
            let stored = stored.get(&landed.row);
            let (arrival, kept) = ledger::landing(stored, owner, (landed.prior, landed.was));
            let from = self
                .plan
                .rows()
                .iter()
                .find(|row| row.id == landed.row)
                .map_or(Cell::Absent, |row| row.from.clone());
            let arrived = Arrived {
                other: of.other.clone(),
                row: landed.row,
                how,
                from,
                value: landed.value,
                arrival,
            };
            ledger::write(tx, who, &into.summary.id, &arrived, (stored, kept))?;
        }
        let read = crate::rules::removed_lineages(&applied.next, &into.live);
        let source_removed = scope::removed_of(tx, &of.source)?;
        into.removed
            .append(&source_removed, |lineage| read.contains(lineage));
        let staged = land(
            tx,
            who,
            into,
            (&of.from, &carried),
            applied.next,
            picks,
            trusted,
        )?;
        share(tx, (&into.summary.id, &of.other), &applied.base)?;
        Ok(staged)
    }
}

/// `plan` landing in Environment `environment`, so a value the browser sealed for
/// another Environment arrives Not set.
pub(crate) fn landing_in(plan: Plan, environment: &EnvironmentId) -> Plan {
    match uuid::Uuid::parse_str(environment.as_str()) {
        Ok(id) => plan.receiving(id),
        Err(_) => plan,
    }
}

/// A Sync's guard: the receiver's revision and the rows as planned, ticks included.
pub(super) fn version(into: &Environment, plan: &Plan) -> String {
    format!("{}:{}", into.summary.revision, short_digest(&plan.digest()))
}

/// The rows at which `working` holds other than `base`.
fn changed(base: &SavedEnvironmentIntent, working: &SavedEnvironmentIntent) -> BTreeSet<RowId> {
    let rows = plan(
        Sides {
            base: Some(base),
            from: working,
            into: base,
            hostnames: Hostnames {
                from: String::new(),
                into: String::new(),
            },
        },
        &Rules::new(Way::Copy),
    );
    rows.rows().iter().map(|row| row.id.clone()).collect()
}

/// `picks` without a row whose new node isn't picked too.
pub(crate) fn whole(rows: &[PlannedRow], mut picks: BTreeSet<RowId>) -> BTreeSet<RowId> {
    // A node's own row requires nothing, so one pass sees every node as picked.
    for row in rows {
        if row
            .requires
            .as_ref()
            .is_some_and(|node| !picks.contains(node))
        {
            picks.remove(&row.id);
        }
    }
    picks
}

pub(crate) fn json_of(cell: &impl Serialize) -> String {
    serde_json::to_string(cell).expect("a cell is JSON")
}

/// The row a table keys by `lineage` and `at`.
pub(crate) fn row_id(lineage: &str, at: &str) -> Result<RowId, RpcError> {
    format!("{lineage}:{at}")
        .parse()
        .map_err(|_| error::corrupt("Sync row"))
}

/// `row` named in the first of `intents` that has it: its node, and where in it as
/// the catalog says it (`image`, `env.KEY`, `mounts.VOLUME`).
pub(crate) fn named(intents: &[&SavedEnvironmentIntent], row: &RowId) -> Option<NamedRow> {
    intents.iter().find_map(|intent| {
        let (node, at) = ployz_core::config::name_of(intent, row)?;
        let (node, kind) = match node {
            NodeRef::Service(service) => (
                NodeName::Service(ServiceName::parse(service.slug.as_str()).ok()?),
                EnvironmentNodeType::Service,
            ),
            NodeRef::Volume(volume) => (
                NodeName::Volume(VolumeName::parse(volume.name.as_str()).ok()?),
                EnvironmentNodeType::Volume,
            ),
            NodeRef::Config(config) => (
                NodeName::Config(config.name.clone().into()),
                EnvironmentNodeType::Config,
            ),
        };
        let name = match row.at() {
            At::Node => None,
            At::Variable(key) => Some(format!("env.{key}")),
            At::Setting(setting) => {
                Some(ServiceSetting::of(*setting).map_or(at, |setting| setting.name().to_owned()))
            }
            At::Data | At::Name | At::Storage | At::Mount(_) | At::File(_) | At::ConfigMount(_) => {
                Some(at)
            }
        };
        Some(NamedRow {
            row: row.clone(),
            node,
            kind,
            name,
        })
    })
}

/// Each of `rows` by every name it has in `intents`: a row renamed on one side
/// answers to either name. What [`resolve`] resolves against.
pub(crate) fn named_in<'row>(
    intents: &[&SavedEnvironmentIntent],
    rows: impl IntoIterator<Item = &'row RowId>,
) -> Vec<NamedRow> {
    rows.into_iter()
        .flat_map(|row| intents.iter().filter_map(|intent| named(&[intent], row)))
        .collect()
}

/// `cell`, `intent`'s at `row`, as reads show it: text for a variable with references
/// by `names`, `${{ secret(…) }}` for a generated one, `{"secret": true}`, or
/// `{"secret": false}` for one without a value.
pub(crate) fn shown(
    intent: &SavedEnvironmentIntent,
    names: &BTreeMap<String, String>,
    row: &RowId,
    cell: &Cell,
) -> Value {
    match cell {
        Cell::Absent => Value::Null,
        Cell::Secret { .. } => json!({ "secret": true }),
        Cell::SecretWithoutValue => json!({ "secret": false }),
        // Each Environment makes its own value: a row moves the generator, never a value.
        Cell::Value(_) if let Some(generator) = ployz_core::config::cell_generator(cell) => {
            json!(generator.to_string())
        }
        Cell::Value(value) => match row.at() {
            At::Variable(key) => Some(key),
            At::File(_) => {
                return crate::config_item::shown_file(value.clone(), names);
            }
            At::Node
            | At::Data
            | At::Name
            | At::Storage
            | At::Setting(_)
            | At::Mount(_)
            | At::ConfigMount(_) => None,
        }
        .and_then(|key| {
            intent
                .services
                .iter()
                .find(|service| service.lineage_id == row.lineage())?
                .variables
                .iter()
                .find(|variable| variable.key == *key)
        })
        .map_or_else(
            || crate::settings::shown(&row.at().to_string(), value.clone()),
            |variable| crate::variables::shown(variable, names),
        ),
    }
}

/// What the pair `ids` last shared: a Branch and its Parent always have one.
pub(crate) fn base_of(
    tx: &mut dyn Tx,
    ids: (&EnvironmentId, &EnvironmentId),
) -> Result<SavedEnvironmentIntent, RpcError> {
    stored(tx, ids)?.ok_or_else(|| error::corrupt("Sync base"))
}

/// The pair `a`, `b` as `config_sync_base` keys it: its IDs in order.
fn ordered<'id>(a: &'id EnvironmentId, b: &'id EnvironmentId) -> [&'id str; 2] {
    let (a, b) = (a.as_str(), b.as_str());
    if a <= b { [a, b] } else { [b, a] }
}

/// What the pair last shared; none when it never synced.
pub(super) fn stored(
    tx: &mut dyn Tx,
    (a, b): (&EnvironmentId, &EnvironmentId),
) -> Result<Option<SavedEnvironmentIntent>, RpcError> {
    let [a, b] = ordered(a, b);
    tx.query(
        "SELECT base FROM config_sync_base WHERE environment_id = ?1 AND other_id = ?2",
        &[a.into(), b.into()],
    )?
    .first()
    .map(|row| row.intent(0, "Sync base"))
    .transpose()
}

/// What `a` and `b` last shared is `base` now.
pub(crate) fn share(
    tx: &mut dyn Tx,
    (a, b): (&EnvironmentId, &EnvironmentId),
    base: &SavedEnvironmentIntent,
) -> Result<(), RpcError> {
    let [a, b] = ordered(a, b);
    tx.execute(
        "INSERT INTO config_sync_base (environment_id, other_id, organization_id, base) \
         SELECT id, ?2, organization_id, ?3 FROM config_environment WHERE id = ?1 \
         AND EXISTS (SELECT 1 FROM config_environment WHERE id = ?2) \
         ON CONFLICT (environment_id, other_id) DO UPDATE SET base = excluded.base",
        &[a.into(), b.into(), document(base).as_str().into()],
    )?;
    Ok(())
}

/// Put `cells` into the bases `receiver` shares with each other side, as one write
/// per pair. A pair gone with its other side has no base left to put back.
pub(super) fn rewind_bases(
    tx: &mut dyn Tx,
    receiver: &EnvironmentId,
    cells: Vec<(EnvironmentId, RowId, Cell)>,
) -> Result<(), RpcError> {
    let mut bases: BTreeMap<EnvironmentId, SavedEnvironmentIntent> = BTreeMap::new();
    for (other, row, cell) in cells {
        let base = match bases.remove(&other) {
            Some(base) => base,
            None => match stored(tx, (receiver, &other))? {
                Some(base) => base,
                None => continue,
            },
        };
        let base = put_back(&base, &row, &cell).map_err(config)?;
        bases.insert(other, base);
    }
    for (other, base) in &bases {
        share(tx, (receiver, other), base)?;
    }
    Ok(())
}

/// After a discard took `receiver`'s Working State from `before` to `after`: each
/// row that arrived there and is gone again returns its pair's base to what it held
/// before, so it is offered again. A Follow's stays a hint, keeping no Sync's claim;
/// a Sync's is forgotten.
pub(crate) fn rewind(
    tx: &mut dyn Tx,
    receiver: &EnvironmentId,
    (before, after): (&SavedEnvironmentIntent, &SavedEnvironmentIntent),
) -> Result<(), RpcError> {
    let mut undone = Vec::new();
    let (before, after) = (Cells::of(before, ""), Cells::of(after, ""));
    for arrived in arrived(tx, receiver, Which::Pending)? {
        let Arrival::Pending { prior, .. } = arrived.arrival else {
            continue;
        };
        let row = arrived.row;
        if before.at(&row) == after.at(&row) {
            continue;
        }
        let key = ledger::Key {
            receiver: receiver.as_str(),
            other: arrived.other.as_str(),
            row: &row,
        };
        match arrived.how {
            How::Follow => ledger::rewind_follow(tx, &key)?,
            How::Sync => ledger::rewind_sync(tx, &key)?,
        }
        undone.push((arrived.other, row, prior));
    }
    rewind_bases(tx, receiver, undone)
}

/// `receiver` deployed `saved` to the nodes of `lineages`: what arrived there and
/// shipped stays, with the Sync that landed it, if one did. An arrival `saved` lacks,
/// one that landed after it was published, stays pending.
pub(crate) fn deployed(
    tx: &mut dyn Tx,
    receiver: &EnvironmentId,
    saved: &SavedEnvironmentIntent,
    lineages: &BTreeSet<String>,
) -> Result<(), RpcError> {
    let pending = arrived(tx, receiver, Which::Pending)?;
    if pending.is_empty() {
        return Ok(());
    }
    let environment = scope::load_by_id(tx, receiver)?;
    let saved = Cells::of(saved, &suffix(tx, &environment)?);
    for arrived in &pending {
        let row = &arrived.row;
        if !lineages.contains(row.lineage()) || *saved.at(row) != arrived.value {
            continue;
        }
        let sync = match &arrived.arrival {
            Arrival::Pending {
                claim: Claim::Released { sync, .. },
                ..
            } => Some(sync),
            Arrival::Pending {
                claim: Claim::Nobody,
                ..
            } => None,
            // An owned row stays pending: its proposal ends at a manual Save or Deploy.
            Arrival::Pending {
                claim: Claim::Owned { .. },
                ..
            }
            | Arrival::Hint
            | Arrival::Settled { .. } => continue,
        };
        let key = ledger::Key {
            receiver: receiver.as_str(),
            other: arrived.other.as_str(),
            row,
        };
        ledger::settle(tx, &key, sync)?;
    }
    Ok(())
}

/// `other`'s `value` at `row` was delivered to `receiver` but stays a Use hint there
/// ([`ledger::hinting`] says what that writes over what arrived there before).
pub(super) fn hint(
    tx: &mut dyn Tx,
    who: &Actor,
    (receiver, other): (&EnvironmentId, &EnvironmentId),
    row: &RowId,
    value: &Cell,
) -> Result<(), RpcError> {
    let stored = arrived(tx, receiver, Which::From(other))?
        .into_iter()
        .find(|arrived| arrived.row == *row)
        .map(|arrived| arrived.arrival);
    let key = ledger::Key {
        receiver: receiver.as_str(),
        other: other.as_str(),
        row,
    };
    match ledger::hinting(stored.as_ref()) {
        ledger::Hinting::Nothing => Ok(()),
        ledger::Hinting::Source(sync) => ledger::hint_source(tx, &key, sync, value),
        ledger::Hinting::Hint => {
            let arrived = Arrived {
                other: other.clone(),
                row: row.clone(),
                how: How::Follow,
                from: value.clone(),
                value: value.clone(),
                arrival: Arrival::Hint,
            };
            let kept = ledger::Kept::NEITHER;
            ledger::write(tx, who, receiver, &arrived, (stored.as_ref(), kept))
        }
    }
}

/// How a row arrived. A `follow` row may carry a Sync's claim: a Follow or a take
/// that landed over a pending row a Sync landed keeps it ([`ledger::landing`]).
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum How {
    Follow,
    Sync,
}

/// Where an arrived row stands in its receiver.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Arrival {
    /// Staged, not deployed: `prior` is what the pair base held before it landed, to
    /// rewind a discard.
    Pending { prior: Cell, claim: Claim },
    /// A Follow the receiver changed too, or discarded.
    Hint,
    /// Deployed, with the Sync that landed it, if one did: that Sync's Undo then
    /// refuses it as deployed.
    Settled { sync: Option<SyncId> },
}

/// The Sync that landed a pending row, if one did, with the receiver's own from
/// before it (`was`) to undo it. A Follow or a take that landed over such a row since
/// keeps its claim, Released, so that Sync's Undo still sees the row; Undo never puts
/// that `was` back, since [`undo`] refuses on the row's `how = 'follow'` first and
/// `proposal::undo` refuses any Released row.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Claim {
    Nobody,
    Released {
        sync: SyncId,
        was: SealedCell,
    },
    Owned {
        sync: SyncId,
        proposal: ProposalId,
        was: SealedCell,
    },
}

impl Claim {
    /// The receiver's own from before the Sync that landed it.
    pub(super) fn was(self) -> Result<SealedCell, RpcError> {
        match self {
            Self::Nobody => Err(error::corrupt("Sync")),
            Self::Released { was, .. } | Self::Owned { was, .. } => Ok(was),
        }
    }
}

/// A row that arrived in a receiver from `other`.
pub(super) struct Arrived {
    pub(super) other: EnvironmentId,
    pub(super) row: RowId,
    pub(super) how: How,
    /// `other`'s cell, which was delivered; redacted. A secret sealed for `other`
    /// lands without its value, so this tells which of its values came.
    pub(super) from: Cell,
    /// The cell that landed; redacted.
    pub(super) value: Cell,
    pub(super) arrival: Arrival,
}

/// What a row no Sync claims holds as the receiver's own. Nothing undoes it, so the
/// value isn't kept: it may be plain text sealed since. A Cloud an older release runs
/// during a rolling deploy reads it on every pending row, so it is Absent, not NULL.
pub(crate) const UNKEPT: SealedCell = SealedCell::Cell(Cell::Absent);

/// Forget the receiver's own kept in each row of `receiver` no Sync claims.
pub(crate) fn forget_unclaimed(tx: &mut dyn Tx, receiver: &str) -> Result<(), RpcError> {
    tx.execute(
        "UPDATE config_sync_arrival SET was = ?2 \
         WHERE environment_id = ?1 AND sync_id IS NULL AND was IS NOT NULL AND was <> ?2",
        &[receiver.into(), json_of(&UNKEPT).as_str().into()],
    )?;
    Ok(())
}

/// The arrival ledger's edges: each moves one receiver's `config_sync_arrival` rows
/// between absent, pending (under its [`Claim`]), a Use hint and settled, and carries
/// in its WHERE the from-state its caller read under the receiver's lock. An edge
/// over rows its caller read fails corrupt when it changes another number of rows;
/// `end` and `take_out`'s claim drop change rows no caller read, so count none.
///
/// Outside these, the table is written by [`forget_unclaimed`] (which `end` and
/// `take_out` call, and `0028`'s step), `storage::steps::repair_arrivals` (`0037`'s step), the
/// content scrubs in `seal_scrub::walk`, which change cells and
/// never a state or claim, the Environment purge, a proposal's rebind, whose foreign
/// key moves `other_id`, and the migrations `0005`, `0011` and `0037`.
pub(super) mod ledger {
    use super::*;
    use crate::storage::Param;

    /// One arrival's key: what arrived in `receiver` from `other` at `row`.
    pub(in crate::branch) struct Key<'a> {
        pub(in crate::branch) receiver: &'a str,
        pub(in crate::branch) other: &'a str,
        pub(in crate::branch) row: &'a RowId,
    }

    /// The columns an [`Arrival`] sets besides what landed.
    struct Columns<'a> {
        state: &'static str,
        prior: Option<String>,
        was: Option<String>,
        sync: Option<&'a str>,
        proposal: Option<&'a str>,
    }

    impl<'a> Columns<'a> {
        fn of(arrival: &'a Arrival) -> Self {
            match arrival {
                Arrival::Pending { prior, claim } => {
                    let (sync, proposal, was) = match claim {
                        Claim::Nobody => (None, None, &UNKEPT),
                        Claim::Released { sync, was } => (Some(sync.as_str()), None, was),
                        Claim::Owned {
                            sync,
                            proposal,
                            was,
                        } => (Some(sync.as_str()), Some(proposal.as_str()), was),
                    };
                    Self {
                        state: "pending",
                        prior: Some(json_of(prior)),
                        was: Some(json_of(was)),
                        sync,
                        proposal,
                    }
                }
                Arrival::Hint => Self {
                    state: "hint",
                    prior: None,
                    was: None,
                    sync: None,
                    proposal: None,
                },
                Arrival::Settled { sync } => Self {
                    state: "settled",
                    prior: None,
                    was: None,
                    sync: sync.as_ref().map(SyncId::as_str),
                    proposal: None,
                },
            }
        }
    }

    /// What a landing writes over `stored`, the row that arrived there from that
    /// side before, if one did, for `owner`, given the landing's own `prior` (the
    /// pair base's cell before it) and `was` (the receiver's own cell before it):
    ///
    /// | `owner` | `stored` | writes |
    /// |---|---|---|
    /// | None | pending, Released or Owned | Released by its Sync, its `was` and `prior` kept |
    /// | None | pending, Nobody | Nobody, its `prior` kept |
    /// | None | absent, a Use hint or settled | Nobody, the landing's `prior` |
    /// | Proposal `id` | pending, Owned by `id` | Owned by the new Sync, its `was` and `prior` kept |
    /// | Proposal `id` | anything else | Owned, the landing's `was` and `prior` |
    ///
    /// A row at one key is owned only by the proposal of its `other`: one proposal per
    /// receiver and source, which the foreign key ties to it. So Owned by another
    /// proposal is "anything else", which the landing takes over.
    ///
    /// [`Kept`] says which stored cells it keeps, which [`write`] keeps as the row
    /// holds them when it writes.
    pub(super) fn landing(
        stored: Option<&Arrival>,
        owner: Owner<'_>,
        (prior, was): (Cell, SealedCell),
    ) -> (Arrival, Kept) {
        let kept = match stored {
            Some(Arrival::Pending { prior, claim }) => Some((prior, claim)),
            Some(Arrival::Hint | Arrival::Settled { .. }) | None => None,
        };
        let (prior, claim, kept) = match (owner, kept) {
            (
                Owner::None,
                Some((prior, Claim::Released { sync, was } | Claim::Owned { sync, was, .. })),
            ) => (
                prior.clone(),
                Claim::Released {
                    sync: sync.clone(),
                    was: was.clone(),
                },
                Kept::BOTH,
            ),
            (Owner::None, Some((prior, Claim::Nobody))) => {
                (prior.clone(), Claim::Nobody, Kept::PRIOR)
            }
            (Owner::None, None) => (prior, Claim::Nobody, Kept::NEITHER),
            (
                Owner::Proposal { id, sync },
                Some((
                    prior,
                    Claim::Owned {
                        proposal,
                        was: kept,
                        ..
                    },
                )),
            ) if proposal == id => (
                prior.clone(),
                Claim::Owned {
                    sync: sync.clone(),
                    proposal: id.clone(),
                    was: kept.clone(),
                },
                Kept::BOTH,
            ),
            (Owner::Proposal { id, sync }, _) => (
                prior,
                Claim::Owned {
                    sync: sync.clone(),
                    proposal: id.clone(),
                    was,
                },
                Kept::NEITHER,
            ),
        };
        (Arrival::Pending { prior, claim }, kept)
    }

    /// Which of a stored pending row's `prior` and `was` a landing keeps.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub(super) struct Kept {
        pub(super) prior: bool,
        pub(super) was: bool,
    }

    impl Kept {
        pub(super) const BOTH: Self = Self {
            prior: true,
            was: true,
        };
        /// A row no Sync claims keeps no `was` ([`UNKEPT`]).
        pub(super) const PRIOR: Self = Self {
            prior: true,
            was: false,
        };
        pub(super) const NEITHER: Self = Self {
            prior: false,
            was: false,
        };
    }

    /// What a Use hint writes over `stored`, the row that arrived there from that side
    /// before, if one did.
    #[derive(Debug, PartialEq)]
    pub(super) enum Hinting<'a> {
        /// Over a pending row Released by this Sync: only `source`, the cell delivered,
        /// so the Use hint is offered and the next Follow skips it. The row keeps its
        /// state, `how`, value, claim, `prior` and `was`, so that Sync's Undo sees it.
        Source(&'a SyncId),
        /// Over a pending row a proposal owns: nothing.
        Nothing,
        /// Over nothing, a Use hint, a settled row or a pending row no Sync claims: the
        /// Use hint, a Follow's, keeping no claim.
        Hint,
    }

    pub(super) fn hinting(stored: Option<&Arrival>) -> Hinting<'_> {
        match stored {
            Some(Arrival::Pending {
                claim: Claim::Released { sync, .. },
                ..
            }) => Hinting::Source(sync),
            Some(Arrival::Pending {
                claim: Claim::Owned { .. },
                ..
            }) => Hinting::Nothing,
            Some(
                Arrival::Pending {
                    claim: Claim::Nobody,
                    ..
                }
                | Arrival::Hint
                | Arrival::Settled { .. },
            )
            | None => Hinting::Hint,
        }
    }

    /// Land: write `arrived` in `receiver` over `stored`, what arrived at its key
    /// before as the caller read it. Where nothing was, an insert; else an update of
    /// that row only while it still holds `stored`: its state and, but for a Use
    /// hint, its claim. A Use hint's `sync_id` isn't compared: one an older Store's
    /// Discard wrote keeps a Sync's. A cell `kept` stays as the row holds it now, not
    /// as the caller read it: a scrub of an ancestor's seal takes no lock here, and
    /// may have emptied it since.
    pub(super) fn write(
        tx: &mut dyn Tx,
        who: &Actor,
        receiver: &EnvironmentId,
        arrived: &Arrived,
        (stored, kept): (Option<&Arrival>, Kept),
    ) -> Result<(), RpcError> {
        let to = Columns::of(&arrived.arrival);
        let at = arrived.row.at().to_string();
        let how = storage::name_of(arrived.how);
        let (value, source) = (json_of(&arrived.value), json_of(&arrived.from));
        let mut params: Vec<Param<'_>> = vec![
            receiver.as_str().into(),
            arrived.other.as_str().into(),
            arrived.row.lineage().into(),
            at.as_str().into(),
            how.as_str().into(),
            to.state.into(),
            value.as_str().into(),
            to.prior.as_deref().into(),
            to.was.as_deref().into(),
            to.sync.into(),
            to.proposal.into(),
            source.as_str().into(),
        ];
        let Some(stored) = stored else {
            params.push(who.organization.as_str().into());
            tx.execute(
                "INSERT INTO config_sync_arrival (environment_id, other_id, lineage, at, how, \
                 state, value, prior, was, sync_id, proposal_id, source, organization_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                &params,
            )?;
            return Ok(());
        };
        let from = Columns::of(stored);
        let keep = |kept: bool| if kept { "kept" } else { "landed" };
        params.push(from.state.into());
        params.push(from.sync.into());
        params.push(from.proposal.into());
        params.push(keep(kept.prior).into());
        params.push(keep(kept.was).into());
        let changed = tx.execute(
            "UPDATE config_sync_arrival SET how = ?5, state = ?6, value = ?7, \
             prior = CASE WHEN ?16 = 'kept' THEN prior ELSE ?8 END, \
             was = CASE WHEN ?17 = 'kept' THEN was ELSE ?9 END, \
             sync_id = ?10, proposal_id = ?11, source = ?12 \
             WHERE environment_id = ?1 AND other_id = ?2 AND lineage = ?3 AND at = ?4 \
             AND state = ?13 AND (state = 'hint' OR (COALESCE(sync_id, '') = COALESCE(?14, '') \
             AND COALESCE(proposal_id, '') = COALESCE(?15, '')))",
            &params,
        )?;
        exactly(changed, 1)
    }

    /// `changed` rows where the caller read `expected` under the receiver's lock, or
    /// corrupt: a row it read no longer holds what it read.
    fn exactly(changed: usize, expected: usize) -> Result<(), RpcError> {
        if changed == expected {
            Ok(())
        } else {
            Err(error::corrupt("Sync"))
        }
    }

    /// Hint over a pending row Released by `sync`: write only `source`, the cell
    /// `delivered` ([`Hinting::Source`]).
    pub(super) fn hint_source(
        tx: &mut dyn Tx,
        key: &Key<'_>,
        sync: &SyncId,
        delivered: &Cell,
    ) -> Result<(), RpcError> {
        let (at, source) = (key.row.at().to_string(), json_of(delivered));
        let changed = tx.execute(
            "UPDATE config_sync_arrival SET source = ?5 \
             WHERE environment_id = ?1 AND other_id = ?2 AND lineage = ?3 AND at = ?4 \
             AND state = 'pending' AND sync_id = ?6 AND proposal_id IS NULL",
            &[
                key.receiver.into(),
                key.other.into(),
                key.row.lineage().into(),
                at.as_str().into(),
                source.as_str().into(),
                sync.as_str().into(),
            ],
        )?;
        exactly(changed, 1)
    }

    /// Rewind a Follow's row: from pending, unowned, `how = 'follow'`, to a Use hint,
    /// which keeps no claim, a Sync's included.
    pub(super) fn rewind_follow(tx: &mut dyn Tx, key: &Key<'_>) -> Result<(), RpcError> {
        let to = Columns::of(&Arrival::Hint);
        let at = key.row.at().to_string();
        let changed = tx.execute(
            "UPDATE config_sync_arrival SET state = ?5, prior = ?6, was = ?7, sync_id = ?8 \
             WHERE environment_id = ?1 AND other_id = ?2 AND lineage = ?3 AND at = ?4 \
             AND state = 'pending' AND how = 'follow' AND proposal_id IS NULL",
            &[
                key.receiver.into(),
                key.other.into(),
                key.row.lineage().into(),
                at.as_str().into(),
                to.state.into(),
                to.prior.as_deref().into(),
                to.was.as_deref().into(),
                to.sync.into(),
            ],
        )?;
        exactly(changed, 1)
    }

    /// Rewind a Sync's row: from pending, `how = 'sync'`, under any claim, to absent.
    pub(super) fn rewind_sync(tx: &mut dyn Tx, key: &Key<'_>) -> Result<(), RpcError> {
        let at = key.row.at().to_string();
        let changed = tx.execute(
            "DELETE FROM config_sync_arrival \
             WHERE environment_id = ?1 AND other_id = ?2 AND lineage = ?3 AND at = ?4 \
             AND state = 'pending' AND how = 'sync'",
            &[
                key.receiver.into(),
                key.other.into(),
                key.row.lineage().into(),
                at.as_str().into(),
            ],
        )?;
        exactly(changed, 1)
    }

    /// Settle: from pending, Released by `sync` or, with none, claimed by Nobody, to
    /// settled, keeping that Sync and `source`.
    pub(super) fn settle(
        tx: &mut dyn Tx,
        key: &Key<'_>,
        sync: Option<&SyncId>,
    ) -> Result<(), RpcError> {
        let target = Arrival::Settled {
            sync: sync.cloned(),
        };
        let to = Columns::of(&target);
        let at = key.row.at().to_string();
        let changed = tx.execute(
            "UPDATE config_sync_arrival SET state = ?5, prior = ?6, was = ?7, sync_id = ?8 \
             WHERE environment_id = ?1 AND other_id = ?2 AND lineage = ?3 AND at = ?4 \
             AND state = 'pending' AND proposal_id IS NULL \
             AND COALESCE(sync_id, '') = COALESCE(?9, '')",
            &[
                key.receiver.into(),
                key.other.into(),
                key.row.lineage().into(),
                at.as_str().into(),
                to.state.into(),
                to.prior.as_deref().into(),
                to.was.as_deref().into(),
                to.sync.into(),
                sync.map(SyncId::as_str).into(),
            ],
        )?;
        exactly(changed, 1)
    }

    /// Release: from pending, Owned by `proposal`, to Released by its Sync, keeping
    /// `prior` and `was`. The draft changed the row, so it is the draft's own.
    pub(in crate::branch) fn release(
        tx: &mut dyn Tx,
        key: &Key<'_>,
        proposal: &ProposalId,
    ) -> Result<(), RpcError> {
        let at = key.row.at().to_string();
        let changed = tx.execute(
            "UPDATE config_sync_arrival SET proposal_id = NULL, source = NULL \
             WHERE environment_id = ?1 AND other_id = ?2 AND lineage = ?3 AND at = ?4 \
             AND state = 'pending' AND proposal_id = ?5",
            &[
                key.receiver.into(),
                key.other.into(),
                key.row.lineage().into(),
                at.as_str().into(),
                proposal.as_str().into(),
            ],
        )?;
        exactly(changed, 1)
    }

    /// The proposals in `receiver`'s draft an end ends: every one, or those `from`
    /// one source.
    pub(in crate::branch) struct Ended<'a> {
        pub(in crate::branch) receiver: &'a EnvironmentId,
        pub(in crate::branch) from: Option<&'a EnvironmentId>,
    }

    impl<'a> Ended<'a> {
        /// Their ids, as a `SELECT id FROM config_proposal` over the params it returns,
        /// whose `?1` is the receiver.
        pub(in crate::branch) fn select(&self) -> (String, Vec<Param<'a>>) {
            let mut of = "SELECT id FROM config_proposal WHERE environment_id = ?1".to_owned();
            let mut params = vec![self.receiver.as_str().into()];
            if let Some(from) = self.from {
                of.push_str(" AND source_environment_id = ?2");
                params.push(from.as_str().into());
            }
            (of, params)
        }
    }

    /// End the proposals `ended` selects: from pending, Owned by one, to Released by
    /// its Sync. For one a later Sync refreshed (`first_sync <> last_sync`) no Sync of
    /// it is undone after, so every pending row of the receiver one of its Syncs
    /// claims, owned or released, drops that claim. Then [`forget_unclaimed`] forgets
    /// the receiver's own those rows kept. No count is checked: nobody read the rows.
    pub(in crate::branch) fn end(tx: &mut dyn Tx, ended: &Ended<'_>) -> Result<(), RpcError> {
        let (ended_ids, params) = ended.select();
        let ended_ids = ended_ids.as_str();
        tx.execute(
            &format!(
                "UPDATE config_sync_arrival SET sync_id = CASE WHEN proposal_id IN \
                 ({ended_ids} AND first_sync <> last_sync) THEN NULL ELSE sync_id END, \
                 proposal_id = NULL, source = NULL \
                 WHERE environment_id = ?1 AND state = 'pending' AND proposal_id IN ({ended_ids})"
            ),
            &params,
        )?;
        tx.execute(
            &format!(
                "UPDATE config_sync_arrival SET sync_id = NULL \
                 WHERE environment_id = ?1 AND state = 'pending' AND sync_id IN \
                 (SELECT sync_id FROM config_sync_receipt WHERE proposal_id IN \
                 ({ended_ids} AND first_sync <> last_sync))"
            ),
            &params,
        )?;
        forget_unclaimed(tx, ended.receiver.as_str())
    }

    /// Take out `proposal` from `receiver`: from pending, Owned by it, to absent, the
    /// `owned` rows the caller read. Then each pending row a Follow or a take released
    /// from one of its `syncs` drops that claim, and [`forget_unclaimed`] forgets the
    /// receiver's own it kept. A row the draft released keeps its Sync. Nothing is put
    /// back here.
    pub(in crate::branch) fn take_out(
        tx: &mut dyn Tx,
        receiver: &EnvironmentId,
        (proposal, owned): (&ProposalId, usize),
        syncs: &BTreeSet<SyncId>,
    ) -> Result<(), RpcError> {
        let changed = tx.execute(
            "DELETE FROM config_sync_arrival \
             WHERE environment_id = ?1 AND proposal_id = ?2 AND state = 'pending'",
            &[receiver.as_str().into(), proposal.as_str().into()],
        )?;
        exactly(changed, owned)?;
        for sync in syncs {
            tx.execute(
                "UPDATE config_sync_arrival SET sync_id = NULL \
                 WHERE environment_id = ?1 AND sync_id = ?2 AND state = 'pending' \
                 AND how = 'follow' AND proposal_id IS NULL",
                &[receiver.as_str().into(), sync.as_str().into()],
            )?;
        }
        forget_unclaimed(tx, receiver.as_str())
    }

    /// Undo `sync` in `receiver`: from pending, claimed by it, to absent, the `rows`
    /// the caller read.
    pub(super) fn undo(
        tx: &mut dyn Tx,
        receiver: &EnvironmentId,
        (sync, rows): (&SyncId, usize),
    ) -> Result<(), RpcError> {
        let changed = tx.execute(
            "DELETE FROM config_sync_arrival \
             WHERE environment_id = ?1 AND sync_id = ?2 AND state = 'pending'",
            &[receiver.as_str().into(), sync.as_str().into()],
        )?;
        exactly(changed, rows)
    }

    /// Withdraw what a Follow from `other` staged of node `lineage` in `receiver`:
    /// from pending, `how = 'follow'`, claimed by Nobody, to absent, the `rows` the
    /// caller read.
    pub(super) fn withdraw(
        tx: &mut dyn Tx,
        receiver: &EnvironmentId,
        other: &EnvironmentId,
        (lineage, rows): (&str, usize),
    ) -> Result<(), RpcError> {
        let changed = tx.execute(
            "DELETE FROM config_sync_arrival \
             WHERE environment_id = ?1 AND other_id = ?2 AND lineage = ?3 \
             AND how = 'follow' AND state = 'pending' AND sync_id IS NULL \
             AND proposal_id IS NULL",
            &[
                receiver.as_str().into(),
                other.as_str().into(),
                lineage.into(),
            ],
        )?;
        exactly(changed, rows)
    }
}

/// Which of an Environment's arrivals [`arrived`] reads.
pub(super) enum Which<'id> {
    /// Those not yet deployed.
    Pending,
    /// Those from this Environment.
    From(&'id EnvironmentId),
    /// Those this Sync landed.
    Of(&'id SyncId),
    /// Those this proposal owns.
    Owned(&'id ProposalId),
}

/// What arrived in `receiver` that `which` selects.
pub(super) fn arrived(
    tx: &mut dyn Tx,
    receiver: &EnvironmentId,
    which: Which<'_>,
) -> Result<Vec<Arrived>, RpcError> {
    const SELECT: &str = "SELECT other_id, lineage, at, how, state, value, prior, was, sync_id, \
         proposal_id, source FROM config_sync_arrival WHERE environment_id = ?1";
    let receiver = receiver.as_str().into();
    let rows = match which {
        Which::Pending => tx.query(&format!("{SELECT} AND state = 'pending'"), &[receiver])?,
        Which::From(other) => tx.query(
            &format!("{SELECT} AND other_id = ?2"),
            &[receiver, other.as_str().into()],
        )?,
        Which::Of(sync) => tx.query(
            &format!("{SELECT} AND sync_id = ?2"),
            &[receiver, sync.as_str().into()],
        )?,
        Which::Owned(proposal) => tx.query(
            &format!("{SELECT} AND proposal_id = ?2"),
            &[receiver, proposal.as_str().into()],
        )?,
    };
    rows.iter()
        .map(|row| {
            let arrival = match row.text(4)? {
                "pending" => Arrival::Pending {
                    prior: row.json(6, "Sync")?,
                    claim: match (
                        row.parse_optional(8, "Sync")?,
                        row.parse_optional(9, "Sync")?,
                    ) {
                        (None, None) => Claim::Nobody,
                        (Some(sync), None) => Claim::Released {
                            sync,
                            was: row.json(7, "Sync")?,
                        },
                        (Some(sync), Some(proposal)) => Claim::Owned {
                            sync,
                            proposal,
                            was: row.json(7, "Sync")?,
                        },
                        (None, Some(_)) => return Err(error::corrupt("Sync")),
                    },
                },
                "hint" => Arrival::Hint,
                "settled" => Arrival::Settled {
                    sync: row.parse_optional(8, "Sync")?,
                },
                _ => return Err(error::corrupt("Sync")),
            };
            let value: Cell = row.json(5, "Sync")?;
            // A row with no source offers what landed: the release or end of its
            // proposal cleared it, and no Use hint has delivered a cell there since.
            let from = match row.optional_text(10)? {
                Some(from) => serde_json::from_str(from).map_err(|_| error::corrupt("Sync"))?,
                None => value.clone(),
            };
            Ok(Arrived {
                other: row.parse(0, "Sync")?,
                row: row_id(row.text(1)?, row.text(2)?)?,
                how: row.variant(3, "Sync")?,
                from,
                value,
                arrival,
            })
        })
        .collect()
}

/// What arrived in `receiver` from `other`, by row: the cell `other` held.
pub(crate) fn arrivals(
    tx: &mut dyn Tx,
    receiver: &EnvironmentId,
    other: &EnvironmentId,
) -> Result<BTreeMap<RowId, Cell>, RpcError> {
    Ok(arrived(tx, receiver, Which::From(other))?
        .into_iter()
        .map(|arrived| (arrived.row, arrived.from))
        .collect())
}

/// Undo Sync `sync` in `receiver`, whose lock the caller holds: put back what each
/// row it landed held before, and the pair bases' too. Refused when a row deployed,
/// when a Follow or a take landed over a row since (`how = 'follow'`, whatever it
/// holds), or when a row changed since it landed (for a new node, any of its rows).
/// False when no row carries the Sync there.
pub(super) fn undo(
    tx: &mut dyn Tx,
    receiver: &mut Environment,
    sync: &SyncId,
    trusted: &Trusted,
) -> Result<bool, RpcError> {
    let id = receiver.summary.id.clone();
    let rows = arrived(tx, &id, Which::Of(sync))?;
    if rows.is_empty() {
        return Ok(false);
    }
    let label = |row: &RowId| {
        named(&[&receiver.working], row).map_or_else(|| row.to_string(), |row| row.to_string())
    };
    let mut others = Vec::new();
    let mut landed = Vec::new();
    let mut followed = None;
    for arrived in rows {
        let Arrival::Pending { prior, claim } = arrived.arrival else {
            return Err(error::conflict(
                format!(
                    "{} is deployed: change it back instead",
                    label(&arrived.row)
                ),
                json!({ "row": arrived.row }),
            ));
        };
        if matches!(arrived.how, How::Follow) && followed.is_none() {
            followed = Some((arrived.row.clone(), arrived.value.clone()));
        }
        others.push(arrived.other);
        landed.push(Landed {
            row: arrived.row,
            value: arrived.value,
            prior,
            was: claim.was()?,
        });
    }
    let suffix = suffix(tx, receiver)?;
    // A Follow or a take landed over the row since this Sync did: the row changed.
    if let Some((row, value)) = followed {
        return Err(changed_since(
            &receiver.working,
            &suffix,
            &row,
            Some(&value),
        ));
    }
    receiver.working = match unapply(&receiver.working, &suffix, &landed) {
        Ok(working) => working,
        Err(Unapplied::Changed(row)) => {
            let value = landed
                .iter()
                .find(|landed| landed.row == row)
                .map(|landed| &landed.value);
            return Err(changed_since(&receiver.working, &suffix, &row, value));
        }
        Err(Unapplied::Invalid(error)) => return Err(config(error)),
    };
    scope::save_working(tx, receiver, trusted)?;
    ledger::undo(tx, &id, (sync, landed.len()))?;
    let priors = others
        .into_iter()
        .zip(landed)
        .map(|(other, landed)| (other, landed.row, landed.prior))
        .collect();
    rewind_bases(tx, &receiver.summary.id, priors)?;
    Ok(true)
}

/// `other` deployed the removal of node `lineage`: take it back from `receiver`,
/// whose lock the caller holds, if a Follow from `other` brought it as a new node and
/// `receiver` holds it just as it arrived, never saved or deployed, and rewind the
/// pair's base to before it arrived. Nothing `receiver` holds as its own is removed
/// (#3705). False when only another of its nodes still referencing it keeps it:
/// try again once that one goes.
pub(super) fn withdraw(
    tx: &mut dyn Tx,
    receiver: &mut Environment,
    other: &EnvironmentId,
    lineage: &str,
    trusted: &Trusted,
) -> Result<bool, RpcError> {
    let id = receiver.summary.id.clone();
    let node = RowId::node(lineage);
    let arrivals: Vec<Arrived> = arrived(tx, &id, Which::From(other))?
        .into_iter()
        .filter(|arrived| arrived.row.lineage() == lineage)
        .collect();
    // Only what a Follow staged and nobody claims, still pending, the node among it.
    let followed = arrivals.iter().any(|arrived| arrived.row == node)
        && arrivals.iter().all(|arrived| {
            matches!(
                (&arrived.how, &arrived.arrival),
                (
                    How::Follow,
                    Arrival::Pending {
                        claim: Claim::Nobody,
                        ..
                    }
                )
            )
        });
    if !followed {
        return Ok(true);
    }
    let suffix = suffix(tx, receiver)?;
    if let Some(saved) = review::latest_saved(tx, &id)?
        && *Cells::of(&saved.intent, &suffix).at(&node) != Cell::Absent
    {
        return Ok(true);
    }
    // The base holds what landed, the Parent's later changes too: a row the receiver
    // holds otherwise, and not as it arrived, is its own.
    let Some(base) = stored(tx, (&id, other))? else {
        return Ok(true);
    };
    let landed: BTreeMap<RowId, Cell> = arrivals
        .into_iter()
        .map(|arrived| (arrived.row, arrived.value))
        .collect();
    if !unchanged_since(&base, &receiver.working, &suffix, lineage, &landed) {
        return Ok(true);
    }
    // A node another of the receiver's nodes references holds that reference up,
    // until that node goes too.
    if referenced_by_others(&receiver.working, lineage) {
        return Ok(false);
    }
    receiver.working = put_back(&receiver.working, &node, &Cell::Absent).map_err(config)?;
    scope::save_working(tx, receiver, trusted)?;
    ledger::withdraw(tx, &id, other, (lineage, landed.len()))?;
    rewind_bases(tx, &id, vec![(other.clone(), node, Cell::Absent)])?;
    Ok(true)
}

/// Undo refused: `row` changed since a Sync landed there. `value` is what last landed
/// at `row`, if anything did, read only to tell a plain value sealed since, which
/// never becomes plain again, so it can't be changed back.
pub(super) fn changed_since(
    working: &SavedEnvironmentIntent,
    suffix: &str,
    row: &RowId,
    value: Option<&Cell>,
) -> RpcError {
    let label = named(&[working], row).map_or_else(|| row.to_string(), |row| row.to_string());
    let sealed_since =
        matches!(value, Some(Cell::Value(_))) && Cells::of(working, suffix).at(row).is_secret();
    let message = if sealed_since {
        format!("{label} is sealed since it synced: set it as you want it instead")
    } else {
        format!("{label} changed since it synced: change it back instead")
    };
    error::conflict(message, json!({ "row": row }))
}

/// How many changes a Sync of `from` into `into` carries by default: the Sync view's
/// ticked rows.
pub(crate) fn changes_into(
    tx: &mut dyn Tx,
    from: &Environment,
    into: &Environment,
) -> Result<usize, RpcError> {
    let (way, _, _) = super::proposal::planned(tx, from, into)?;
    Ok(way
        .plan(&into.working)
        .rows()
        .iter()
        .filter(|row| matches!(row.verdict, Verdict::Moves { ticked: true, .. }))
        .count())
}

/// What the Services of one side of a Sync carry besides their configuration, by
/// Service ID: their sealed registry credentials and their Deployment Policies.
/// Captured when the Sync is made, so landing never reads a side that may be gone.
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

/// Whether `service` pulls its image with a registry credential.
const fn credentialed(service: &SavedServiceIntent) -> bool {
    matches!(
        service.config.source,
        ServiceSource::Image {
            credentials: ServiceImageCredentials::Configured { .. },
            ..
        }
    )
}

/// Land `next` as `branch`'s Working State: the nodes arriving from `from` bring
/// their registry credentials and Deployment Policies (`carried`) and get their Node
/// Introductions, and a picked credential row brings its credential to the Service
/// already there. Returns the nodes staged.
pub(crate) fn land(
    tx: &mut dyn Tx,
    who: &Actor,
    branch: &mut Environment,
    (from, carried): (&SavedEnvironmentIntent, &Carried),
    next: SavedEnvironmentIntent,
    picks: &BTreeSet<RowId>,
    trusted: &Trusted,
) -> Result<Vec<NodeName>, RpcError> {
    crate::domain::refuse_too_long_copies(trusted.cluster_domain(), (&branch.working, &next))?;
    let before = std::mem::replace(&mut branch.working, next);
    // What arrives from `from` may bring a rule it already broke; that's not new.
    scope::save_working_from(tx, branch, scope::Write::arriving(Some(from)), trusted)?;
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
        if let Some(source) = source_of(&service.lineage_id) {
            if credentialed(service)
                && let Some(sealed) = carried.credentials.get(&source)
            {
                registry::store(tx, who, &id, &service.id, sealed)?;
            }
            if let Some(policy) = carried.policies.get(&source) {
                policy::store(tx, who, &id, &service.id, policy)?;
            }
        }
        scope::introduce(tx, who, &id, scope::Node::Service(service))?;
    }
    // A picked source that turns credentials on brings the credential; one already on
    // keeps the receiver's own.
    for pick in picks
        .iter()
        .filter(|pick| *pick.at() == At::Setting(Setting::Source))
    {
        let lineage = pick.lineage();
        let on =
            |service: &&SavedServiceIntent| service.lineage_id == lineage && credentialed(service);
        let receiver = branch.working.services.iter().find(on);
        let was_on = before.services.iter().any(|service| on(&service));
        let sealed = source_of(lineage).and_then(|source| carried.credentials.get(&source));
        if let (Some(receiver), false, Some(sealed)) = (receiver, was_on, sealed) {
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
    for config in &branch.working.configs {
        if !before
            .configs
            .iter()
            .any(|old| old.resource_id == config.resource_id)
        {
            staged.push(NodeName::Config(config.name.clone().into()));
            scope::introduce(tx, who, &id, scope::Node::Config(config))?;
        }
    }
    // A removed node keeps its credential, policy and Introduction: Undo brings it
    // back by ID.
    for service in &before.services {
        if !branch.working.services.iter().any(|s| s.id == service.id) {
            staged.push(NodeName::Service(
                ServiceName::parse(service.slug.as_str())
                    .map_err(|_| error::corrupt("Service name"))?,
            ));
        }
    }
    for config in &before.configs {
        if !branch
            .working
            .configs
            .iter()
            .any(|c| c.resource_id == config.resource_id)
        {
            staged.push(NodeName::Config(config.name.clone().into()));
        }
    }
    staged.sort();
    Ok(staged)
}

#[cfg(test)]
mod tests {
    use super::ledger::{Hinting, Kept, hinting, landing};
    use super::*;
    use crate::{CreateProject, OrganizationId, ProjectId, ProjectName, SealingKey};

    fn sync(n: u8) -> SyncId {
        SyncId::parse(format!("00000000-0000-4000-8000-0000000002{n:02}")).unwrap()
    }

    fn proposal(n: u8) -> ProposalId {
        ProposalId::parse(format!("00000000-0000-4000-8000-0000000003{n:02}")).unwrap()
    }

    fn cell(value: &str) -> Cell {
        Cell::Value(json!(value))
    }

    fn sealed(value: &str) -> SealedCell {
        SealedCell::Cell(cell(value))
    }

    /// A pending row whose `prior` and `was` are the stored ones.
    fn pending(claim: Claim) -> Arrival {
        Arrival::Pending {
            prior: cell("stored prior"),
            claim,
        }
    }

    fn released() -> Claim {
        Claim::Released {
            sync: sync(1),
            was: sealed("stored was"),
        }
    }

    fn owned(by: u8) -> Claim {
        Claim::Owned {
            sync: sync(1),
            proposal: proposal(by),
            was: sealed("stored was"),
        }
    }

    /// What a landing for `owner` writes over `stored`, with its own `prior` and `was`,
    /// and which stored cells it keeps.
    fn landed(stored: Option<&Arrival>, owner: Owner<'_>) -> (Arrival, Kept) {
        landing(
            stored,
            owner,
            (cell("landing prior"), sealed("landing was")),
        )
    }

    /// The row a landing nobody owns writes over a pending row it keeps the claim of.
    fn released_kept() -> Arrival {
        Arrival::Pending {
            prior: cell("stored prior"),
            claim: released(),
        }
    }

    #[test]
    fn a_landing_nobody_owns_over_a_released_row_keeps_its_sync_was_and_prior() {
        assert_eq!(
            landed(Some(&pending(released())), Owner::None),
            (released_kept(), Kept::BOTH)
        );
    }

    #[test]
    fn a_landing_nobody_owns_over_an_owned_row_releases_it_with_its_sync_was_and_prior() {
        assert_eq!(
            landed(Some(&pending(owned(1))), Owner::None),
            (released_kept(), Kept::BOTH)
        );
    }

    #[test]
    fn a_landing_nobody_owns_over_a_pending_row_nobody_claims_keeps_its_prior() {
        assert_eq!(
            landed(Some(&pending(Claim::Nobody)), Owner::None),
            (
                Arrival::Pending {
                    prior: cell("stored prior"),
                    claim: Claim::Nobody,
                },
                Kept::PRIOR
            )
        );
    }

    #[test]
    fn a_landing_nobody_owns_over_no_pending_row_starts_from_the_base_it_found() {
        let fresh = Arrival::Pending {
            prior: cell("landing prior"),
            claim: Claim::Nobody,
        };
        for stored in [
            None,
            Some(Arrival::Hint),
            Some(Arrival::Settled { sync: None }),
            Some(Arrival::Settled {
                sync: Some(sync(1)),
            }),
        ] {
            assert_eq!(
                landed(stored.as_ref(), Owner::None),
                (fresh.clone(), Kept::NEITHER),
                "{stored:?}"
            );
        }
    }

    #[test]
    fn a_sync_landing_over_its_own_proposals_row_keeps_its_was_and_prior() {
        let (id, next) = (proposal(1), sync(2));
        let owner = Owner::Proposal {
            id: &id,
            sync: &next,
        };
        assert_eq!(
            landed(Some(&pending(owned(1))), owner),
            (
                Arrival::Pending {
                    prior: cell("stored prior"),
                    claim: Claim::Owned {
                        sync: sync(2),
                        proposal: proposal(1),
                        was: sealed("stored was"),
                    },
                },
                Kept::BOTH
            )
        );
    }

    #[test]
    fn a_sync_landing_over_any_other_row_takes_it_over_from_what_it_found() {
        let (id, next) = (proposal(1), sync(2));
        let owner = Owner::Proposal {
            id: &id,
            sync: &next,
        };
        let fresh = Arrival::Pending {
            prior: cell("landing prior"),
            claim: Claim::Owned {
                sync: sync(2),
                proposal: proposal(1),
                was: sealed("landing was"),
            },
        };
        for stored in [
            None,
            Some(pending(Claim::Nobody)),
            Some(pending(released())),
            Some(pending(owned(2))),
            Some(Arrival::Hint),
            Some(Arrival::Settled {
                sync: Some(sync(1)),
            }),
        ] {
            assert_eq!(
                landed(stored.as_ref(), owner),
                (fresh.clone(), Kept::NEITHER),
                "{stored:?}"
            );
        }
    }

    #[test]
    fn a_use_hint_over_a_released_row_writes_only_its_source() {
        assert_eq!(
            hinting(Some(&pending(released()))),
            Hinting::Source(&sync(1))
        );
    }

    #[test]
    fn a_use_hint_over_an_owned_row_writes_nothing() {
        assert_eq!(hinting(Some(&pending(owned(1)))), Hinting::Nothing);
    }

    #[test]
    fn a_use_hint_over_any_other_row_replaces_it() {
        for stored in [
            None,
            Some(pending(Claim::Nobody)),
            Some(Arrival::Hint),
            Some(Arrival::Settled { sync: None }),
            Some(Arrival::Settled {
                sync: Some(sync(1)),
            }),
        ] {
            assert_eq!(hinting(stored.as_ref()), Hinting::Hint, "{stored:?}");
        }
    }

    const ORGANIZATION: &str = "org_ledger";

    fn uuid(n: u8) -> String {
        format!("00000000-0000-4000-8000-0000000000{n:02}")
    }

    /// A Store with one Project and its Environment, the receiver; on a fresh
    /// Postgres database when `PLOYZ_STORE_TEST_POSTGRES` names a server.
    fn shop() -> (crate::ConfigStore, Actor, EnvironmentId) {
        let url = match std::env::var("PLOYZ_STORE_TEST_POSTGRES") {
            Ok(server) => {
                let database = format!("ledger_{}", uuid::Uuid::new_v4().simple());
                postgres::Client::connect(&server, postgres::NoTls)
                    .unwrap()
                    .batch_execute(&format!("CREATE DATABASE {database}"))
                    .unwrap();
                format!("{server}/{database}")
            }
            Err(_) => "sqlite::memory:".to_owned(),
        };
        let store = crate::ConfigStore::open(&url, SealingKey::new(b"test").unwrap()).unwrap();
        let who = Actor::system(OrganizationId::parse(ORGANIZATION).unwrap());
        let receiver = EnvironmentId::parse(uuid(2)).unwrap();
        store
            .write(
                &who,
                &CreateProject {
                    id: ProjectId::parse(uuid(1)).unwrap(),
                    name: ProjectName::parse("shop").unwrap(),
                    default_environment: receiver.clone(),
                },
            )
            .unwrap();
        (store, who, receiver)
    }

    /// A row Sync 1 landed from `other` in `receiver`, released, whose `prior` and
    /// `was` hold a plain `secret`.
    fn plant_released(tx: &mut dyn Tx, receiver: &EnvironmentId, other: &str, row: &RowId) {
        tx.execute(
            "INSERT INTO config_sync_arrival (environment_id, other_id, lineage, at, \
             organization_id, how, state, value, prior, was, sync_id, source) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'sync', 'pending', ?6, ?7, ?8, ?9, ?6)",
            &[
                receiver.as_str().into(),
                other.into(),
                row.lineage().into(),
                row.at().to_string().as_str().into(),
                ORGANIZATION.into(),
                json_of(&cell("2")).as_str().into(),
                json_of(&cell("secret")).as_str().into(),
                json_of(&sealed("secret")).as_str().into(),
                sync(1).as_str().into(),
            ],
        )
        .unwrap();
    }

    /// A Follow's landing over a row a Sync landed keeps the row's `prior` and `was`.
    /// A seal in an ancestor scrubs both without the receiver's lock, so between the
    /// read the landing decides from and its write: the scrubbed cells stay.
    #[test]
    fn a_landing_keeps_the_cells_a_scrub_emptied_after_it_read_the_row() {
        let (store, who, receiver) = shop();
        let other = EnvironmentId::parse(uuid(9)).unwrap();
        let row = row_id(&uuid(3), "variables.PLAIN").unwrap();
        let (prior, was) = store
            .storage
            .write(|tx| {
                plant_released(tx, &receiver, other.as_str(), &row);
                let stored = arrived(tx, &receiver, Which::From(&other))?
                    .into_iter()
                    .find(|arrived| arrived.row == row)
                    .map(|arrived| arrived.arrival)
                    .unwrap();
                // The scrub's compare-and-set writes (`empty_in_bases`).
                for (column, scrubbed, plain) in [
                    ("prior", json_of(&cell("")), json_of(&cell("secret"))),
                    ("was", json_of(&sealed("")), json_of(&sealed("secret"))),
                ] {
                    tx.execute(
                        &format!(
                            "UPDATE config_sync_arrival SET {column} = ?2 \
                             WHERE environment_id = ?1 AND {column} = ?3"
                        ),
                        &[
                            receiver.as_str().into(),
                            scrubbed.as_str().into(),
                            plain.as_str().into(),
                        ],
                    )?;
                }
                let (arrival, kept) =
                    landing(Some(&stored), Owner::None, (cell("landing prior"), UNKEPT));
                let arrived = Arrived {
                    other: other.clone(),
                    row: row.clone(),
                    how: How::Follow,
                    from: cell("3"),
                    value: cell("3"),
                    arrival,
                };
                ledger::write(tx, &who, &receiver, &arrived, (Some(&stored), kept))?;
                let rows = tx.query(
                    "SELECT prior, was FROM config_sync_arrival WHERE environment_id = ?1",
                    &[receiver.as_str().into()],
                )?;
                let row = rows.first().unwrap();
                Ok((row.text(0)?.to_owned(), row.text(1)?.to_owned()))
            })
            .unwrap();
        assert_eq!((prior, was), (json_of(&cell("")), json_of(&sealed(""))));
    }

    /// An edge that meets a row other than the one its caller read fails corrupt,
    /// writing nothing: a landing over a row whose claim changed, and a rewind of a
    /// row that is gone.
    #[test]
    fn an_edge_that_misses_the_row_its_caller_read_fails_corrupt() {
        let (store, who, receiver) = shop();
        let other = EnvironmentId::parse(uuid(9)).unwrap();
        let row = row_id(&uuid(3), "variables.PLAIN").unwrap();
        let failed = store.storage.write(|tx| {
            plant_released(tx, &receiver, other.as_str(), &row);
            let read = Arrival::Pending {
                prior: cell("secret"),
                claim: Claim::Nobody,
            };
            let arrived = Arrived {
                other: other.clone(),
                row: row.clone(),
                how: How::Follow,
                from: cell("3"),
                value: cell("3"),
                arrival: read.clone(),
            };
            let landed = ledger::write(tx, &who, &receiver, &arrived, (Some(&read), Kept::PRIOR));
            let gone = row_id(&uuid(3), "variables.GONE").unwrap();
            let key = ledger::Key {
                receiver: receiver.as_str(),
                other: other.as_str(),
                row: &gone,
            };
            let rewound = ledger::rewind_sync(tx, &key);
            Ok([landed, rewound].map(|result| result.map_err(|error| error.message)))
        });
        let corrupt = Err(error::corrupt("Sync").message);
        assert_eq!(failed.unwrap(), [corrupt.clone(), corrupt]);
    }

    /// M4: `0037` turns CHECKs back on for the connection that copied its rows.
    #[test]
    fn a_migrated_sqlite_store_checks_every_arrival_it_writes() {
        let store =
            crate::ConfigStore::open("sqlite::memory:", crate::SealingKey::new(b"test").unwrap())
                .unwrap();
        let ignored = store
            .storage
            .write(|tx| {
                tx.query("PRAGMA ignore_check_constraints", &[])?
                    .first()
                    .map(|row| row.int(0))
                    .transpose()
            })
            .unwrap();
        assert_eq!(ignored, Some(0));
    }
}
