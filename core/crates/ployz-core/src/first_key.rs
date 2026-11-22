//! The First Key: the one key an install line hands a fresh Server to trust, and the file
//! beside the data directory that says whether this disk may still use it.

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::DeviceCode;
use crate::authority::{
    KeyRef, NewKey, Passkey, PasskeyApprover, Timestamp, TrustedKeys, TrustedPasskey,
};

const LINE_PREFIX: &str = "pk1_";
/// A passkey's JSON stays well under this; anything longer is not a key.
const LINE_LIMIT: usize = 4096;

/// The key an install line carries: a person's passkey, or the Device of the CLI that printed it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LineKey {
    Device { key: crate::DeviceKey },
    Passkey { passkey: Passkey },
}

/// Why an install line's key didn't read.
#[derive(Debug, thiserror::Error)]
#[error("A key from an install line starts with pk1_ and holds one Device or passkey.")]
pub struct LineKeyError;

impl LineKey {
    /// Read a key as an install line prints it: `pk1_` and base64url JSON.
    ///
    /// # Errors
    /// Returns [`LineKeyError`] for anything else, including a key longer than any real one.
    pub fn parse(line: &str) -> Result<Self, LineKeyError> {
        let encoded = line.strip_prefix(LINE_PREFIX).ok_or(LineKeyError)?;
        if encoded.len() > LINE_LIMIT {
            return Err(LineKeyError);
        }
        let json = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| LineKeyError)?;
        serde_json::from_slice(&json).map_err(|_| LineKeyError)
    }

    #[must_use]
    pub fn key(&self) -> KeyRef {
        NewKey::from(self.clone()).key()
    }

    #[must_use]
    pub fn code(&self) -> DeviceCode {
        self.key().code()
    }

    /// The only key a join or founding anchored on this line trusts as signer.
    #[must_use]
    pub fn trusted(&self) -> TrustedKeys {
        match self {
            Self::Device { key } => TrustedKeys::new([*key], [], []),
            Self::Passkey { passkey } => TrustedKeys::new(
                [],
                [TrustedPasskey {
                    passkey: passkey.clone(),
                    approved_by: PasskeyApprover::Server,
                }],
                [],
            ),
        }
    }

    /// What a person compares before a Server trusts this key: its code, and for a passkey
    /// the account and the site it signs in to.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Device { key } => format!("device {}", key.check_code()),
            Self::Passkey { passkey } => format!(
                "passkey {} for {} on {}",
                passkey.code(),
                passkey.account,
                passkey.relying_party.id
            ),
        }
    }
}

impl std::fmt::Display for LineKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let json = serde_json::to_vec(self).map_err(|_| std::fmt::Error)?;
        write!(
            formatter,
            "{LINE_PREFIX}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
        )
    }
}

impl From<LineKey> for NewKey {
    fn from(key: LineKey) -> Self {
        match key {
            LineKey::Device { key } => Self::Device { key },
            LineKey::Passkey { passkey } => Self::Passkey { passkey },
        }
    }
}

/// What the First Key file holds.
#[expect(
    clippy::large_enum_variant,
    reason = "a file format read once per enrollment, kept unboxed so serde sees the key"
)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum FirstKey {
    /// A fresh disk's install line named this key, and no enrollment took it yet.
    Waiting { key: LineKey },
    /// This disk enrolled once. No install line arms it again; only `ployz key add` on
    /// this Server does.
    Spent {
        /// The key that was waiting, when one was.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<DeviceCode>,
        at: Timestamp,
    },
}

impl FirstKey {
    #[must_use]
    pub fn waiting(&self) -> Option<&LineKey> {
        match self {
            Self::Waiting { key } => Some(key),
            Self::Spent { .. } => None,
        }
    }
}

/// Whether the Local Machine record in `data_dir` says this disk enrolled, read without
/// ployzd: it names an enrollment, or a joining or participating body, also inside a reset.
///
/// # Errors
/// Returns an I/O error other than a missing record.
#[cfg(not(target_arch = "wasm32"))]
pub fn disk_enrolled(data_dir: &std::path::Path) -> std::io::Result<bool> {
    let bytes = match std::fs::read(data_dir.join("machine.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let record: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let enrolled_phase = |body: &serde_json::Value| {
        matches!(
            body.get("phase").and_then(serde_json::Value::as_str),
            Some("joining" | "participating")
        )
    };
    let body = record.get("body");
    Ok(record
        .get("enrollment")
        .is_some_and(|enrollment| !enrollment.is_null())
        || body.is_some_and(enrolled_phase)
        || body
            .and_then(|body| body.get("prior"))
            .is_some_and(enrolled_phase))
}

#[cfg(not(target_arch = "wasm32"))]
pub use file::{Armed, FirstKeyError, FirstKeyFile, HeldFirstKey};

#[cfg(not(target_arch = "wasm32"))]
mod file {
    use std::{
        fs::{self, File, OpenOptions},
        io::{self, Write as _},
        os::unix::fs::OpenOptionsExt as _,
        path::{Path, PathBuf},
    };

    use super::{FirstKey, LineKey};
    use crate::authority::Timestamp;

    /// The First Key file beside a data directory, named after it the way the Machine pool's
    /// file is (`/var/lib/ployz-first-key.json`), so a reset or uninstall that removes the
    /// data directory leaves it. Arming, enrollment and reset each hold [`FirstKeyFile::lock`].
    #[derive(Clone, Debug)]
    pub struct FirstKeyFile {
        path: PathBuf,
        lock: PathBuf,
    }

    #[derive(Debug, thiserror::Error)]
    pub enum FirstKeyError {
        #[error("Could not use the First Key file {path}.")]
        Io {
            path: PathBuf,
            #[source]
            source: io::Error,
        },
        #[error("The First Key file {path} is not one Ployz wrote.")]
        Unreadable {
            path: PathBuf,
            #[source]
            source: serde_json::Error,
        },
    }

    /// What arming a key found.
    #[expect(
        clippy::large_enum_variant,
        reason = "returned once per arming, and boxing would only move the key"
    )]
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub enum Armed {
        /// No file was there; it now waits for this key.
        Created,
        /// It already waits for this key, and nothing was written.
        AlreadyWaiting,
        /// It holds something else, which only a confirmed re-arm replaces.
        Held(FirstKey),
    }

    impl FirstKeyFile {
        #[must_use]
        pub fn beside(data_dir: &Path) -> Self {
            let beside = |suffix: &str| {
                let mut name = data_dir.file_name().unwrap_or_default().to_os_string();
                name.push(suffix);
                data_dir.with_file_name(name)
            };
            Self {
                path: beside("-first-key.json"),
                lock: beside("-first-key.lock"),
            }
        }

        #[must_use]
        pub fn path(&self) -> &Path {
            &self.path
        }

        /// Wait for the lock arming, enrollment and reset share, and hold it.
        ///
        /// # Errors
        /// Returns [`FirstKeyError::Io`] when the lock file can't be opened or locked.
        pub fn lock(&self) -> Result<HeldFirstKey<'_>, FirstKeyError> {
            let io = |source| FirstKeyError::Io {
                path: self.lock.clone(),
                source,
            };
            if let Some(parent) = self.lock.parent() {
                fs::create_dir_all(parent).map_err(io)?;
            }
            let lock = OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .mode(0o600)
                .open(&self.lock)
                .map_err(io)?;
            lock.lock().map_err(io)?;
            Ok(HeldFirstKey {
                file: self,
                _lock: lock,
            })
        }
    }

    /// The First Key file, read and written only while its lock is held.
    #[derive(Debug)]
    pub struct HeldFirstKey<'a> {
        file: &'a FirstKeyFile,
        _lock: File,
    }

    impl HeldFirstKey<'_> {
        /// # Errors
        /// Returns [`FirstKeyError`] for an unreadable or malformed file. A missing file is `None`.
        pub fn read(&self) -> Result<Option<FirstKey>, FirstKeyError> {
            let path = &self.file.path;
            match fs::read(path) {
                Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|source| {
                    FirstKeyError::Unreadable {
                        path: path.clone(),
                        source,
                    }
                }),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(source) => Err(FirstKeyError::Io {
                    path: path.clone(),
                    source,
                }),
            }
        }

        /// Make the file wait for `key` when there is none. The same key again writes
        /// nothing; anything else is left for a confirmed [`Self::rearm`].
        ///
        /// # Errors
        /// Returns [`FirstKeyError`] when the file can't be read or written.
        pub fn arm(&self, key: &LineKey) -> Result<Armed, FirstKeyError> {
            match self.read()? {
                None => self.create(&FirstKey::Waiting { key: key.clone() }),
                Some(FirstKey::Waiting { key: held }) if &held == key => Ok(Armed::AlreadyWaiting),
                Some(held) => Ok(Armed::Held(held)),
            }
        }

        /// Make the file wait for `key`, whatever it held. Only a person at this Server's
        /// terminal asks for this, after reading what `key` is.
        ///
        /// # Errors
        /// Returns [`FirstKeyError`] when the file can't be written.
        pub fn rearm(&self, key: &LineKey) -> Result<(), FirstKeyError> {
            self.replace(&FirstKey::Waiting { key: key.clone() })
        }

        /// Mark this disk enrolled, keeping the code of the key that waited. A file already
        /// Spent is left as it is, and a missing one is created Spent.
        ///
        /// # Errors
        /// Returns [`FirstKeyError`] when the file can't be read or written.
        pub fn spend(&self, at: Timestamp) -> Result<(), FirstKeyError> {
            let code = match self.read() {
                Ok(Some(FirstKey::Spent { .. })) => return Ok(()),
                Ok(Some(FirstKey::Waiting { key })) => Some(key.code()),
                // A file nobody can read must still end Spent, or the disk looks fresh.
                Ok(None) | Err(FirstKeyError::Unreadable { .. }) => None,
                Err(error) => return Err(error),
            };
            self.replace(&FirstKey::Spent { code, at })
        }

        fn create(&self, state: &FirstKey) -> Result<Armed, FirstKeyError> {
            let temporary = self.write_temporary(state)?;
            let linked = fs::hard_link(&temporary, &self.file.path);
            let _ = fs::remove_file(&temporary);
            match linked {
                Ok(()) => {
                    self.sync_directory()?;
                    Ok(Armed::Created)
                }
                // Another writer that held no lock got there first; its file stands.
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => match self.read()? {
                    Some(FirstKey::Waiting { key }) if Some(&key) == state.waiting() => {
                        Ok(Armed::AlreadyWaiting)
                    }
                    Some(held) => Ok(Armed::Held(held)),
                    None => Err(self.io(error)),
                },
                Err(error) => Err(self.io(error)),
            }
        }

        fn replace(&self, state: &FirstKey) -> Result<(), FirstKeyError> {
            let temporary = self.write_temporary(state)?;
            fs::rename(&temporary, &self.file.path).map_err(|error| self.io(error))?;
            self.sync_directory()
        }

        fn write_temporary(&self, state: &FirstKey) -> Result<PathBuf, FirstKeyError> {
            let temporary = self
                .file
                .path
                .with_extension(format!("json.{}.tmp", std::process::id()));
            let bytes = serde_json::to_vec(state).expect("a First Key serializes");
            let mut file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o600)
                .open(&temporary)
                .map_err(|error| self.io(error))?;
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .map_err(|error| self.io(error))?;
            Ok(temporary)
        }

        fn sync_directory(&self) -> Result<(), FirstKeyError> {
            match self.file.path.parent() {
                Some(parent) => File::open(parent)
                    .and_then(|directory| directory.sync_all())
                    .map_err(|error| self.io(error)),
                None => Ok(()),
            }
        }

        fn io(&self, source: io::Error) -> FirstKeyError {
            FirstKeyError::Io {
                path: self.file.path.clone(),
                source,
            }
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "first_key_tests.rs"]
mod tests;
