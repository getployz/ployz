//! The Fleet Token and the file it lives in: reading, writing, keeping private, locking, and what
//! the loop does when the file changes or goes.

use std::{
    fs,
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
    time::Duration,
};

use ployz_core::RpcErrorCode;

use super::paths::Paths;
use super::{CheckinClient, Host, KeyError, unit};
use crate::failure::Failure;

pub(super) const TOKEN_SETTLE: Duration = Duration::from_secs(2);
const FLEET_TOKEN_PREFIX: &str = "pflt_";
const FLEET_TOKEN_SECRET_CHARS: usize = 43;
const TOKEN_MODE: u32 = 0o600;

#[derive(PartialEq, Eq)]
pub(crate) struct FleetToken(String);

impl FleetToken {
    pub(crate) fn read(path: &Path) -> Result<Self, KeyError> {
        let text = fs::read_to_string(path).map_err(|source| KeyError::Token {
            path: path.to_owned(),
            source,
        })?;
        Self::parse(&text).ok_or_else(|| KeyError::NotAToken {
            path: path.to_owned(),
        })
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        let token = text.trim();
        let secret = token.strip_prefix(FLEET_TOKEN_PREFIX).unwrap_or_default();
        (secret.len() == FLEET_TOKEN_SECRET_CHARS
            && secret
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'))
        .then(|| Self(token.to_owned()))
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }

    #[cfg(test)]
    pub(super) fn literal(token: &str) -> Self {
        Self(token.to_owned())
    }

    /// Writes this token to `path` in one step, readable by root only.
    pub(crate) fn write(&self, path: &Path) -> io::Result<()> {
        let directory = path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(TOKEN_MODE))?;
        writeln!(file, "{}", self.0)?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|error| error.error)?;
        Ok(())
    }
}

/// The token in the file when it differs from the one sent. A file caught mid-write is read
/// again once it settles, so a replacement is not deleted as the old token.
pub(super) async fn replaced_token(client: &CheckinClient, path: &Path) -> Option<FleetToken> {
    let token = match FleetToken::read(path) {
        Err(KeyError::Token { source, .. }) if source.kind() == io::ErrorKind::NotFound => None,
        Err(_) => {
            tokio::time::sleep(TOKEN_SETTLE).await;
            FleetToken::read(path).ok()
        }
        Ok(token) => Some(token),
    };
    token.filter(|token| !client.sends(token))
}

/// The Fleet Token the loop checks in with. With the file gone there is nothing to check in
/// with, so the loop disables itself and ends cleanly instead of failing at every restart.
pub(crate) async fn loop_token<H: Host>(
    host: &H,
    paths: &Paths,
) -> Result<Option<FleetToken>, KeyError> {
    match FleetToken::read(&paths.token_file) {
        Err(KeyError::Token { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            crate::ui::note(format!(
                "No Fleet Token at {}. The check-in loop stops.",
                paths.token_file.display()
            ));
            if let Err(why) = host.disable_loop().await {
                crate::ui::note(format!("Could not disable {}: {why}", unit::CHECKIN));
            }
            Ok(None)
        }
        read => read.map(Some),
    }
}

/// What the token file holds against the token the loop sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Held {
    /// The token sent, which is now removed.
    Sent,
    /// A token written since, left alone.
    Replaced,
    Gone,
}

/// Removes the token file only while it still holds the token Cloud answered, so a token written
/// since survives.
pub(super) fn retire(client: &CheckinClient, path: &Path) -> Held {
    let held = FleetToken::read(path);
    let still_sent = held.as_ref().is_ok_and(|token| client.sends(token));
    let gone = matches!(&held, Err(KeyError::Token { source, .. }) if source.kind() == io::ErrorKind::NotFound);
    if still_sent && let Err(error) = fs::remove_file(path) {
        crate::ui::note(format!(
            "Could not remove the Fleet Token at {}: {error}",
            path.display()
        ));
    }
    if still_sent {
        Held::Sent
    } else if gone {
        Held::Gone
    } else {
        Held::Replaced
    }
}

/// Held while an install replaces the token file, so another install can't swap the token between
/// its gate and its write.
pub(crate) struct TokenFileLock {
    _directory: fs::File,
}

impl TokenFileLock {
    pub(crate) fn take(token_file: &Path) -> Result<Self, Failure> {
        let directory = token_file
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let locked = fs::create_dir_all(directory)
            .and_then(|()| fs::File::open(directory))
            .and_then(|handle| {
                rustix::fs::flock(&handle, rustix::fs::FlockOperation::LockExclusive)?;
                Ok(Self { _directory: handle })
            });
        locked.map_err(|error| {
            Failure::caused(
                RpcErrorCode::Internal,
                format!("Could not lock {}.", directory.display()),
                error,
            )
        })
    }
}

pub(crate) fn ensure_readable(token_file: &Path) -> Result<(), Failure> {
    replaces(token_file, |_| false).map(drop)
}

/// Whether the file holds something other than the token `sends` accepts.
pub(crate) fn replaces(
    token_file: &Path,
    sends: impl Fn(&FleetToken) -> bool,
) -> Result<bool, Failure> {
    match FleetToken::read(token_file) {
        Ok(held) => Ok(!sends(&held)),
        Err(KeyError::Token { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            Ok(false)
        }
        Err(KeyError::NotAToken { .. }) => Ok(true),
        Err(KeyError::Token { path, source }) => Err(Failure::caused(
            RpcErrorCode::InvalidArgument,
            format!(
                "Could not read {}, so this install can't tell whether it replaces a Fleet Token. Fix or remove that file, then run the install again.",
                path.display()
            ),
            source,
        )),
        Err(error) => Err(super::client::key_failure(error)),
    }
}

pub(crate) fn keep_private(path: &Path) -> Result<(), Failure> {
    let failed = |error| {
        Failure::caused(
            RpcErrorCode::Internal,
            format!("Could not make {} readable by root only.", path.display()),
            error,
        )
    };
    let mode = fs::metadata(path).map_err(failed)?.permissions().mode();
    if mode & 0o077 == 0 {
        return Ok(());
    }
    fs::set_permissions(path, fs::Permissions::from_mode(TOKEN_MODE)).map_err(failed)?;
    crate::ui::note(format!("Made {} readable by root only.", path.display()));
    Ok(())
}
