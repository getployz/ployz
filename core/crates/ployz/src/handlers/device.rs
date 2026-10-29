//! `ployz device`: the people's keys every Machine admits at Full scope. Cloud relays a
//! Device's key to the approver, who checks it against the code the Device printed.

use std::collections::BTreeMap;
use std::path::Path;

use clap::{ArgMatches, Command};
use ployz_core::{
    DeviceCode, DeviceKey, DeviceSet, InspectRequest, MachineFailure, MachineId, MachineName,
    MachineTarget, RpcError, SetDeviceRequest, op,
};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::task::JoinSet;

use super::{Error, config_path, leaf_matches, runtime, server};
use crate::cli::{base, positional};
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
    base(command, "Approve a device on every server")
        .after_help(concat!("Examples:\n", "  ployz device approve mmg42klgyq"))
        .arg(positional("code", true).help("The code ployz device request printed"))
}

pub(crate) fn list_command(command: Command) -> Command {
    base(command, "List approved devices")
}

pub(crate) fn remove_command(command: Command) -> Command {
    base(command, "Remove a device from every server")
        .after_help(concat!("Examples:\n", "  ployz device rm mmg42klgyq"))
        .arg(positional("code", true).help("The device's code, from ployz device ls"))
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
    let code = typed_code(leaf)?;
    let store = CredentialStore::beside(&config_path(leaf)?);
    runtime()?.block_on(async {
        let credential = cloud_account::from_env(&store).await?;
        let settled = approve_with(&credential, code, async |request| {
            let mut client = server::connect_full(leaf, context(leaf)).await?;
            set_everywhere(&mut client, request).await
        })
        .await?;
        settled.finish()
    })
}

pub(crate) fn remove(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let code = typed_code(leaf)?;
    runtime()?.block_on(async {
        let mut client = server::connect_full(leaf, context(leaf)).await?;
        set_everywhere(&mut client, SetDeviceRequest::Remove { code })
            .await?
            .finish()
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
            ["DEVICE", "SERVERS", "MISSING ON"],
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
            table.row([
                Cell::from(device.code.as_str()),
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

async fn set_everywhere(client: &mut Client, request: SetDeviceRequest) -> Result<Settled, Error> {
    let reach = Reach::of(client).await?;
    let (answers, gaps) = reach
        .fan_out(|machine_id| set_on(client.clone(), machine_id, request.clone()))
        .await;
    Ok(Settled {
        answers: answers
            .into_iter()
            .map(|(machine_id, set)| Answer {
                server: reach.name(&machine_id),
                machine_id,
                set,
            })
            .collect(),
        gaps: reach.named(gaps),
    })
}

async fn set_on(
    client: Client,
    machine_id: MachineId,
    request: SetDeviceRequest,
) -> Result<Option<DeviceSet>, RpcError> {
    let target = MachineTarget::from(&machine_id);
    if !compat::sets_devices(&client, &target).await? {
        return Ok(None);
    }
    client
        .invoke::<op::SetDevice>(request, &target, None)
        .await
        .map(Some)
}

async fn devices_on(
    client: Client,
    machine_id: MachineId,
) -> Result<Option<Vec<DeviceCode>>, RpcError> {
    let target = MachineTarget::from(&machine_id);
    if !compat::sets_devices(&client, &target).await? {
        return Ok(None);
    }
    let details = client
        .invoke::<op::Inspect>(InspectRequest::default(), &target, None)
        .await?;
    Ok(Some(
        details
            .management_clients
            .iter()
            .filter_map(DeviceCode::named_by)
            .collect(),
    ))
}

pub(super) fn request_hint() -> Hint {
    Hint::Next("ployz device request".into())
}

/// Daemons from before Devices. Delete once every supported daemon sets them.
mod compat {
    use ployz_core::{DescribeContractRequest, MachineTarget, RpcError, SET_DEVICE_CAPABILITY, op};

    use crate::connect::Client;

    /// Whether the Machine at `target` keeps a Device roster; one that doesn't is an
    /// omission, not a failure.
    pub(super) async fn sets_devices(
        client: &Client,
        target: &MachineTarget,
    ) -> Result<bool, RpcError> {
        Ok(client
            .invoke::<op::DescribeContract>(DescribeContractRequest {}, target, None)
            .await?
            .supports(SET_DEVICE_CAPABILITY))
    }
}

struct Reach {
    asked: Vec<MachineId>,
    silent: Vec<MachineId>,
    entry: Option<MachineId>,
    names: BTreeMap<MachineId, MachineName>,
}

impl Reach {
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
    set: DeviceSet,
}

impl Settled {
    /// None when no Server answered, since the gap warnings name every one.
    fn table(&self) -> Option<Table> {
        if self.answers.is_empty() && !self.gaps.is_complete() {
            return None;
        }
        let mut table = Table::new(["SERVER", "RESULT"], "No servers answered.");
        for answer in &self.answers {
            let (result, tone) = match answer.set {
                DeviceSet::Approved { .. } => ("approved", Tone::Good),
                DeviceSet::AlreadyApproved { .. } => ("already approved", Tone::Good),
                DeviceSet::Removed { .. } => ("removed", Tone::Good),
                DeviceSet::NotPresent { .. } => ("not present", Tone::Good),
            };
            table.row([Cell::from(&answer.server), Cell::status(result, tone)]);
        }
        Some(table)
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
    machines: Vec<MachineId>,
    missing: Vec<MachineId>,
}

fn roster(answers: &[(MachineId, Vec<DeviceCode>)]) -> Vec<Device> {
    let mut on = BTreeMap::<DeviceCode, Vec<MachineId>>::new();
    for (machine_id, codes) in answers {
        for code in codes {
            on.entry(*code).or_default().push(*machine_id);
        }
    }
    on.into_iter()
        .map(|(code, machines)| Device {
            code,
            missing: answers
                .iter()
                .map(|(machine_id, _)| *machine_id)
                .filter(|machine_id| !machines.contains(machine_id))
                .collect(),
            machines,
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
