//! `ployz key`: CI and agent keys. Each is a grant a person signs with this computer's
//! Device: the places it may deploy to and the Volumes it may mount. The CLI makes the
//! key, resolves names through the Store, shows what it resolved, and hands the signed
//! change to every Server over its own connection.
//!
//! `ployz key add pk1_…` instead trusts an install command's key on this Server only;
//! see [`line`].

use std::collections::BTreeMap;

use clap::{Arg, ArgGroup, ArgMatches, Command};
use ployz_core::authority::{
    GRANT_MOVED, Grant, GrantKind, GrantVolume, KeyChange, KeyKind, KeyName, KeyPlace, KeyPlaces,
    KeyScope, NewKey, Prior, SigningPublicKey, Timestamp,
};
use ployz_core::{
    CheckCode, DeviceCode, DeviceKey, DeviceSet, Hint, InspectRequest, KEY_PRIOR_CAPABILITY,
    KEY_STEP_CAPABILITY, MachineFailure, MachineId, MachineTarget, Retired, RpcError, RpcErrorCode,
    SCOPED_KEYS_CAPABILITY, SetDeviceRequest, op,
};
use serde::Serialize;

use super::device::Reach;
use super::{Error, config_path, leaf_matches, route, runtime};
use crate::cli::{base, many, positional, switch, value, yes};
use crate::cluster::Call;
use crate::connect::Client;
use crate::device::{self, DeviceSigning};
use crate::ui::{self, Cell, Gaps, Table, Tone};

mod line;
pub(super) mod members;
mod rotate;
mod wanted;

pub(crate) use rotate::{rotate, rotate_command};

use members::{Bundle, Member, Roster};
use wanted::{Typed, Wanted};

/// How many times `key scope` reads again after a Server says the grant moved.
const ROUNDS: usize = 3;

/// What `key ls` and `key scope` show for a CI key made before every key named its
/// Environments.
const UNSCOPED: &str = "nowhere: it names no environment, so make a new key with --env";

const LEAKED: &str = "If a key leaks, run ployz key rm <code>, then deploy from a terminal. \
                      Don't rotate it: rotating keeps what it deployed running.";

pub(crate) fn group(command: Command) -> Command {
    command
        .about(
            "Make and manage the keys CI and agents deploy with, or trust an install command's key",
        )
        .after_help(LEAKED)
}

pub(crate) fn add_command(command: Command) -> Command {
    scope_args(base(
        command,
        "Make a CI or agent key, or trust the key from an install command on this Server",
    ))
    .long_about(concat!(
        "With --ci or --agent, make a CI or agent key on every server, and print its ",
        "PLOYZ_KEY once.\n\n",
        "With the key from an install command (pk1_…), trust it on this Server only. Run it ",
        "on each Server that should trust it. Before a Server joins a Cluster, it replaces ",
        "the key the Server trusts to join or found one, once you confirm at this Server's ",
        "terminal.",
    ))
    .after_help(concat!(
        "Examples:\n",
        "  ployz key add --ci --env production --name github-actions\n",
        "  ployz key add --agent --env staging --service web\n",
        "  ployz key add --ci --env production --volume db/data\n",
        "  sudo ployz key add pk1_eyJraW5kIjoi…",
    ))
    .arg(
        positional("line", false)
            .help("The key from your install command (pk1_…), to trust on this Server")
            .conflicts_with_all(["env", "service", "volume", "name"]),
    )
    .arg(switch("ci", None).help("A key for a CI pipeline"))
    .arg(switch("agent", None).help("A key for an agent"))
    .group(
        ArgGroup::new("kind")
            .args(["line", "ci", "agent"])
            .required(true),
    )
}

pub(crate) fn scope_command(command: Command) -> Command {
    scope_args(base(
        command,
        "Change where a CI or agent key may deploy, on every server",
    ))
    .after_help(concat!(
        "The flags name the whole new grant: a volume or name the key holds and the flags ",
        "leave out is dropped. A change that only narrows the key goes through without ",
        "asking. One that widens it, drops a volume or the name, or misses a server asks ",
        "first.\n\n",
        "Examples:\n",
        "  ployz key scope mmg42klgyq --env production --service web",
    ))
    .arg(code_arg())
    .arg(yes(
        "Fail instead of asking when the change widens the key or drops a volume or its name",
    ))
}

pub(crate) fn remove_command(command: Command) -> Command {
    base(
        command,
        "Stop trusting a CI or agent key on every server, for good",
    )
    .after_help(concat!(
        "It removes only CI and agent keys. Remove a device or passkey with ployz device rm.\n",
        "If the key leaked, deploy from a terminal afterwards.\n\n",
        "Examples:\n",
        "  ployz key rm mmg42klgyq",
    ))
    .arg(code_arg())
}

pub(crate) fn list_command(command: Command) -> Command {
    base(command, "List CI and agent keys and where each may deploy")
}

fn code_arg() -> Arg {
    positional("code", true).help("The key's code from ployz key ls")
}

/// `--env`, `--service`, `--volume`, `--name` and `--project`. `--env` is the key's
/// places, never the Environment a command addresses, so `PLOYZ_ENV` doesn't fill it.
fn scope_args(command: Command) -> Command {
    command
        .arg(
            many("env", None)
                .value_name("ENV")
                .help("An environment the key may deploy to; repeat for more. At least one"),
        )
        .arg(
            value("service", None)
                .value_name("SERVICE")
                .help("Only this service, in the one --env"),
        )
        .arg(
            many("volume", None)
                .value_name("SERVICE/VOLUME")
                .help("A volume the key's deploys may mount, in each --env; repeat for more"),
        )
        .arg(
            value("name", None)
                .value_name("NAME")
                .help("What to call the key, such as the pipeline that holds it"),
        )
        .arg(super::store::project_arg())
}

pub(crate) fn add(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    if let Some(typed) = leaf.get_one::<String>("line") {
        return line::add(typed);
    }
    let kind = match leaf.get_flag("agent") {
        true => GrantKind::Agent,
        false => GrantKind::Ci,
    };
    let typed = Typed::of(leaf)?;
    let signing = own_signing(leaf)?;
    let store = super::store::store(root)?;
    let places = wanted::places(&store, &typed)?;
    let runtime = runtime()?;
    let mut client = runtime.block_on(route::connect_full(leaf, context(leaf)))?;
    let roster = runtime.block_on(Roster::read(&mut client))?;
    roster.require_all()?;
    let cluster = roster.cluster()?;
    let wanted = wanted::wanted(&store, &typed, places, (roster.trusted()?, cluster))?;
    let seed = device::key::generate();
    let public = SigningPublicKey::from(device::key::public(&seed));
    show(kind, public, &wanted, &roster);
    ui::confirm(
        &format!("Make {} key {}?", KeyKind::from(kind), public.code()),
        || {
            Error::coded(
                RpcErrorCode::ConfirmationRequired,
                "Making a key needs you at a terminal. Nothing changed.",
            )
        },
        "Nothing changed.",
    )?;
    let change = KeyChange::Trust {
        key: new_key(kind, public, &wanted),
        members: roster.members.iter().map(|member| member.key).collect(),
        prior: None,
    };
    let bundle = Bundle::sign(&signing, cluster, vec![change])?;
    let (answers, gaps) = runtime.block_on(bundle.send(&client, &roster.reach));
    let made = Made {
        code: public.code(),
        kind,
        name: wanted.name,
        ployz_key: hex::encode(seed),
        servers: answered(&roster.reach, answers),
    };
    let partial = !gaps.is_complete();
    ui::finish_fanout("key", &made, &gaps, || {
        ui::rows(&results(&made.servers));
        ui::stream(format_args!("PLOYZ_KEY={}", made.ployz_key));
        ui::note("Store PLOYZ_KEY as a secret where the key deploys from. It isn't shown again.");
        if partial {
            ui::note(format_args!(
                "To trust it on the servers that missed it, run ployz key scope {} with the same flags.",
                made.code
            ));
        }
    })
}

/// Say what a new key may do before the person signs it.
fn show(kind: GrantKind, key: SigningPublicKey, wanted: &Wanted, roster: &Roster) {
    let named = wanted
        .name
        .as_ref()
        .map(|name| format!(" ({name})"))
        .unwrap_or_default();
    ui::note(format_args!(
        "{} key {}{named} can deploy to:",
        KeyKind::from(kind),
        DeviceKey::from(key).check_code()
    ));
    for line in &wanted.shown {
        ui::note(format_args!("  {line}"));
    }
    ui::note(format_args!("Trusted on: {}.", roster.names()));
}

fn new_key(kind: GrantKind, key: SigningPublicKey, wanted: &Wanted) -> Box<NewKey> {
    let (scope, volumes, name) = (
        wanted.scope.clone(),
        wanted.volumes.clone(),
        wanted.name.clone(),
    );
    Box::new(match kind {
        GrantKind::Ci => NewKey::Ci {
            key,
            scope,
            volumes,
            name,
        },
        GrantKind::Agent => NewKey::Agent {
            key,
            scope,
            volumes,
            name,
        },
    })
}

/// This computer's Device, which alone signs a key change here. A CI or agent key in
/// `PLOYZ_KEY` never makes or changes keys.
fn own_signing(leaf: &ArgMatches) -> Result<DeviceSigning, Error> {
    if std::env::var_os(crate::env::KEY).is_some() {
        return Err(Error::usage(
            "PLOYZ_KEY is set, but a CI or agent key can't make or change keys. Unset it and run this from your own computer.",
        ));
    }
    DeviceSigning::load(&device::key::path(&config_path(leaf)?))?.ok_or_else(|| {
        Error::conflict(
            "Making or changing a key needs this computer's approved device. Nothing changed.",
        )
        .hint(Hint::Next(
            "ployz device approve --this-device --connect ssh://root@HOST".to_owned(),
        ))
    })
}

pub(crate) fn scope(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let code = typed_code(leaf)?;
    let typed = Typed::of(leaf)?;
    let signing = own_signing(leaf)?;
    let store = super::store::store(root)?;
    let yes = leaf.get_flag("yes");
    let runtime = runtime()?;
    let mut client = runtime.block_on(route::connect_full(leaf, context(leaf)))?;
    let mut settled: BTreeMap<MachineId, (String, DeviceSet)> = BTreeMap::new();
    let mut resolved: Option<Vec<wanted::Resolved>> = None;
    for round in 1..=ROUNDS {
        let roster = runtime.block_on(Roster::read(&mut client))?;
        let (key, kind) = held_key(&roster.members, code)?;
        let places = match resolved.take() {
            Some(places) => places,
            None => wanted::places(&store, &typed)?,
        };
        let wanted = wanted::wanted(
            &store,
            &typed,
            places.clone(),
            (roster.trusted()?, roster.cluster()?),
        )?;
        resolved = Some(places);
        let held: Vec<(&Member, Option<&Grant>)> = roster
            .members
            .iter()
            .map(|member| (member, member.grant(code).map(|held| &held.grant)))
            .collect();
        let after = Contents::wanted(&wanted);
        let before = before(&held);
        let changes = edits(kind, key, &wanted, &held);
        if changes.is_empty() {
            let scoped = scoped(code, before, after, &settled);
            return finish_scope(&scoped, &roster.gaps);
        }
        show_change(code, &before, &after, &roster);
        // Asked again each round: a grant that moved is a new change to read.
        let grants: Vec<Option<&Grant>> = held.iter().map(|(_, grant)| *grant).collect();
        if let Consent::Ask(why) = consent(code, &grants, roster.gaps.is_complete(), &after, yes)? {
            ui::confirm(
                &format!("Change key {code}?"),
                || why.refusal(code),
                "Nothing changed.",
            )?;
        }
        let bundle = Bundle::sign(&signing, roster.cluster()?, changes)?;
        let (answers, mut gaps) = runtime.block_on(bundle.send(&client, &roster.reach));
        for (machine_id, set) in answers {
            settled.insert(machine_id, (roster.reach.name(&machine_id), set));
        }
        settled.retain(|machine_id, _| {
            !gaps
                .failures
                .iter()
                .any(|failure| failure.machine_id == *machine_id)
        });
        let moved = gaps.failures.iter().any(grant_moved);
        if !moved || round == ROUNDS {
            // A Member the read missed may still hold the old grant.
            let unread = |machine_id: &MachineId| !settled.contains_key(machine_id);
            let failures: Vec<_> = roster
                .gaps
                .failures
                .iter()
                .filter(|failure| unread(&failure.machine_id))
                .cloned()
                .collect();
            let omitted: Vec<_> = roster.gaps.omitted.iter().copied().filter(unread).collect();
            gaps.extend(&failures, &omitted);
            let scoped = scoped(code, before, after, &settled);
            return finish_scope(&scoped, &gaps);
        }
        ui::note("A server's copy of the key changed while this ran. Reading it again.");
    }
    unreachable!("the last round returns")
}

/// Whether a Member refused a key change because the grant it named had moved there,
/// the one refusal a fresh read can settle.
fn grant_moved(failure: &MachineFailure<RpcError>) -> bool {
    failure.error.details.get(GRANT_MOVED).is_some()
}

/// The key `code` names in full, and its kind, as the Members hold it.
fn held_key(members: &[Member], code: DeviceCode) -> Result<(SigningPublicKey, GrantKind), Error> {
    let mut held = members
        .iter()
        .filter_map(|member| member.grant(code))
        .map(|held| (held.grant.key, held.grant.kind));
    let Some(first) = held.next() else {
        return Err(Error::not_found(format!(
            "No server holds a CI or agent key {code}. Nothing changed."
        ))
        .hint(Hint::Next("ployz key ls".to_owned())));
    };
    if held.any(|other| other != first) {
        return Err(Error::conflict(format!(
            "Code {code} names more than one key on your servers. Nothing changed."
        )));
    }
    Ok(first)
}

/// One trust per grant the Members hold, each naming the grant it replaces, for every
/// Member that doesn't already hold `wanted`.
fn edits(
    kind: GrantKind,
    key: SigningPublicKey,
    wanted: &Wanted,
    held: &[(&Member, Option<&Grant>)],
) -> Vec<KeyChange> {
    let mut by_read: BTreeMap<Option<u64>, (Option<Timestamp>, Vec<_>)> = BTreeMap::new();
    for (member, grant) in held {
        if grant.is_some_and(|grant| holds_exactly(grant, wanted)) {
            continue;
        }
        let read = grant.map(|grant| grant.issued_at);
        by_read
            .entry(read.map(Timestamp::as_millis))
            .or_insert_with(|| (read, Vec::new()))
            .1
            .push(member.key);
    }
    by_read
        .into_values()
        .map(|(read, members)| KeyChange::Trust {
            key: new_key(kind, key, wanted),
            members,
            prior: read.map(|issued_at| Prior::Edit { issued_at }),
        })
        .collect()
}

fn holds_exactly(grant: &Grant, wanted: &Wanted) -> bool {
    Contents::of(grant) == Contents::wanted(wanted)
}

/// What a grant gives, apart from when it was issued.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct Contents {
    scope: KeyScope,
    volumes: Vec<GrantVolume>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<KeyName>,
}

impl Contents {
    pub(super) fn of(grant: &Grant) -> Self {
        Self {
            scope: grant.scope.clone(),
            volumes: grant.volumes.clone(),
            name: grant.name.clone(),
        }
    }

    fn wanted(wanted: &Wanted) -> Self {
        Self {
            scope: wanted.scope.clone(),
            volumes: wanted.volumes.clone(),
            name: wanted.name.clone(),
        }
    }

    /// One line for each place, each Volume and the name.
    fn lines(&self) -> Vec<String> {
        let mut lines = match &self.scope.places {
            KeyPlaces::Unscoped => vec![UNSCOPED.to_owned()],
            KeyPlaces::Only(places) => places
                .iter()
                .map(|place| match &place.service {
                    Some(service) => {
                        format!("deploys service {} in {}", service.name, place.namespace)
                    }
                    None => format!("deploys to {}", place.namespace),
                })
                .collect(),
        };
        lines.extend(
            self.volumes
                .iter()
                .map(|volume| format!("mounts volume {}", volume.name)),
        );
        lines.push(match &self.name {
            Some(name) => format!("named {name}"),
            None => "no name".to_owned(),
        });
        lines
    }
}

/// One grant as some Members hold it before a change; `None` where they hold none.
#[derive(Serialize)]
struct Held {
    grant: Option<Contents>,
    machines: Vec<MachineId>,
}

fn before(held: &[(&Member, Option<&Grant>)]) -> Vec<Held> {
    let mut groups: Vec<Held> = Vec::new();
    for (member, grant) in held {
        let grant = grant.map(Contents::of);
        match groups.iter_mut().find(|group| group.grant == grant) {
            Some(group) => group.machines.push(member.key.id),
            None => groups.push(Held {
                grant,
                machines: vec![member.key.id],
            }),
        }
    }
    groups
}

/// Print what the key holds now on each Member and what it becomes, before anything is
/// signed, so a dropped Volume or name is never silent.
fn show_change(code: DeviceCode, before: &[Held], after: &Contents, roster: &Roster) {
    for held in before {
        let servers = held
            .machines
            .iter()
            .map(|machine_id| roster.reach.name(machine_id))
            .collect::<Vec<_>>()
            .join(", ");
        match &held.grant {
            Some(grant) => {
                ui::note(format_args!("Key {code} now, on {servers}:"));
                for line in grant.lines() {
                    ui::note(format_args!("  {line}"));
                }
            }
            None => ui::note(format_args!("{servers} doesn't hold key {code}.")),
        }
    }
    ui::note("It becomes:");
    for line in after.lines() {
        ui::note(format_args!("  {line}"));
    }
}

/// Whether a scope change may go out without asking.
#[derive(Debug, Eq, PartialEq)]
enum Consent {
    Silent,
    Ask(Ask),
}

/// Why a scope change needs a person.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Ask {
    /// A Member didn't answer, so what it holds is unknown.
    Unanswered,
    /// A Member holds none of the key, or less than the change gives.
    Widens,
    /// The flags leave out a Volume or the name a Member's grant holds.
    Drops,
}

impl Ask {
    fn refusal(self, code: DeviceCode) -> Error {
        let why = match self {
            Self::Unanswered => {
                format!("A server didn't answer, so changing key {code} needs you at a terminal.")
            }
            Self::Widens => {
                format!("This change widens key {code}, so it needs you at a terminal.")
            }
            Self::Drops => format!(
                "This change drops a volume or the name key {code} holds, so it needs you at a terminal. To keep them, name them again with --volume and --name."
            ),
        };
        Error::coded(
            RpcErrorCode::ConfirmationRequired,
            format!(
                "{why} --yes only skips the ask for a change that only narrows. Nothing changed."
            ),
        )
    }
}

/// A change goes out without asking only when every Member answered and `after` sits
/// inside the grant each holds and keeps its Volumes and name. Anything else asks, and
/// `--yes` refuses rather than change a key unread.
///
/// # Errors
/// Returns `confirmation_required` when the change would ask and `yes` is set.
fn consent(
    code: DeviceCode,
    held: &[Option<&Grant>],
    complete: bool,
    after: &Contents,
    yes: bool,
) -> Result<Consent, Error> {
    let ask = if !complete {
        Some(Ask::Unanswered)
    } else if held
        .iter()
        .any(|grant| grant.is_none_or(|grant| !inside(&after.scope, &after.volumes, grant)))
    {
        Some(Ask::Widens)
    } else if held.iter().flatten().any(|grant| drops(grant, after)) {
        Some(Ask::Drops)
    } else {
        None
    };
    match (ask, yes) {
        (None, _) => Ok(Consent::Silent),
        (Some(ask), true) => Err(ask.refusal(code)),
        (Some(ask), false) => Ok(Consent::Ask(ask)),
    }
}

/// Whether `after` leaves out a Volume or the name `grant` holds.
fn drops(grant: &Grant, after: &Contents) -> bool {
    grant
        .volumes
        .iter()
        .any(|volume| !after.volumes.contains(volume))
        || (grant.name.is_some() && grant.name != after.name)
}

/// Whether `scope` and `volumes` ask for nothing `grant` doesn't already give.
fn inside(scope: &KeyScope, volumes: &[GrantVolume], grant: &Grant) -> bool {
    let places = match (&scope.places, &grant.scope.places) {
        (KeyPlaces::Only(wanted), KeyPlaces::Only(held)) => wanted
            .iter()
            .all(|place| held.iter().any(|held| place_inside(place, held))),
        _ => false,
    };
    places
        && scope.caps.within(grant.scope.caps)
        && volumes.iter().all(|volume| grant.volumes.contains(volume))
}

fn place_inside(place: &KeyPlace, held: &KeyPlace) -> bool {
    place.environment == held.environment
        && place.namespace == held.namespace
        && (held.service.is_none() || held.service == place.service)
}

/// A scope change's result: the grants before, the grant after, and what each Member did.
#[derive(Serialize)]
struct Scoped {
    key: DeviceCode,
    before: Vec<Held>,
    after: Contents,
    servers: Vec<Answer>,
}

fn scoped(
    key: DeviceCode,
    before: Vec<Held>,
    after: Contents,
    settled: &BTreeMap<MachineId, (String, DeviceSet)>,
) -> Scoped {
    let servers = settled
        .iter()
        .map(|(machine_id, (server, outcome))| Answer {
            machine_id: *machine_id,
            server: server.clone(),
            outcome: *outcome,
        })
        .collect();
    Scoped {
        key,
        before,
        after,
        servers,
    }
}

fn finish_scope(scoped: &Scoped, gaps: &Gaps) -> Result<(), Error> {
    if scoped.servers.is_empty() && gaps.is_complete() {
        return ui::done(
            scoped,
            format_args!(
                "Key {} already holds that scope on every server.",
                scoped.key
            ),
        );
    }
    ui::finish_fanout("scope", scoped, gaps, || {
        ui::rows(&results(&scoped.servers));
    })
}

pub(crate) fn list(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    runtime()?.block_on(async {
        let mut client = route::connect(leaf, context(leaf)).await?;
        let reach = Reach::of(&mut client).await?;
        let (answers, gaps) = reach
            .fan_out(|machine_id| grants_on(client.clone(), machine_id))
            .await;
        let rows = listed(&answers);
        let mut table = Table::new(
            [
                "KEY",
                "KIND",
                "NAME",
                "DEPLOYS TO",
                "VOLUMES",
                "SERVERS",
                "MISSING ON",
            ],
            "No CI or agent keys. Make one with ployz key add --ci.",
        );
        for row in &rows {
            let missing = row
                .missing
                .iter()
                .map(|machine_id| reach.name(machine_id))
                .collect::<Vec<_>>()
                .join(", ");
            let tone = match row.missing.is_empty() {
                true => Tone::Good,
                false => Tone::Change,
            };
            table.row([
                Cell::from(row.grant.key.code().to_string()),
                Cell::from(KeyKind::from(row.grant.kind).to_string()),
                Cell::from(
                    row.grant
                        .name
                        .as_ref()
                        .map(KeyName::to_string)
                        .unwrap_or_default(),
                ),
                Cell::from(places_label(&row.grant.scope.places)),
                Cell::from(
                    row.grant
                        .volumes
                        .iter()
                        .map(|volume| volume.name.to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
                Cell::status(format!("{} of {}", row.machines.len(), answers.len()), tone),
                Cell::from(missing),
            ]);
        }
        ui::finish_fanout("keys", &rows, &reach.named(gaps), || ui::rows(&table))
    })
}

async fn grants_on(client: Client, machine_id: MachineId) -> Result<Option<Vec<Grant>>, RpcError> {
    let target = MachineTarget::from(&machine_id);
    if !client
        .supports(Some(&target), SCOPED_KEYS_CAPABILITY)
        .await
        .map_err(crate::connect::rpc_error)?
    {
        return Ok(None);
    }
    let details = client
        .call_shared::<op::Inspect>(InspectRequest::default(), Call::once().to(&target))
        .await
        .map_err(crate::connect::rpc_error)?;
    Ok(Some(
        details.grants.into_iter().map(|held| held.grant).collect(),
    ))
}

/// One grant as a set of Servers holds it. A key whose grant differs between Servers,
/// such as one a scope change missed, gets a row per grant.
#[derive(Serialize)]
struct Listed {
    #[serde(flatten)]
    grant: Grant,
    machines: Vec<MachineId>,
    missing: Vec<MachineId>,
}

fn listed(answers: &[(MachineId, Vec<Grant>)]) -> Vec<Listed> {
    let mut rows: Vec<Listed> = Vec::new();
    for (machine_id, grants) in answers {
        for grant in grants {
            match rows.iter_mut().find(|row| &row.grant == grant) {
                Some(row) => row.machines.push(*machine_id),
                None => rows.push(Listed {
                    grant: grant.clone(),
                    machines: vec![*machine_id],
                    missing: Vec::new(),
                }),
            }
        }
    }
    for row in &mut rows {
        row.missing = answers
            .iter()
            .map(|(machine_id, _)| *machine_id)
            .filter(|machine_id| !row.machines.contains(machine_id))
            .collect();
    }
    rows
}

fn places_label(places: &KeyPlaces) -> String {
    match places {
        KeyPlaces::Unscoped => UNSCOPED.to_owned(),
        KeyPlaces::Only(places) => places
            .iter()
            .map(|place| match &place.service {
                Some(service) => format!("{} in {}", service.name, place.namespace),
                None => place.namespace.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// A new key and the one time its secret is shown.
#[derive(Serialize)]
struct Made {
    code: DeviceCode,
    kind: GrantKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<KeyName>,
    ployz_key: String,
    servers: Vec<Answer>,
}

#[derive(Serialize)]
struct Answer {
    machine_id: MachineId,
    server: String,
    #[serde(flatten)]
    outcome: DeviceSet,
}

fn answered(reach: &Reach, answers: Vec<(MachineId, DeviceSet)>) -> Vec<Answer> {
    answers
        .into_iter()
        .map(|(machine_id, outcome)| Answer {
            server: reach.name(&machine_id),
            machine_id,
            outcome,
        })
        .collect()
}

fn results(servers: &[Answer]) -> Table {
    let mut table = Table::new(["SERVER", "RESULT"], "No server changed.");
    for answer in servers {
        let result = match answer.outcome {
            DeviceSet::Approved { .. } => "trusted",
            DeviceSet::AlreadyApproved { .. } => "already trusted",
            DeviceSet::Removed { .. } => "removed",
            DeviceSet::NotPresent { .. } => "not present",
        };
        table.row([Cell::from(&answer.server), Cell::status(result, Tone::Good)]);
    }
    table
}

fn context(leaf: &ArgMatches) -> Option<&str> {
    leaf.get_one::<String>("context").map(String::as_str)
}

/// The code typed, as `ployz key ls` shows it or in full.
fn typed_code(leaf: &ArgMatches) -> Result<DeviceCode, Error> {
    let typed = leaf.get_one::<String>("code").expect("code is required");
    DeviceCode::parse(typed).or_else(|named| {
        CheckCode::parse(typed)
            .map(|check| check.code())
            .map_err(|_| Error::from(named))
    })
}

/// Stop trusting a CI or agent key on every Member, by its full key. A code that names a
/// Device or passkey is refused: `ployz device rm` removes those. Each Member refuses
/// the removal itself where the key is a Device.
pub(crate) fn remove(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let code = typed_code(leaf)?;
    runtime()?.block_on(async {
        let mut client = route::connect_full(leaf, context(leaf)).await?;
        let reach = Reach::every_member(&mut client).await?;
        let (seen, unread) = reach
            .fan_out(|machine_id| seen_on(client.clone(), machine_id, code))
            .await;
        let unread = reach.named(unread);
        let Some(key) = grant_key(code, &seen)? else {
            if unread.is_complete() {
                return Err(Error::not_found(format!(
                    "No server holds a CI or agent key {code}. Nothing was removed."
                ))
                .hint(Hint::Next("ployz key ls".to_owned())));
            }
            return ui::finish_fanout("servers", &None::<Vec<Answer>>, &unread, || {
                ui::note(format_args!(
                    "No server that answered holds a CI or agent key {code}. Nothing was removed."
                ));
            });
        };
        let (removed, gaps) = remove_everywhere(&client, &reach, key, &Removal::Grant).await;
        if gaps.is_complete() {
            // A rotation that stopped short of removing this key waits on nothing more.
            rotate::forget(&config_path(leaf)?, code)?;
        }
        let servers = answered(&reach, removed);
        ui::finish_fanout("servers", &servers, &gaps, || ui::rows(&results(&servers)))
    })
}

/// What one Member holds under a code.
struct Seen {
    /// The CI and agent keys it trusts or removed for good under the code, in full.
    keys: Vec<SigningPublicKey>,
    /// Whether it trusts a Device or passkey under the code.
    personal: bool,
}

async fn seen_on(
    client: Client,
    machine_id: MachineId,
    code: DeviceCode,
) -> Result<Option<Seen>, RpcError> {
    let target = MachineTarget::from(&machine_id);
    if !client
        .supports(Some(&target), KEY_STEP_CAPABILITY)
        .await
        .map_err(crate::connect::rpc_error)?
    {
        return Ok(None);
    }
    let details = client
        .call_shared::<op::Inspect>(InspectRequest::default(), Call::once().to(&target))
        .await
        .map_err(crate::connect::rpc_error)?;
    let personal = details.devices.iter().any(|key| key.code() == code)
        || details
            .passkeys
            .iter()
            .any(|trusted| trusted.passkey.code() == code);
    let held = details
        .grants
        .iter()
        .map(|trusted| trusted.grant.key)
        .filter(|key| key.code() == code);
    // A key removed for good stays named in full, so a run again reaches the Members the
    // first missed. Only one removed as a CI or agent key counts: a Device's, or one
    // removed before Members kept which, is no evidence of a grant.
    let removed = details
        .removed_grants
        .iter()
        .copied()
        .filter(|key| key.code() == code);
    Ok(Some(Seen {
        keys: held.chain(removed).collect(),
        personal,
    }))
}

/// The one CI or agent key the Members that answered name under `code`, `None` when they
/// name none.
///
/// # Errors
/// Refuses a Device's or passkey's code, and one naming two keys.
fn grant_key(
    code: DeviceCode,
    seen: &[(MachineId, Seen)],
) -> Result<Option<SigningPublicKey>, Error> {
    if seen.iter().any(|(_, seen)| seen.personal) {
        return Err(Error::conflict(format!(
            "{code} is a device or passkey, not a CI or agent key. Nothing was removed."
        ))
        .hint(Hint::Next(format!("ployz device rm {code}"))));
    }
    let mut keys: Vec<SigningPublicKey> = Vec::new();
    for key in seen.iter().flat_map(|(_, seen)| &seen.keys) {
        if !keys.contains(key) {
            keys.push(*key);
        }
    }
    match keys.as_slice() {
        [] => Ok(None),
        [key] => Ok(Some(*key)),
        _ => Err(Error::conflict(format!(
            "Code {code} names more than one key on your servers. Nothing was removed."
        ))),
    }
}

/// What a removal of a CI or agent key asks of each Member.
pub(super) enum Removal {
    /// Remove it wherever it isn't a Device.
    Grant,
    /// Retire a rotated key, on each Member only while both grants are as the rotation
    /// read them there. A Member with no entry is left as it is.
    Rotated(BTreeMap<MachineId, Retired>),
}

/// Remove `key` for good on every Member `reach` covers, as `removal` asks. A Member too
/// old to refuse a Device's key is left out.
pub(super) async fn remove_everywhere(
    client: &Client,
    reach: &Reach,
    key: SigningPublicKey,
    removal: &Removal,
) -> (Vec<(MachineId, DeviceSet)>, Gaps) {
    let (answers, gaps) = reach
        .fan_out(|machine_id| {
            let retired = match removal {
                Removal::Grant => Some(None),
                Removal::Rotated(read) => read.get(&machine_id).copied().map(Some),
            };
            remove_on(client.clone(), machine_id, key, retired)
        })
        .await;
    (answers, reach.named(gaps))
}

async fn remove_on(
    client: Client,
    machine_id: MachineId,
    key: SigningPublicKey,
    retired: Option<Option<Retired>>,
) -> Result<Option<DeviceSet>, RpcError> {
    let Some(retired) = retired else {
        return Ok(None);
    };
    let target = MachineTarget::from(&machine_id);
    if !client
        .supports(Some(&target), KEY_PRIOR_CAPABILITY)
        .await
        .map_err(crate::connect::rpc_error)?
    {
        return Ok(None);
    }
    client
        .call_shared::<op::SetDevice>(
            SetDeviceRequest::RemoveGrant { key, retired },
            Call::once().to(&target),
        )
        .await
        .map_err(crate::connect::rpc_error)
        .map(Some)
}

#[cfg(test)]
#[path = "key_tests.rs"]
mod tests;
