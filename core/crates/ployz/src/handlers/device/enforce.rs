//! `ployz device enforce`: activate the Cluster once, from this Device.
//!
//! Nothing is signed until every Server in the set is pinned by this computer itself: from
//! the known-Machines file, from the Server it reached over SSH, or from a fingerprint the
//! person confirmed against `ployz status` on that Server. Cloud's Server Access supplies
//! addresses only. A legacy key survives only when the person confirms it. Each Server
//! decides for itself whether it admits the set, and restarts to commit it. Then this Device
//! generates the recipient key new values seal to, installs it on each activated Server, and
//! backs it up in Cloud under a recovery code it prints once.
//!
//! A run after one that some Servers committed finishes that activation: it signs the set
//! under the operation they hold, which they take again as a no-op, and has a Server that
//! holds the recipient key share it with the rest, so the Cluster keeps one key and one code.

use std::collections::{BTreeMap, BTreeSet};

use clap::{Arg, ArgAction, ArgMatches, Command};
use ployz_core::{
    ActivateEnforcementRequest, ActivationReport, ActivationState, Ciphertext, DeviceCode,
    DeviceKey, ENFORCED_CAPABILITY, Hint, InspectRequest, LegacyKey, MachineDetails, MachineId,
    MachineTarget, OperationId, RecipientKeySet, RecipientSecret, RecoveryCode, RpcError, Sealing,
    SealingPublicKey, SetDeviceEnforcementRequest, SetRecipientKeyRequest, TrustedKeyKind,
    authority::{
        Member, SealingRotation, Signed, Signer, SignerKey, StartingSet, Timestamp,
        rotation::sealed_key_binding, validity,
    },
    op,
};
use serde::Serialize;

use super::super::recovery::{KEEP_BOTH_CODES, print_code, store_recipient_backup};
use super::super::{Error, config_path, leaf_matches, route, runtime};
use super::{Reach, compat, context, key, known::KnownMachines};
use crate::cli::{base, yes};
use crate::cluster::{Call, Wait};
use crate::connect::Client;
use crate::ui::{self, Cell, Table, Tone};

pub(crate) fn enforce_command(command: Command) -> Command {
    base(
        command,
        "Turn on Device enforcement for the Cluster, once, from this device",
    )
    .long_about(
        "Signs the set of Servers every Server trusts from now on, with this device's key, and \
         turns Device enforcement on for good. Each Server is pinned by this computer first: \
         from a join this device approved, from a Server you reach over SSH, or from a \
         fingerprint you confirm against `ployz status` on that Server. A Server that holds \
         keys or Servers from before takes the set only over its own SSH, so reach every such \
         Server in one run. A Server left out is no longer part of the Cluster once the others \
         activate. Keys added while enforcement was off are removed unless you keep them. \
         There is no off.",
    )
    .after_help(concat!(
        "Examples:\n",
        "  ployz device enforce --connect ssh://root@203.0.113.10\n",
        "  ployz device enforce --pin 3f9a1c2e --keep mmg42klgyq"
    ))
    .arg(
        Arg::new("pin")
            .long("pin")
            .value_name("FINGERPRINT")
            .action(ArgAction::Append)
            .help("Trust the Server whose key has this fingerprint, as `ployz status` on it shows"),
    )
    .arg(
        Arg::new("keep")
            .long("keep")
            .value_name("CODE")
            .action(ArgAction::Append)
            .help("Keep this key added while enforcement was off, after checking it with `ployz device ls` on that computer"),
    )
    .arg(yes("Don't ask: keep no older key and leave out every Server not yet pinned"))
}

/// One Server as this run found it.
struct Found {
    machine_id: MachineId,
    member: Member,
    report: Option<ActivationReport>,
    identity: Option<SealingPublicKey>,
    /// The recipient key this Server holds once a rotation installed one.
    recipient: Option<SealingPublicKey>,
    enforces: bool,
}

/// What became of one Server.
#[derive(Clone, Debug, Serialize)]
pub(super) struct Outcome {
    pub(super) machine_id: MachineId,
    pub(super) server: String,
    pub(super) state: State,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "why")]
pub(super) enum State {
    Activated,
    LeftOut(String),
    Refused(String),
}

pub(crate) fn enforce(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let config = config_path(leaf)?;
    let secret = key::load(&key::path(&config))?.ok_or_else(|| {
        Error::conflict("Turning on Device enforcement needs this computer's approved Device key.")
            .hint(Hint::Next("ployz device request".to_owned()))
    })?;
    let signer = Signer::from_seed(&secret);
    let typed_pins: BTreeSet<String> = leaf
        .get_many::<String>("pin")
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    let typed_keeps = leaf
        .get_many::<String>("keep")
        .into_iter()
        .flatten()
        .map(|code| DeviceCode::parse(code))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let unattended = leaf.get_flag("yes");
    runtime()?.block_on(async {
        let mut client = route::connect_full(leaf, context(leaf)).await?;
        // Every Server, not only those this run reaches with Full scope: a Server missing
        // from the signed set is cut off once the others commit it.
        let reach = Reach::of(&mut client).await?;
        let (found, gaps) = reach
            .fan_out(|machine_id| found_on(client.clone(), machine_id))
            .await;
        let found: Vec<Found> = found.into_iter().map(|(_, server)| server).collect();
        let known = KnownMachines::beside(&config);
        let pinned = known.load()?;
        // A Server reached over its own SSH or local socket answers for itself, and takes the
        // set from there whatever it holds from before.
        let direct: BTreeSet<MachineId> = reach
            .asked
            .iter()
            .filter(|machine| client.reaches_full(machine, false))
            .copied()
            .collect();
        let Pinning {
            included,
            newly,
            mut outcomes,
        } = pin_each(
            &found,
            &reach,
            &Pinned {
                known: &pinned,
                direct: &direct,
                typed: &typed_pins,
            },
            |name, member| {
                if unattended {
                    Ok(false)
                } else {
                    confirms_fingerprint(name, member)
                }
            },
        )?;
        for member in newly {
            known.record(member)?;
        }
        if included.is_empty() {
            return Err(Error::conflict(
                "No Server is pinned on this computer, so nothing was signed.",
            )
            .hint(Hint::Next(
                "ployz device enforce --connect ssh://root@SERVER".to_owned(),
            )));
        }
        refuse_unreachable(&included, &direct, &reach)?;
        let own = DeviceKey::from(signer.public()).code();
        let kept = kept_keys(&legacy_keys(&included, &reach, own), |key| {
            Ok(typed_keeps.contains(&key.code) || (!unattended && ui::can_prompt() && keeps(key)?))
        })?;
        let resumed = resumed(&included, &reach)?;
        if let Some(operation) = resumed {
            ui::note(
                if included
                    .iter()
                    .all(|server| committed_under(server, operation))
                {
                    format!("Every Server is already activated under {operation}.")
                } else {
                    format!(
                        "Finishing activation {operation}, which some of these Servers already committed."
                    )
                },
            );
        }
        let issued_at = Timestamp::now();
        let set = StartingSet {
            operation: resumed.unwrap_or_else(OperationId::random),
            members: included
                .iter()
                .map(|server| server.member.clone())
                .collect(),
            issued_at,
            expires_at: issued_at.plus(validity::MAX_LIFETIME_MILLIS),
            signer: SignerKey::Device(signer.public().into()),
            kept_keys: kept.iter().copied().collect(),
        };
        ui::note(set_summary(&reach, &set));
        for outcome in &outcomes {
            ui::warn(format!(
                "{0} is left out. Once the others activate it's no longer part of the Cluster: add it back with `ployz server add root@HOST --reset --name {0}`.",
                outcome.server
            ));
        }
        let signed = Signed::sign(&set, &signer).map_err(Error::command)?;
        let request = SetDeviceEnforcementRequest {
            enforced: true,
            starting_set: Some(signed.clone()),
        };
        let mut activated = Vec::new();
        for server in &included {
            let name = reach.name(&server.machine_id);
            let delivered = if direct.contains(&server.machine_id) {
                deliver(&client, server.machine_id, request.clone()).await
            } else {
                deliver_over_mesh(&client, server.machine_id, &signed).await
            };
            if delivered.is_ok() && !committed_under(server, set.operation) {
                ui::note(format!("{name} restarts to activate."));
            }
            match delivered {
                Ok(()) => match settled(&client, server.machine_id, &name, set.operation).await {
                    Ok(report) => {
                        if let Some(router) = &report.private_access {
                            ui::note(kept_router(&name, router));
                        }
                        activated.push(*server);
                        outcomes.push(Outcome {
                            machine_id: server.machine_id,
                            server: name,
                            state: State::Activated,
                        });
                    }
                    Err(why) => outcomes.push(refused(server, &name, why)),
                },
                Err(error) => outcomes.push(refused(server, &name, error.message)),
            }
        }
        if !activated.is_empty() {
            install_recipient(&config, &client, &reach, &activated, &set, &signer).await?;
        }
        print_retired(&activated, &reach, &kept, own);
        let mut gaps = gaps;
        gaps.omitted.extend(
            outcomes
                .iter()
                .filter(|outcome| outcome.state != State::Activated)
                .map(|outcome| outcome.machine_id),
        );
        let gaps = reach.named(gaps);
        ui::finish_fanout("servers", &outcomes, &gaps, || {
            let mut table = Table::new(["SERVER", "ACTIVATION"], "No servers answered.");
            for outcome in &outcomes {
                let (state, tone) = match &outcome.state {
                    State::Activated => ("activated".to_owned(), Tone::Good),
                    State::LeftOut(why) => (format!("left out: {why}"), Tone::Change),
                    State::Refused(why) => (format!("refused: {why}"), Tone::Change),
                };
                table.row([Cell::from(&outcome.server), Cell::status(state, tone)]);
            }
            ui::rows(&table);
        })
    })
}

async fn found_on(client: Client, machine_id: MachineId) -> Result<Option<Found>, RpcError> {
    let target = MachineTarget::from(&machine_id);
    let enforces = compat::supports(&client, &target, ENFORCED_CAPABILITY).await?;
    let details: MachineDetails = client
        .call_shared::<op::Inspect>(InspectRequest::default(), Call::once().to(&target))
        .await
        .map_err(crate::connect::rpc_error)?;
    let Some(machine) = details
        .machine
        .as_ref()
        .filter(|machine| machine.id == machine_id)
    else {
        return Ok(None);
    };
    Ok(Some(Found {
        machine_id,
        member: Member::from(machine),
        report: details.activation.clone(),
        identity: match details.sealing {
            Some(Sealing::Held { public_key }) => Some(public_key),
            _ => None,
        },
        recipient: details.sealing_recipient,
        enforces,
    }))
}

/// What this computer trusts each Server's key from, other than asking.
struct Pinned<'a> {
    known: &'a BTreeMap<MachineId, Member>,
    /// The Servers this run reached over their own SSH or local socket, which answer for
    /// themselves.
    direct: &'a BTreeSet<MachineId>,
    /// Fingerprints the person typed with `--pin`.
    typed: &'a BTreeSet<String>,
}

struct Pinning<'a> {
    included: Vec<&'a Found>,
    /// Members pinned by this run, to record once every Server checked out.
    newly: Vec<Member>,
    outcomes: Vec<Outcome>,
}

/// Include each Server whose Member this computer pinned or pins now. A Server that answers
/// with another key than its pin stops the whole run before anything is signed.
fn pin_each<'a>(
    found: &'a [Found],
    reach: &Reach,
    pinned: &Pinned<'_>,
    mut confirms: impl FnMut(&str, &Member) -> Result<bool, Error>,
) -> Result<Pinning<'a>, Error> {
    let mut pinning = Pinning {
        included: Vec::new(),
        newly: Vec::new(),
        outcomes: Vec::new(),
    };
    for server in found {
        let name = reach.name(&server.machine_id);
        if !server.enforces {
            pinning.outcomes.push(left_out(
                server,
                &name,
                "runs a Ployz too old to activate.".to_owned(),
            ));
            continue;
        }
        if let Some(pin) = pinned.known.get(&server.machine_id) {
            if *pin != server.member {
                return Err(substituted(&name, pin, &server.member));
            }
        } else if pinned.direct.contains(&server.machine_id)
            || pinned
                .typed
                .contains(&server.member.public_key.fingerprint())
            || confirms(&name, &server.member)?
        {
            pinning.newly.push(server.member.clone());
        } else {
            pinning.outcomes.push(left_out(
                server,
                &name,
                "isn't pinned on this computer. Check its key with `ployz status` on it, or reach it over SSH.".to_owned(),
            ));
            continue;
        }
        pinning.included.push(server);
    }
    Ok(pinning)
}

/// The line naming a Private Access router a Server keeps from before activation.
fn kept_router(name: &str, router: &ployz_core::PrivateAccessKept) -> String {
    let tailnet = router.tailnet.as_deref().map_or_else(
        || "a tailnet it couldn't name".to_owned(),
        |tailnet| format!("tailnet {tailnet}"),
    );
    format!(
        "{name} keeps the Private Access router it had before activation, on {tailnet}. If you don't recognize it, run `ployz private-access off`, then turn it on again from this device."
    )
}

fn left_out(server: &Found, name: &str, why: String) -> Outcome {
    Outcome {
        machine_id: server.machine_id,
        server: name.to_owned(),
        state: State::LeftOut(why),
    }
}

fn refused(server: &Found, name: &str, why: String) -> Outcome {
    Outcome {
        machine_id: server.machine_id,
        server: name.to_owned(),
        state: State::Refused(why),
    }
}

fn substituted(name: &str, pin: &Member, answered: &Member) -> Error {
    Error::conflict(format!(
        "Server {name} answered with key {}, not key {} this computer pinned for it, so nothing was signed. Run `sudo ployz status` on it to check which key it holds.",
        answered.public_key.fingerprint(),
        pin.public_key.fingerprint(),
    ))
}

/// Ask whether `ployz status` on the Server shows `member`'s fingerprint.
fn confirms_fingerprint(name: &str, member: &Member) -> Result<bool, Error> {
    if !ui::can_prompt() {
        return Ok(false);
    }
    let answer = ui::select(
        &format!(
            "Does `ployz status` on {name} show key {}?",
            member.public_key.fingerprint()
        ),
        &["No, leave it out", "Yes, it matches"],
        0,
        || Error::usage("Confirming a Server's fingerprint needs a terminal."),
        "Nothing was signed.",
    )?;
    Ok(answer == 1)
}

/// The legacy keys the person keeps, each one `keep` confirms by code. The rest are
/// listed first so the person sees what activation removes.
fn kept_keys(
    legacy: &BTreeMap<DeviceCode, (LegacyKey, Vec<String>)>,
    mut keep: impl FnMut(&LegacyKey) -> Result<bool, Error>,
) -> Result<BTreeSet<DeviceCode>, Error> {
    if legacy.is_empty() {
        return Ok(BTreeSet::new());
    }
    ui::note("These keys were added while Device enforcement was off. Activation removes them:");
    for (key, servers) in legacy.values() {
        ui::note(format!("  {}   on {}", key_label(key), servers.join(", ")));
    }
    ui::note("To keep a key, check its code first with `ployz device ls` on that computer.");
    let mut kept = BTreeSet::new();
    for (code, (key, _)) in legacy {
        if keep(key)? {
            kept.insert(*code);
        }
    }
    Ok(kept)
}

fn keeps(key: &LegacyKey) -> Result<bool, Error> {
    Ok(ui::select(
        &format!("Keep {}?", key_label(key)),
        &["No, remove it", "Yes, I checked its code"],
        0,
        || Error::usage("Keeping a key needs a terminal or --keep."),
        "Nothing was signed.",
    )? == 1)
}

/// Each legacy key the included Servers report, other than `own`, with the Servers holding it.
fn legacy_keys(
    included: &[&Found],
    reach: &Reach,
    own: DeviceCode,
) -> BTreeMap<DeviceCode, (LegacyKey, Vec<String>)> {
    let mut legacy: BTreeMap<DeviceCode, (LegacyKey, Vec<String>)> = BTreeMap::new();
    for server in included {
        for key in server
            .report
            .iter()
            .flat_map(|report| &report.legacy_keys)
            .filter(|key| key.code != own)
        {
            legacy
                .entry(key.code)
                .or_insert_with(|| (key.clone(), Vec::new()))
                .1
                .push(reach.name(&server.machine_id));
        }
    }
    legacy
}

fn key_label(key: &LegacyKey) -> String {
    match (&key.kind, &key.account) {
        (TrustedKeyKind::Passkey, Some(account)) => format!("Passkey {account} {}", key.code),
        (TrustedKeyKind::Passkey, None) => format!("Passkey {}", key.code),
        (TrustedKeyKind::Device, _) => format!("Device {}", key.code),
    }
}

/// The legacy keys activation removed from the Servers that activated.
fn print_retired(
    activated: &[&Found],
    reach: &Reach,
    kept: &BTreeSet<DeviceCode>,
    own: DeviceCode,
) {
    let retired: Vec<_> = legacy_keys(activated, reach, own)
        .into_values()
        .filter(|(key, _)| !kept.contains(&key.code))
        .collect();
    if retired.is_empty() {
        return;
    }
    ui::note("Removed while activating:");
    for (key, servers) in retired {
        ui::note(format!("  {}   on {}", key_label(&key), servers.join(", ")));
    }
    ui::note(
        "To add one back, run `ployz device request` on that computer, then `ployz device approve <code>` here.",
    );
}

fn set_summary(reach: &Reach, set: &StartingSet) -> String {
    let servers = set
        .members
        .iter()
        .map(|member| {
            format!(
                "{} ({}, {}, key {})",
                reach.name(&member.id),
                member.subnet,
                member.id,
                member.public_key.fingerprint()
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let kept = if set.kept_keys.is_empty() {
        "none".to_owned()
    } else {
        set.kept_keys
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "Signing the {} Servers every Server trusts from now on: {servers}. Keeping older keys: {kept}.",
        set.members.len()
    )
}

/// A Server only the mesh reaches admits the set only when it holds evidence for the signer
/// and every Member. One holding keys or Servers from before would refuse while the others
/// commit a set naming it, so nothing is signed until every such Server is reached directly.
fn refuse_unreachable(
    included: &[&Found],
    direct: &BTreeSet<MachineId>,
    reach: &Reach,
) -> Result<(), Error> {
    let unreached: Vec<String> = included
        .iter()
        .filter(|server| !direct.contains(&server.machine_id))
        .filter(|server| {
            server
                .report
                .as_ref()
                .is_none_or(|report| !report.nothing_legacy())
        })
        .map(|server| reach.name(&server.machine_id))
        .collect();
    if unreached.is_empty() {
        return Ok(());
    }
    Err(Error::conflict(format!(
        "{} hold keys or Servers added while enforcement was off, so each takes the set only over its own SSH. Nothing was signed.",
        unreached.join(", ")
    ))
    .hint(Hint::Next(
        "ployz device enforce without --connect, in the context `ployz server add` saved SSH connections to"
            .to_owned(),
    )))
}

/// Hand the signed set to a Server through the mesh, which admits it against its own
/// trusted snapshot.
/// The operation a run must sign under: the one Servers already committed, so a rerun after
/// a Server was refused or stopped finishes that activation rather than starting another.
///
/// # Errors
/// Refuses, before anything is signed, when Servers committed different operations: no one
/// set activates them together.
fn resumed(included: &[&Found], reach: &Reach) -> Result<Option<OperationId>, Error> {
    let mut under: BTreeMap<OperationId, Vec<String>> = BTreeMap::new();
    for server in included {
        if let Some(ActivationReport {
            state: ActivationState::Activated,
            operation: Some(operation),
            ..
        }) = &server.report
        {
            under
                .entry(*operation)
                .or_default()
                .push(reach.name(&server.machine_id));
        }
    }
    match under.len() {
        0 => Ok(None),
        1 => Ok(under.into_keys().next()),
        _ => Err(Error::conflict(format!(
            "These Servers were activated under different starting sets, so no one set activates them together: {}. Nothing was signed.",
            under
                .into_iter()
                .map(|(operation, servers)| format!("{} under {operation}", servers.join(", ")))
                .collect::<Vec<_>>()
                .join("; ")
        ))
        .hint(Hint::Next(
            "ployz server add root@HOST --reset for each Server under one set, then approve its join"
                .to_owned(),
        ))),
    }
}

/// Whether `server` reported itself activated under `operation` before this run.
fn committed_under(server: &Found, operation: OperationId) -> bool {
    matches!(
        &server.report,
        Some(ActivationReport {
            state: ActivationState::Activated,
            operation: Some(committed),
            ..
        }) if *committed == operation
    )
}

/// Where the recipient key comes from: a fresh one, or the one activated Servers already hold.
enum Recipient<'a> {
    Fresh(RecipientSecret),
    Held {
        recipient: SealingPublicKey,
        holder: &'a Found,
    },
}

impl Recipient<'_> {
    fn public(&self) -> SealingPublicKey {
        match self {
            Self::Fresh(secret) => secret.public(),
            Self::Held { recipient, .. } => *recipient,
        }
    }
}

/// The recipient key the activated Servers already hold, with one Server holding it. `None`
/// when none holds one. More than one key can't be healed by a run.
fn held_recipient<'a>(activated: &[&'a Found]) -> Result<Option<Recipient<'a>>, String> {
    let held: BTreeMap<SealingPublicKey, &Found> = activated
        .iter()
        .filter_map(|server| server.recipient.map(|recipient| (recipient, *server)))
        .collect();
    match held.len() {
        0 => Ok(None),
        1 => Ok(held
            .into_iter()
            .next()
            .map(|(recipient, holder)| Recipient::Held { recipient, holder })),
        _ => Err(format!(
            "The activated Servers hold different recipient keys ({}), so no recipient key was installed.",
            held.keys()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

async fn deliver_over_mesh(
    client: &Client,
    machine_id: MachineId,
    starting_set: &Signed<StartingSet>,
) -> Result<(), RpcError> {
    client
        .call_shared::<op::ActivateEnforcement>(
            ActivateEnforcementRequest {
                starting_set: starting_set.clone(),
            },
            Call::once().to(&MachineTarget::from(&machine_id)),
        )
        .await
        .map_err(crate::connect::rpc_error)
        .map(drop)
}

async fn deliver(
    client: &Client,
    machine_id: MachineId,
    request: SetDeviceEnforcementRequest,
) -> Result<(), RpcError> {
    client
        .call_shared::<op::SetDeviceEnforcement>(
            request,
            Call::once().to(&MachineTarget::from(&machine_id)),
        )
        .await
        .map_err(crate::connect::rpc_error)
        .map(drop)
}

/// What a wait prints while a Server restarts to commit its activation, which it does on
/// purpose, in place of the connectivity warning. The note before it names the Server.
const ACTIVATING: crate::setup_retry::Expected =
    crate::setup_retry::Expected("Waiting for it to come back…");

/// Wait for the Server to come back from its restart and report this activation committed.
async fn settled(
    client: &Client,
    machine_id: MachineId,
    name: &str,
    operation: OperationId,
) -> Result<ActivationReport, String> {
    let target = MachineTarget::from(&machine_id);
    let mut client = client.clone();
    for _ in 0..60 {
        let details = client
            .call::<op::Inspect>(
                InspectRequest::default(),
                Call::read().to(&target).waiting(Wait {
                    server: Some(name),
                    expected: Some(ACTIVATING),
                    ..Wait::standard()
                }),
            )
            .await
            .map_err(|_| {
                "it didn't come back from its restart. Check it with `ployz status`.".to_owned()
            })?;
        match details.activation {
            Some(report) if report.state == ActivationState::Activated => {
                return if report.operation == Some(operation) {
                    Ok(report)
                } else {
                    Err("it was activated under another set".to_owned())
                };
            }
            Some(_) | None => tokio::time::sleep(std::time::Duration::from_secs(1)).await,
        }
    }
    Err("it didn't report the activation committed. Check it with `ployz status`.".to_owned())
}

/// Sign the rotation naming the recipient key and install it on each activated Server that
/// lacks it, sealed to that Server's own WireGuard key. The key is the one activated Servers
/// already hold, shared by a holder, or else one generated here and backed up.
async fn install_recipient(
    config: &std::path::Path,
    client: &Client,
    reach: &Reach,
    activated: &[&Found],
    set: &StartingSet,
    signer: &Signer,
) -> Result<(), Error> {
    let identities: BTreeSet<_> = activated
        .iter()
        .filter_map(|server| server.identity)
        .collect();
    let [identity] = identities.into_iter().collect::<Vec<_>>()[..] else {
        ui::warn(
            "The activated Servers don't report one Cluster Sealing Key, so no recipient key was installed. Run `ployz device enforce` again from a Server's SSH.",
        );
        return Ok(());
    };
    let source = match held_recipient(activated) {
        Ok(Some(held)) => held,
        Ok(None) => Recipient::Fresh(RecipientSecret::generate()),
        Err(why) => {
            ui::warn(why);
            return Ok(());
        }
    };
    let rotation = SealingRotation {
        cluster: identity,
        operation: set.operation,
        recipient: source.public(),
        members: set.members.clone(),
        signer: SignerKey::Device(signer.public().into()),
    };
    ui::note(format!(
        "Signing the Cluster's recipient key {} for {} Servers. New sealed values go to it.",
        rotation.recipient,
        rotation.members.len()
    ));
    let signed = Signed::sign(&rotation, signer).map_err(Error::command)?;
    let binding = sealed_key_binding(&identity, &set.operation);
    for server in activated {
        if server.recipient == Some(rotation.recipient) {
            continue;
        }
        let name = reach.name(&server.machine_id);
        let sealed = match &source {
            Recipient::Fresh(secret) => secret
                .seal_to(&server.member.public_key, &binding)
                .map_err(Error::command)?,
            Recipient::Held { holder, .. } => {
                match shared(client, holder.machine_id, &signed, server.machine_id).await {
                    Ok(sealed) => sealed,
                    Err(error) => {
                        ui::warn(format!(
                            "{} didn't share the recipient key with {name}: {}",
                            reach.name(&holder.machine_id),
                            error.message
                        ));
                        continue;
                    }
                }
            }
        };
        let installed = client
            .call_shared::<op::SetRecipientKey>(
                SetRecipientKeyRequest::Install {
                    rotation: signed.clone(),
                    sealed: Some(sealed),
                },
                Call::once().to(&MachineTarget::from(&server.machine_id)),
            )
            .await;
        if let Err(error) = installed {
            ui::warn(format!(
                "Server {name} didn't install the recipient key: {}",
                crate::connect::rpc_error(error).message
            ));
        }
    }
    crate::sealing_pin::Pins::beside(config).name_recipient(identity, rotation.recipient)?;
    ui::note("Values sealed from now on open only on your activated Servers.");
    match source {
        Recipient::Fresh(secret) => recovery_code(config, &secret, &signed).await,
        Recipient::Held { .. } => {
            ui::note(
                "The recipient key is the one your Servers already hold, so the recovery code you made for it still opens it.",
            );
            Ok(())
        }
    }
}

/// The recipient key `holder` holds, sealed to `to`'s WireGuard key under `rotation`.
async fn shared(
    client: &Client,
    holder: MachineId,
    rotation: &Signed<SealingRotation>,
    to: MachineId,
) -> Result<Ciphertext, RpcError> {
    let answer = client
        .call_shared::<op::SetRecipientKey>(
            SetRecipientKeyRequest::Share {
                rotation: rotation.clone(),
                to,
            },
            Call::once().to(&MachineTarget::from(&holder)),
        )
        .await
        .map_err(crate::connect::rpc_error)?;
    match answer {
        RecipientKeySet::Shared { to: shared, sealed } if shared == to => Ok(sealed),
        RecipientKeySet::Shared { .. }
        | RecipientKeySet::Generated { .. }
        | RecipientKeySet::Installed { .. } => Err(RpcError::new(
            ployz_core::RpcErrorCode::Internal,
            "the Server answered with something other than the shared key",
        )),
    }
}

/// Back up the recipient key in Cloud under a fresh recovery code and print the code once,
/// on a terminal only, since no one else will see it. Activation already happened, so a
/// failure here only says how to make the code later.
async fn recovery_code(
    config: &std::path::Path,
    secret: &RecipientSecret,
    rotation: &Signed<SealingRotation>,
) -> Result<(), Error> {
    const LATER: &str = "Make a recovery code for values sealed from now on with `ployz recovery rotate` on an activated Server, or over SSH to one.";
    if ui::json() || !ui::stdout_is_terminal() {
        ui::note(LATER);
        return Ok(());
    }
    let store = crate::cloud_login::CredentialStore::beside(config);
    let Ok(credential) = crate::cloud_account::from_env(&store).await else {
        ui::note(format!(
            "Not signed in, so no recovery code was made. {LATER}"
        ));
        return Ok(());
    };
    let code = RecoveryCode::generate();
    let wrapped = crate::cloud_enroll::RecoveryBackup {
        public_key: secret.public(),
        backup: ployz_core::sealing_backup::back_up(secret.secret(), &code),
    };
    let stored = async {
        let organization = crate::cloud_account::acting_in(&credential).await?;
        store_recipient_backup(&credential, &organization.id, &wrapped, rotation).await
    }
    .await;
    if let Err(error) = stored {
        ui::warn(format!(
            "Cloud didn't keep the recovery backup, so no recovery code was made: {error} {LATER}"
        ));
        return Ok(());
    }
    print_code(secret.public(), &code, KEEP_BOTH_CODES)
}

#[cfg(test)]
#[path = "enforce_tests.rs"]
mod tests;
