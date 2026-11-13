//! The Cluster Sealing public key this computer pinned, by Organization.
//!
//! The CLI never seals to a key on Cloud's word. Over a connection the person chose that
//! Cloud doesn't carry, the local socket or SSH, the entry Server reports its own key.
//! Through Cloud, a relayed key is compared with the one `ployz device pin` read from a
//! Server that way, and a seal goes ahead only when they match.

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

use ployz_core::{
    ActivationState, Hint, InspectRequest, MachineDetails, MachineName, RpcErrorCode, Sealing,
    SealingPublicKey, op,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    cluster::{Call, Wait},
    connect::Client,
    context::{ConnectionSource, Transport},
    failure::Failure,
};

/// The command that pins the key, named by every refusal here.
pub(crate) const PIN_COMMAND: &str = "ployz device pin --connect ssh://USER@SERVER";

/// A Cluster's Sealing public key as a Server reported it over a connection Cloud doesn't
/// carry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Pin {
    pub(crate) public_key: SealingPublicKey,
    /// The Server that reported it.
    pub(crate) server: MachineName,
    /// The recipient key new values seal to once the Cluster activated, as a Server reported
    /// it over SSH or the local socket or as `ployz device enforce` here named it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) recipient: Option<SealingPublicKey>,
}

impl Pin {
    /// What an entry's own report seals to: [`pin_from`] gave no pin for an activated
    /// Server without a recipient key, so its identity is used only before activation.
    fn seal_for(&self) -> SealFor {
        SealFor {
            cluster: self.public_key,
            recipient: self.recipient.unwrap_or(self.public_key),
        }
    }
}

/// The Cluster a value seals for and the key it seals to: its recipient key once it
/// activated, its identity before.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SealFor {
    pub(crate) cluster: SealingPublicKey,
    pub(crate) recipient: SealingPublicKey,
}

#[derive(Default, Serialize, Deserialize)]
struct PinFile {
    #[serde(default)]
    organizations: BTreeMap<String, Pin>,
}

#[derive(Debug, Error)]
pub(crate) enum PinError {
    #[error("couldn't read {path}")]
    Read { path: PathBuf, source: io::Error },
    #[error("couldn't write {path}")]
    Write { path: PathBuf, source: io::Error },
    #[error("{path} isn't a list of pinned Sealing Keys. Delete it, then pin again")]
    Malformed {
        path: PathBuf,
        source: serde_json::Error,
    },
}

impl From<PinError> for Failure {
    fn from(error: PinError) -> Self {
        Self::command(error)
    }
}

/// This computer's pins, kept beside its config.
pub(crate) struct Pins {
    path: PathBuf,
}

impl Pins {
    pub(crate) fn beside(config: &Path) -> Self {
        Self {
            path: crate::config_dir::ConfigDir::new(config).sealing_keys(),
        }
    }

    /// The key pinned for `organization`, if any.
    ///
    /// # Errors
    ///
    /// Fails when the file can't be read or isn't a pin file.
    pub(crate) fn get(&self, organization: &str) -> Result<Option<Pin>, PinError> {
        Ok(self.load()?.organizations.remove(organization))
    }

    /// Pin `pin` for `organization`, answering the pin it replaced.
    ///
    /// # Errors
    ///
    /// Fails when the file can't be read, isn't a pin file, or can't be written.
    pub(crate) fn set(&self, organization: &str, pin: Pin) -> Result<Option<Pin>, PinError> {
        let mut file = self.load()?;
        let replaced = file.organizations.insert(organization.to_owned(), pin);
        self.save(&file)?;
        Ok(replaced)
    }

    /// The recipient key a pin of the Cluster whose identity is `cluster` names, if any.
    ///
    /// # Errors
    ///
    /// Fails when the file can't be read or isn't a pin file.
    pub(crate) fn recipient_of(
        &self,
        cluster: SealingPublicKey,
    ) -> Result<Option<SealingPublicKey>, PinError> {
        Ok(self
            .load()?
            .organizations
            .into_values()
            .filter(|pin| pin.public_key == cluster)
            .find_map(|pin| pin.recipient))
    }

    /// Name `recipient` on every pin of the Cluster whose identity is `cluster`, as
    /// `ployz device enforce` does once it installed the recipient it generated here.
    ///
    /// # Errors
    ///
    /// Fails when the file can't be read, isn't a pin file, or can't be written.
    pub(crate) fn name_recipient(
        &self,
        cluster: SealingPublicKey,
        recipient: SealingPublicKey,
    ) -> Result<(), PinError> {
        let mut file = self.load()?;
        let mut named = false;
        for pin in file.organizations.values_mut() {
            if pin.public_key == cluster {
                pin.recipient = Some(recipient);
                named = true;
            }
        }
        if named {
            self.save(&file)?;
        }
        Ok(())
    }

    fn load(&self) -> Result<PinFile, PinError> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|source| PinError::Malformed {
                path: self.path.clone(),
                source,
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(PinFile::default()),
            Err(source) => Err(PinError::Read {
                path: self.path.clone(),
                source,
            }),
        }
    }

    fn save(&self, file: &PinFile) -> Result<(), PinError> {
        let write = |source| PinError::Write {
            path: self.path.clone(),
            source,
        };
        let dir = self.path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).map_err(write)?;
        let mut temporary = tempfile::NamedTempFile::new_in(dir).map_err(write)?;
        serde_json::to_writer_pretty(&mut temporary, file)
            .map_err(|error| write(io::Error::other(error)))?;
        temporary.as_file().sync_all().map_err(write)?;
        temporary
            .persist(&self.path)
            .map_err(|error| write(error.error))?;
        Ok(())
    }
}

/// What a seal through a client trusts for the Cluster's key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SealTo {
    /// The person chose the connection and Cloud doesn't carry it, so the entry Server
    /// reports its own key.
    Entry,
    /// Cloud chose or carries the connection, so only the key this computer pinned for
    /// the Organization counts. None when it pinned none.
    Pinned(Option<Pin>),
}

impl SealTo {
    /// What a seal through `client` trusts: the entry's own report over a connection
    /// the person chose, else the pin `pinned` reads for the Organization.
    ///
    /// # Errors
    ///
    /// As `pinned`, which runs only when the entry can't answer for itself.
    pub(crate) async fn of(
        client: &Client,
        pinned: impl AsyncFnOnce() -> Result<Option<Pin>, Failure>,
    ) -> Result<Self, Failure> {
        if Self::direct(client) {
            return Ok(Self::Entry);
        }
        Ok(Self::Pinned(pinned().await?))
    }

    /// Whether the person chose `client`'s connection and it reaches the Server's daemon
    /// over SSH or the local socket, which Cloud doesn't carry and which say who answers.
    /// A connection from Cloud's Server Access is Cloud's choice whatever its transport,
    /// and plain TCP authenticates no one.
    pub(crate) fn direct(client: &Client) -> bool {
        trusts(client.connection_source(), client.connection().transport())
    }

    /// The key to seal to through `client`. `relayed` is the key the Cluster's rows
    /// claim, which counts only when it is the pinned one.
    ///
    /// # Errors
    ///
    /// Refuses a key nothing here can check: a relayed key with no pin or another pin,
    /// or an entry that reports no key it holds. Nothing is sealed then.
    pub(crate) async fn key(
        &self,
        client: &mut Client,
        relayed: Option<SealingPublicKey>,
    ) -> Result<SealFor, Failure> {
        match self {
            Self::Entry => Ok(entry_pin(client).await?.seal_for()),
            Self::Pinned(pin) => {
                let Some(relayed) = relayed else {
                    return Err(Failure::coded(
                        RpcErrorCode::Unavailable,
                        "No online Server holds the Cluster's Sealing Key, so nothing can be \
                         sealed for it.",
                    ));
                };
                check(pin.as_ref(), relayed)
            }
        }
    }
}

fn trusts(source: &ConnectionSource, transport: &Transport) -> bool {
    !matches!(source, ConnectionSource::Cloud)
        && matches!(transport, Transport::Ssh { .. } | Transport::Unix(_))
}

/// `relayed`, a key Cloud passed on, when it is the one this computer pinned, sealing to the
/// recipient key the pin names. Cloud can hide that a Cluster activated, and everything
/// planted before activation holds its identity key, so a pin that names no recipient
/// seals nothing through Cloud.
///
/// # Errors
///
/// Refuses with `conflict` when nothing is pinned, the pin is another key, or it names no
/// recipient key, naming the command that pins it.
pub(crate) fn check(pin: Option<&Pin>, relayed: SealingPublicKey) -> Result<SealFor, Failure> {
    match pin {
        Some(Pin {
            public_key,
            recipient: Some(recipient),
            ..
        }) if *public_key == relayed => Ok(SealFor {
            cluster: *public_key,
            recipient: *recipient,
        }),
        Some(pin) if pin.public_key == relayed => Err(Failure::conflict(
            "This computer pinned your cluster's key before it was activated, so it can't tell \
             which key new values go to, and nothing was sealed. Activate your cluster with \
             `ployz device enforce`, or if it already is, pin its key again from a server.",
        )
        .hint(Hint::Next(PIN_COMMAND.to_owned()))),
        Some(pin) => Err(Failure::conflict(format!(
            "Ployz Cloud sent a Sealing Key other than the one this computer pinned from {}, \
             so nothing was sealed. If your Cluster's key changed, pin it again from a Server.",
            pin.server
        ))
        .hint(Hint::Next(PIN_COMMAND.to_owned()))),
        None => Err(Failure::conflict(
            "This computer hasn't pinned your Cluster's Sealing Key, so it can't check the key \
             Ployz Cloud sent, and nothing was sealed. Pin it once from a Server.",
        )
        .hint(Hint::Next(PIN_COMMAND.to_owned()))),
    }
}

/// The Sealing Key the entry Server reports holding, with its name.
///
/// # Errors
///
/// Fails when the Server doesn't answer, or holds no key it can open with.
pub(crate) async fn entry_pin(client: &mut Client) -> Result<Pin, Failure> {
    let details = client
        .call::<op::Inspect>(
            InspectRequest::default(),
            Call::read().waiting(Wait::standard()),
        )
        .await?;
    pin_from(details)
}

/// The pin an entry's own report gives. A Server that activated, or is activating, with no
/// recipient key gives none: everything planted before activation holds its identity key. A
/// Server born enforcing reports activated with no operation. Nothing was planted before it,
/// so its identity is where new values go, and the pin names it as the recipient.
fn pin_from(details: MachineDetails) -> Result<Pin, Failure> {
    let (activated, born_enforcing) =
        details
            .activation
            .as_ref()
            .map_or((false, false), |report| match report.state {
                ActivationState::Activating => (true, false),
                ActivationState::Activated => {
                    (report.operation.is_some(), report.operation.is_none())
                }
                ActivationState::Awaiting => (false, false),
            });
    match (details.sealing, details.machine) {
        (Some(Sealing::Held { .. }), Some(machine))
            if activated && details.sealing_recipient.is_none() =>
        {
            Err(Failure::conflict(format!(
                "Server {} is activated but holds no recipient key yet, so nothing can be \
                 sealed for it. Run `ployz device enforce` again to finish activating it.",
                machine.name
            ))
            .hint(Hint::Next("ployz device enforce".to_owned())))
        }
        (Some(Sealing::Held { public_key }), Some(machine)) => Ok(Pin {
            public_key,
            server: machine.name,
            recipient: details
                .sealing_recipient
                .or(born_enforcing.then_some(public_key)),
        }),
        (_, machine) => Err(Failure::conflict(format!(
            "Server {} holds no Sealing Key it can open with, so nothing can be sealed for it.",
            machine.map_or_else(
                || details.id.to_string(),
                |machine| machine.name.to_string()
            )
        ))
        .hint(Hint::Inspect("ployz status".to_owned()))),
    }
}

#[cfg(test)]
#[path = "sealing_pin_tests.rs"]
mod tests;
