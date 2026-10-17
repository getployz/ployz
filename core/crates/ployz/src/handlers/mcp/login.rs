use std::io;
use std::path::{Path, PathBuf};

use clap::{ArgMatches, Command};
use serde::{Deserialize, Serialize};

use super::super::account::in_cloud;
use super::super::{Error, config_path, leaf_matches};
use crate::cloud_account::{self, Credential, TokenKind};
use crate::cloud_login::{LoginError, Secret};
use crate::ui;

/// Days an agent token works before `ployz mcp login` is needed again.
const LIFETIME_DAYS: u16 = 365;

pub(crate) const NOT_LOGGED_IN: &str =
    "`ployz mcp` has no credential of its own; run `ployz mcp login` first.";

pub(crate) fn login_command(command: Command) -> Command {
    command.about("Give `ployz mcp` its own agent token, which answers no approval")
}

/// The agent token `ployz mcp login` made, kept beside the Ployz config.
#[derive(Serialize, Deserialize)]
pub(super) struct Stored {
    cloud: String,
    id: String,
    organization: String,
    token: Secret,
}

impl Stored {
    pub(super) fn credential(&self) -> Credential {
        Credential::Token {
            cloud: self.cloud.clone(),
            token: self.token.clone(),
        }
    }
}

fn path(config: &Path) -> PathBuf {
    config.with_file_name("mcp.json")
}

fn failed(path: PathBuf, source: io::Error) -> LoginError {
    LoginError::Store { path, source }
}

/// The agent token stored beside `config`, if `ployz mcp login` made one.
///
/// # Errors
///
/// Returns a store that cannot be read or parsed.
pub(super) fn stored(config: &Path) -> Result<Option<Stored>, LoginError> {
    let path = path(config);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|source| LoginError::Corrupt { path, source }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(failed(path, source)),
    }
}

fn save(config: &Path, stored: &Stored) -> Result<(), LoginError> {
    let path = path(config);
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|source| failed(path.clone(), source))?;
    // A named temp file is created 0600, so the secret is never world-readable.
    let mut file =
        tempfile::NamedTempFile::new_in(dir).map_err(|source| failed(path.clone(), source))?;
    serde_json::to_writer_pretty(&mut file, stored)
        .map_err(|source| failed(path.clone(), io::Error::other(source)))?;
    file.persist(&path)
        .map_err(|error| failed(path.clone(), error.error))?;
    Ok(())
}

pub(crate) fn login(root: &ArgMatches) -> Result<(), Error> {
    let config = config_path(leaf_matches(root))?;
    let previous = stored(&config)?;
    let (made, replaced) = in_cloud(root, async |_, credential| {
        let made =
            cloud_account::new_token(credential, "ployz mcp", TokenKind::Agent, LIFETIME_DAYS)
                .await?;
        save(
            &config,
            &Stored {
                cloud: credential.cloud().to_owned(),
                id: made.id.clone(),
                organization: made.organization.clone(),
                token: Secret::new(made.secret.clone()),
            },
        )?;
        let replaced = match previous {
            Some(previous) => {
                let removed = cloud_account::remove_token(credential, &previous.id).await;
                Some((previous.id, removed.map(drop)))
            }
            None => None,
        };
        Ok((made, replaced))
    })?;
    if let Some((id, Err(error))) = &replaced {
        ui::warn(format!(
            "Could not revoke the agent token {id} this replaced: {error}. Run `ployz token rm {id}`."
        ));
    }
    let token = serde_json::json!({
        "id": made.id,
        "name": made.name,
        "kind": made.kind,
        "organization": made.organization,
        "expires_at": made.expires_at,
    });
    ui::finish(&serde_json::json!({ "token": token }), || {
        ui::note(format_args!(
            "`ployz mcp` now acts with its own agent token {} in Organization {}, expiring {}.",
            made.id, made.organization, made.expires_at
        ));
        if let Some((id, Ok(()))) = &replaced {
            ui::note(format_args!("Revoked the agent token {id} it replaced."));
        }
    })
}
