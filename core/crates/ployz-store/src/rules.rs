//! The rules an Environment's authored configuration keeps as a whole, which no
//! single Setting can check alone: a Volume without Shared Writes has one writer, a
//! variable references only Services and variables that exist, variables never
//! reference each other in a cycle, and a Setup Command runs in a Service there is.
//!
//! Every write of Working State, and of an Environment's Setup Commands, passes
//! [`check_write`]. It refuses only a problem the write adds: an Environment that
//! already breaks a rule — saved before the rule existed, or copied from a Parent
//! that does — keeps editing and deploying, and a Deploy never re-checks.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::config::{
    BUILT_IN_VARIABLES, SavedEnvironmentIntent, SavedServiceIntent, SavedVariableValue, ValuePart,
    ValuePartOwner,
};
use ployz_core::{RpcError, ServiceName};
use serde_json::json;

use crate::SetupCommand;
use crate::error;
use crate::scope::EnvironmentSummary;

/// What the rules read of one Environment.
#[derive(Clone, Copy)]
pub(crate) struct Facts<'a> {
    /// Its Working State.
    pub(crate) working: &'a SavedEnvironmentIntent,
    /// The name of each node it uses live, by lineage.
    pub(crate) live: &'a BTreeMap<String, String>,
    /// The Setup Commands it hands each new Branch.
    pub(crate) setup: &'a [SetupCommand],
}

/// Refuse a write that turns `before` into `after` if `after` breaks a rule that
/// neither `before` nor `inherited` already broke. `inherited` is what the write
/// copies or restores — the Parent a Branch is made from, the other side of a
/// Sync or Follow, the Head a Discard returns to — read with `after`'s Live Nodes
/// and Setup Commands, so what it brings along is not new.
///
/// # Errors
/// The first problem the write adds, naming what breaks the rule and how to fix
/// it: `conflict` when the write removed what something else needs, or a second
/// writer of a Volume; `invalid_argument` for a value that is wrong as written;
/// `not_found` for a Setup Command naming no Service.
pub(crate) fn check_write(
    environment: &EnvironmentSummary,
    before: Facts<'_>,
    inherited: Option<&SavedEnvironmentIntent>,
    after: Facts<'_>,
) -> Result<(), RpcError> {
    let mut known = problems(before);
    if let Some(working) = inherited {
        known.extend(problems(Facts { working, ..after }));
    }
    let added: Vec<Problem> = problems(after)
        .into_iter()
        .filter(|problem| !known.iter().any(|old| old.covers(problem)))
        .collect();
    let Some(first) = added.first() else {
        return Ok(());
    };
    let refusing = Refusing {
        environment,
        before,
        after,
    };
    Err(match first {
        Problem::Writers { volume, replicas } => refusing.writers(volume, *replicas),
        Problem::Broken { service, wants, .. } => {
            // Every variable the write breaks the same way, so one refusal names them all.
            let keys: Vec<&str> = added
                .iter()
                .filter_map(|other| {
                    if let Problem::Broken {
                        service: s,
                        key,
                        wants: w,
                    } = other
                        && (s, w) == (service, wants)
                    {
                        Some(key.as_str())
                    } else {
                        None
                    }
                })
                .collect();
            refusing.broken(service, &keys, wants)
        }
        Problem::Cycle(members) => refusing.cycle(members),
        Problem::NoSetupService(service) => refusing.setup(service),
    })
}

/// One way an Environment breaks a rule, by lineage so a Branch copy has the same.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Problem {
    /// A Volume without Shared Writes, by lineage, and how many replicas write it.
    Writers { volume: String, replicas: u32 },
    /// Variable `key` of Service `service` (a lineage) references what isn't there.
    Broken {
        service: String,
        key: String,
        wants: Wants,
    },
    /// Variables, as (Service lineage, key), that reference each other in a cycle:
    /// one strongly connected group, so an unrelated edit finds the same one.
    Cycle(BTreeSet<(String, String)>),
    /// A Setup Command runs in this Service name, which no Service has.
    NoSetupService(ServiceName),
}

/// What a broken reference names that isn't there.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Wants {
    /// A Service, by lineage, neither in the Environment nor used live.
    Service(String),
    /// Variable `key` of the Service `owner` (a lineage), which it doesn't have.
    Variable { owner: String, key: String },
}

impl Problem {
    /// Whether `self`, already there, accounts for `other`: the same problem, a
    /// Volume with no fewer writers, or a cycle at least as wide.
    fn covers(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Writers { volume, replicas },
                Self::Writers {
                    volume: other,
                    replicas: more,
                },
            ) => volume == other && replicas >= more,
            (Self::Cycle(wide), Self::Cycle(narrow)) => wide.is_superset(narrow),
            _ => self == other,
        }
    }
}

/// Every rule `facts` breaks.
fn problems(facts: Facts<'_>) -> BTreeSet<Problem> {
    let Facts {
        working,
        live,
        setup,
    } = facts;
    let mut found = BTreeSet::new();
    for volume in working
        .volumes
        .iter()
        .filter(|volume| !volume.shared_writes)
    {
        let replicas = writers(working, &volume.resource_id)
            .iter()
            .map(|service| u32::from(service.config.replicas))
            .sum();
        if replicas > 1 {
            found.insert(Problem::Writers {
                volume: volume.resource_lineage_id.clone(),
                replicas,
            });
        }
    }
    for referrer in &working.services {
        for (key, owner, wanted) in references(referrer) {
            let mut broken = |wants| {
                found.insert(Problem::Broken {
                    service: referrer.lineage_id.clone(),
                    key: key.to_owned(),
                    wants,
                });
            };
            let owner = match owner {
                ValuePartOwner::Self_ => referrer,
                ValuePartOwner::Service { lineage_id } => match service(working, lineage_id) {
                    Some(owner) => owner,
                    // A node used live: its owner provides its variables.
                    None if live.contains_key(lineage_id) => continue,
                    None => {
                        broken(Wants::Service(lineage_id.clone()));
                        continue;
                    }
                },
            };
            let has = owner
                .variables
                .iter()
                .any(|variable| variable.key == wanted);
            if !has && !BUILT_IN_VARIABLES.contains(&wanted) {
                broken(Wants::Variable {
                    owner: owner.lineage_id.clone(),
                    key: wanted.to_owned(),
                });
            }
        }
    }
    found.extend(cycles(working).into_iter().map(Problem::Cycle));
    for command in setup {
        if !working
            .services
            .iter()
            .any(|service| service.slug == command.service.as_str())
        {
            found.insert(Problem::NoSetupService(command.service.clone()));
        }
    }
    found
}

/// The Service of `working` with lineage `lineage`.
fn service<'a>(
    working: &'a SavedEnvironmentIntent,
    lineage: &str,
) -> Option<&'a SavedServiceIntent> {
    working
        .services
        .iter()
        .find(|service| service.lineage_id == lineage)
}

/// Each reference in `service`'s variables: (its variable, the owner, the key).
fn references(service: &SavedServiceIntent) -> impl Iterator<Item = (&str, &ValuePartOwner, &str)> {
    service.variables.iter().flat_map(|variable| {
        let parts = match &variable.value {
            SavedVariableValue::Template { parts } => parts.as_slice(),
            SavedVariableValue::Literal { .. }
            | SavedVariableValue::Secret { .. }
            | SavedVariableValue::SecretWithoutValue => &[],
        };
        parts.iter().filter_map(move |part| match part {
            ValuePart::Ref { owner, key } => Some((variable.key.as_str(), owner, key.as_str())),
            ValuePart::Text { .. } => None,
        })
    })
}

/// The groups of variables that reference each other in a cycle, including one
/// that references itself.
// ponytail: a search per variable, O(V·(V+E)); Tarjan if Environments grow to
// thousands of variables.
fn cycles(working: &SavedEnvironmentIntent) -> BTreeSet<BTreeSet<(String, String)>> {
    let mut edges: BTreeMap<(&str, &str), Vec<(&str, &str)>> = BTreeMap::new();
    for service in &working.services {
        for variable in &service.variables {
            edges
                .entry((&service.lineage_id, &variable.key))
                .or_default();
        }
        for (key, owner, wanted) in references(service) {
            let owner = match owner {
                ValuePartOwner::Self_ => service.lineage_id.as_str(),
                ValuePartOwner::Service { lineage_id } => lineage_id,
            };
            edges
                .entry((&service.lineage_id, key))
                .or_default()
                .push((owner, wanted));
        }
    }
    let reach = |from: (&str, &str)| {
        let mut seen = BTreeSet::new();
        let mut next = edges.get(&from).cloned().unwrap_or_default();
        while let Some(node) = next.pop() {
            if seen.insert(node) {
                next.extend(edges.get(&node).into_iter().flatten().copied());
            }
        }
        seen
    };
    let reached: BTreeMap<_, _> = edges.keys().map(|&node| (node, reach(node))).collect();
    reached
        .iter()
        .filter(|(node, seen)| seen.contains(*node))
        .map(|(node, seen)| {
            seen.iter()
                .filter(|other| reached.get(*other).is_some_and(|back| back.contains(node)))
                .map(|&(lineage, key)| (lineage.to_owned(), key.to_owned()))
                .collect()
        })
        .collect()
}

/// The Services of `intent` that mount Volume `id` and run a replica.
fn writers<'a>(intent: &'a SavedEnvironmentIntent, id: &str) -> Vec<&'a SavedServiceIntent> {
    intent
        .services
        .iter()
        .filter(|service| service.config.replicas > 0)
        .filter(|service| {
            service
                .volume_attachments
                .iter()
                .any(|mount| mount.volume_resource_id == id)
        })
        .collect()
}

/// `services` by name, comma-separated.
fn slugs(services: &[&SavedServiceIntent]) -> String {
    services
        .iter()
        .map(|service| service.slug.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The refusal of a write that turns `before` into `after`, one way per rule.
struct Refusing<'a> {
    environment: &'a EnvironmentSummary,
    before: Facts<'a>,
    after: Facts<'a>,
}

impl<'a> Refusing<'a> {
    /// `--env … --project …`, for a `next` command.
    fn scope(&self) -> String {
        let EnvironmentSummary { name, project, .. } = self.environment;
        format!("--env {name} --project {project}")
    }

    /// A lineage's Service name, in `after` or, once removed, in `before`.
    fn name(&self, lineage: &str) -> String {
        [self.after, self.before]
            .iter()
            .find_map(|facts| {
                service(facts.working, lineage)
                    .map(|service| service.slug.clone())
                    .or_else(|| facts.live.get(lineage).cloned())
            })
            .unwrap_or_else(|| "a removed Service".to_owned())
    }

    /// A second writer of Volume `volume` (lineage): turning Shared Writes off over
    /// several, a replica more of the one Service mounting it, or another Service.
    fn writers(&self, volume: &str, replicas: u32) -> RpcError {
        let find = |working: &'a SavedEnvironmentIntent| {
            let mut volumes = working.volumes.iter();
            volumes.find(|candidate| candidate.resource_lineage_id == volume)
        };
        let node = find(self.after.working).expect("a Volume with writers is in Working State");
        let was = find(self.before.working);
        let turned_off = was.is_some_and(|was| was.shared_writes);
        let writing = writers(self.after.working, &node.resource_id);
        let old = was
            .map(|was| writers(self.before.working, &was.resource_id))
            .unwrap_or_default();
        let scope = self.scope();
        let name = &node.name;
        let allow = format!("ployz volume set {name} --shared-writes");
        let (message, next) = match writing.as_slice() {
            _ if turned_off => (
                format!(
                    "{name} is written by {}; leave one Service with 1 replica before turning shared writes off",
                    slugs(&writing)
                ),
                format!("ployz volume inspect {name} {scope}"),
            ),
            [service] => (
                format!(
                    "{name} is attached to {}; set replicas to 1, or allow shared writes: {allow}",
                    service.slug
                ),
                format!("{allow} {scope}"),
            ),
            // Name the Services that mounted it first; for a new Volume, its first.
            [first, ..] => (
                format!(
                    "{name} is already mounted by {}; allow shared writes first: {allow}",
                    if old.is_empty() {
                        first.slug.clone()
                    } else {
                        slugs(&old)
                    }
                ),
                format!("{allow} {scope}"),
            ),
            [] => unreachable!("more than one writer means a Service writes it"),
        };
        error::conflict(
            message,
            json!({ "volume": name, "writers": replicas, "next": next }),
        )
    }

    /// Variables `keys` of `service` (a lineage) reference what isn't there:
    /// `conflict` once the write removed it, else `invalid_argument` for the value.
    fn broken(&self, referrer: &str, keys: &[&str], wants: &Wants) -> RpcError {
        let referrer = self.name(referrer);
        let removed = |target: String| {
            error::conflict(
                format!(
                    "{referrer} references {target} ({}); remove those references first",
                    keys.join(", ")
                ),
                json!({ "service": referrer, "references": target, "variables": keys }),
            )
        };
        let (owner, wanted) = match wants {
            Wants::Service(lineage) => return removed(self.name(lineage)),
            Wants::Variable { owner, key } => (owner, key),
        };
        let owner_name = self.name(owner);
        let had = service(self.before.working, owner)
            .is_some_and(|old| old.variables.iter().any(|variable| variable.key == *wanted));
        if had {
            return removed(format!("{owner_name}.{wanted}"));
        }
        let has: Vec<&str> = service(self.after.working, owner)
            .map(|owner| {
                crate::variables::sorted(owner)
                    .into_iter()
                    .map(|variable| variable.key.as_str())
                    .collect()
            })
            .unwrap_or_default();
        let key = keys.first().expect("the refused problem names its own key");
        error::invalid(
            format!(
                "{key}: {owner_name} has no variable {wanted}; it has {}, and the built-ins {}",
                if has.is_empty() {
                    "none of its own".to_owned()
                } else {
                    has.join(", ")
                },
                BUILT_IN_VARIABLES.join(", "),
            ),
            json!({
                "variable": key,
                "service": owner_name,
                "keys": has,
                "built_ins": BUILT_IN_VARIABLES,
            }),
        )
    }

    /// Variables `members` reference each other, or one itself, in a cycle.
    fn cycle(&self, members: &BTreeSet<(String, String)>) -> RpcError {
        let mut names: Vec<String> = members
            .iter()
            .map(|(lineage, key)| format!("{}.{key}", self.name(lineage)))
            .collect();
        names.sort();
        let message = match names.as_slice() {
            [one] => format!("{one} references itself"),
            [rest @ .., last] => format!(
                "{} and {last} reference each other in a cycle",
                rest.join(", ")
            ),
            [] => unreachable!("a cycle has a member"),
        };
        error::invalid(message, json!({ "cycle": names }))
    }

    /// A Setup Command runs in `service`, which no Service has: `conflict` once the
    /// write removed or renamed it, else `not_found` like any unknown Service name.
    fn setup(&self, service: &ServiceName) -> RpcError {
        let had = self
            .before
            .working
            .services
            .iter()
            .any(|old| old.slug == service.as_str());
        if !had {
            return error::choices(
                format!(
                    "No Service named {service} in Environment {} to run a Setup Command",
                    self.environment.name
                ),
                service.as_str(),
                self.after
                    .working
                    .services
                    .iter()
                    .map(|service| service.slug.as_str()),
            );
        }
        let scope = self.scope();
        let next = match self.after.setup {
            [_] => format!("ployz env setup --clear {scope}"),
            _ => format!("ployz env setup --setup SERVICE=COMMAND {scope}"),
        };
        error::conflict(
            format!(
                "New Branches run a Setup Command in {service}; set the Setup Commands without it first"
            ),
            json!({ "service": service, "next": next }),
        )
    }
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
pub(crate) mod tests {
    use ployz_core::config::{
        SavedEnvironmentIntent, SavedVariableValue, ValuePart, ValuePartOwner,
    };
    use ployz_core::{RpcError, RpcErrorCode, ServiceName};
    use serde_json::{Value, json};

    use crate::{
        Actor, Change, ConfigStore, CreateBranch, CreateProject, CreateService, CreateVolume, Edit,
        EnvironmentId, EnvironmentName, EnvironmentRef, Mount, OrganizationId, ProjectId,
        ProjectName, RemoveService, ServiceLineageId, SetBranchSetup, SettingPath, SetupCommand,
        VolumeId, VolumeName,
    };

    fn uuid(n: u8) -> String {
        format!("00000000-0000-4000-8000-0000000000{n:02}")
    }

    /// Production: `db`, which mounts Volume `data`, and `redis`.
    fn shop() -> (ConfigStore, Actor) {
        let store =
            ConfigStore::open("sqlite::memory:", crate::SealingKey::new(b"test").unwrap()).unwrap();
        let who = Actor::system(OrganizationId::parse("org").unwrap());
        store
            .write(
                &who,
                &CreateProject {
                    id: ProjectId::parse(uuid(1)).unwrap(),
                    name: ProjectName::parse("shop").unwrap(),
                    default_environment: EnvironmentId::parse(uuid(2)).unwrap(),
                },
            )
            .unwrap();
        for (n, name) in [(3, "db"), (4, "redis")] {
            store
                .write(
                    &who,
                    &CreateService {
                        id: ServiceLineageId::parse(uuid(n)).unwrap(),
                        environment: EnvironmentRef::default(),
                        name: ServiceName::parse(name).unwrap(),
                        image: Some("postgres:17".into()),
                        template: None,
                    },
                )
                .unwrap();
        }
        store
            .write(
                &who,
                &CreateVolume {
                    id: VolumeId::parse(uuid(5)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: VolumeName::parse("data").unwrap(),
                    storage: ployz_core::config::VolumeKind::Docker {},
                    mounts: vec![Mount {
                        service: ServiceName::parse("db").unwrap(),
                        path: "/data".into(),
                    }],
                    shared_writes: false,
                },
            )
            .unwrap();
        (store, who)
    }

    /// Write production's Working State as `change` leaves it, past every rule:
    /// how an Environment saved before a rule existed looks.
    pub(crate) fn seed(
        store: &ConfigStore,
        who: &Actor,
        change: impl FnOnce(&mut SavedEnvironmentIntent),
    ) {
        store
            .storage
            .write(|tx| {
                let mut environment = crate::scope::lock(tx, who, &EnvironmentRef::default())?;
                change(&mut environment.working);
                tx.execute(
                    "UPDATE config_environment SET working = ?1 WHERE id = ?2",
                    &[
                        serde_json::to_string(&environment.working)
                            .unwrap()
                            .as_str()
                            .into(),
                        environment.summary.id.as_str().into(),
                    ],
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn set(store: &ConfigStore, who: &Actor, changes: &[(&str, Value)]) -> Result<(), RpcError> {
        edit(
            store,
            who,
            changes
                .iter()
                .map(|(path, value)| Change::Set {
                    path: SettingPath::parse(path).unwrap(),
                    value: value.clone(),
                })
                .collect(),
        )
    }

    fn edit(store: &ConfigStore, who: &Actor, changes: Vec<Change>) -> Result<(), RpcError> {
        store
            .write(
                who,
                &Edit {
                    environment: EnvironmentRef::default(),
                    expect: None,
                    changes,
                },
            )
            .map(drop)
    }

    fn remove(store: &ConfigStore, who: &Actor, service: &str) -> Result<(), RpcError> {
        store
            .write(
                who,
                &RemoveService {
                    environment: EnvironmentRef::default(),
                    service: ServiceName::parse(service).unwrap(),
                },
            )
            .map(drop)
    }

    /// A Branch of production copying `copy`.
    fn branch(store: &ConfigStore, who: &Actor, copy: &[&str]) -> Result<(), RpcError> {
        store
            .write(
                who,
                &CreateBranch {
                    id: EnvironmentId::parse(uuid(9)).unwrap(),
                    from: EnvironmentRef::default(),
                    name: EnvironmentName::parse("feature").unwrap(),
                    copy: copy
                        .iter()
                        .map(|name| crate::NodeName::parse(name).unwrap())
                        .collect(),
                    live: Vec::new(),
                    setup: Vec::new(),
                    keep: false,
                    fix: None,
                },
            )
            .map(drop)
    }

    fn setup(store: &ConfigStore, who: &Actor, services: &[&str]) -> Result<(), RpcError> {
        store
            .write(
                who,
                &SetBranchSetup {
                    environment: EnvironmentRef::default(),
                    setup: services
                        .iter()
                        .map(|service| SetupCommand {
                            service: ServiceName::parse(*service).unwrap(),
                            command: "seed".into(),
                        })
                        .collect(),
                },
            )
            .map(drop)
    }

    /// An edit no rule cares about.
    fn unrelated(store: &ConfigStore, who: &Actor) -> Result<(), RpcError> {
        set(store, who, &[("redis.env.MODE", json!("fast"))])
    }

    #[test]
    fn a_sync_that_lands_a_broken_reference_is_refused() {
        let (store, who) = shop();
        branch(&store, &who, &["db", "redis"]).unwrap();
        let feature = EnvironmentRef {
            project: None,
            environment: Some(EnvironmentName::parse("feature").unwrap()),
        };
        let changes = [
            ("db.env.PASS", json!("pw")),
            ("redis.env.DB_PASS", json!("${{ db.PASS }}")),
        ];
        store
            .write(
                &who,
                &Edit {
                    environment: feature.clone(),
                    expect: None,
                    changes: changes
                        .iter()
                        .map(|(path, value)| Change::Set {
                            path: SettingPath::parse(path).unwrap(),
                            value: value.clone(),
                        })
                        .collect(),
                },
            )
            .unwrap();
        // Syncing the reference without the variable it names breaks production.
        let query = crate::SyncQuery {
            from: feature.clone(),
            into: None,
            when: crate::When::Now,
        };
        let view = store.read(&who, &query).unwrap();
        let sync = |labels: &[&str]| {
            store.write(
                &who,
                &crate::SyncChanges {
                    from: feature.clone(),
                    into: None,
                    when: crate::When::Now,
                    close_after: false,
                    version: view.version.clone(),
                    picks: Some(labels.iter().map(|label| (*label).to_owned()).collect()),
                    skip: Vec::new(),
                    values: std::collections::BTreeMap::new(),
                },
            )
        };
        let refused = sync(&["redis.env.DB_PASS"]).unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::InvalidArgument, "{refused:?}");
        assert!(
            refused
                .message
                .starts_with("DB_PASS: db has no variable PASS")
        );
        sync(&["redis.env.DB_PASS", "db.env.PASS"]).unwrap();
    }

    #[test]
    fn a_write_is_refused_only_for_what_it_breaks() {
        let (store, who) = shop();
        set(&store, &who, &[("db.replicas", json!(2))]).unwrap_err();
        // What production already breaks keeps it editing, and a Branch copies it.
        seed(&store, &who, |working| {
            for service in &mut working.services {
                service.config.replicas = 2;
            }
        });
        unrelated(&store, &who).unwrap();
        branch(&store, &who, &["db"]).unwrap();
        // Breaking it further is still refused.
        let refused = set(&store, &who, &[("db.replicas", json!(3))]).unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::Conflict);
    }

    #[test]
    fn removing_what_a_variable_references_is_refused() {
        let (store, who) = shop();
        set(
            &store,
            &who,
            &[
                ("db.env.PASS", json!("pw")),
                ("redis.env.DB_HOST", json!("${{ db.PLOYZ_PRIVATE_DOMAIN }}")),
                ("redis.env.DB_PASS", json!("${{ db.PASS }}")),
            ],
        )
        .unwrap();
        let refused = remove(&store, &who, "db").unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::Conflict);
        assert_eq!(
            refused.message,
            "redis references db (DB_HOST, DB_PASS); remove those references first"
        );
        let unset = Change::Unset {
            path: SettingPath::parse("db.env.PASS").unwrap(),
        };
        let refused = edit(&store, &who, vec![unset]).unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::Conflict);
        assert_eq!(
            refused.message,
            "redis references db.PASS (DB_PASS); remove those references first"
        );
        let refused = set(&store, &who, &[("redis.env.X", json!("${{ db.NOPE }}"))]).unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
        assert!(
            refused
                .message
                .starts_with("X: db has no variable NOPE; it has PASS"),
            "{refused:?}"
        );

        // Saved with db gone, production keeps editing; a Branch copies the reference.
        seed(&store, &who, |working| {
            working.services.retain(|service| service.slug != "db");
        });
        unrelated(&store, &who).unwrap();
        branch(&store, &who, &["redis"]).unwrap();
    }

    #[test]
    fn a_reference_cycle_is_refused_when_written() {
        let (store, who) = shop();
        let refused = set(
            &store,
            &who,
            &[
                ("redis.env.A", json!("${{ B }}")),
                ("redis.env.B", json!("${{ db.C }}")),
                ("db.env.C", json!("${{ redis.A }}")),
            ],
        )
        .unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
        assert_eq!(
            refused.message,
            "db.C, redis.A and redis.B reference each other in a cycle"
        );
        let refused = set(&store, &who, &[("redis.env.SELF", json!("${{ SELF }}"))]).unwrap_err();
        assert_eq!(refused.message, "redis.SELF references itself");

        set(&store, &who, &[("redis.env.SELF", json!("x"))]).unwrap();
        seed(&store, &who, |working| {
            let redis = working
                .services
                .iter_mut()
                .find(|service| service.slug == "redis")
                .unwrap();
            redis.variables[0].value = SavedVariableValue::Template {
                parts: vec![ValuePart::Ref {
                    owner: ValuePartOwner::Self_,
                    key: "SELF".into(),
                }],
            };
        });
        unrelated(&store, &who).unwrap();
        branch(&store, &who, &["redis"]).unwrap();
        // Widening the cycle is new.
        let wider = [
            ("redis.env.SELF", json!("${{ OTHER }}")),
            ("redis.env.OTHER", json!("${{ SELF }}")),
        ];
        set(&store, &who, &wider).unwrap_err();
    }

    #[test]
    fn a_setup_command_runs_in_a_service_there_is() {
        let (store, who) = shop();
        let refused = setup(&store, &who, &["nosuch"]).unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::NotFound);
        assert_eq!(
            refused.message,
            "No Service named nosuch in Environment production to run a Setup Command"
        );
        setup(&store, &who, &["db"]).unwrap();
        let refused = remove(&store, &who, "db").unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::Conflict);
        assert_eq!(
            refused.message,
            "New Branches run a Setup Command in db; set the Setup Commands without it first"
        );
        assert_eq!(
            refused.details["next"],
            "ployz env setup --clear --env production --project shop"
        );

        // Saved with db gone, production keeps editing and changing its other Setup Commands.
        seed(&store, &who, |working| {
            working.services.retain(|service| service.slug != "db");
        });
        unrelated(&store, &who).unwrap();
        setup(&store, &who, &["db", "redis"]).unwrap();
    }
}
