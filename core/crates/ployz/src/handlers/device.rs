//! `ployz device`: the people's keys every Machine admits at Full scope. Cloud relays a
//! Device's key to the approver, who checks it against the code the Device printed.

use std::collections::BTreeMap;
use std::path::Path;

use clap::{Arg, ArgAction, ArgMatches, Command};
use ployz_core::{
    DeviceCode, DeviceEnforcementSet, DeviceKey, DeviceSet, InspectRequest, MachineFailure,
    MachineId, MachineName, MachineTarget, OperationId, REMOVE_TRUSTED_KEY_CAPABILITY,
    RemoveTrustedKeyRequest, Rpc, RpcError, RpcErrorCode, SET_DEVICE_CAPABILITY,
    SET_DEVICE_ENFORCEMENT_CAPABILITY, SET_PASSKEY_CAPABILITY, SetDeviceEnforcementRequest,
    SetDeviceRequest, SetPasskeyRequest, TrustedKeyKind, TrustedKeyRemoved,
    authority::{
        Member, Passkey, PasskeyAccount, PasskeyApprover, PasskeyPublicKey, Signed, Signer,
        StartingSet, Timestamp,
        trusted_key::{CredentialId, RelyingParty},
        validity,
    },
    op,
};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::task::JoinSet;

use super::{Error, config_path, leaf_matches, runtime, server};
use crate::cli::{base, positional, yes};
use crate::cloud_account::{self, Credential};
use crate::cloud_login::{CredentialStore, LoginError};
use crate::connect::Client;
use crate::ui::{self, Cell, Gaps, Hint, Table, Tone};

pub(crate) fn group(command: Command) -> Command {
    command.about("Approve the devices that can exec and port-forward")
}

pub(crate) fn request_command(command: Command) -> Command {
    command
        .about("Ask to approve this device")
        .after_help(concat!("Examples:\n", "  ployz device request"))
}

pub(crate) fn approve_command(command: Command) -> Command {
    base(command, "Approve a device or a passkey on every server")
        .after_help(concat!(
            "Examples:\n",
            "  ployz device approve mmg42klgyq\n",
            "  sudo ployz device approve --passkey hq3vbn5x2c"
        ))
        .arg(positional("code", true).help(
            "The code the requesting device or server printed, or the passkey's code from Ployz Cloud",
        ))
        .arg(
            Arg::new("join")
                .long("join")
                .action(ArgAction::SetTrue)
                .help("Approve a waiting Server join with this device's key"),
        )
        .arg(
            Arg::new("passkey")
                .long("passkey")
                .action(ArgAction::SetTrue)
                .conflicts_with("join")
                .help("Approve a passkey added in Ployz Cloud, which may approve workflows and nothing else"),
        )
        .arg(yes("Approve without asking"))
}

pub(crate) fn list_command(command: Command) -> Command {
    base(command, "List approved devices and passkeys")
}

pub(crate) fn remove_command(command: Command) -> Command {
    base(command, "Remove a device or a passkey from every server")
        .after_help(concat!("Examples:\n", "  ployz device rm mmg42klgyq"))
        .arg(positional("code", true).help("The device's or passkey's code, from ployz device ls"))
}

pub(crate) fn enforce_command(command: Command) -> Command {
    base(
        command,
        "Require an approved device's signature on every service restart",
    )
    .after_help(concat!(
        "Examples:\n",
        "  ployz device enforce\n",
        "  ployz device enforce --off"
    ))
    .arg(
        Arg::new("off")
            .long("off")
            .action(ArgAction::SetTrue)
            .help("Let restarts through without a device's signature again"),
    )
}

pub(crate) fn request(root: &ArgMatches) -> Result<(), Error> {
    let config = config_path(leaf_matches(root))?;
    let store = CredentialStore::beside(&config);
    let requested = runtime()?.block_on(async {
        let credential = cloud_account::from_env(&store).await?;
        request_at(&credential, &key::path(&config)).await
    })?;
    let code = requested.code;
    ui::finish(&requested, || {
        ui::stream(code.as_str());
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
        cloud_account::call::<Requested>(credential, Method::POST, "device-requests", Some(&body))
            .await?;
    if filed.code != code {
        return Err(Error::conflict(format!(
            "Ployz Cloud filed this device as {} but its key hashes to {code}. Nothing changed.",
            filed.code
        )));
    }
    Ok(Requested {
        code,
        expires_at: filed.expires_at,
    })
}

pub(crate) fn approve(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    if leaf.get_flag("join") {
        let code = leaf
            .get_one::<String>("code")
            .expect("required code")
            .trim()
            .to_ascii_lowercase();
        return runtime()?.block_on(approve_waiting_join(leaf, &code));
    }
    let code = typed_code(leaf)?;
    let store = CredentialStore::beside(&config_path(leaf)?);
    if leaf.get_flag("passkey") {
        return runtime()?.block_on(approve_passkey(leaf, &store, code));
    }
    runtime()?.block_on(async {
        let credential = cloud_account::from_env(&store).await?;
        let settled = approve_with(&credential, code, async |request| {
            confirm_device(leaf, code)?;
            let mut client = server::connect_full(leaf, context(leaf)).await?;
            set_everywhere(&mut client, |client, machine_id| {
                set_on::<op::SetDevice>(client, machine_id, SET_DEVICE_CAPABILITY, request.clone())
            })
            .await
        })
        .await?;
        settled.finish()
    })
}

/// Say what approving the Device `code` grants, and ask. A code copied from a compromised
/// Ployz Cloud page could name someone else's Device, so the grant is spelled out first.
fn confirm_device(leaf: &ArgMatches, code: DeviceCode) -> Result<(), Error> {
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
            .hint(Hint::Retry(super::typed_with(&["--yes"])))
        },
        "Nothing approved.",
    )
}

/// Show the passkey waiting at `code`, ask, then add it to every Server.
async fn approve_passkey(
    leaf: &ArgMatches,
    store: &CredentialStore,
    code: DeviceCode,
) -> Result<(), Error> {
    let credential = cloud_account::from_env(store).await?;
    let passkey = pending_passkey(&credential, code).await?;
    ui::note(format!(
        "Passkey {code} for {}. It can approve workflows on these servers and nothing else.",
        passkey.account
    ));
    if !leaf.get_flag("yes") {
        ui::confirm(
            &format!("Approve passkey {code} for {}?", passkey.account),
            || {
                Error::coded(
                    RpcErrorCode::ConfirmationRequired,
                    format!("Approving passkey {code} needs confirmation. Nothing changed."),
                )
                .hint(Hint::Retry(super::typed_with(&["--yes"])))
            },
            "Nothing approved.",
        )?;
    }
    let mut client = server::connect_full(leaf, context(leaf)).await?;
    let request = SetPasskeyRequest::Approve {
        passkey: Box::new(passkey),
    };
    set_everywhere(&mut client, |client, machine_id| {
        set_on::<op::SetPasskey>(client, machine_id, SET_PASSKEY_CAPABILITY, request.clone())
    })
    .await?
    .finish()
}

/// The passkey waiting in Cloud at `code`, refused unless its key hashes to `code`. Its
/// relying party comes from this CLI's own Cloud URL, so Cloud can't point a Server at
/// another site.
async fn pending_passkey(credential: &Credential, code: DeviceCode) -> Result<Passkey, Error> {
    let path = format!("passkey-requests/{code}");
    let pending = match cloud_account::call::<PendingPasskey>(credential, Method::GET, &path, None)
        .await
    {
        Err(LoginError::Status { body, .. })
            if cloud_account::refusal(body.as_bytes())
                .is_some_and(|refused| refused.code == RpcErrorCode::NotFound) =>
        {
            return Err(Error::not_found(format!(
                "No passkey waits for {code}. Add one again in your Ployz Cloud account; it waits 24h."
            )));
        }
        reply => reply?,
    };
    let cloud = url::Url::parse(credential.cloud()).map_err(Error::command)?;
    let passkey = Passkey {
        credential_id: pending.credential_id,
        public_key: pending.public_key,
        account: pending.account,
        relying_party: RelyingParty::of(&cloud)?,
    };
    if passkey.code() != code {
        return Err(Error::conflict(format!(
            "The passkey Ployz Cloud returned doesn't match {code}. Nothing changed."
        )));
    }
    Ok(passkey)
}

/// A passkey waiting in Cloud. Only its key's code makes it trustworthy.
#[derive(Deserialize)]
struct PendingPasskey {
    credential_id: CredentialId,
    public_key: PasskeyPublicKey,
    account: PasskeyAccount,
}

async fn approve_waiting_join(leaf: &ArgMatches, code: &str) -> Result<(), Error> {
    let mut client = server::connect(leaf, context(leaf)).await?;
    let reach = Reach::of(&mut client).await?;
    let secret = key::load(&key::path(&config_path(leaf)?))?.ok_or_else(|| {
        Error::conflict("Approving a Server join needs this computer's approved Device key.")
    })?;
    let signer = ployz_core::authority::Signer::from_seed(&secret);
    let mut matching = Vec::new();
    let asked = if leaf.get_one::<String>("connect").is_some() {
        reach.entry.into_iter().collect()
    } else {
        reach.asked
    };
    for machine in &asked {
        let target = MachineTarget::from(machine);
        let pending = client
            .invoke::<op::PendingJoins>(ployz_core::PendingJoinsRequest {}, &target, None)
            .await?;
        matching.extend(
            pending
                .joins
                .into_iter()
                .filter(|body| body.code() == code)
                .map(|body| (*machine, body)),
        );
    }
    let [(admitting, body)] = matching.as_mut_slice() else {
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
    show_join(body);
    body.signer = signer.public().into();
    let approval = ployz_core::authority::Signed::sign(body, &signer).map_err(Error::command)?;
    client
        .invoke::<op::ApproveJoin>(
            ployz_core::ApproveJoinRequest { approval },
            &MachineTarget::from(&*admitting),
            None,
        )
        .await?;
    ui::finish(&json!({ "approved_join": body.machine.id }), || {
        ui::stream("Approved the Server join. Rerun its join command.")
    })
}

fn show_join(body: &ployz_core::authority::JoinApproval) {
    ui::note(join_summary(body));
}

fn join_summary(body: &ployz_core::authority::JoinApproval) -> String {
    let endpoints = body
        .machine
        .advertised_endpoints
        .iter()
        .map(|endpoint| endpoint.0.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Approving Server {} on {} with endpoints {} (join code {}).",
        body.machine.name,
        body.machine.subnet,
        if endpoints.is_empty() {
            "none"
        } else {
            &endpoints
        },
        body.code()
    )
}

pub(super) async fn sign_waiting_join(
    client: &mut Client,
    config: &Path,
    expected: &ployz_core::RegisterRequest,
) -> Result<bool, Error> {
    let Some(secret) = key::load(&key::path(config))? else {
        return Ok(false);
    };
    let signer = ployz_core::authority::Signer::from_seed(&secret);
    let pending = client
        .call::<op::PendingJoins>(ployz_core::PendingJoinsRequest {}, None)
        .await?;
    let Some(mut body) = pending
        .joins
        .into_iter()
        .find(|body| body.machine.id == expected.machine_id)
    else {
        return Ok(false);
    };
    if !body.machine.matches_request(expected) {
        return Err(Error::conflict(
            "The waiting join does not match the Server identity, keys, allocation, or policy requested. Nothing was approved.",
        ));
    }
    show_join(&body);
    body.signer = signer.public().into();
    let approval = ployz_core::authority::Signed::sign(&body, &signer).map_err(Error::command)?;
    client
        .call::<op::ApproveJoin>(ployz_core::ApproveJoinRequest { approval }, None)
        .await?;
    Ok(true)
}

pub(crate) fn remove(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let code = typed_code(leaf)?;
    runtime()?.block_on(async {
        let mut client = server::connect_full(leaf, context(leaf)).await?;
        let settled = set_everywhere(&mut client, |client, machine_id| {
            remove_on(client, machine_id, code)
        })
        .await?;
        settled.warn_approved_passkeys(code);
        settled.finish()
    })
}

pub(crate) fn list(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    runtime()?.block_on(async {
        let mut client = server::connect(leaf, context(leaf)).await?;
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
            let (kind, account) = match &device.key {
                KeyKind::Device => ("device", ""),
                KeyKind::Passkey { account } => ("passkey, approve-only", account.as_str()),
            };
            table.row([
                Cell::from(device.code.as_str()),
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

pub(crate) fn enforce(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let enforced = !leaf.get_flag("off");
    runtime()?.block_on(async {
        let mut client = server::connect_full(leaf, context(leaf)).await?;
        let reach = Reach::full(&mut client).await?;
        let starting_set = if enforced {
            Some(sign_starting_set(&config_path(leaf)?, &client, &reach).await?)
        } else {
            None
        };
        let request = SetDeviceEnforcementRequest {
            enforced,
            starting_set,
        };
        let (answers, gaps) = reach
            .fan_out(|machine_id| enforce_on(client.clone(), machine_id, request.clone()))
            .await;
        let answers: Vec<Enforcement> = answers
            .into_iter()
            .map(|(machine_id, set)| Enforcement {
                server: reach.name(&machine_id),
                machine_id,
                enforced: set.enforced,
            })
            .collect();
        let gaps = reach.named(gaps);
        let mut table = Table::new(["SERVER", "DEVICE ENFORCEMENT"], "No servers answered.");
        for answer in &answers {
            let (state, tone) = if answer.enforced {
                ("on", Tone::Good)
            } else {
                ("off", Tone::Change)
            };
            table.row([Cell::from(&answer.server), Cell::status(state, tone)]);
        }
        ui::finish_fanout("machines", &answers, &gaps, || {
            if !answers.is_empty() || gaps.is_complete() {
                ui::rows(&table);
            }
        })
    })
}

#[derive(Serialize)]
struct Enforcement {
    machine_id: MachineId,
    server: String,
    enforced: bool,
}

async fn enforce_on(
    client: Client,
    machine_id: MachineId,
    request: SetDeviceEnforcementRequest,
) -> Result<Option<DeviceEnforcementSet>, RpcError> {
    let target = MachineTarget::from(&machine_id);
    if !compat::supports(&client, &target, SET_DEVICE_ENFORCEMENT_CAPABILITY).await? {
        return Ok(None);
    }
    client
        .invoke::<op::SetDeviceEnforcement>(request, &target, None)
        .await
        .map(Some)
}

/// Sign the Servers that answer for themselves as the Members every Server trusts once
/// enforcement is on. A row no Server answers for is left out.
async fn sign_starting_set(
    config: &Path,
    client: &Client,
    reach: &Reach,
) -> Result<Signed<StartingSet>, Error> {
    let secret = key::load(&key::path(config))?.ok_or_else(|| {
        Error::conflict("Turning on Device enforcement needs this computer's approved Device key.")
    })?;
    let signer = Signer::from_seed(&secret);
    let (answers, _) = reach
        .fan_out(|machine_id| member_of(client.clone(), machine_id))
        .await;
    let members: Vec<Member> = answers.into_iter().map(|(_, member)| member).collect();
    ui::note(starting_set_summary(reach, &members));
    let issued_at = Timestamp::now();
    Signed::sign(
        &StartingSet {
            operation: OperationId::random(),
            members,
            issued_at,
            expires_at: issued_at.plus(validity::MAX_LIFETIME_MILLIS),
            signer: signer.public().into(),
        },
        &signer,
    )
    .map_err(Error::command)
}

async fn member_of(client: Client, machine_id: MachineId) -> Result<Option<Member>, RpcError> {
    let details = client
        .invoke::<op::Inspect>(
            InspectRequest::default(),
            &MachineTarget::from(&machine_id),
            None,
        )
        .await?;
    Ok(details
        .machine
        .as_ref()
        .filter(|machine| machine.id == machine_id)
        .map(Member::from))
}

fn starting_set_summary(reach: &Reach, members: &[Member]) -> String {
    let servers = members
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
    format!(
        "Trusting {} Servers from the start: {}.",
        members.len(),
        if servers.is_empty() { "none" } else { &servers }
    )
}

fn context(leaf: &ArgMatches) -> Option<&str> {
    leaf.get_one::<String>("context").map(String::as_str)
}

fn typed_code(leaf: &ArgMatches) -> Result<DeviceCode, Error> {
    Ok(DeviceCode::parse(
        leaf.get_one::<String>("code").expect("code is required"),
    )?)
}

#[derive(Debug, Deserialize, Serialize)]
struct Requested {
    code: DeviceCode,
    expires_at: String,
}

#[derive(Deserialize)]
struct Pending {
    key: DeviceKey,
}

/// Fetch the key waiting at `code`, refuse it unless it hashes to `code`, then `set` it.
async fn approve_with<T>(
    credential: &Credential,
    code: DeviceCode,
    set: impl AsyncFnOnce(SetDeviceRequest) -> Result<T, Error>,
) -> Result<T, Error> {
    let path = format!("device-requests/{code}");
    let pending = match cloud_account::call::<Pending>(credential, Method::GET, &path, None).await {
        Err(LoginError::Status { body, .. })
            if cloud_account::refusal(body.as_bytes())
                .is_some_and(|refused| refused.code == ployz_core::RpcErrorCode::NotFound) =>
        {
            return Err(Error::not_found(format!(
                "No request for {code}. Requests expire after 24h."
            ))
            .hint(request_hint()));
        }
        reply => reply?,
    };
    if pending.key.code() != code {
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
    let reach = Reach::full(client).await?;
    let (answers, gaps) = reach
        .fan_out(|machine_id| set(client.clone(), machine_id))
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
    client.invoke::<T>(request, &target, None).await.map(Some)
}

/// Remove the key `code` names on one Server, Device or passkey. The Server picks the
/// kind under the admission its approvals take, so an approval since any read here is
/// still removed. A Server older than remove-trusted-key.v1 holds no passkey, so
/// set-device.v1 removes all it trusts under the code. A removed Device's passkeys are
/// read afterwards only to warn that they stay trusted.
async fn remove_on(
    client: Client,
    machine_id: MachineId,
    code: DeviceCode,
) -> Result<Option<Outcome>, RpcError> {
    let target = MachineTarget::from(&machine_id);
    let removed = if compat::supports(&client, &target, REMOVE_TRUSTED_KEY_CAPABILITY).await? {
        client
            .invoke::<op::RemoveTrustedKey>(RemoveTrustedKeyRequest { code }, &target, None)
            .await?
    } else if compat::supports(&client, &target, SET_DEVICE_CAPABILITY).await? {
        match client
            .invoke::<op::SetDevice>(SetDeviceRequest::Remove { code }, &target, None)
            .await?
        {
            DeviceSet::Removed { code } => TrustedKeyRemoved::Removed {
                code,
                kind: TrustedKeyKind::Device,
            },
            DeviceSet::NotPresent { code }
            | DeviceSet::Approved { code }
            | DeviceSet::AlreadyApproved { code } => TrustedKeyRemoved::NotPresent { code },
        }
    } else {
        return Ok(None);
    };
    let device_removed = matches!(
        removed,
        TrustedKeyRemoved::Removed {
            kind: TrustedKeyKind::Device,
            ..
        }
    );
    let approved_passkeys = if device_removed {
        client
            .invoke::<op::Inspect>(InspectRequest::default(), &target, None)
            .await
            .map(|details| details.passkeys)
            .unwrap_or_default()
            .into_iter()
            .filter(|trusted| trusted.approved_by == PasskeyApprover::Device { code })
            .map(|trusted| trusted.passkey.code())
            .collect()
    } else {
        Vec::new()
    };
    Ok(Some(Outcome {
        change: Change::Removed(removed),
        approved_passkeys,
    }))
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
        .invoke::<op::Inspect>(InspectRequest::default(), &target, None)
        .await?;
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
    use ployz_core::{DescribeContractRequest, MachineTarget, RpcError, op};

    use crate::connect::Client;

    /// Whether the Machine at `target` promises `capability`; one that doesn't is an
    /// omission, not a failure.
    pub(super) async fn supports(
        client: &Client,
        target: &MachineTarget,
        capability: &str,
    ) -> Result<bool, RpcError> {
        Ok(client
            .invoke::<op::DescribeContract>(DescribeContractRequest {}, target, None)
            .await?
            .supports(capability))
    }
}

struct Reach {
    asked: Vec<MachineId>,
    silent: Vec<MachineId>,
    entry: Option<MachineId>,
    names: BTreeMap<MachineId, MachineName>,
}

impl Reach {
    async fn full(client: &mut Client) -> Result<Self, Error> {
        let mut reach = Self::of(client).await?;
        let direct = client.full_machines();
        if client.full_single() {
            reach.asked.retain(|machine| Some(*machine) == reach.entry);
            reach.silent.retain(|machine| Some(*machine) == reach.entry);
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

    async fn of(client: &mut Client) -> Result<Self, Error> {
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
        })
    }

    async fn fan_out<T, F, Fut>(&self, call: F) -> (Vec<(MachineId, T)>, Gaps)
    where
        T: Send + 'static,
        F: Fn(MachineId) -> Fut,
        Fut: Future<Output = Result<Option<T>, RpcError>> + Send + 'static,
    {
        let entry = self.entry.filter(|id| self.asked.contains(id));
        let mut tasks = JoinSet::new();
        for &machine_id in self.asked.iter().filter(|id| Some(**id) != entry) {
            let answer = call(machine_id);
            tasks.spawn(async move { (machine_id, answer.await) });
        }
        let mut answers = Vec::new();
        let mut gaps = Gaps::default();
        gaps.omitted.extend(&self.silent);
        let mut sort = |machine_id, answer| match answer {
            Ok(Some(answer)) => answers.push((machine_id, answer)),
            Ok(None) => gaps.omitted.push(machine_id),
            Err(error) => gaps.failures.push(MachineFailure { machine_id, error }),
        };
        while let Some(joined) = tasks.join_next().await {
            let (machine_id, answer) = joined.expect("a Device fan-out task does not panic");
            sort(machine_id, answer);
        }
        if let Some(machine_id) = entry {
            sort(machine_id, call(machine_id).await);
        }
        answers
            .sort_by_key(|(machine_id, _)| self.asked.iter().position(|asked| asked == machine_id));
        (answers, gaps)
    }

    fn name(&self, machine_id: &MachineId) -> String {
        self.names
            .get(machine_id)
            .map_or_else(|| machine_id.to_string(), ToString::to_string)
    }

    fn named(&self, gaps: Gaps) -> Gaps {
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

/// What one Server changed. For a removed Device, also the passkeys it had approved
/// there, which stay trusted until removed by their own code.
#[derive(Serialize)]
struct Outcome {
    #[serde(flatten)]
    change: Change,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    approved_passkeys: Vec<DeviceCode>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(untagged)]
enum Change {
    Set(DeviceSet),
    Removed(TrustedKeyRemoved),
}

impl From<DeviceSet> for Outcome {
    fn from(set: DeviceSet) -> Self {
        Self {
            change: Change::Set(set),
            approved_passkeys: Vec::new(),
        }
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
            let result = match answer.outcome.change {
                Change::Set(DeviceSet::Approved { .. }) => "approved",
                Change::Set(DeviceSet::AlreadyApproved { .. }) => "already approved",
                Change::Set(DeviceSet::Removed { .. })
                | Change::Removed(TrustedKeyRemoved::Removed {
                    kind: TrustedKeyKind::Device,
                    ..
                }) => "removed device",
                Change::Removed(TrustedKeyRemoved::Removed {
                    kind: TrustedKeyKind::Passkey,
                    ..
                }) => "removed passkey",
                Change::Set(DeviceSet::NotPresent { .. })
                | Change::Removed(TrustedKeyRemoved::NotPresent { .. }) => "not present",
            };
            table.row([Cell::from(&answer.server), Cell::status(result, Tone::Good)]);
        }
        Some(table)
    }

    /// Warn about each passkey the removed Device `code` approved, which stays trusted.
    fn warn_approved_passkeys(&self, code: DeviceCode) {
        let mut left = BTreeMap::<DeviceCode, Vec<&str>>::new();
        for answer in &self.answers {
            for passkey in &answer.outcome.approved_passkeys {
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

    fn finish(&self) -> Result<(), Error> {
        let table = self.table();
        ui::finish_fanout("machines", &self.answers, &self.gaps, || {
            if let Some(table) = &table {
                ui::rows(table);
            }
        })
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
    Passkey { account: PasskeyAccount },
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

pub(crate) mod key {
    use std::{
        fs::{self, OpenOptions},
        io::{self, Write},
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
        path::{Path, PathBuf},
    };

    use ployz_core::DeviceKey;
    use thiserror::Error;

    #[derive(Debug, Error)]
    pub(crate) enum KeyError {
        #[error("couldn't read {path}")]
        Read { path: PathBuf, source: io::Error },
        #[error("couldn't write {path}")]
        Write { path: PathBuf, source: io::Error },
        #[error("{path} is readable by other users. Run: chmod 600 {path}")]
        Exposed { path: PathBuf },
        #[error("{path} isn't a device key. Delete it and run ployz device request")]
        Malformed { path: PathBuf },
    }

    impl From<KeyError> for crate::failure::Failure {
        fn from(error: KeyError) -> Self {
            Self::command(error)
        }
    }

    pub(crate) fn path(config: &Path) -> PathBuf {
        config.with_file_name("device.key")
    }

    pub(crate) fn public(secret: &[u8; 32]) -> DeviceKey {
        DeviceKey::from_bytes(*iroh::SecretKey::from_bytes(secret).public().as_bytes())
    }

    pub(crate) fn load(path: &Path) -> Result<Option<[u8; 32]>, KeyError> {
        let read = |source| KeyError::Read {
            path: path.to_owned(),
            source,
        };
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(read(source)),
        };
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(KeyError::Exposed {
                path: path.to_owned(),
            });
        }
        let bytes = fs::read(path).map_err(read)?;
        bytes.try_into().map(Some).map_err(|_| KeyError::Malformed {
            path: path.to_owned(),
        })
    }

    pub(crate) fn generate() -> [u8; 32] {
        iroh::SecretKey::generate().to_bytes()
    }

    /// Write `secret` to `path`, readable only by its owner. False when a key is already
    /// there, which is never replaced.
    pub(crate) fn store(path: &Path, secret: &[u8; 32]) -> Result<bool, KeyError> {
        let write = |source| KeyError::Write {
            path: path.to_owned(),
            source,
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(write)?;
        }
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
            Err(source) => return Err(write(source)),
        };
        file.write_all(secret).map_err(write)?;
        file.sync_all().map_err(write)?;
        Ok(true)
    }
}

#[cfg(test)]
#[path = "device_tests.rs"]
mod tests;
