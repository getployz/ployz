//! The Config Store: all authored configuration and its history, behind one
//! synchronous interface. `read` answers a [`Query`] and `write` applies a
//! [`Command`], each in one transaction; in-process callers use the typed method
//! for each, which runs the same code. Storage is its only I/O.

pub mod catalog;
mod command;
mod error;
mod id;
mod query;
mod review;
mod scope;
mod settings;
mod storage;

use ployz_core::RpcError;

pub use command::*;
pub use id::*;
pub use query::*;
pub use review::{DiffView, NodeChange};
pub use scope::{EnvironmentRef, EnvironmentSummary};
pub use settings::{Apply, SettingPath};

/// Who is asking, and in which Organization. Every read and write is scoped to it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Actor {
    /// The Organization whose configuration it reads and writes.
    pub organization: OrganizationId,
}

/// One Config Store over one database.
pub struct ConfigStore {
    storage: storage::Storage,
}

impl ConfigStore {
    /// Open the Store at `url` (`sqlite:PATH`, or `sqlite::memory:` for tests),
    /// creating and migrating its tables as needed.
    ///
    /// # Errors
    /// Returns `invalid_argument` for an unsupported URL, or a storage error.
    pub fn open(url: &str) -> Result<Self, RpcError> {
        Ok(Self {
            storage: storage::Storage::open(url)?,
        })
    }

    /// Answer `query` from one consistent state.
    ///
    /// # Errors
    /// Returns an RPC error: `not_found`, `ambiguous` or `invalid_argument` for what the
    /// query names, or a storage error.
    pub fn read(&self, who: &Actor, query: &Query) -> Result<View, RpcError> {
        self.storage.read(|tx| query::run(tx, who, query))
    }

    /// Apply `command` in one transaction: all of it, or none.
    ///
    /// # Errors
    /// Returns an RPC error: `invalid_argument`, `not_found`, `ambiguous` or `conflict`
    /// for what the command asks, or a storage error.
    pub fn write(&self, who: &Actor, command: &Command) -> Result<Written, RpcError> {
        self.storage.write(|tx| command::run(tx, who, command))
    }

    /// [`Query::Environment`]: an Environment's Settings.
    ///
    /// # Errors
    /// As [`read`](Self::read).
    pub fn environment(
        &self,
        who: &Actor,
        query: &EnvironmentQuery,
    ) -> Result<EnvironmentView, RpcError> {
        self.storage.read(|tx| query::environment(tx, who, query))
    }

    /// [`Command::CreateProject`].
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn create_project(
        &self,
        who: &Actor,
        create: &CreateProject,
    ) -> Result<ProjectCreated, RpcError> {
        self.storage
            .write(|tx| command::create_project(tx, who, create))
    }

    /// [`Command::CreateEnvironment`].
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn create_environment(
        &self,
        who: &Actor,
        create: &CreateEnvironment,
    ) -> Result<EnvironmentCreated, RpcError> {
        self.storage
            .write(|tx| command::create_environment(tx, who, create))
    }

    /// [`Command::CreateService`].
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn create_service(
        &self,
        who: &Actor,
        create: &CreateService,
    ) -> Result<ServiceCreated, RpcError> {
        self.storage
            .write(|tx| command::create_service(tx, who, create))
    }

    /// [`Command::Edit`].
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn edit(&self, who: &Actor, edit: &Edit) -> Result<Edited, RpcError> {
        self.storage.write(|tx| command::edit(tx, who, edit))
    }

    /// [`Query::Diff`]: an Environment's changes and the version to act on them by.
    ///
    /// # Errors
    /// As [`read`](Self::read).
    pub fn diff(&self, who: &Actor, query: &DiffQuery) -> Result<DiffView, RpcError> {
        self.storage.read(|tx| query::diff(tx, who, query))
    }

    /// [`Command::Publish`].
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn publish(&self, who: &Actor, publish: &Publish) -> Result<Published, RpcError> {
        self.storage.write(|tx| command::publish(tx, who, publish))
    }

    /// [`Command::Discard`].
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn discard(&self, who: &Actor, discard: &Discard) -> Result<Discarded, RpcError> {
        self.storage.write(|tx| command::discard(tx, who, discard))
    }
}
