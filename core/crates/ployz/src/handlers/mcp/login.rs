use std::io;
use std::path::{Path, PathBuf};

use clap::{ArgMatches, Command};
use serde::{Deserialize, Serialize};

use super::super::account::in_cloud;
use super::super::{Error, config_path, leaf_matches};
use crate::cloud_account::{self, Credential, TokenKind};
use crate::cloud_login::{LoginError, Secret};
use crate::ui;

const LIFETIME_DAYS: u16 = 365;

pub(crate) const NOT_LOGGED_IN: &str =
    "`ployz mcp` has no credential of its own; run `ployz mcp login` first.";

pub(crate) fn login_command(command: Command) -> Command {
    command.about("Give `ployz mcp` its own agent token, which answers no approval")
}

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

/// Whether a saved `mcp.json` survives a crash.
enum Saved {
    Durable,
    /// It holds the new token, but a crash may bring back the one before.
    Unflushed(io::Error),
}

fn save(config: &Path, stored: &Stored) -> Result<Saved, LoginError> {
    let path = path(config);
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|source| failed(path.clone(), source))?;
    // A named temp file is created 0600, so the secret is never world-readable.
    let mut file =
        tempfile::NamedTempFile::new_in(dir).map_err(|source| failed(path.clone(), source))?;
    serde_json::to_writer_pretty(&mut file, stored)
        .map_err(|source| failed(path.clone(), io::Error::other(source)))?;
    // Durable before the caller revokes the token this replaces.
    file.as_file()
        .sync_all()
        .map_err(|source| failed(path.clone(), source))?;
    file.persist(&path)
        .map_err(|error| failed(path.clone(), error.error))?;
    // mcp.json now holds the new token, so a failure here must not revoke it.
    Ok(
        match std::fs::File::open(dir).and_then(|dir| dir.sync_all()) {
            Ok(()) => Saved::Durable,
            Err(error) => Saved::Unflushed(error),
        },
    )
}

pub(crate) fn login(root: &ArgMatches) -> Result<(), Error> {
    let config = config_path(leaf_matches(root))?;
    let previous = match stored(&config) {
        Ok(previous) => previous,
        Err(error @ LoginError::Corrupt { .. }) => {
            ui::warn(format!("{error} Replacing it."));
            None
        }
        Err(error) => return Err(error.into()),
    };
    let (made, revoked, left) = in_cloud(root, async |_, credential| {
        let made =
            cloud_account::new_token(credential, "ployz mcp", TokenKind::Agent, LIFETIME_DAYS)
                .await?;
        let saved = if made.kind == TokenKind::Agent {
            save(
                &config,
                &Stored {
                    cloud: credential.cloud().to_owned(),
                    id: made.id.clone(),
                    organization: made.organization.clone(),
                    token: Secret::new(made.secret.clone()),
                },
            )
        } else {
            Err(LoginError::Reply(format!(
                "it made a {} token where `ployz mcp` needs an agent token",
                made.kind.as_str()
            )))
        };
        let saved = match saved {
            Ok(saved) => saved,
            Err(error) => {
                if let Err(revoking) = cloud_account::remove_token(credential, &made.id).await {
                    ui::warn(format!(
                        "Could not revoke the token {} just made: {revoking}. Run `ployz token rm {}`.",
                        made.id, made.id
                    ));
                }
                return Err(error);
            }
        };
        let (revoked, left) = match previous
            .map(|previous| fate(previous, credential.cloud(), &made.organization, &saved))
        {
            None => (None, None),
            Some(Err(left)) => (None, Some(left)),
            Some(Ok(id)) => match revoke(credential, &id).await {
                Ok(true) => (Some(id), None),
                Ok(false) => (None, None),
                Err(error) => {
                    let left = Left {
                        why: format!("revoking it failed ({error})"),
                        fix: format!("run `ployz token rm {id}`"),
                        id,
                    };
                    (None, Some(left))
                }
            },
        };
        Ok((made, revoked, left))
    })?;
    if let Some(left) = &left {
        ui::warn(format!(
            "The agent token {} this replaced is still live: {}. To revoke it, {}.",
            left.id, left.why, left.fix
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
        if let Some(id) = &revoked {
            ui::note(format_args!("Revoked the agent token {id} it replaced."));
        }
    })
}

/// An agent token this login leaves live, why, and how to revoke it.
#[derive(Debug)]
struct Left {
    id: String,
    why: String,
    fix: String,
}

/// The agent token `mcp.json` held before is revoked with this login's sign-in only in the
/// Cloud and Organization that sign-in acts in, and only once the new token is durable.
fn fate(previous: Stored, cloud: &str, organization: &str, saved: &Saved) -> Result<String, Left> {
    let id = previous.id;
    let (why, fix) = if previous.cloud != cloud {
        (
            format!("it is in Ployz Cloud at {}", previous.cloud),
            format!("sign in there and run `ployz token rm {id}`"),
        )
    } else if previous.organization != organization {
        (
            format!("it is in Organization {}", previous.organization),
            format!(
                "run `ployz org use {}`, then `ployz token rm {id}`",
                previous.organization
            ),
        )
    } else if let Saved::Unflushed(error) = saved {
        (
            format!(
                "a crash may bring it back into mcp.json, which could not be flushed ({error})"
            ),
            format!("run `ployz token rm {id}`"),
        )
    } else {
        return Ok(id);
    };
    Err(Left { id, why, fix })
}

/// Revoke the token `ployz mcp` held before, only while this Organization lists it as an
/// agent token, so an edited `mcp.json` never revokes a CLI token.
async fn revoke(credential: &Credential, id: &str) -> Result<bool, LoginError> {
    let listed = cloud_account::credentials(credential).await?;
    if !listed
        .tokens
        .iter()
        .any(|token| token.id == id && token.kind == TokenKind::Agent)
    {
        return Ok(false);
    }
    cloud_account::remove_token(credential, id).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(cloud: &str, organization: &str) -> Stored {
        Stored {
            cloud: cloud.to_owned(),
            id: "tok_old".to_owned(),
            organization: organization.to_owned(),
            token: Secret::new("ployz_old".to_owned()),
        }
    }

    const CLOUD: &str = "https://cloud.example";

    #[test]
    fn the_token_before_is_revoked_in_its_own_organization_once_the_new_one_is_durable() {
        let fate = fate(held(CLOUD, "ada"), CLOUD, "ada", &Saved::Durable);
        assert_eq!(fate.unwrap(), "tok_old");
    }

    #[test]
    fn a_token_before_in_another_organization_is_left_with_the_lines_that_revoke_it() {
        let left = fate(held(CLOUD, "ada"), CLOUD, "adaside", &Saved::Durable).unwrap_err();
        assert_eq!(left.id, "tok_old");
        assert_eq!(
            left.fix,
            "run `ployz org use ada`, then `ployz token rm tok_old`"
        );
    }

    #[test]
    fn a_token_before_in_another_cloud_is_left_and_named() {
        let left = fate(
            held("https://other.example", "ada"),
            CLOUD,
            "ada",
            &Saved::Durable,
        )
        .unwrap_err();
        assert!(left.why.contains("https://other.example"), "{left:?}");
        assert_eq!(left.fix, "sign in there and run `ployz token rm tok_old`");
    }

    #[test]
    fn a_new_token_that_may_not_survive_a_crash_keeps_the_one_before_live() {
        let unflushed = Saved::Unflushed(io::Error::other("disk"));
        let left = fate(held(CLOUD, "ada"), CLOUD, "ada", &unflushed).unwrap_err();
        assert_eq!(left.fix, "run `ployz token rm tok_old`");
    }
}
