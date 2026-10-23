//! `ployz recover` and `ployz recovery rotate`. The recovery code passes between a person
//! and the Server's daemon: never argv, the environment, Cloud, or a log line.

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use clap::{ArgMatches, Command};
use ployz_core::{
    RecoverSealingKeyRequest, RecoveryCode, RecoveryRefusal, RpcErrorCode, SealingKeyBackup,
    SealingPublicKey, WrapSealingKeyRequest, op,
};
use serde::Deserialize;
use serde_json::json;

use super::{Error, config_path, leaf_matches, runtime, server};
use crate::cli::{base, switch};
use crate::cloud_account::{self, Credential};
use crate::cloud_enroll::RecoveryBackup;
use crate::cloud_login::LoginError;
use crate::connect::{Client, ConnectError, DEFAULT_LOCAL_SOCKET, SystemConnector};
use crate::context::Transport;
use crate::ui::{self, Fields, Hint};

pub(crate) const RECOVERY_CODE_NOT_SAVED: &str =
    "Recovery code not saved. Run ployz recovery rotate now to make one.";
pub(crate) const KEEP_THE_CODE: &str = "Keep the recovery code safe; ployz recover needs it.";
const CODE_LIMIT: usize = 256;
const NOT_AVAILABLE: &str = "Sealing Key recovery isn't available for this organization.";
const ROTATE_AGAIN: &str =
    "The old recovery code may no longer work. Run ployz recovery rotate now.";
const RECOVERY_ROW: &str = "sealing/recovery";
const ROTATE_ROW: &str = "sealing/rotate";

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
    base(command, "Make a new recovery code").long_about(
        "Wraps the Sealing Key under a fresh code, stores that backup in Cloud, then prints \
         the code once. The old code stops working. Runs on the Server or over --connect; \
         never through Cloud.",
    )
}

pub(super) fn recover(root: &ArgMatches) -> Result<(), Error> {
    recover_from(root, std::io::stdin().lock())
}

fn recover_from(root: &ArgMatches, stdin: impl Read) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let runtime = runtime()?;
    let mut client = runtime.block_on(dial(matches, "ployz recover"))?;
    let credential = server::signed_in(&runtime, matches)?;
    let code = read_code(matches, stdin)?;
    let recovered = runtime.block_on(async {
        let first = recover_with(&mut client, &code, None).await;
        match first {
            Err(error) if refusal(&error) == Some(RecoveryRefusal::NoBackup) => {
                let backup = cloud_backup(credential.as_ref()).await?;
                recover_with(&mut client, &code, Some(backup)).await
            }
            other => other,
        }
        .map_err(recover_failure)
    })?;
    ui::fields(
        &json!({ "publicKey": recovered }),
        &Fields::new().field("Sealing key", recovered),
    )
}

pub(super) fn rotate(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let runtime = runtime()?;
    let Some(credential) = server::signed_in(&runtime, matches)? else {
        return Err(Error::coded(
            RpcErrorCode::Unauthenticated,
            "Sign in first: ployz recovery rotate stores the new backup in Cloud.",
        )
        .hint(Hint::Next("ployz login".to_owned())));
    };
    let mut client = runtime.block_on(dial(matches, "ployz recovery rotate"))?;
    let (public_key, code) = runtime.block_on(async {
        let organization = cloud_account::acting_in(&credential).await?;
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
        Ok((wrapped.public_key, code))
    })?;
    print_code(public_key, &code)
}

/// The Sealing Key wrapped under `code`, for Cloud to keep until `ployz recover` needs it.
pub(super) async fn wrap_sealing_key(
    client: &mut Client,
    code: &RecoveryCode,
) -> Result<RecoveryBackup, Error> {
    let wrapped = client
        .call_repeatable::<op::WrapSealingKey>(WrapSealingKeyRequest { code: code.clone() }, None)
        .await?;
    Ok(RecoveryBackup {
        public_key: wrapped.public_key,
        backup: wrapped.backup,
    })
}

fn print_code(public_key: SealingPublicKey, code: &RecoveryCode) -> Result<(), Error> {
    let code = code.reveal();
    if ui::json() {
        return ui::emit(&json!({ "publicKey": public_key, "recoveryCode": code.as_str() }))
            .map_err(|error| error.context(RECOVERY_CODE_NOT_SAVED));
    }
    ui::write_fields(&Fields::new().field("Recovery code", code.as_str())).map_err(|error| {
        Error::caused(RpcErrorCode::Unavailable, RECOVERY_CODE_NOT_SAVED, error)
    })?;
    ui::note(KEEP_THE_CODE);
    Ok(())
}

/// The connection `--connect` or the context names, else the local socket. A Cloud-routed
/// context is refused before any dial: the code never crosses a Management transport.
async fn dial(matches: &ArgMatches, verb: &str) -> Result<Client, Error> {
    let selected = crate::connect::resolve_connections(
        &config_path(matches)?,
        matches.get_one::<String>("connect").map(String::as_str),
        matches.get_one::<String>("context").map(String::as_str),
        Path::new(DEFAULT_LOCAL_SOCKET),
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
) -> Result<SealingPublicKey, ConnectError> {
    let recovered = client
        .call_repeatable::<op::RecoverSealingKey>(
            RecoverSealingKeyRequest {
                code: code.clone(),
                backup,
            },
            None,
        )
        .await?;
    Ok(recovered.public_key)
}

fn refusal(error: &ConnectError) -> Option<RecoveryRefusal> {
    let ConnectError::Rpc(transport) = error else {
        return None;
    };
    transport.to_rpc_error().recovery_refusal()
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
    }
}

/// Cloud's backup for the signed-in organization, for a Server that lost its own.
async fn cloud_backup(credential: Option<&Credential>) -> Result<SealingKeyBackup, Error> {
    let Some(credential) = credential else {
        return Err(Error::coded(
            RpcErrorCode::Unauthenticated,
            "This Server holds no backup of the Sealing Key. Sign in so ployz recover can use Cloud's.",
        )
        .hint(Hint::Next("ployz login".to_owned())));
    };
    let organization = cloud_account::acting_in(credential).await?;
    recovery_row(credential, &organization.id)
        .await?
        .and_then(|row| row.backup)
        .ok_or_else(|| {
            Error::conflict(
                "Nothing to recover from: neither this Server nor Cloud holds a backup.",
            )
        })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecoveryRow {
    public_key: SealingPublicKey,
    backup: Option<SealingKeyBackup>,
    digest: Option<String>,
}

#[derive(Deserialize)]
struct Rotated {
    digest: String,
}

/// Cloud's recovery row, or `None` when the route is dark or the organization has none.
async fn recovery_row(
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
    if matches!(
        error,
        LoginError::Unreachable { .. } | LoginError::Unanswered { .. } | LoginError::Reply(_)
    ) {
        return Error::unavailable(format!(
            "Cloud didn't confirm the new backup. {ROTATE_AGAIN}"
        ));
    }
    error.into()
}
