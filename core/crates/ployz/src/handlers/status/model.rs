//! What `ployz status` says needs someone to act: an [`Attention`] and the [`Subject`] it is about.
//! The Server check and the link handler build them; neither imports the other.

use ployz_core::{
    ContainerAddress, ContainerId, ContainerRun, ContainerRuntimeObservation, CopyRotation,
    MachineName, QualifiedService,
};
use ployz_store::DeploymentId;
use serde::Serialize;

/// Something that needs someone to act, with the command that acts.
#[derive(Serialize)]
pub(in crate::handlers) struct Attention {
    reason: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    deployment: Option<DeploymentId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<String>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    subject: Option<Subject>,
}

impl Attention {
    /// `reason` and `message` are authored where the fact is found.
    pub(in crate::handlers) fn new(
        subject: Option<Subject>,
        reason: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            reason: reason.into(),
            message: message.into(),
            deployment: None,
            next: None,
            subject,
        }
    }

    pub(in crate::handlers) fn deployment(mut self, deployment: DeploymentId) -> Self {
        self.deployment = Some(deployment);
        self
    }

    /// The command that acts.
    pub(in crate::handlers) fn next(mut self, next: impl Into<String>) -> Self {
        self.next = Some(next.into());
        self
    }

    #[cfg(test)]
    pub(in crate::handlers) fn reason(&self) -> &str {
        &self.reason
    }

    pub(in crate::handlers) fn message(&self) -> &str {
        &self.message
    }

    pub(in crate::handlers) fn command(&self) -> Option<&str> {
        self.next.as_deref()
    }

    pub(in crate::handlers) fn subject(&self) -> Option<&Subject> {
        self.subject.as_ref()
    }
}

#[derive(Serialize)]
#[serde(untagged)]
pub(in crate::handlers) enum Subject {
    NotPickedUp {
        service: QualifiedService,
        /// Each such Machine by name, else by ID when the Machine list lacks it.
        machines: Vec<String>,
    },
    NotServing {
        service: QualifiedService,
        serving: usize,
        wanted: usize,
        copies: Vec<NotServingCopy>,
    },
    LeftPending {
        service: QualifiedService,
        copies: Vec<NotServingCopy>,
    },
    Down {
        machine: MachineName,
    },
    DiskFull {
        machine: MachineName,
        free_bytes: u64,
    },
    NoCopies {
        service: QualifiedService,
    },
    StaleAddress {
        service: QualifiedService,
        machine: String,
        addresses: Vec<ContainerAddress>,
    },
    /// The Servers that didn't answer, by name where known.
    Unanswered {
        servers: Vec<String>,
    },
}

#[derive(Serialize)]
pub(in crate::handlers) struct NotServingCopy {
    pub(in crate::handlers) machine: String,
    pub(in crate::handlers) container: ContainerId,
    pub(in crate::handlers) rotation: CopyRotation,
    pub(in crate::handlers) runtime: ContainerRuntimeObservation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::handlers) run: Option<ContainerRun>,
}
