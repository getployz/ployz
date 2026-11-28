//! `ployz device`: the people's keys every Machine admits at Full scope. Cloud relays a
//! Device's key to the approver, who checks it against the code the Device printed: its
//! [`CheckCode`], which Cloud can't grind a second key to match.

use crate::cloud;
use crate::cluster::{Call, Deadline, fan_out_entry_last};
use std::collections::BTreeMap;
use std::path::Path;

use clap::{Arg, ArgAction, ArgMatches, Command};
use ployz_core::{
    CheckCode, DeviceCode, DeviceKey, DeviceSet, InspectRequest, KEY_STEP_CAPABILITY,
    MachineFailure, MachineId, MachineName, MachineTarget, Rpc, RpcError, RpcErrorCode,
    SET_DEVICE_CAPABILITY, SetDeviceRequest,
    authority::{KeyRef, PasskeyAccount, PasskeyApprover},
    op,
};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Error, config_path, leaf_matches, route, runtime};
use crate::cli::{base, positional, yes};
use crate::cloud::account::Credential;
use crate::cloud::login::{CredentialStore, LoginError};
use crate::connect::Client;
use crate::ui::{self, Cell, Gaps, Table, Tone};
use ployz_core::Hint;

mod pin;

use crate::device::{
    DeviceSigning, key, refuse_another_key, refuse_unpinned, show_join, sign_join,
};
pub(super) use pin::seal_to;
pub(crate) use pin::{join_pin, pin, pin_command};

pub(crate) fn group(command: Command) -> Command {
    command.about("Approve the devices that can exec and port-forward")
}

pub(crate) fn request_command(command: Command) -> Command {
    command
        .about("Ask to approve this device")
        .after_help(concat!("Examples:\n", "  ployz device request"))
}

pub(crate) fn approve_command(command: Command) -> Command {
    base(command, "Approve a device on every server")
        .after_help(concat!(
            "Examples:\n",
            "  ployz device approve mmg42-klgyq-p3w7d-xa5re\n",
            "  ployz device approve --this-device --connect ssh://root@HOST"
        ))
        .arg(positional("code", false).required_unless_present("this-device").help(
            "The code the requesting device or server printed",
        ))
        .arg(
            Arg::new("this-device")
                .long("this-device")
                .action(ArgAction::SetTrue)
                .conflicts_with_all(["code", "join"])
                .help("Approve this computer's own device, made now if it has none. Run it from your computer with --connect ssh://root@HOST"),
        )
        .arg(
            Arg::new("join")
                .long("join")
                .action(ArgAction::SetTrue)
                .help("Approve a waiting Server join with this device's key"),
        )
        .arg(yes("Approve without asking"))
}

pub(crate) fn list_command(command: Command) -> Command {
    base(command, "List approved devices and passkeys")
}

pub(crate) fn remove_command(command: Command) -> Command {
    base(command, "Remove a device or a passkey from every server")
        .after_help(concat!("Examples:\n", "  ployz device rm mmg42klgyq"))
        .arg(
            positional("code", true).help(
                "The device's or passkey's code from ployz device ls, or the device's full code",
            ),
        )
}

pub(crate) fn request(root: &ArgMatches) -> Result<(), Error> {
    let config = config_path(leaf_matches(root))?;
    let store = CredentialStore::beside(&config);
    let requested = runtime()?.block_on(async {
        let credential = cloud::account::from_env(&store).await?;
        request_at(&credential, &key::path(&config)).await
    })?;
    let code = requested.code;
    ui::finish(&requested, || {
        ui::stream(code);
        ui::note(format_args!(
            "Approve it from an approved device, or over SSH on a server:\n  ployz device approve {code}"
        ));
    })
}

/// File the key at `path` with Cloud, writing a new one there only once Cloud has filed it:
/// a refused request must leave no key for exec and port-forward to sign with.
async fn request_at(credential: &Credential, path: &Path) -> Result<Requested, Error> {
    if let Some(secret) = key::load(path)? {
        return request_with(credential, key::public(&secret)).await;
    }
    let secret = key::generate();
    let requested = request_with(credential, key::public(&secret)).await?;
    if key::store(path, &secret)? {
        return Ok(requested);
    }
    // A concurrent request wrote its key first: file that one, the key exec will sign with.
    let kept = key::load(path)?.ok_or_else(|| key::KeyError::Write {
        path: path.to_owned(),
        source: std::io::ErrorKind::NotFound.into(),
    })?;
    request_with(credential, key::public(&kept)).await
}

/// File `key` with Cloud and show the code computed here from it. A reply naming another
/// code is refused: the approver's check only means something against the local code.
async fn request_with(credential: &Credential, key: DeviceKey) -> Result<Requested, Error> {
    let code = key.code();
    let body = json!({ "key": key.to_string() });
    let filed =
        cloud::account::call::<Filed>(credential, Method::POST, "device-requests", Some(&body))
            .await?;
    if filed.code != code {
        return Err(Error::conflict(format!(
            "Ployz Cloud filed this device as {} but its key hashes to {code}. Nothing changed.",
            filed.code
        )));
    }
    Ok(Requested {
        code: key.check_code(),
        expires_at: filed.expires_at,
    })
}

pub(crate) fn approve(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    if leaf.get_flag("this-device") {
        return runtime()?.block_on(approve_this_device(leaf));
    }
    if leaf.get_flag("join") {
        let code = leaf
            .get_one::<String>("code")
            .expect("required code")
            .trim()
            .to_ascii_lowercase();
        return runtime()?.block_on(approve_waiting_join(leaf, &code));
    }
    let store = CredentialStore::beside(&config_path(leaf)?);
    let code = CheckCode::parse(typed(leaf))?;
    runtime()?.block_on(async {
        let credential = cloud::account::from_env(&store).await?;
        let settled = approve_with(&credential, code, async |request| {
            confirm_device(root, code)?;
            let mut client = route::connect_full(leaf, context(leaf)).await?;
            set_everywhere(&mut client, |client, machine_id| {
                set_on::<op::SetDevice>(client, machine_id, SET_DEVICE_CAPABILITY, request.clone())
            })
            .await
        })
        .await?;
        settled.finish()
    })
}

/// Approve this computer's Device key, made now if it has none, on the Server `client` reaches
/// over a connection the person chose. A new Cluster's first Server trusts no key, and every
/// later join needs a Device to sign it.
///
/// # Errors
/// Fails when the key can't be read or written, or the Server refuses it.
pub(crate) async fn approve_own(client: &Client, config: &Path) -> Result<DeviceCode, Error> {
    let key = own_key(config)?;
    client
        .call_shared::<op::SetDevice>(SetDeviceRequest::Approve { key }, Call::once())
        .await
        .map_err(crate::connect::rpc_error)?;
    Ok(key.code())
}

/// This computer's Device key beside `config`, made now if it has none.
pub(crate) fn own_key(config: &Path) -> Result<DeviceKey, Error> {
    let path = key::path(config);
    let secret = match key::load(&path)? {
        Some(secret) => secret,
        None => {
            let secret = key::generate();
            key::store(&path, &secret)?;
            key::load(&path)?.unwrap_or(secret)
        }
    };
    Ok(key::public(&secret))
}

/// Approve this computer's own Device on every Server it reaches. The key comes from this
/// computer's own file, never from Ployz Cloud, though with no `--connect` the route may run
/// through Cloud's Server Access. Refused over a Server's own socket, where the key would be
/// root's on that Server and ride into every later join approval.
async fn approve_this_device(leaf: &ArgMatches) -> Result<(), Error> {
    let mut client = route::connect_full(leaf, context(leaf)).await?;
    if client
        .full_routes()
        .connections()
        .iter()
        .any(|route| matches!(route.transport(), crate::context::Transport::Unix(_)))
    {
        return Err(Error::usage(
            "Run this from your own computer, with --connect ssh://root@HOST. A device made on a server would stay trusted after that server is gone.",
        ));
    }
    let key = own_key(&config_path(leaf)?)?;
    ui::note(format_args!(
        "Approving this computer's device {}.",
        key.check_code()
    ));
    let request = SetDeviceRequest::Approve { key };
    set_everywhere(&mut client, |client, machine_id| {
        set_on::<op::SetDevice>(client, machine_id, SET_DEVICE_CAPABILITY, request.clone())
    })
    .await?
    .finish()
}

/// Say what approving the Device `code` grants, and ask. A code copied from a compromised
/// Ployz Cloud page could name someone else's Device, so the grant is spelled out first.
fn confirm_device(root: &ArgMatches, code: CheckCode) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    ui::note(format!(
        "Device {code} gets Full access to every server you approve it on: exec, port-forward, and approving other devices and passkeys."
    ));
    if leaf.get_flag("yes") {
        return Ok(());
    }
    ui::confirm(
        &format!("Approve device {code}?"),
        || {
            Error::coded(
                RpcErrorCode::ConfirmationRequired,
                format!("Approving device {code} needs confirmation. Nothing changed."),
            )
            .hint(super::Rerun::again(root).switch("yes"))
        },
        "Nothing approved.",
    )
}

async fn approve_waiting_join(leaf: &ArgMatches, code: &str) -> Result<(), Error> {
    let mut client = route::connect(leaf, context(leaf)).await?;
    let reach = Reach::of(&mut client).await?;
    let signing = DeviceSigning::load(&key::path(&config_path(leaf)?))?.ok_or_else(|| {
        Error::conflict("Approving a Server join needs this computer's approved Device key.")
    })?;
    let mut matching = Vec::new();
    let asked = if crate::cli::connection(leaf).is_some() {
        reach.entry.into_iter().collect()
    } else {
        reach.asked
    };
    for machine in &asked {
        let target = MachineTarget::from(machine);
        let pending = client
            .call_shared::<op::PendingJoins>(
                ployz_core::PendingJoinsRequest {},
                Call::once().to(&target),
            )
            .await
            .map_err(crate::connect::rpc_error)?;
        let first_keys = pending.first_keys;
        matching.extend(
            pending
                .joins
                .into_iter()
                .filter(|body| body.code() == code)
                .map(|body| {
                    let hint = first_keys.get(&body.machine.id).copied();
                    (*machine, body, hint)
                }),
        );
    }
    let [(admitting, body, first_key)] = matching.as_mut_slice() else {
        return Err(if matching.is_empty() {
            Error::not_found(format!(
                "No Server join is waiting for {code}. Run the Server's join command again."
            ))
        } else {
            Error::conflict(
                "More than one admission is waiting for that join. Nothing was approved. Select one admitting Server with --connect.",
            )
        });
    };
    // Cloud relays this body, so it can name a Cluster of its own; only a route the person
    // chose, or the Cluster this computer pinned for the Organization, vouches for it.
    if let crate::sealing_pin::SealTo::Pinned(pin) =
        pin::join_seal_to(&config_path(leaf)?, &client).await?
    {
        refuse_unpinned(body, pin.as_ref())?;
    }
    if let Some(expected) = *first_key {
        refuse_another_key(body, expected, signing.device_key())?;
    }
    show_join(body);
    let approval = sign_join(&signing, body)?;
    client
        .call_shared::<op::ApproveJoin>(
            ployz_core::ApproveJoinRequest { approval },
            Call::once().to(&MachineTarget::from(&*admitting)),
        )
        .await
        .map_err(crate::connect::rpc_error)?;
    ui::finish(&json!({ "approved_join": body.machine.id }), || {
        ui::stream(
            "Approved the Server join. Enroll it again from the dashboard, or rerun its join command.",
        )
    })
}

pub(crate) fn remove(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let code = typed_code(leaf).or_else(|named| {
        CheckCode::parse(typed(leaf))
            .map(|check| check.code())
            .map_err(|_| named)
    })?;
    let own_key = key::path(&config_path(leaf)?);
    let own = key::load(&own_key)?.map(|secret| key::public(&secret));
    if own.is_some_and(|own| own.code() == code) {
        ui::warn(format!(
            "This is this computer's own device. Once removed, these Servers never trust it again. To manage them from here later, delete {} and run: ployz device approve --this-device --connect ssh://root@HOST",
            own_key.display()
        ));
    }
    runtime()?.block_on(async {
        let mut client = route::connect_full(leaf, context(leaf)).await?;
        let own = own
            .filter(|own| own.code() == code)
            .map(|key| KeyRef::Signing { key });
        let key = full_key(&mut client, code, own).await?;
        let settled = set_everywhere(&mut client, |client, machine_id| {
            remove_on(client, machine_id, code, key)
        })
        .await?;
        settled.warn_approved_passkeys(code);
        settled.finish()
    })
}

pub(crate) fn list(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let own = key::load(&key::path(&config_path(leaf)?))?.map(|secret| key::public(&secret));
    runtime()?.block_on(async {
        let mut client = route::connect(leaf, context(leaf)).await?;
        let reach = Reach::of(&mut client).await?;
        let (answers, gaps) = reach
            .fan_out(|machine_id| devices_on(client.clone(), machine_id))
            .await;
        let roster = roster(&answers);
        let mut table = Table::new(
            [
                "DEVICE",
                "KIND",
                "ACCOUNT",
                "APPROVED BY",
                "SERVERS",
                "MISSING ON",
            ],
            "No devices. Approve one with ployz device approve <code>.",
        );
        for device in &roster {
            let on = format!("{} of {}", device.machines.len(), answers.len());
            let missing = device
                .missing
                .iter()
                .map(|machine_id| reach.name(machine_id))
                .collect::<Vec<_>>()
                .join(", ");
            let tone = if device.missing.is_empty() {
                Tone::Good
            } else {
                Tone::Change
            };
            let (kind, account, shown) = match &device.key {
                KeyKind::Device => (
                    "device",
                    "",
                    match own.filter(|own| own.code() == device.code) {
                        Some(own) => format!("{} (this computer)", own.check_code()),
                        None => device.code.to_string(),
                    },
                ),
                KeyKind::Passkey { account, full_code } => (
                    "passkey, approve-only",
                    account.as_str(),
                    full_code.to_string(),
                ),
            };
            table.row([
                Cell::from(shown),
                Cell::from(kind),
                Cell::from(account),
                Cell::from(device.approvers(&reach)),
                Cell::status(on, tone),
                Cell::from(missing),
            ]);
        }
        ui::finish_fanout("devices", &roster, &reach.named(gaps), || {
            ui::rows(&table);
        })
    })
}

fn context(leaf: &ArgMatches) -> Option<&str> {
    leaf.get_one::<String>("context").map(String::as_str)
}

fn typed(leaf: &ArgMatches) -> &str {
    leaf.get_one::<String>("code").expect("code is required")
}

fn typed_code(leaf: &ArgMatches) -> Result<DeviceCode, Error> {
    Ok(DeviceCode::parse(typed(leaf))?)
}

/// Cloud's answer to a filed request, which names the request by the key's name.
#[derive(Deserialize)]
struct Filed {
    code: DeviceCode,
    expires_at: String,
}

#[derive(Debug, Serialize)]
struct Requested {
    code: CheckCode,
    expires_at: String,
}

#[derive(Deserialize)]
struct Pending {
    key: DeviceKey,
}

/// Fetch the key waiting under `code`'s name, refuse it unless it hashes to all of `code`,
/// then `set` it.
async fn approve_with<T>(
    credential: &Credential,
    code: CheckCode,
    set: impl AsyncFnOnce(SetDeviceRequest) -> Result<T, Error>,
) -> Result<T, Error> {
    let path = format!("device-requests/{}", code.code());
    let pending = match cloud::account::call::<Pending>(credential, Method::GET, &path, None).await
    {
        Err(LoginError::Status { body, .. })
            if cloud::account::refusal(body.as_bytes())
                .is_some_and(|refused| refused.code == ployz_core::RpcErrorCode::NotFound) =>
        {
            return Err(Error::not_found(format!(
                "No request for {code}. Requests expire after 24h."
            ))
            .hint(request_hint()));
        }
        reply => reply?,
    };
    if pending.key.check_code() != code {
        return Err(Error::conflict(format!(
            "The key Ployz Cloud returned doesn't match {code}. Nothing changed."
        )));
    }
    set(SetDeviceRequest::Approve { key: pending.key }).await
}

async fn set_everywhere<F, Fut, O>(client: &mut Client, set: F) -> Result<Settled, Error>
where
    F: Fn(Client, MachineId) -> Fut,
    Fut: Future<Output = Result<Option<O>, RpcError>> + Send + 'static,
    O: Into<Outcome> + Send + 'static,
{
    let mut reach = Reach::full(client).await?;
    let client: &Client = client;
    let (answers, gaps) = reach
        .fan_out_through_joins(client, |machine_id| set(client.clone(), machine_id))
        .await;
    Ok(Settled {
        answers: answers
            .into_iter()
            .map(|(machine_id, outcome)| Answer {
                server: reach.name(&machine_id),
                machine_id,
                outcome: outcome.into(),
            })
            .collect(),
        gaps: reach.named(gaps),
    })
}

async fn set_on<T: Rpc<Response = DeviceSet>>(
    client: Client,
    machine_id: MachineId,
    capability: &'static str,
    request: T::Request,
) -> Result<Option<DeviceSet>, RpcError> {
    let target = MachineTarget::from(&machine_id);
    if !compat::supports(&client, &target, capability).await? {
        return Ok(None);
    }
    client
        .call_shared::<T>(request, Call::once().to(&target))
        .await
        .map_err(crate::connect::rpc_error)
        .map(Some)
}

/// The full key `code` names: `own` when it is this computer's Device, and each key a
/// Server trusts or already removed under it, so a Server that never held the key can still
/// mark it removed, even on a run after another Server removed it. `None` when neither names
/// one.
///
/// # Errors
/// Refuses a code that names two different keys.
async fn full_key(
    client: &mut Client,
    code: DeviceCode,
    own: Option<KeyRef>,
) -> Result<Option<KeyRef>, Error> {
    let reach = Reach::full(client).await?;
    let (answers, _) = reach
        .fan_out(|machine_id| keys_on(client.clone(), machine_id, code))
        .await;
    let mut keys: Vec<KeyRef> = Vec::new();
    for key in own
        .into_iter()
        .chain(answers.into_iter().flat_map(|(_, keys)| keys))
    {
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    match keys.as_slice() {
        [] => Ok(None),
        [key] => Ok(Some(*key)),
        _ => Err(Error::conflict(format!(
            "Code {code} names more than one key on your Servers. Nothing was removed."
        ))),
    }
}

/// The keys one Server with key-step.v1 trusts or removed under `code`, by full key.
async fn keys_on(
    client: Client,
    machine_id: MachineId,
    code: DeviceCode,
) -> Result<Option<Vec<KeyRef>>, RpcError> {
    let target = MachineTarget::from(&machine_id);
    if !compat::supports(&client, &target, KEY_STEP_CAPABILITY).await? {
        return Ok(None);
    }
    let details = client
        .call_shared::<op::Inspect>(InspectRequest::default(), Call::once().to(&target))
        .await
        .map_err(crate::connect::rpc_error)?;
    let devices = details
        .devices
        .into_iter()
        .filter(|key| key.code() == code)
        .map(|key| KeyRef::Signing { key });
    let passkeys = details
        .passkeys
        .into_iter()
        .filter(|trusted| trusted.passkey.code() == code)
        .map(|trusted| KeyRef::Passkey {
            key: trusted.passkey.public_key,
        });
    let grants = details
        .grants
        .into_iter()
        .filter(|trusted| trusted.grant.key.code() == code)
        .map(|trusted| KeyRef::Signing {
            key: trusted.grant.key.into(),
        });
    let removed = details
        .removed_keys
        .into_iter()
        .filter(|key| key.code() == code);
    Ok(Some(
        devices
            .chain(passkeys)
            .chain(grants)
            .chain(removed)
            .collect(),
    ))
}

/// Remove the key `code` names on one Server, Device or passkey, for good. A Server with
/// key-step.v1 may hold passkeys: its Inspect says which kind the code names and, for a
/// Device, the passkeys it approved there, since a Device that removed itself can no
/// longer ask. It is handed `key` in full when one is known, so it marks the key removed
/// even if it never held it and a grant still on its way there is refused. Without the key
/// in full, one that doesn't hold it can't mark it, so its answer is [`Removal::NotMarked`].
/// Set-device.v1 answers as 0.3 did, so the kind shown is the one Inspect read.
async fn remove_on(
    client: Client,
    machine_id: MachineId,
    code: DeviceCode,
    key: Option<KeyRef>,
) -> Result<Option<Removal>, RpcError> {
    let target = MachineTarget::from(&machine_id);
    if !compat::supports(&client, &target, SET_DEVICE_CAPABILITY).await? {
        return Ok(None);
    }
    let mut passkey = false;
    let mut grant = false;
    let mut approved_passkeys = Vec::new();
    let mut request = SetDeviceRequest::Remove { code };
    let marks = compat::supports(&client, &target, KEY_STEP_CAPABILITY).await?;
    if marks {
        if let Some(key) = key {
            request = SetDeviceRequest::RemoveKey { key };
        }
        let details = client
            .call_shared::<op::Inspect>(InspectRequest::default(), Call::once().to(&target))
            .await
            .map_err(crate::connect::rpc_error)?;
        grant = details
            .grants
            .iter()
            .any(|trusted| trusted.grant.key.code() == code);
        let listed = details.passkeys;
        passkey = listed.iter().any(|trusted| trusted.passkey.code() == code);
        let approver = PasskeyApprover::Device { code };
        approved_passkeys = listed
            .iter()
            .filter(|trusted| trusted.approved_by == approver)
            .map(|trusted| trusted.passkey.code())
            .collect();
    }
    let removed = match client
        .call_shared::<op::SetDevice>(request, Call::once().to(&target))
        .await
        .map_err(crate::connect::rpc_error)?
    {
        DeviceSet::Removed { code } if passkey => Removal::RemovedPasskey { code },
        DeviceSet::Removed { code } if grant => Removal::RemovedKey { code },
        DeviceSet::Removed { code } => Removal::RemovedDevice {
            code,
            approved_passkeys,
        },
        DeviceSet::NotPresent { code } if marks && key.is_none() => Removal::NotMarked { code },
        DeviceSet::NotPresent { code }
        | DeviceSet::Approved { code }
        | DeviceSet::AlreadyApproved { code } => Removal::NotPresent { code },
    };
    Ok(Some(removed))
}

/// One key a Server lists, with who approved it there when it is a passkey.
struct Listed {
    code: DeviceCode,
    key: KeyKind,
    approved_by: Option<PasskeyApprover>,
}

async fn devices_on(
    client: Client,
    machine_id: MachineId,
) -> Result<Option<Vec<Listed>>, RpcError> {
    let target = MachineTarget::from(&machine_id);
    if !compat::supports(&client, &target, SET_DEVICE_CAPABILITY).await? {
        return Ok(None);
    }
    let details = client
        .call_shared::<op::Inspect>(InspectRequest::default(), Call::once().to(&target))
        .await
        .map_err(crate::connect::rpc_error)?;
    let devices = details
        .management_clients
        .iter()
        .filter_map(DeviceCode::named_by)
        .map(|code| Listed {
            code,
            key: KeyKind::Device,
            approved_by: None,
        });
    let passkeys = details.passkeys.into_iter().map(|trusted| Listed {
        code: trusted.passkey.code(),
        key: KeyKind::Passkey {
            full_code: trusted.passkey.public_key.check_code(),
            account: trusted.passkey.account,
        },
        approved_by: Some(trusted.approved_by),
    });
    Ok(Some(devices.chain(passkeys).collect()))
}

pub(super) fn request_hint() -> Hint {
    Hint::Next("ployz device request".into())
}

/// Daemons from before Devices, or before Device enforcement. Delete once every supported
/// daemon has both.
mod compat {
    use ployz_core::{MachineTarget, RpcError};

    use crate::connect::Client;

    /// Whether the Machine at `target` promises `capability`; one that doesn't is an
    /// omission, not a failure.
    pub(super) async fn supports(
        client: &Client,
        target: &MachineTarget,
        capability: &str,
    ) -> Result<bool, RpcError> {
        client
            .supports(Some(target), capability)
            .await
            .map_err(crate::connect::rpc_error)
    }
}

/// The Servers a fan-out asks, the ones it can't, and how warnings name each. `names`
/// holds every Member the fan-out knows of, also those outside its scope.
pub(super) struct Reach {
    asked: Vec<MachineId>,
    silent: Vec<MachineId>,
    entry: Option<MachineId>,
    names: BTreeMap<MachineId, MachineName>,
    /// Whether the fan-out also asks the Members that joined while it ran. A change the
    /// person aimed at one Server never reaches past it.
    follows_joins: bool,
}

impl Reach {
    async fn full(client: &mut Client) -> Result<Self, Error> {
        let mut reach = Self::of(client).await?;
        let direct = client.full_machines();
        if client.full_routes().single() {
            reach.asked.retain(|machine| Some(*machine) == reach.entry);
            reach.silent.retain(|machine| Some(*machine) == reach.entry);
            reach.follows_joins = false;
        } else if !direct.is_empty() {
            reach.silent.extend(
                reach
                    .asked
                    .iter()
                    .filter(|machine| !direct.contains(machine))
                    .copied(),
            );
            reach.asked.retain(|machine| direct.contains(machine));
        }
        Ok(reach)
    }

    /// Like [`Self::full`], but a Member this CLI has no Full route to stays in the
    /// result as omitted, so a change it misses is never reported complete.
    pub(super) async fn every_member(client: &mut Client) -> Result<Self, Error> {
        let mut reach = Self::of(client).await?;
        let direct = client.full_machines();
        let single = client.full_routes().single();
        let entry = reach.entry;
        let (asked, unrouted): (Vec<_>, Vec<_>) =
            reach
                .asked
                .iter()
                .copied()
                .partition(|machine| match single {
                    true => Some(*machine) == entry,
                    false => direct.is_empty() || direct.contains(machine),
                });
        reach.asked = asked;
        reach.silent.extend(unrouted);
        Ok(reach)
    }

    pub(super) async fn of(client: &mut Client) -> Result<Self, Error> {
        let machines = client.machines().await?;
        let (asked, silent) = machines
            .iter()
            .partition::<Vec<_>, _>(|observed| observed.invites_rpc());
        Ok(Self {
            asked: asked.iter().map(|observed| observed.machine.id).collect(),
            silent: silent.iter().map(|observed| observed.machine.id).collect(),
            entry: client.entry_machine(),
            names: machines
                .iter()
                .map(|observed| (observed.machine.id, observed.machine.name.clone()))
                .collect(),
            follows_joins: true,
        })
    }

    /// [`Self::fan_out`], then the same call on each Member that joined while it ran, until
    /// no Server that answered lists a Member this reach never named. A joiner is sealed what
    /// its admitting Server held then, so it may lack the change, and a Member found this way
    /// may itself have admitted another.
    pub(super) async fn fan_out_through_joins<T, F, Fut>(
        &mut self,
        client: &Client,
        call: F,
    ) -> (Vec<(MachineId, T)>, Gaps)
    where
        F: Fn(MachineId) -> Fut,
        Fut: Future<Output = Result<Option<T>, RpcError>>,
    {
        let (mut answers, mut gaps) = self.fan_out(&call).await;
        if !self.follows_joins {
            return (answers, gaps);
        }
        let unlisted = loop {
            let (joined, unlisted) = self
                .joined_since(client, answers.iter().map(|(machine_id, _)| *machine_id))
                .await;
            if joined.names.is_empty() {
                break unlisted;
            }
            let (later, later_gaps) = joined.fan_out(&call).await;
            answers.extend(later);
            gaps.extend(&later_gaps.failures, &later_gaps.omitted);
            self.asked.extend(joined.asked);
            self.silent.extend(joined.silent);
            self.names.extend(joined.names);
        };
        gaps.extend(&unlisted, &[]);
        (answers, gaps)
    }

    /// The Members the `answered` Servers list now that this reach never named, and the
    /// Servers that answered but couldn't list theirs within the target deadline: a gap,
    /// since a Member one of them knows of may then go unreached. A Member found here is
    /// asked whatever its membership reads: a joiner is listed before peers see it up, and a
    /// call it can't answer is a gap too.
    async fn joined_since(
        &self,
        client: &Client,
        answered: impl Iterator<Item = MachineId>,
    ) -> (Self, Vec<MachineFailure<RpcError>>) {
        let listed = fan_out_entry_last(answered, None, |machine_id| {
            let client = client.clone();
            async move {
                client
                    .call_shared::<op::ListMachines>(
                        ployz_core::ListMachinesRequest {},
                        Call::once()
                            .to(&MachineTarget::from(&machine_id))
                            .within(Deadline::Target(crate::connect::TARGET_RPC_TIMEOUT)),
                    )
                    .await
                    .map(|list| Some(list.machines))
                    .map_err(crate::connect::rpc_error)
            }
        })
        .await;
        let direct = client.full_machines();
        let mut joined = Self {
            asked: Vec::new(),
            silent: Vec::new(),
            entry: None,
            names: BTreeMap::new(),
            follows_joins: true,
        };
        for observed in listed
            .successes
            .into_iter()
            .flat_map(|success| success.value)
        {
            let id = observed.machine.id;
            if self.names.contains_key(&id) || joined.names.contains_key(&id) {
                continue;
            }
            joined.names.insert(id, observed.machine.name.clone());
            if direct.is_empty() || direct.contains(&id) {
                joined.asked.push(id);
            } else {
                joined.silent.push(id);
            }
        }
        (joined, listed.failures)
    }

    pub(super) async fn fan_out<T, F, Fut>(&self, call: F) -> (Vec<(MachineId, T)>, Gaps)
    where
        F: Fn(MachineId) -> Fut,
        Fut: Future<Output = Result<Option<T>, RpcError>>,
    {
        let entry = self.entry.filter(|id| self.asked.contains(id));
        let result = fan_out_entry_last(self.asked.iter().copied(), entry, call).await;
        let mut gaps = Gaps::default();
        gaps.omitted.extend(&self.silent);
        gaps.omitted.extend(result.omissions);
        gaps.failures.extend(result.failures);
        let answers = result
            .successes
            .into_iter()
            .map(|success| (success.machine_id, success.value))
            .collect();
        (answers, gaps)
    }

    /// The Machine this CLI entered through, when the fan-out asks it.
    pub(super) fn entry(&self) -> Option<MachineId> {
        self.entry.filter(|id| self.asked.contains(id))
    }

    pub(super) fn name(&self, machine_id: &MachineId) -> String {
        self.names
            .get(machine_id)
            .map_or_else(|| machine_id.to_string(), ToString::to_string)
    }

    pub(super) fn named(&self, gaps: Gaps) -> Gaps {
        gaps.named(self.names.iter().map(|(id, name)| (*id, name)))
    }
}

struct Settled {
    answers: Vec<Answer>,
    gaps: Gaps,
}

#[derive(Serialize)]
struct Answer {
    machine_id: MachineId,
    server: String,
    #[serde(flatten)]
    outcome: Outcome,
}

/// What one Server changed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(untagged)]
enum Outcome {
    Set(DeviceSet),
    #[serde(serialize_with = "removal_json")]
    Removed(Removal),
}

/// What `device rm` removed on one Server.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Removal {
    RemovedDevice {
        code: DeviceCode,
        /// Passkeys the removed Device had approved there. They stay trusted.
        approved_passkeys: Vec<DeviceCode>,
    },
    RemovedPasskey {
        code: DeviceCode,
    },
    /// A CI or agent key.
    RemovedKey {
        code: DeviceCode,
    },
    NotPresent {
        code: DeviceCode,
    },
    /// Not trusted there, and not marked removed either: no Server named the key in full,
    /// so an approval of it still on its way there would be accepted.
    NotMarked {
        code: DeviceCode,
    },
}

/// `device rm --json` keeps main's `outcome: removed` and adds the kind beside it.
#[derive(Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum RemovalJson<'a> {
    Removed {
        code: DeviceCode,
        kind: &'static str,
        #[serde(skip_serializing_if = "<[_]>::is_empty")]
        approved_passkeys: &'a [DeviceCode],
    },
    NotPresent {
        code: DeviceCode,
    },
    NotMarked {
        code: DeviceCode,
    },
}

fn removal_json<S: serde::Serializer>(removed: &Removal, serializer: S) -> Result<S::Ok, S::Error> {
    let json = match removed {
        Removal::RemovedDevice {
            code,
            approved_passkeys,
        } => RemovalJson::Removed {
            code: *code,
            kind: "device",
            approved_passkeys,
        },
        Removal::RemovedPasskey { code } => RemovalJson::Removed {
            code: *code,
            kind: "passkey",
            approved_passkeys: &[],
        },
        Removal::RemovedKey { code } => RemovalJson::Removed {
            code: *code,
            kind: "key",
            approved_passkeys: &[],
        },
        Removal::NotPresent { code } => RemovalJson::NotPresent { code: *code },
        Removal::NotMarked { code } => RemovalJson::NotMarked { code: *code },
    };
    json.serialize(serializer)
}

impl From<DeviceSet> for Outcome {
    fn from(set: DeviceSet) -> Self {
        Self::Set(set)
    }
}

impl From<Removal> for Outcome {
    fn from(removed: Removal) -> Self {
        Self::Removed(removed)
    }
}

impl Settled {
    /// None when no Server answered, since the gap warnings name every one.
    fn table(&self) -> Option<Table> {
        if self.answers.is_empty() && !self.gaps.is_complete() {
            return None;
        }
        let mut table = Table::new(["SERVER", "RESULT"], "No servers answered.");
        for answer in &self.answers {
            let (result, tone) = match answer.outcome {
                Outcome::Set(DeviceSet::Approved { .. }) => ("approved", Tone::Good),
                Outcome::Set(DeviceSet::AlreadyApproved { .. }) => ("already approved", Tone::Good),
                Outcome::Set(DeviceSet::Removed { .. })
                | Outcome::Removed(Removal::RemovedDevice { .. }) => ("removed device", Tone::Good),
                Outcome::Removed(Removal::RemovedPasskey { .. }) => ("removed passkey", Tone::Good),
                Outcome::Removed(Removal::RemovedKey { .. }) => ("removed key", Tone::Good),
                Outcome::Set(DeviceSet::NotPresent { .. })
                | Outcome::Removed(Removal::NotPresent { .. }) => ("not present", Tone::Good),
                Outcome::Removed(Removal::NotMarked { .. }) => ("not marked removed", Tone::Bad),
            };
            table.row([Cell::from(&answer.server), Cell::status(result, tone)]);
        }
        Some(table)
    }

    /// Warn about each passkey the removed Device `code` approved, which stays trusted.
    fn warn_approved_passkeys(&self, code: DeviceCode) {
        let mut left = BTreeMap::<DeviceCode, Vec<&str>>::new();
        for answer in &self.answers {
            let Outcome::Removed(Removal::RemovedDevice {
                approved_passkeys, ..
            }) = &answer.outcome
            else {
                continue;
            };
            for passkey in approved_passkeys {
                left.entry(*passkey).or_default().push(&answer.server);
            }
        }
        for (passkey, servers) in left {
            ui::warn(format!(
                "Passkey {passkey}, which device {code} approved, stays trusted on {}. Remove it with: ployz device rm {passkey}",
                servers.join(", ")
            ));
        }
    }

    /// Finish as a fan-out does, and as a partial result when a Server couldn't mark the
    /// key removed.
    fn finish(&self) -> Result<(), Error> {
        let table = self.table();
        let finished = ui::finish_fanout("machines", &self.answers, &self.gaps, || {
            if let Some(table) = &table {
                ui::rows(table);
            }
        });
        let unmarked: Vec<&str> = self
            .answers
            .iter()
            .filter(|answer| matches!(answer.outcome, Outcome::Removed(Removal::NotMarked { .. })))
            .map(|answer| answer.server.as_str())
            .collect();
        if unmarked.is_empty() {
            return finished;
        }
        ui::warn(format!(
            "No server named this key in full, so {} couldn't mark it removed and would still accept an approval of it already on its way. Check the code with ployz device ls, and run the removal again once a server that trusted the key answers.",
            unmarked.join(", ")
        ));
        finished.and(Err(Error::partial()))
    }
}

#[derive(Serialize)]
struct Device {
    code: DeviceCode,
    #[serde(flatten)]
    key: KeyKind,
    machines: Vec<MachineId>,
    missing: Vec<MachineId>,
    /// For a passkey, who approved it on each Server that lists it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    approved_by: Vec<Approval>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
struct Approval {
    machine_id: MachineId,
    #[serde(flatten)]
    by: PasskeyApprover,
}

impl Device {
    /// Each distinct approver, as the Servers that list this passkey report it.
    fn approvers(&self, reach: &Reach) -> String {
        let mut labels: Vec<String> = Vec::new();
        for approval in &self.approved_by {
            let server = reach.name(&approval.machine_id);
            let label = match approval.by {
                PasskeyApprover::Device { code } => format!("device {code}"),
                PasskeyApprover::Server => format!("on {server}"),
                PasskeyApprover::Routed => format!("routed to {server}"),
                PasskeyApprover::Unknown => "unknown".to_owned(),
            };
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
        labels.join(", ")
    }
}

/// What a listed key is: a Device that manages Servers, or a passkey that only approves
/// workflows, shown with the Cloud account it belongs to.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum KeyKind {
    Device,
    Passkey {
        account: PasskeyAccount,
        full_code: CheckCode,
    },
}

/// One row per key, Devices first, then passkeys under them.
fn roster(answers: &[(MachineId, Vec<Listed>)]) -> Vec<Device> {
    let mut on = BTreeMap::<(bool, DeviceCode), Device>::new();
    for (machine_id, keys) in answers {
        for listed in keys {
            let passkey = matches!(listed.key, KeyKind::Passkey { .. });
            let row = on.entry((passkey, listed.code)).or_insert_with(|| Device {
                code: listed.code,
                key: listed.key.clone(),
                machines: Vec::new(),
                missing: Vec::new(),
                approved_by: Vec::new(),
            });
            row.machines.push(*machine_id);
            row.approved_by
                .extend(listed.approved_by.map(|by| Approval {
                    machine_id: *machine_id,
                    by,
                }));
        }
    }
    on.into_values()
        .map(|mut row| {
            row.missing = answers
                .iter()
                .map(|(machine_id, _)| *machine_id)
                .filter(|machine_id| !row.machines.contains(machine_id))
                .collect();
            row
        })
        .collect()
}

#[cfg(test)]
#[path = "device_tests.rs"]
mod tests;
