//! How changes move between two Environments: which base, which rules, and the ledger
//! of what arrived. Sync, Follow, a take and Own Copy all move through [`Move`];
//! nothing else lands one Environment's rows in another.
//!
//! A pair's base is what the two last shared (`config_sync_base`, keyed by the pair's
//! IDs in order), advanced by what lands. Each landed row is an arrival
//! (`config_sync_arrival`): until the receiver deploys it, a discard puts the base's
//! prior cell back so it is offered again, and Undo puts the receiver's own back.

use super::*;
use crate::removal::short_digest;
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
    /// Why nothing moves.
    nothing: String,
    pub(crate) from: SavedEnvironmentIntent,
    pub(crate) base: SavedEnvironmentIntent,
    pub(crate) hostnames: Hostnames,
    pub(super) rules: Rules,
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
        let (from_marks, into_marks) = marks(tx, &owner.summary.id, &branch.summary.id)?;
        Ok(Self {
            source: owner.summary.id.clone(),
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
            None => match made_with(tx, &from.summary.id)? {
                Some(base) => base,
                None => made_with(tx, &into.summary.id)?.unwrap_or_else(|| into.working.clone()),
            },
        };
        // Syncing anywhere but into its Parent, a Branch's own changes are those since
        // it last shared with its Parent; the rest it only inherited.
        let own = match row(tx, &from.summary.id)? {
            Some(row) if row.parent != into.summary.id => {
                let parent = base_of(tx, (&from.summary.id, &row.parent))?;
                Some(changed(&parent, &from.working))
            }
            _ => None,
        };
        let (from_marks, into_marks) = marks(tx, &from.summary.id, &into.summary.id)?;
        Ok(Self {
            source: from.summary.id.clone(),
            other: from.summary.id.clone(),
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
            },
        })
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
        Self::deployed(tx, (owner, branch), Way::Copy, nothing, copied)
    }

    /// The rows from `from` into `into`.
    pub(crate) fn plan(&self, into: &SavedEnvironmentIntent) -> Plan {
        plan(
            Sides {
                base: Some(&self.base),
                from: &self.from,
                into,
                hostnames: self.hostnames.clone(),
            },
            &self.rules,
        )
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

    pub(crate) fn version(&self) -> &str {
        &self.version
    }

    /// Land `picks` in `into`, `values` filling the secrets that need one, and advance
    /// the pair's base by exactly what landed, recording each row's arrival. `sync`
    /// names the Sync that lands them, for Undo. Returns the nodes staged.
    pub(crate) fn apply(
        &self,
        tx: &mut dyn Tx,
        who: &Actor,
        into: &mut Environment,
        picks: &BTreeSet<RowId>,
        values: &BTreeMap<RowId, SealedSecret>,
        sync: Option<&SyncId>,
    ) -> Result<Vec<NodeName>, RpcError> {
        let of = self.of;
        if picks.is_empty() {
            return Err(error::conflict(
                of.nothing.clone(),
                json!({ "version": self.version }),
            ));
        }
        let applied = self.plan.apply(picks, values).map_err(config)?;
        let carried = Carried::of(tx, &of.source, &of.from)?;
        let staged = land(tx, who, into, (&of.from, &carried), applied.next, picks)?;
        let how = match of.rules.way {
            Way::Follow => How::Follow,
            Way::Sync | Way::Copy => How::Sync,
        };
        for landed in applied.landed {
            let arrived = Arrived {
                other: of.other.clone(),
                row: landed.row,
                how,
                value: landed.value,
                arrival: Arrival::Pending {
                    prior: landed.prior,
                    was: landed.was,
                    sync: sync.cloned(),
                },
            };
            arrive(tx, who, &into.summary.id, &arrived)?;
        }
        share(tx, (&into.summary.id, &of.other), &applied.base)?;
        Ok(staged)
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
        let node = match node {
            NodeRef::Service(service) => {
                NodeName::Service(ServiceName::parse(service.slug.as_str()).ok()?)
            }
            NodeRef::Volume(volume) => {
                NodeName::Volume(VolumeName::parse(volume.name.as_str()).ok()?)
            }
        };
        let name = match row.at() {
            At::Node => None,
            At::Variable(key) => Some(format!("env.{key}")),
            At::Setting(setting) => {
                Some(ServiceSetting::of(*setting).map_or(at, |setting| setting.name().to_owned()))
            }
            At::Data | At::Name | At::Storage | At::Mount(_) => Some(at),
        };
        Some(NamedRow {
            row: row.clone(),
            node,
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
/// by `names`, `{"secret": true}`, or `{"secret": false}` for one without a value.
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
        Cell::Value(value) => match row.at() {
            At::Variable(key) => Some(key),
            At::Node | At::Data | At::Name | At::Storage | At::Setting(_) | At::Mount(_) => None,
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
fn base_of(
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
fn stored(
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

/// What Branch `id` was made with; none for a root.
fn made_with(
    tx: &mut dyn Tx,
    id: &EnvironmentId,
) -> Result<Option<SavedEnvironmentIntent>, RpcError> {
    tx.query(
        "SELECT made_with FROM config_environment_branch WHERE environment_id = ?1",
        &[id.as_str().into()],
    )?
    .first()
    .map(|row| row.intent(0, "Branch"))
    .transpose()
}

/// What `a` and `b` last shared is `base` now.
pub(super) fn share(
    tx: &mut dyn Tx,
    (a, b): (&EnvironmentId, &EnvironmentId),
    base: &SavedEnvironmentIntent,
) -> Result<(), RpcError> {
    let [a, b] = ordered(a, b);
    tx.execute(
        "INSERT INTO config_sync_base (environment_id, other_id, organization_id, base) \
         SELECT id, ?2, organization_id, ?3 FROM config_environment WHERE id = ?1 \
         ON CONFLICT (environment_id, other_id) DO UPDATE SET base = excluded.base",
        &[a.into(), b.into(), document(base).as_str().into()],
    )?;
    Ok(())
}

/// Put `cells` into the bases `receiver` shares with each other side, as one write
/// per pair. A pair gone with its other side has no base left to put back.
// ponytail: a prior that no longer fits the base (its node went since) leaves that
// row of the base as it is; it then reads as the receiver's own change. So a
// discarded Follow removal is not offered again: the node it brings back reads as
// the Branch's own. Put the node back into the base whole if that matters.
fn rewind_bases(
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
        let base = put(&base, &row, &cell).unwrap_or(base);
        bases.insert(other, base);
    }
    for (other, base) in &bases {
        share(tx, (receiver, other), base)?;
    }
    Ok(())
}

/// After a discard took `receiver`'s Working State from `before` to `after`: each
/// row that arrived there and is gone again returns its pair's base to what it held
/// before, so it is offered again. A Follow's stays a hint; a Sync's is forgotten.
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
        let at = row.at().to_string();
        let key = [
            receiver.as_str().into(),
            arrived.other.as_str().into(),
            row.lineage().into(),
            at.as_str().into(),
        ];
        match arrived.how {
            How::Follow => tx.execute(
                "UPDATE config_sync_arrival SET state = 'hint', prior = NULL, was = NULL \
                 WHERE environment_id = ?1 AND other_id = ?2 AND lineage = ?3 AND at = ?4",
                &key,
            )?,
            How::Sync => tx.execute(
                "DELETE FROM config_sync_arrival \
                 WHERE environment_id = ?1 AND other_id = ?2 AND lineage = ?3 AND at = ?4",
                &key,
            )?,
        };
        undone.push((arrived.other, row, prior));
    }
    rewind_bases(tx, receiver, undone)
}

/// `receiver` deployed `saved` to the nodes of `lineages`: what arrived there and
/// shipped stays. An arrival `saved` lacks, one that landed after it was published,
/// stays pending.
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
        tx.execute(
            "UPDATE config_sync_arrival SET state = 'settled', prior = NULL, was = NULL \
             WHERE environment_id = ?1 AND other_id = ?2 AND lineage = ?3 AND at = ?4",
            &[
                receiver.as_str().into(),
                arrived.other.as_str().into(),
                row.lineage().into(),
                row.at().to_string().as_str().into(),
            ],
        )?;
    }
    Ok(())
}

/// `other`'s `value` at `row` was delivered to `receiver` but stays a hint there.
pub(super) fn hint(
    tx: &mut dyn Tx,
    who: &Actor,
    (receiver, other): (&EnvironmentId, &EnvironmentId),
    row: &RowId,
    value: &Cell,
) -> Result<(), RpcError> {
    let arrived = Arrived {
        other: other.clone(),
        row: row.clone(),
        how: How::Follow,
        value: value.clone(),
        arrival: Arrival::Hint,
    };
    arrive(tx, who, receiver, &arrived)
}

/// How a row arrived.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum How {
    Follow,
    Sync,
}

/// Where an arrived row stands in its receiver.
enum Arrival {
    /// Staged, not deployed: `prior` is the pair base's cell before it landed and
    /// `was` the receiver's own, to rewind a discard and to undo `sync`, the Sync
    /// that landed it.
    Pending {
        prior: Cell,
        was: SealedCell,
        sync: Option<SyncId>,
    },
    /// A Follow the receiver changed too, or discarded.
    Hint,
    /// Deployed.
    Settled,
}

/// A row that arrived in a receiver from `other`.
struct Arrived {
    other: EnvironmentId,
    row: RowId,
    how: How,
    /// The cell delivered; redacted.
    value: Cell,
    arrival: Arrival,
}

/// Record `arrived` in `receiver`, replacing what arrived at that row from that side
/// before. A row still pending keeps the base's cell from before its first arrival.
fn arrive(
    tx: &mut dyn Tx,
    who: &Actor,
    receiver: &EnvironmentId,
    arrived: &Arrived,
) -> Result<(), RpcError> {
    let (state, prior, was, sync) = match &arrived.arrival {
        Arrival::Pending { prior, was, sync } => (
            "pending",
            Some(json_of(prior)),
            Some(json_of(was)),
            sync.as_ref().map(SyncId::as_str),
        ),
        Arrival::Hint => ("hint", None, None, None),
        Arrival::Settled => ("settled", None, None, None),
    };
    tx.execute(
        "INSERT INTO config_sync_arrival (environment_id, other_id, lineage, at, \
         organization_id, how, state, value, prior, was, sync_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
         ON CONFLICT (environment_id, other_id, lineage, at) DO UPDATE SET \
         how = excluded.how, state = excluded.state, value = excluded.value, \
         was = excluded.was, sync_id = excluded.sync_id, prior = CASE \
         WHEN config_sync_arrival.state = 'pending' AND excluded.state = 'pending' \
         THEN config_sync_arrival.prior ELSE excluded.prior END",
        &[
            receiver.as_str().into(),
            arrived.other.as_str().into(),
            arrived.row.lineage().into(),
            arrived.row.at().to_string().as_str().into(),
            who.organization.as_str().into(),
            storage::variant_text(&arrived.how).as_str().into(),
            state.into(),
            json_of(&arrived.value).as_str().into(),
            prior.as_deref().into(),
            was.as_deref().into(),
            sync.into(),
        ],
    )?;
    Ok(())
}

/// Which of an Environment's arrivals [`arrived`] reads.
enum Which<'a> {
    /// Those not yet deployed.
    Pending,
    /// Those from this Environment.
    From(&'a EnvironmentId),
    /// Those this Sync landed.
    Of(&'a SyncId),
}

/// What arrived in `receiver` that `which` selects.
fn arrived(
    tx: &mut dyn Tx,
    receiver: &EnvironmentId,
    which: Which<'_>,
) -> Result<Vec<Arrived>, RpcError> {
    const SELECT: &str = "SELECT other_id, lineage, at, how, state, value, prior, was, sync_id \
         FROM config_sync_arrival WHERE environment_id = ?1";
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
    };
    rows.iter()
        .map(|row| {
            let arrival = match row.text(4)? {
                "pending" => Arrival::Pending {
                    prior: row.json(6, "Sync")?,
                    was: row.json(7, "Sync")?,
                    sync: row.parse_optional(8, "Sync")?,
                },
                "hint" => Arrival::Hint,
                "settled" => Arrival::Settled,
                _ => return Err(error::corrupt("Sync")),
            };
            Ok(Arrived {
                other: row.parse(0, "Sync")?,
                row: row_id(row.text(1)?, row.text(2)?)?,
                how: row.variant(3, "Sync")?,
                value: row.json(5, "Sync")?,
                arrival,
            })
        })
        .collect()
}

/// What arrived in `receiver` from `other`, by row: the cell delivered.
pub(super) fn arrivals(
    tx: &mut dyn Tx,
    receiver: &EnvironmentId,
    other: &EnvironmentId,
) -> Result<BTreeMap<RowId, Cell>, RpcError> {
    Ok(arrived(tx, receiver, Which::From(other))?
        .into_iter()
        .map(|arrived| (arrived.row, arrived.value))
        .collect())
}

/// Undo Sync `sync` in `receiver`, whose lock the caller holds: put back what each
/// row it landed held before, and the pair bases' too. Refused when a row changed
/// since it landed (for a new node, any of its rows), or deployed. False when the
/// Sync landed nothing there.
pub(super) fn undo(
    tx: &mut dyn Tx,
    receiver: &mut Environment,
    sync: &SyncId,
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
    for arrived in rows {
        let Arrival::Pending { prior, was, .. } = arrived.arrival else {
            return Err(error::conflict(
                format!(
                    "{} is deployed: change it back instead",
                    label(&arrived.row)
                ),
                json!({ "row": arrived.row }),
            ));
        };
        others.push(arrived.other);
        landed.push(Landed {
            row: arrived.row,
            value: arrived.value,
            prior,
            was,
        });
    }
    receiver.working = match unapply(&receiver.working, &suffix(tx, receiver)?, &landed) {
        Ok(working) => working,
        Err(Unapplied::Changed(row)) => {
            return Err(error::conflict(
                format!(
                    "{} changed since it synced: change it back instead",
                    label(&row)
                ),
                json!({ "row": row }),
            ));
        }
        Err(Unapplied::Invalid(error)) => return Err(config(error)),
    };
    scope::save_working(tx, receiver)?;
    tx.execute(
        "DELETE FROM config_sync_arrival WHERE environment_id = ?1 AND sync_id = ?2",
        &[id.as_str().into(), sync.as_str().into()],
    )?;
    let priors = others
        .into_iter()
        .zip(landed)
        .map(|(other, landed)| (other, landed.row, landed.prior))
        .collect();
    rewind_bases(tx, &receiver.summary.id, priors)?;
    Ok(true)
}

/// How many changes a Sync of `from` into `into` carries by default: the Sync view's
/// ticked rows.
pub(crate) fn changes_into(
    tx: &mut dyn Tx,
    from: &Environment,
    into: &Environment,
) -> Result<usize, RpcError> {
    Ok(Move::sync(tx, from, into)?
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
    for pick in picks
        .iter()
        .filter(|pick| *pick.at() == At::Setting(Setting::Credentials))
    {
        let lineage = pick.lineage();
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
