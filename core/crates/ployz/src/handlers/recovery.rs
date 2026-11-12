use crate::cluster::{Call, Wait};
use std::io::Read;
use std::sync::Arc;

use clap::{ArgMatches, Command};
use ployz_core::{
    DescribeContractRequest, Hint, InspectRequest, MachineLayout, RECIPIENT_RECOVERY_CAPABILITY,
    RECOVER_SEALING_KEY_CAPABILITY, RecoverSealingKeyRequest, RecoveryCode, RecoveryRefusal,
    RpcErrorCode, SealingKeyBackup, SealingKeyRole, SealingKeyWrapped, SealingPublicKey,
    WRAP_SEALING_KEY_CAPABILITY, WrapSealingKeyRequest,
    authority::{SealingRotation, Signed},
    op,
};
use serde::Deserialize;
use serde_json::json;

use super::{Error, config_path, leaf_matches, route, runtime};
use crate::cli::{base, switch};
use crate::cloud_account::{self, Credential};
use crate::cloud_enroll::RecoveryBackup;
use crate::cloud_login::LoginError;
use crate::connect::{Client, ConnectError, SystemConnector};
use crate::context::Transport;
use crate::ui::{self, Fields};

pub(crate) const RECOVERY_CODE_NOT_SAVED: &str =
    "Recovery code not saved. Run ployz recovery rotate now to make one.";
pub(crate) const KEEP_THE_CODE: &str = "Keep the recovery code safe; ployz recover needs it.";
pub(crate) const KEEP_BOTH_CODES: &str = "Keep this recovery code safe: it opens values sealed \
     after activation. Your earlier recovery code still opens values sealed before it.";
const CODE_LIMIT: usize = 256;
const NOT_AVAILABLE: &str = "Sealing Key recovery isn't available for this organization.";
const ROTATE_AGAIN: &str =
    "The old recovery code may no longer work. Run ployz recovery rotate now.";
const RECOVERY_ROW: &str = "sealing/recovery";
const ROTATE_ROW: &str = "sealing/rotate";
const RECIPIENT_ROW: &str = "sealing/recipient";
const ROTATE_RECIPIENT_ROW: &str = "sealing/recipient/rotate";
const NO_ONE_SEES_THE_CODE: &str = "No terminal to show the new recovery code; nothing was \
     rotated. Run it in a terminal, or pass --json --show-recovery-code.";

pub(super) fn group(command: Command) -> Command {
    base(command, "Back up the Sealing Key with a recovery code")
}

pub(super) fn recover_command(command: Command) -> Command {
    base(command, "Open the Sealing Key with a recovery code")
        .long_about(
            "Asks for the recovery code without echoing it, or reads it from stdin with \
             --code-stdin, and hands it to the Server's daemon. Runs on the Server or over \
             --connect; never through Cloud.",
        )
        .arg(switch("code-stdin", None).help("Read the recovery code from stdin"))
}

pub(super) fn rotate_command(command: Command) -> Command {
    base(command, "Make a new recovery code")
        .long_about(
            "Wraps the Sealing Key under a fresh code, stores that backup in Cloud, then prints \
         the code once. The old code stops working. Once the Cluster activated, it wraps the \
         recipient key that opens values sealed after activation instead, and the code from \
         before keeps opening older values. Runs on the Server or over --connect; never \
         through Cloud.",
        )
        .arg(switch("show-recovery-code", None).help("Print the recovery code under --json"))
}

/// Whether a person sees a fresh recovery code: on a terminal, or under `--json` only on request.
pub(super) fn shows_code(matches: &ArgMatches, json: bool, stdout_is_terminal: bool) -> bool {
    if json {
        matches.get_flag("show-recovery-code")
    } else {
        stdout_is_terminal
    }
}

pub(super) fn recover(root: &ArgMatches) -> Result<(), Error> {
    // Unlocked: the hidden prompt reads stdin itself, and would wait forever on a held lock.
    recover_from(root, std::io::stdin())
}

fn recover_from(root: &ArgMatches, stdin: impl Read) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let runtime = runtime()?;
    let mut client = runtime.block_on(dial(matches, "ployz recover"))?;
    runtime.block_on(require(
        &mut client,
        RECOVER_SEALING_KEY_CAPABILITY,
        "ployz recover",
    ))?;
    let code = read_code(matches, stdin)?;
    let (recovered, role) = match runtime.block_on(recover_with(&mut client, &code, None, None)) {
        Ok(recovered) => (recovered, SealingKeyRole::Identity),
        Err(refused) => {
            let why = refusal(&refused);
            if !matches!(
                why,
                Some(RecoveryRefusal::NoBackup | RecoveryRefusal::WrongCode)
            ) {
                return Err(recover_failure(refused));
            }
            // A rotation may have left Cloud a backup under a newer code than the seed's,
            // and after activation Cloud also keeps the recipient key's.
            let credential = match route::signed_in(&runtime, matches) {
                Ok(credential) => credential,
                Err(error) if why == Some(RecoveryRefusal::NoBackup) => return Err(error),
                Err(_) => None,
            };
            let Some(credential) = credential else {
                if why == Some(RecoveryRefusal::NoBackup) {
                    return Err(sign_in_for_backup());
                }
                return Err(recover_failure(refused));
            };
            runtime.block_on(recover_from_cloud(&mut client, &credential, &code, refused))?
        }
    };
    let label = match role {
        SealingKeyRole::Identity => "Sealing key",
        SealingKeyRole::Recipient => "Recipient key",
    };
    ui::fields(
        &json!({ "publicKey": recovered, "key": role }),
        &Fields::new().field(label, recovered),
    )
}

/// Try Cloud's backup of the identity key, then, while the code is still wrong, Cloud's
/// backup of the recipient key with the rotation that named it.
async fn recover_from_cloud(
    client: &mut Client,
    credential: &Credential,
    code: &RecoveryCode,
    refused: ConnectError,
) -> Result<(SealingPublicKey, SealingKeyRole), Error> {
    let organization = cloud_account::acting_in(credential).await?;
    let identity = recovery_row(credential, &organization.id)
        .await?
        .and_then(|row| row.backup);
    let recipient = recipient_row(credential, &organization.id).await?;
    if refusal(&refused) == Some(RecoveryRefusal::NoBackup)
        && identity.is_none()
        && recipient.is_none()
    {
        return Err(Error::conflict(
            "Nothing to recover from: neither this Server nor Cloud holds a backup.",
        ));
    }
    let mut last = refused;
    if let Some(backup) = identity {
        match recover_with(client, code, Some(backup), None).await {
            Ok(recovered) => return Ok((recovered, SealingKeyRole::Identity)),
            Err(error) => last = error,
        }
    }
    let wrong = matches!(
        refusal(&last),
        Some(RecoveryRefusal::WrongCode | RecoveryRefusal::NoBackup)
    );
    if let Some(row) = recipient.filter(|_| wrong) {
        require(client, RECIPIENT_RECOVERY_CAPABILITY, "ployz recover").await?;
        match recover_with(client, code, Some(row.backup), Some(row.rotation)).await {
            Ok(recovered) => return Ok((recovered, SealingKeyRole::Recipient)),
            Err(error) if refusal(&error) == Some(RecoveryRefusal::WrongCode) => {}
            Err(error) => last = error,
        }
    }
    Err(recover_failure(last))
}

fn sign_in_for_backup() -> Error {
    Error::coded(
        RpcErrorCode::Unauthenticated,
        "This Server holds no backup of the Sealing Key. Sign in so ployz recover can use Cloud's.",
    )
    .hint(Hint::Next("ployz login".to_owned()))
}

pub(super) fn rotate(root: &ArgMatches) -> Result<(), Error> {
    rotate_for(root, ui::stdout_is_terminal())
}

fn rotate_for(root: &ArgMatches, stdout_is_terminal: bool) -> Result<(), Error> {
    let matches = leaf_matches(root);
    // Rotating retires the old code, so never mint a new one no one will see.
    if !shows_code(matches, ui::json(), stdout_is_terminal) {
        return Err(Error::usage(NO_ONE_SEES_THE_CODE));
    }
    let runtime = runtime()?;
    let Some(credential) = route::signed_in(&runtime, matches)? else {
        return Err(Error::coded(
            RpcErrorCode::Unauthenticated,
            "Sign in first: ployz recovery rotate stores the new backup in Cloud.",
        )
        .hint(Hint::Next("ployz login".to_owned())));
    };
    let mut client = runtime.block_on(dial(matches, "ployz recovery rotate"))?;
    runtime.block_on(require(
        &mut client,
        WRAP_SEALING_KEY_CAPABILITY,
        "ployz recovery rotate",
    ))?;
    let config = config_path(matches)?;
    let (public_key, code, role) = runtime.block_on(async {
        let organization = cloud_account::acting_in(&credential).await?;
        let details = client
            .call::<op::Inspect>(
                InspectRequest::default(),
                Call::read().waiting(Wait::standard()),
            )
            .await?;
        if let Some(recipient) = details.sealing_recipient {
            let pinned = crate::sealing_pin::Pins::beside(&config)
                .get(&organization.id)?
                .and_then(|pin| pin.recipient);
            if let Some(pinned) = pinned.filter(|pinned| *pinned != recipient) {
                return Err(Error::conflict(format!(
                    "This Server holds recipient key {recipient}, not key {pinned} this computer pinned, so no code was made."
                )));
            }
            require(
                &mut client,
                RECIPIENT_RECOVERY_CAPABILITY,
                "ployz recovery rotate",
            )
            .await?;
            let code = RecoveryCode::generate();
            let wrapped = wrap(&mut client, &code, SealingKeyRole::Recipient).await?;
            let rotation = wrapped
                .rotation
                .filter(|_| wrapped.public_key == recipient)
                .ok_or_else(|| {
                    Error::conflict(
                        "This Server wrapped a key other than its recipient key; nothing was rotated.",
                    )
                })?;
            store_recipient_backup(
                &credential,
                &organization.id,
                &RecoveryBackup {
                    public_key: recipient,
                    backup: wrapped.backup,
                },
                &rotation,
            )
            .await?;
            return Ok((recipient, code, SealingKeyRole::Recipient));
        }
        let row = recovery_row(&credential, &organization.id)
            .await?
            .ok_or_else(|| Error::unavailable(NOT_AVAILABLE))?;
        let code = RecoveryCode::generate();
        let wrapped = wrap_sealing_key(&mut client, &code).await?;
        if wrapped.public_key != row.public_key {
            return Err(Error::conflict(
                "This Server holds a Sealing Key other than the one Cloud backs up; nothing was rotated.",
            ));
        }
        let rotated: Rotated = cloud_account::call(
            &credential,
            reqwest::Method::POST,
            ROTATE_ROW,
            Some(&json!({
                "organizationId": organization.id,
                "publicKey": wrapped.public_key,
                "currentDigest": row.digest,
                "backup": wrapped.backup,
            })),
        )
        .await
        .map_err(rotate_failure)?;
        if rotated.digest != wrapped.digest() {
            return Err(Error::unavailable(format!(
                "Cloud stored a backup other than the one this Server wrapped. {ROTATE_AGAIN}"
            )));
        }
        Ok((wrapped.public_key, code, SealingKeyRole::Identity))
    })?;
    print_code(
        public_key,
        &code,
        match role {
            SealingKeyRole::Identity => KEEP_THE_CODE,
            SealingKeyRole::Recipient => KEEP_BOTH_CODES,
        },
    )
}

/// Store `wrapped`, the recipient key under a fresh code, as Cloud's backup of it, beside the
/// signed rotation a recovering Server verifies.
///
/// # Errors
/// Fails when Cloud refuses, another rotation reached it first, or it stored other bytes.
pub(crate) async fn store_recipient_backup(
    credential: &Credential,
    organization_id: &str,
    wrapped: &RecoveryBackup,
    rotation: &Signed<SealingRotation>,
) -> Result<(), Error> {
    let current = recipient_row(credential, organization_id)
        .await?
        .map(|row| row.digest);
    let rotated: Rotated = cloud_account::call(
        credential,
        reqwest::Method::POST,
        ROTATE_RECIPIENT_ROW,
        Some(&json!({
            "organizationId": organization_id,
            "publicKey": wrapped.public_key,
            "rotation": rotation,
            "currentDigest": current,
            "backup": wrapped.backup,
        })),
    )
    .await
    .map_err(rotate_failure)?;
    if rotated.digest != wrapped.digest() {
        return Err(Error::unavailable(format!(
            "Cloud stored a backup other than the one this Server wrapped. {ROTATE_AGAIN}"
        )));
    }
    Ok(())
}

pub(super) async fn wrap_sealing_key(
    client: &mut Client,
    code: &RecoveryCode,
) -> Result<RecoveryBackup, Error> {
    let wrapped = wrap(client, code, SealingKeyRole::Identity).await?;
    Ok(RecoveryBackup {
        public_key: wrapped.public_key,
        backup: wrapped.backup,
    })
}

async fn wrap(
    client: &mut Client,
    code: &RecoveryCode,
    key: SealingKeyRole,
) -> Result<SealingKeyWrapped, Error> {
    Ok(client
        .call::<op::WrapSealingKey>(
            WrapSealingKeyRequest {
                code: code.clone(),
                key,
            },
            Call::keyed().waiting(Wait::standard()),
        )
        .await?)
}

pub(crate) fn print_code(
    public_key: SealingPublicKey,
    code: &RecoveryCode,
    keep: &str,
) -> Result<(), Error> {
    let code = code.reveal();
    if ui::json() {
        return ui::emit(&json!({ "publicKey": public_key, "recoveryCode": code.as_str() }))
            .map_err(|error| error.context(RECOVERY_CODE_NOT_SAVED));
    }
    ui::write_fields(&Fields::new().field("Recovery code", code.as_str())).map_err(|error| {
        Error::caused(RpcErrorCode::Unavailable, RECOVERY_CODE_NOT_SAVED, error)
    })?;
    ui::note(keep);
    Ok(())
}

pub(super) async fn dial(matches: &ArgMatches, verb: &str) -> Result<Client, Error> {
    let selected = crate::connect::resolve_connections(
        &config_path(matches)?,
        matches.get_one::<String>("connect").map(String::as_str),
        matches.get_one::<String>("context").map(String::as_str),
        &MachineLayout::production().socket(),
    )?;
    if selected
        .connections
        .iter()
        .any(|connection| matches!(connection.transport(), Transport::Management(_)))
    {
        return Err(cloud_routed(verb));
    }
    let connector = SystemConnector::default().with_ssh_timeout(crate::cli::ssh_timeout(matches));
    Ok(crate::connect::connect_selected_in_order(selected, Arc::new(connector)).await?)
}

/// Refuses a Server whose daemon predates `capability`, naming the Server.
async fn require(client: &mut Client, capability: &str, verb: &str) -> Result<(), Error> {
    let contract = client
        .call::<op::DescribeContract>(
            DescribeContractRequest {},
            Call::read().waiting(Wait::standard()),
        )
        .await?;
    if contract.supports(capability) {
        return Ok(());
    }
    Err(Error::detailed(
        RpcErrorCode::Unsupported,
        format!(
            "Server {} needs a newer daemon for {verb}. Update ployzd there, then run it again.",
            contract.machine_id
        ),
        json!({ "capability": capability, "target": contract.machine_id }),
    ))
}

fn cloud_routed(verb: &str) -> Error {
    Error::usage(format!(
        "{verb} doesn't run through Cloud. Run it on the Server, or pass --connect ssh://user@host."
    ))
}

fn read_code(matches: &ArgMatches, stdin: impl Read) -> Result<RecoveryCode, Error> {
    let raw = if matches.get_flag("code-stdin") {
        let bytes = super::read_bounded(stdin, CODE_LIMIT)
            .map_err(|error| {
                Error::caused(
                    RpcErrorCode::InvalidArgument,
                    "Could not read the recovery code from stdin",
                    error,
                )
            })?
            .ok_or_else(|| {
                Error::usage(format!(
                    "stdin holds more than {CODE_LIMIT} bytes; a recovery code is 26 letters and digits"
                ))
            })?;
        String::from_utf8(bytes)
            .map_err(|_| Error::usage("The recovery code on stdin isn't UTF-8"))?
    } else {
        ui::secret(
            "Recovery code",
            || {
                Error::coded(
                    RpcErrorCode::Unauthenticated,
                    "No terminal to ask for the recovery code. Pipe it with --code-stdin.",
                )
            },
            "Cancelled. Nothing was recovered.",
        )?
    };
    RecoveryCode::parse(&raw).map_err(|error| {
        Error::caused(
            RpcErrorCode::InvalidArgument,
            "That isn't a recovery code.",
            error,
        )
    })
}

async fn recover_with(
    client: &mut Client,
    code: &RecoveryCode,
    backup: Option<SealingKeyBackup>,
    rotation: Option<Signed<SealingRotation>>,
) -> Result<SealingPublicKey, ConnectError> {
    let recovered = client
        .call::<op::RecoverSealingKey>(
            RecoverSealingKeyRequest {
                code: code.clone(),
                backup,
                rotation,
            },
            Call::keyed().waiting(Wait::standard()),
        )
        .await?;
    Ok(recovered.public_key)
}

/// The daemon answers a refusal as its own error, so it arrives as [`ConnectError::Remote`].
fn refusal(error: &ConnectError) -> Option<RecoveryRefusal> {
    let ConnectError::Remote(answered) = error else {
        return None;
    };
    answered.evidence::<RecoveryRefusal>()
}

fn recover_failure(error: ConnectError) -> Error {
    let Some(refusal) = refusal(&error) else {
        return error.into();
    };
    match refusal {
        RecoveryRefusal::WrongCode => Error::coded(
            RpcErrorCode::InvalidArgument,
            "That recovery code is wrong.",
        )
        .hint(Hint::Retry(
            "Check the code and run ployz recover again.".to_owned(),
        )),
        RecoveryRefusal::OtherKey => Error::coded(
            RpcErrorCode::InvalidArgument,
            "That recovery code opens another Sealing Key, not the one this Cluster awaits.",
        ),
        RecoveryRefusal::HeldAnother => Error::conflict(
            "This Cluster already holds another Sealing Key; nothing was recovered.",
        ),
        RecoveryRefusal::NothingAwaited | RecoveryRefusal::NotHeld => {
            Error::conflict("Nothing to recover: this Cluster isn't awaiting its Sealing Key.")
        }
        RecoveryRefusal::NoBackup => Error::conflict(
            "Nothing to recover from: this Server holds no backup of the Sealing Key.",
        ),
        RecoveryRefusal::CloudRouted => cloud_routed("ployz recover"),
        RecoveryRefusal::Malformed => Error::coded(
            RpcErrorCode::InvalidArgument,
            "The Server refused the recovery code as malformed.",
        ),
        RecoveryRefusal::NoRecipient => {
            Error::conflict("This Server holds no recipient key from activation.")
        }
        RecoveryRefusal::RotationRefused => Error::conflict(
            "This Server doesn't trust the Device or passkey that named this recipient key, or it activated under another one, so nothing was recovered.",
        ),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RecoveryRow {
    pub(super) public_key: SealingPublicKey,
    backup: Option<SealingKeyBackup>,
    digest: Option<String>,
}

#[derive(Deserialize)]
struct Rotated {
    digest: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecipientRow {
    backup: SealingKeyBackup,
    digest: String,
    rotation: Signed<SealingRotation>,
}

async fn recipient_row(
    credential: &Credential,
    organization_id: &str,
) -> Result<Option<RecipientRow>, Error> {
    match cloud_account::call::<RecipientRow>(
        credential,
        reqwest::Method::POST,
        RECIPIENT_ROW,
        Some(&json!({ "organizationId": organization_id })),
    )
    .await
    {
        Ok(row) => Ok(Some(row)),
        Err(LoginError::Status { status: 404, .. }) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) async fn recovery_row(
    credential: &Credential,
    organization_id: &str,
) -> Result<Option<RecoveryRow>, Error> {
    match cloud_account::call::<RecoveryRow>(
        credential,
        reqwest::Method::POST,
        RECOVERY_ROW,
        Some(&json!({ "organizationId": organization_id })),
    )
    .await
    {
        Ok(row) => Ok(Some(row)),
        Err(LoginError::Status { status: 404, .. }) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn rotate_failure(error: LoginError) -> Error {
    if let LoginError::Status { status: 404, .. } = error {
        return Error::unavailable(NOT_AVAILABLE);
    }
    if let LoginError::Status { status: 409, .. } = error {
        return Error::conflict(format!(
            "Another rotation reached Cloud first. {ROTATE_AGAIN}"
        ));
    }
    // A 5xx may come after Cloud stored the backup, so it confirms nothing.
    if matches!(
        error,
        LoginError::Unreachable { .. }
            | LoginError::Unanswered { .. }
            | LoginError::Reply(_)
            | LoginError::Status {
                status: 500..=599,
                ..
            }
    ) {
        return Error::unavailable(format!(
            "Cloud didn't confirm the new backup. {ROTATE_AGAIN}"
        ));
    }
    error.into()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use ployz_core::{
        CapabilityName, ContractDescription, Evidence, MachineId, RpcRequestBody, RpcResponse,
        SealingKeyRecovered, SealingKeyWrapped,
    };
    use serde_json::Value;

    use super::*;
    use crate::cloud_login::tests::fake_cloud;
    use crate::connect::test_support::unix_daemon_describing;
    use crate::handlers::verbs;

    const CODE: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    const RECIPIENT_CODE: &str = "BCDEFGHIJKLMNOPQRSTUVWXYZA";
    const ROTATE: &[&str] = &["recovery", "rotate", "--show-recovery-code"];
    const CAPABILITY_LEN: usize = 86;

    fn key(byte: u8) -> SealingPublicKey {
        SealingPublicKey::from_bytes([byte; 32])
    }

    fn wrapped_backup() -> SealingKeyBackup {
        SealingKeyBackup::from_bytes(b"wrapped under the new code".to_vec())
    }

    fn cloud_backup_bytes() -> SealingKeyBackup {
        SealingKeyBackup::from_bytes(b"kept by cloud".to_vec())
    }

    struct Cloud {
        url: String,
        requests: Arc<Mutex<Vec<(String, String)>>>,
    }

    /// A Cloud that keeps no recipient backup, as before any activation.
    fn cloud(reply: impl Fn(&str, &Value) -> (u16, Value) + Send + 'static) -> Cloud {
        recipient_cloud(move |route, body| match route {
            "POST /api/cli/sealing/recipient" => (404, json!({ "error": "not found" })),
            _ => reply(route, body),
        })
    }

    fn recipient_cloud(reply: impl Fn(&str, &Value) -> (u16, Value) + Send + 'static) -> Cloud {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        let url = fake_cloud(move |route, body| {
            seen.lock()
                .unwrap()
                .push((route.to_owned(), body.to_owned()));
            let body: Value = serde_json::from_str(body).unwrap();
            reply(route, &body)
        });
        Cloud { url, requests }
    }

    impl Cloud {
        fn routes(&self) -> Vec<String> {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .map(|(route, _)| route.clone())
                .collect()
        }

        fn body_of(&self, route: &str) -> Value {
            let requests = self.requests.lock().unwrap();
            let (_, body) = requests
                .iter()
                .find(|(seen, _)| seen == route)
                .unwrap_or_else(|| panic!("no request to {route}"));
            serde_json::from_str(body).unwrap()
        }

        fn never_saw(&self, secret: &str) {
            for (route, body) in self.requests.lock().unwrap().iter() {
                assert!(!body.contains(secret), "{route} carried the code: {body}");
            }
        }
    }

    fn recovery_row(public_key: SealingPublicKey, backup: Option<SealingKeyBackup>) -> Value {
        json!({ "publicKey": public_key, "backup": backup, "digest": "current-digest" })
    }

    fn digest_of(rotate_body: &Value) -> String {
        let posted = RecoveryBackup {
            public_key: serde_json::from_value(rotate_body.get("publicKey").unwrap().clone())
                .unwrap(),
            backup: serde_json::from_value(rotate_body.get("backup").unwrap().clone()).unwrap(),
        };
        posted.digest()
    }

    struct Daemon {
        socket: PathBuf,
        machine_id: MachineId,
        requests: Arc<AtomicUsize>,
        recoveries: Arc<Mutex<Vec<Option<SealingKeyBackup>>>>,
        _runtime: tokio::runtime::Runtime,
    }

    fn daemon(dir: &Path, held: SealingPublicKey, seed: bool) -> Daemon {
        seeded_under(dir, held, seed.then_some(CODE))
    }

    /// A Server whose seed opens under `seed`, and any sent backup under [`CODE`].
    fn seeded_under(dir: &Path, held: SealingPublicKey, seed: Option<&'static str>) -> Daemon {
        advertising(
            dir,
            held,
            seed,
            &[WRAP_SEALING_KEY_CAPABILITY, RECOVER_SEALING_KEY_CAPABILITY],
        )
    }

    /// [`seeded_under`] advertising only `capabilities`, and answering the rest
    /// `unimplemented` as a daemon that predates them does.
    fn advertising(
        dir: &Path,
        held: SealingPublicKey,
        seed: Option<&'static str>,
        capabilities: &'static [&'static str],
    ) -> Daemon {
        serving(dir, held, seed, capabilities, None)
    }

    /// An activated Server holding `recipient` under `rotation`, whose recipient backups open
    /// under [`RECIPIENT_CODE`].
    fn activated(dir: &Path, held: SealingPublicKey, recipient: SealingPublicKey) -> Daemon {
        serving(
            dir,
            held,
            None,
            &[
                WRAP_SEALING_KEY_CAPABILITY,
                RECOVER_SEALING_KEY_CAPABILITY,
                RECIPIENT_RECOVERY_CAPABILITY,
            ],
            Some(recipient),
        )
    }

    fn rotation(recipient: SealingPublicKey) -> Signed<SealingRotation> {
        let signer = ployz_core::authority::Signer::from_seed(&[7; 32]);
        Signed::sign(
            &SealingRotation {
                cluster: key(1),
                operation: ployz_core::OperationId::random(),
                recipient,
                members: Vec::new(),
                signer: ployz_core::authority::SignerKey::Device(signer.public().into()),
            },
            &signer,
        )
        .unwrap()
    }

    fn serving(
        dir: &Path,
        held: SealingPublicKey,
        seed: Option<&'static str>,
        capabilities: &'static [&'static str],
        recipient: Option<SealingPublicKey>,
    ) -> Daemon {
        let socket = dir.join("ployzd.sock");
        let contract = ContractDescription {
            attest: None,
            machine_id: MachineId::random(),
            protocol_major: ployz_core::PROTOCOL_MAJOR,
            daemon_version: "fixture".into(),
            capabilities: capabilities
                .iter()
                .map(|capability| CapabilityName::parse(*capability).unwrap())
                .collect(),
            sealing: None,
        };
        let machine_id = contract.machine_id;
        let requests = Arc::new(AtomicUsize::new(0));
        let counted = requests.clone();
        let recoveries = Arc::new(Mutex::new(Vec::new()));
        let seen = recoveries.clone();
        let runtime = unix_daemon_describing(&socket, contract, move |body| {
            counted.fetch_add(1, Ordering::SeqCst);
            let seen = seen.clone();
            async move {
                if let RpcRequestBody::Inspect(_) = body {
                    return Ok(RpcResponse::from(inspected(machine_id, held, recipient)));
                }
                if let RpcRequestBody::WrapSealingKey(request) = body {
                    if !capabilities.contains(&WRAP_SEALING_KEY_CAPABILITY) {
                        return Err(tonic::Status::unimplemented(WRAP_SEALING_KEY_CAPABILITY));
                    }
                    return Ok(RpcResponse::from(match (request.key, recipient) {
                        (SealingKeyRole::Recipient, Some(recipient)) => SealingKeyWrapped {
                            public_key: recipient,
                            backup: wrapped_backup(),
                            rotation: Some(rotation(recipient)),
                        },
                        _ => SealingKeyWrapped {
                            public_key: held,
                            backup: wrapped_backup(),
                            rotation: None,
                        },
                    }));
                }
                let RpcRequestBody::RecoverSealingKey(request) = body else {
                    panic!("unexpected request: {body:?}");
                };
                if !capabilities.contains(&RECOVER_SEALING_KEY_CAPABILITY) {
                    return Err(tonic::Status::unimplemented(RECOVER_SEALING_KEY_CAPABILITY));
                }
                seen.lock().unwrap().push(request.backup.clone());
                if let Some(rotation) = &request.rotation {
                    if request.code.as_bytes() != RECIPIENT_CODE.as_bytes() {
                        return Ok(refused(RecoveryRefusal::WrongCode));
                    }
                    let named = rotation.open().unwrap().recipient;
                    return Ok(RpcResponse::from(SealingKeyRecovered { public_key: named }));
                }
                let opens = match (&request.backup, seed) {
                    (Some(_), _) => CODE,
                    (None, Some(seed)) => seed,
                    (None, None) => return Ok(refused(RecoveryRefusal::NoBackup)),
                };
                if request.code.as_bytes() != opens.as_bytes() {
                    return Ok(refused(RecoveryRefusal::WrongCode));
                }
                Ok(RpcResponse::from(SealingKeyRecovered { public_key: held }))
            }
        });
        Daemon {
            socket,
            machine_id,
            requests,
            recoveries,
            _runtime: runtime,
        }
    }

    fn inspected(
        id: MachineId,
        held: SealingPublicKey,
        recipient: Option<SealingPublicKey>,
    ) -> ployz_core::MachineDetails {
        ployz_core::MachineDetails {
            activation: None,
            sealing_recipient: recipient,
            id,
            public_key: ployz_core::WireGuardPublicKey([3; 32]),
            machine: None,
            phase: ployz_core::LocalMachinePhase::Participating,
            advertised_endpoints: Vec::new(),
            store_version: Default::default(),
            rtts: Vec::new(),
            management_clients: Vec::new(),
            passkeys: Vec::new(),
            storage: None,
            upgrade: None,
            telemetry: None,
            token: None,
            sealing: Some(ployz_core::Sealing::Held { public_key: held }),
            founding: Default::default(),
        }
    }

    /// Refused the way ployzd's `recovery_error` answers: an error payload, not a gRPC status.
    fn refused(refusal: RecoveryRefusal) -> RpcResponse {
        RpcResponse::from(ployz_core::RpcError {
            details: refusal.to_details(),
            ..ployz_core::RpcError::caused(
                RpcErrorCode::InvalidArgument,
                &std::io::Error::other("refused"),
            )
        })
    }

    fn config_dir(cloud: Option<&str>, contexts: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = dir.path().join("config.yaml");
        std::fs::write(&config, contexts).unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
        if let Some(cloud) = cloud {
            let stored = json!({
                "state": "signed_in",
                "cloud": cloud,
                "token": "session-token",
                "account": { "id": "u1", "email": "dev@example.test", "name": "Dev" },
                "organization": { "id": "o1", "slug": "acme" },
            });
            std::fs::write(dir.path().join("cloud.json"), stored.to_string()).unwrap();
        }
        dir
    }

    fn root(dir: &Path, connect: Option<&Path>, tail: &[&str]) -> ArgMatches {
        let config = dir.join("config.yaml");
        let mut arguments = vec!["ployz", "--ployz-config", config.to_str().unwrap()];
        let socket;
        if let Some(connect) = connect {
            socket = format!("unix://{}", connect.display());
            arguments.push("--connect");
            arguments.push(&socket);
        }
        arguments.extend(tail);
        crate::cli::command()
            .try_get_matches_from(arguments)
            .unwrap()
    }

    fn rotate_json(root: &ArgMatches) -> (Result<(), Error>, Option<Value>) {
        let _mode = ui::test_mode(true);
        ui::captured(|| rotate(root))
    }

    fn recover_json(root: &ArgMatches, stdin: &str) -> (Result<(), Error>, Option<Value>) {
        let _mode = ui::test_mode(true);
        ui::captured(|| recover_from(root, Cursor::new(stdin.to_owned())))
    }

    fn dashless(code: &str) -> String {
        code.chars().filter(|symbol| *symbol != '-').collect()
    }

    #[test]
    fn rotate_prints_one_code_once_cloud_confirms_the_digest() {
        let cloud = cloud(|route, body| match route {
            "POST /api/cli/sealing/recovery" => (200, recovery_row(key(1), None)),
            "POST /api/cli/sealing/rotate" => (200, json!({ "digest": digest_of(body) })),
            other => panic!("unexpected {other}"),
        });
        let dir = config_dir(Some(&cloud.url), "");
        let daemon = daemon(dir.path(), key(1), true);
        let root = root(dir.path(), Some(&daemon.socket), ROTATE);

        let (result, printed) = rotate_json(&root);

        result.unwrap();
        let printed = printed.unwrap();
        let code = dashless(printed.get("recoveryCode").unwrap().as_str().unwrap());
        RecoveryCode::parse(&code).unwrap();
        assert_eq!(printed.get("publicKey"), Some(&json!(key(1))));
        let rotate = cloud.body_of("POST /api/cli/sealing/rotate");
        assert_eq!(rotate.get("organizationId"), Some(&json!("o1")));
        assert_eq!(rotate.get("currentDigest"), Some(&json!("current-digest")));
        assert_eq!(rotate.get("publicKey"), Some(&json!(key(1))));
        assert_eq!(rotate.get("backup"), Some(&json!(wrapped_backup())));
        cloud.never_saw(&code);
    }

    #[test]
    fn rotate_refuses_before_any_call_when_no_one_would_see_the_code() {
        // Off a terminal the opt-in is not enough, and on one plain --json is not.
        for (json, terminal, tail) in [
            (false, false, ROTATE),
            (true, true, &["recovery", "rotate"][..]),
        ] {
            let cloud = cloud(|route, _| panic!("unexpected {route}"));
            let dir = config_dir(Some(&cloud.url), "");
            let daemon = daemon(dir.path(), key(1), true);
            let root = root(dir.path(), Some(&daemon.socket), tail);
            let _mode = ui::test_mode(json);

            let (result, printed) = ui::captured(|| rotate_for(&root, terminal));

            let error = result.unwrap_err();
            assert!(printed.is_none());
            assert_eq!(error.report().code, RpcErrorCode::InvalidArgument);
            assert!(
                error.to_string().contains("--json --show-recovery-code"),
                "{error}"
            );
            assert!(cloud.routes().is_empty());
            assert_eq!(daemon.requests.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn rotate_prints_no_code_when_another_rotation_won() {
        let cloud = cloud(|route, _| match route {
            "POST /api/cli/sealing/recovery" => (200, recovery_row(key(1), None)),
            "POST /api/cli/sealing/rotate" => (409, json!({ "error": "stale" })),
            other => panic!("unexpected {other}"),
        });
        let dir = config_dir(Some(&cloud.url), "");
        let daemon = daemon(dir.path(), key(1), true);
        let root = root(dir.path(), Some(&daemon.socket), ROTATE);

        let (result, printed) = rotate_json(&root);

        let error = result.unwrap_err();
        assert!(printed.is_none());
        assert_eq!(error.report().code, RpcErrorCode::Conflict);
        assert!(error.to_string().contains(ROTATE_AGAIN), "{error}");
    }

    #[test]
    fn rotate_prints_no_code_when_cloud_lost_the_reply() {
        for reply in [
            (200, json!({ "stored": true })),
            (500, json!({ "error": "internal" })),
            (503, json!({ "error": "unavailable" })),
        ] {
            let cloud = cloud(move |route, _| match route {
                "POST /api/cli/sealing/recovery" => (200, recovery_row(key(1), None)),
                "POST /api/cli/sealing/rotate" => reply.clone(),
                other => panic!("unexpected {other}"),
            });
            let dir = config_dir(Some(&cloud.url), "");
            let daemon = daemon(dir.path(), key(1), true);
            let root = root(dir.path(), Some(&daemon.socket), ROTATE);

            let (result, printed) = rotate_json(&root);

            let error = result.unwrap_err();
            assert!(printed.is_none());
            assert_eq!(error.report().code, RpcErrorCode::Unavailable);
            assert!(error.to_string().contains("didn't confirm"), "{error}");
            assert!(error.to_string().contains(ROTATE_AGAIN), "{error}");
        }
    }

    #[test]
    fn rotate_prints_no_code_when_cloud_stored_another_backup() {
        let cloud = cloud(|route, _| match route {
            "POST /api/cli/sealing/recovery" => (200, recovery_row(key(1), None)),
            "POST /api/cli/sealing/rotate" => (200, json!({ "digest": "someone else's" })),
            other => panic!("unexpected {other}"),
        });
        let dir = config_dir(Some(&cloud.url), "");
        let daemon = daemon(dir.path(), key(1), true);
        let root = root(dir.path(), Some(&daemon.socket), ROTATE);

        let (result, printed) = rotate_json(&root);

        let error = result.unwrap_err();
        assert!(printed.is_none());
        assert_eq!(error.report().code, RpcErrorCode::Unavailable);
        assert!(error.to_string().contains(ROTATE_AGAIN), "{error}");
    }

    #[test]
    fn rotate_refuses_a_server_holding_another_key_than_cloud_backs_up() {
        let cloud = cloud(|route, _| match route {
            "POST /api/cli/sealing/recovery" => (200, recovery_row(key(2), None)),
            other => panic!("unexpected {other}"),
        });
        let dir = config_dir(Some(&cloud.url), "");
        let daemon = daemon(dir.path(), key(1), true);
        let root = root(dir.path(), Some(&daemon.socket), ROTATE);

        let (result, printed) = rotate_json(&root);

        let error = result.unwrap_err();
        assert!(printed.is_none());
        assert_eq!(error.report().code, RpcErrorCode::Conflict);
        assert!(error.to_string().contains("nothing was rotated"), "{error}");
        assert_eq!(cloud.routes(), ["POST /api/cli/sealing/recovery"]);
    }

    #[test]
    fn rotate_is_not_available_while_the_route_is_dark() {
        let cloud = cloud(|_, _| (404, json!({ "error": "not found" })));
        let dir = config_dir(Some(&cloud.url), "");
        let daemon = daemon(dir.path(), key(1), true);
        let root = root(dir.path(), Some(&daemon.socket), ROTATE);

        let (result, printed) = rotate_json(&root);

        let error = result.unwrap_err();
        assert!(printed.is_none());
        assert_eq!(error.report().code, RpcErrorCode::Unavailable);
        assert!(error.to_string().contains("isn't available"), "{error}");
    }

    #[test]
    fn rotate_names_a_server_whose_daemon_cannot_wrap() {
        let cloud = cloud(|route, _| panic!("unexpected {route}"));
        let dir = config_dir(Some(&cloud.url), "");
        let daemon = advertising(
            dir.path(),
            key(1),
            Some(CODE),
            &[RECOVER_SEALING_KEY_CAPABILITY],
        );
        let root = root(dir.path(), Some(&daemon.socket), ROTATE);

        let (result, printed) = rotate_json(&root);

        let error = result.unwrap_err();
        assert!(printed.is_none());
        assert_eq!(error.report().code, RpcErrorCode::Unsupported, "{error}");
        assert_eq!(
            error.to_string(),
            format!(
                "Server {} needs a newer daemon for ployz recovery rotate. Update ployzd there, then run it again.",
                daemon.machine_id
            )
        );
        assert!(cloud.routes().is_empty());
        assert_eq!(daemon.requests.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn rotate_needs_a_sign_in() {
        let dir = config_dir(None, "");
        let socket = dir.path().join("absent.sock");
        let root = root(dir.path(), Some(&socket), ROTATE);

        let (result, printed) = rotate_json(&root);

        let error = result.unwrap_err();
        assert!(printed.is_none());
        assert_eq!(error.report().code, RpcErrorCode::Unauthenticated);
        assert!(
            error
                .hints()
                .contains(&Hint::Next("ployz login".to_owned()))
        );
    }

    #[test]
    fn recover_opens_the_backup_the_server_kept() {
        let dir = config_dir(None, "");
        let daemon = daemon(dir.path(), key(1), true);
        let root = root(
            dir.path(),
            Some(&daemon.socket),
            &["recover", "--code-stdin"],
        );

        let (result, printed) = recover_json(&root, "abcde-fghij-klmno-pqrst-uvwxyz\n");

        result.unwrap();
        assert_eq!(
            printed.unwrap(),
            json!({ "publicKey": key(1), "key": "identity" })
        );
        assert_eq!(*daemon.recoveries.lock().unwrap(), [None]);
    }

    #[test]
    fn recover_from_the_seed_needs_no_usable_cloud_sign_in() {
        for (code, opened) in [(CODE, true), ("ZYXWVUTSRQPONMLKJIHGFEDCBA", false)] {
            let dir = config_dir(None, "");
            std::fs::write(dir.path().join("cloud.json"), "not a sign-in").unwrap();
            let daemon = daemon(dir.path(), key(1), true);
            let root = root(
                dir.path(),
                Some(&daemon.socket),
                &["recover", "--code-stdin"],
            );

            let (result, printed) = recover_json(&root, code);

            if opened {
                result.unwrap();
                assert_eq!(
                    printed.unwrap(),
                    json!({ "publicKey": key(1), "key": "identity" })
                );
            } else {
                assert_eq!(
                    result.unwrap_err().to_string(),
                    "That recovery code is wrong."
                );
            }
            assert_eq!(*daemon.recoveries.lock().unwrap(), [None]);
        }
    }

    #[test]
    fn recover_retries_with_clouds_backup_when_the_server_has_none() {
        let cloud = cloud(|route, _| match route {
            "POST /api/cli/sealing/recovery" => {
                (200, recovery_row(key(1), Some(cloud_backup_bytes())))
            }
            other => panic!("unexpected {other}"),
        });
        let dir = config_dir(Some(&cloud.url), "");
        let daemon = daemon(dir.path(), key(1), false);
        let root = root(
            dir.path(),
            Some(&daemon.socket),
            &["recover", "--code-stdin"],
        );

        let (result, printed) = recover_json(&root, CODE);

        result.unwrap();
        assert_eq!(
            printed.unwrap(),
            json!({ "publicKey": key(1), "key": "identity" })
        );
        assert_eq!(
            *daemon.recoveries.lock().unwrap(),
            [None, Some(cloud_backup_bytes())]
        );
        assert_eq!(
            cloud
                .body_of("POST /api/cli/sealing/recovery")
                .get("organizationId"),
            Some(&json!("o1"))
        );
        cloud.never_saw(CODE);
    }

    #[test]
    fn recover_opens_clouds_backup_when_the_code_was_rotated_past_the_seed() {
        let cloud = cloud(|route, _| match route {
            "POST /api/cli/sealing/recovery" => {
                (200, recovery_row(key(1), Some(cloud_backup_bytes())))
            }
            other => panic!("unexpected {other}"),
        });
        let dir = config_dir(Some(&cloud.url), "");
        let daemon = seeded_under(dir.path(), key(1), Some("ZYXWVUTSRQPONMLKJIHGFEDCBA"));
        let root = root(
            dir.path(),
            Some(&daemon.socket),
            &["recover", "--code-stdin"],
        );

        let (result, printed) = recover_json(&root, CODE);

        result.unwrap();
        assert_eq!(
            printed.unwrap(),
            json!({ "publicKey": key(1), "key": "identity" })
        );
        assert_eq!(
            *daemon.recoveries.lock().unwrap(),
            [None, Some(cloud_backup_bytes())]
        );
        cloud.never_saw(CODE);
    }

    #[test]
    fn recover_names_a_wrong_code_that_opens_neither_backup() {
        let cloud = cloud(|route, _| match route {
            "POST /api/cli/sealing/recovery" => {
                (200, recovery_row(key(1), Some(cloud_backup_bytes())))
            }
            other => panic!("unexpected {other}"),
        });
        let dir = config_dir(Some(&cloud.url), "");
        let daemon = daemon(dir.path(), key(1), true);
        let root = root(
            dir.path(),
            Some(&daemon.socket),
            &["recover", "--code-stdin"],
        );
        let wrong = "ZYXWVUTSRQPONMLKJIHGFEDCBA";

        let (result, printed) = recover_json(&root, wrong);

        let error = result.unwrap_err();
        assert!(printed.is_none());
        assert_eq!(error.to_string(), "That recovery code is wrong.");
        assert_eq!(
            *daemon.recoveries.lock().unwrap(),
            [None, Some(cloud_backup_bytes())]
        );
        cloud.never_saw(wrong);
    }

    #[test]
    fn recover_has_nothing_to_open_when_neither_side_kept_a_backup() {
        for reply in [(200, recovery_row(key(1), None)), (404, json!({}))] {
            let cloud = cloud(move |_, _| reply.clone());
            let dir = config_dir(Some(&cloud.url), "");
            let daemon = daemon(dir.path(), key(1), false);
            let root = root(
                dir.path(),
                Some(&daemon.socket),
                &["recover", "--code-stdin"],
            );

            let (result, printed) = recover_json(&root, CODE);

            let error = result.unwrap_err();
            assert!(printed.is_none());
            assert_eq!(error.report().code, RpcErrorCode::Conflict);
            assert!(
                error.to_string().contains("Nothing to recover from"),
                "{error}"
            );
        }
    }

    #[test]
    fn recover_asks_for_a_sign_in_before_it_can_use_clouds_backup() {
        let dir = config_dir(None, "");
        let daemon = daemon(dir.path(), key(1), false);
        let root = root(
            dir.path(),
            Some(&daemon.socket),
            &["recover", "--code-stdin"],
        );

        let (result, _) = recover_json(&root, CODE);

        let error = result.unwrap_err();
        assert_eq!(error.report().code, RpcErrorCode::Unauthenticated);
        assert!(
            error
                .hints()
                .contains(&Hint::Next("ployz login".to_owned()))
        );
    }

    #[test]
    fn recover_names_a_server_whose_daemon_cannot_recover() {
        let dir = config_dir(None, "");
        let daemon = advertising(
            dir.path(),
            key(1),
            Some(CODE),
            &[WRAP_SEALING_KEY_CAPABILITY],
        );
        let root = root(
            dir.path(),
            Some(&daemon.socket),
            &["recover", "--code-stdin"],
        );

        let (result, printed) = recover_json(&root, CODE);

        let error = result.unwrap_err();
        assert!(printed.is_none());
        assert_eq!(error.report().code, RpcErrorCode::Unsupported, "{error}");
        assert_eq!(
            error.to_string(),
            format!(
                "Server {} needs a newer daemon for ployz recover. Update ployzd there, then run it again.",
                daemon.machine_id
            )
        );
        assert_eq!(daemon.requests.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn recover_names_a_wrong_code() {
        let dir = config_dir(None, "");
        let daemon = daemon(dir.path(), key(1), true);
        let root = root(
            dir.path(),
            Some(&daemon.socket),
            &["recover", "--code-stdin"],
        );

        let (result, printed) = recover_json(&root, "ZYXWVUTSRQPONMLKJIHGFEDCBA");

        let error = result.unwrap_err();
        assert!(printed.is_none());
        assert_eq!(
            error.report().code,
            RpcErrorCode::InvalidArgument,
            "{error}"
        );
        assert_eq!(error.to_string(), "That recovery code is wrong.");
        assert!(matches!(error.hints().as_slice(), [Hint::Retry(_)]));
    }

    #[test]
    fn recover_rejects_a_code_longer_than_stdin_allows() {
        let dir = config_dir(None, "");
        let daemon = daemon(dir.path(), key(1), true);
        let root = root(
            dir.path(),
            Some(&daemon.socket),
            &["recover", "--code-stdin"],
        );

        let (result, _) = recover_json(&root, &"A".repeat(CODE_LIMIT + 1));

        let error = result.unwrap_err();
        assert_eq!(
            error.report().code,
            RpcErrorCode::InvalidArgument,
            "{error}"
        );
        assert!(error.to_string().contains("more than 256 bytes"), "{error}");
        assert!(daemon.recoveries.lock().unwrap().is_empty());
    }

    #[test]
    fn neither_verb_runs_through_cloud() {
        let contexts = format!(
            "current_context: prod\ncontexts:\n  prod:\n    connections:\n      - management: ployz1:{}\n",
            "A".repeat(CAPABILITY_LEN)
        );
        let cloud = cloud(|route, _| panic!("unexpected {route}"));
        let dir = config_dir(Some(&cloud.url), &contexts);
        for tail in [ROTATE, &["recover", "--code-stdin"][..]] {
            let root = root(dir.path(), None, tail);

            let (result, printed) = if tail.first() == Some(&"recover") {
                recover_json(&root, CODE)
            } else {
                rotate_json(&root)
            };

            let error = result.unwrap_err();
            assert!(printed.is_none());
            assert_eq!(
                error.report().code,
                RpcErrorCode::InvalidArgument,
                "{error}"
            );
            assert!(
                error.to_string().contains("doesn't run through Cloud"),
                "{error}"
            );
        }
        assert!(cloud.routes().is_empty());
    }

    #[test]
    fn the_code_has_no_argument_and_neither_verb_is_a_tool() {
        let arguments: Vec<_> = recover_command(Command::new("recover"))
            .get_arguments()
            .filter(|argument| argument.get_id().as_str().contains("code"))
            .map(|argument| {
                (
                    argument.get_id().to_string(),
                    argument.get_action().takes_values(),
                )
            })
            .collect();
        assert_eq!(arguments, [("code-stdin".to_owned(), false)]);
        assert!(verbs::RECOVER.tool.is_none());
        assert!(verbs::RECOVERY_ROTATE.tool.is_none());
        assert!(verbs::find("recover").is_some());
        assert!(verbs::find("recovery rotate").is_some());
    }

    #[test]
    fn a_person_sees_the_code_once_and_nowhere_else() {
        // The code goes to the process's real stdout, so read it from a child run.
        const CHILD: &str = "PLOYZ_RECOVERY_ROTATE_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let cloud = cloud(|route, body| match route {
                "POST /api/cli/sealing/recovery" => (200, recovery_row(key(1), None)),
                "POST /api/cli/sealing/rotate" => (200, json!({ "digest": digest_of(body) })),
                other => panic!("unexpected {other}"),
            });
            let dir = config_dir(Some(&cloud.url), "");
            let daemon = daemon(dir.path(), key(1), true);
            let root = root(dir.path(), Some(&daemon.socket), &["recovery", "rotate"]);
            ui::init(false, ui::Color::Never);
            rotate_for(&root, true).unwrap();
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "handlers::recovery::tests::a_person_sees_the_code_once_and_nowhere_else",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        let codes: Vec<_> = stdout
            .lines()
            .filter(|line| line.contains("Recovery code"))
            .map(|line| dashless(line.rsplit(' ').next().unwrap()))
            .collect();
        let [code] = codes.as_slice() else {
            panic!("expected one code line on stdout: {stdout}");
        };
        RecoveryCode::parse(code).unwrap();
        assert!(!stderr.contains(code.as_str()), "{stderr}");
        assert!(stderr.contains(KEEP_THE_CODE), "{stderr}");
    }

    #[test]
    fn rotate_after_activation_backs_up_the_recipient_key_with_its_rotation() {
        let cloud = recipient_cloud(|route, body| match route {
            "POST /api/cli/sealing/recipient" => (404, json!({ "error": "not found" })),
            "POST /api/cli/sealing/recipient/rotate" => (200, json!({ "digest": digest_of(body) })),
            other => panic!("unexpected {other}"),
        });
        let dir = config_dir(Some(&cloud.url), "");
        let daemon = activated(dir.path(), key(1), key(5));
        let root = root(dir.path(), Some(&daemon.socket), ROTATE);

        let (result, printed) = rotate_json(&root);

        result.unwrap();
        let printed = printed.unwrap();
        let code = dashless(printed.get("recoveryCode").unwrap().as_str().unwrap());
        assert_eq!(printed.get("publicKey"), Some(&json!(key(5))));
        let rotate = cloud.body_of("POST /api/cli/sealing/recipient/rotate");
        assert_eq!(rotate.get("publicKey"), Some(&json!(key(5))));
        assert_eq!(rotate.get("currentDigest"), Some(&Value::Null));
        let posted: Signed<SealingRotation> =
            serde_json::from_value(rotate.get("rotation").unwrap().clone()).unwrap();
        assert_eq!(posted.open().unwrap().recipient, key(5));
        assert!(
            !cloud
                .routes()
                .contains(&"POST /api/cli/sealing/rotate".to_owned())
        );
        cloud.never_saw(&code);
    }

    #[test]
    fn rotate_refuses_a_recipient_other_than_the_one_this_computer_pinned() {
        let cloud = recipient_cloud(|route, _| panic!("unexpected {route}"));
        let dir = config_dir(Some(&cloud.url), "");
        crate::sealing_pin::Pins::beside(&dir.path().join("config.yaml"))
            .set(
                "o1",
                crate::sealing_pin::Pin {
                    public_key: key(1),
                    server: ployz_core::MachineName::parse("web-1").unwrap(),
                    recipient: Some(key(9)),
                },
            )
            .unwrap();
        let daemon = activated(dir.path(), key(1), key(5));
        let root = root(dir.path(), Some(&daemon.socket), ROTATE);

        let (result, printed) = rotate_json(&root);

        let error = result.unwrap_err().to_string();
        assert!(error.contains("no code was made"), "{error}");
        assert_eq!(printed, None);
        assert!(cloud.routes().is_empty());
    }

    #[test]
    fn recover_opens_the_recipient_backup_when_the_code_opens_no_identity_backup() {
        let cloud = recipient_cloud(|route, _| match route {
            "POST /api/cli/sealing/recovery" => {
                (200, recovery_row(key(1), Some(cloud_backup_bytes())))
            }
            "POST /api/cli/sealing/recipient" => (
                200,
                json!({
                    "publicKey": key(5),
                    "backup": wrapped_backup(),
                    "digest": "recipient-digest",
                    "rotation": rotation(key(5)),
                }),
            ),
            other => panic!("unexpected {other}"),
        });
        let dir = config_dir(Some(&cloud.url), "");
        let daemon = activated(dir.path(), key(1), key(5));
        let root = root(
            dir.path(),
            Some(&daemon.socket),
            &["recover", "--code-stdin"],
        );

        let (result, printed) = recover_json(&root, RECIPIENT_CODE);

        result.unwrap();
        assert_eq!(
            printed.unwrap(),
            json!({ "publicKey": key(5), "key": "recipient" })
        );
        assert_eq!(
            *daemon.recoveries.lock().unwrap(),
            [None, Some(cloud_backup_bytes()), Some(wrapped_backup())]
        );
        cloud.never_saw(RECIPIENT_CODE);
    }
}
