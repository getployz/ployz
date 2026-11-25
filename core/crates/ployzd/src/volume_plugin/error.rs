//! Why a plugin route refuses, with the text Docker and the Machine API show for it.

/// A refusal a route can make. Docker and the Machine API read only the text, so each
/// variant's text is part of the protocol and changes only on purpose.
#[derive(Debug, thiserror::Error)]
pub(super) enum PluginError {
    /// A Volume Run holds the root read-only, so a mount waits for the run to finish.
    #[error(
        "VolumeSwitching: Volume {name} is mid-run on this Machine (ZFS dataset {dataset} is read-only); wait for the run to finish"
    )]
    MidRun { name: String, dataset: String },
    /// A promoted root that Docker has not registered yet is not removed under it.
    #[error(
        "VolumeSwitching: Volume {name} is a promoted root Docker has not registered yet; refusing to remove {dataset}"
    )]
    UnregisteredRoot { name: String, dataset: String },
    /// The Machine held a Pool that is not imported, so no Volume is reported absent.
    #[error("this Machine has held a Pool that is not imported")]
    HeldPoolNotImported,
    /// The dataset carries no Provisioned Volume bound, so the plugin does not use it.
    #[error("ZFS dataset {dataset} has no Provisioned Volume bound; refusing to use it")]
    NotProvisioned { dataset: String },
    /// The Pool has no room, with what this Server's Trash holds when it holds anything.
    #[error("{0}{note}", note = trash_note(.1.as_ref()))]
    Capacity(
        ployz_core::StorageCapacityError,
        Option<super::trash::TrashHeld>,
    ),
    /// A Container that mounts the name expects other data, so no fresh root goes under it.
    #[error(
        "Volume {name} {}, so no new data is made under its name until that Container is removed",
        crate::volume_copy::HELD_FOR_OTHER_DATA
    )]
    HeldForOtherData { name: String },
    /// A root restored from the Trash carries an origin other than the one Docker's Create
    /// asks for, so it stays hidden.
    #[error(
        "Volume {name} was restored from the Trash holding other data than this Deploy expects, so Docker doesn't get it; deploy the Volume it held"
    )]
    RestoredForOtherData { name: String },
    /// Docker did not list the Containers that mount the name, so no fresh root goes under
    /// it yet.
    #[error(
        "{}, so no new data is made under Volume {name} until it does: {cause}",
        crate::volume_copy::DOCKER_NOT_ANSWERING
    )]
    HoldersUnlisted { name: String, cause: String },
    /// A dataset carries an origin this Machine did not stamp, or one it cannot read.
    #[error("{0}")]
    Unstamped(String),
    /// Every other refusal and failure, written where it happens: what ZFS or the host
    /// answered, or the request that was invalid.
    #[error("{0}")]
    Message(String),
}

fn trash_note(trash: Option<&super::trash::TrashHeld>) -> String {
    trash.map(|trash| format!(". {trash}")).unwrap_or_default()
}

impl From<ployz_core::StorageCapacityError> for PluginError {
    fn from(error: ployz_core::StorageCapacityError) -> Self {
        Self::Capacity(error, None)
    }
}

impl From<String> for PluginError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl From<crate::dataset_layout::InvalidVolumeName> for PluginError {
    fn from(error: crate::dataset_layout::InvalidVolumeName) -> Self {
        Self::Message(error.to_string())
    }
}

impl From<&str> for PluginError {
    fn from(message: &str) -> Self {
        Self::Message(message.to_owned())
    }
}

/// A capacity refusal answers with its own code, and every other refusal as internal.
impl From<PluginError> for ployz_core::RpcError {
    fn from(error: PluginError) -> Self {
        if let PluginError::Capacity(capacity, trash) = error {
            Self::refused(
                &capacity,
                format!("{capacity}{}", trash_note(trash.as_ref())),
            )
        } else {
            Self::internal(error.to_string())
        }
    }
}

impl From<crate::dataset_layout::InvalidVolumeName> for ployz_core::RpcError {
    fn from(error: crate::dataset_layout::InvalidVolumeName) -> Self {
        PluginError::from(error).into()
    }
}

impl PluginError {
    pub(super) fn is_capacity(&self) -> bool {
        matches!(self, Self::Capacity(..))
    }

    /// The sentence naming what the Trash holds, after a capacity refusal; empty otherwise.
    pub(super) fn trash_note(&self) -> String {
        if let Self::Capacity(_, trash) = self {
            trash_note(trash.as_ref())
        } else {
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PluginError;

    #[test]
    fn each_typed_refusal_keeps_the_text_it_had_as_a_string() {
        let name = "data".to_owned();
        let dataset = "tank/ployz/data".to_owned();
        for (error, text) in [
            (
                PluginError::MidRun {
                    name: name.clone(),
                    dataset: dataset.clone(),
                },
                "VolumeSwitching: Volume data is mid-run on this Machine (ZFS dataset tank/ployz/data is read-only); wait for the run to finish",
            ),
            (
                PluginError::UnregisteredRoot {
                    name,
                    dataset: dataset.clone(),
                },
                "VolumeSwitching: Volume data is a promoted root Docker has not registered yet; refusing to remove tank/ployz/data",
            ),
            (
                PluginError::HeldPoolNotImported,
                "this Machine has held a Pool that is not imported",
            ),
            (
                PluginError::NotProvisioned { dataset },
                "ZFS dataset tank/ployz/data has no Provisioned Volume bound; refusing to use it",
            ),
            (PluginError::from("anything else"), "anything else"),
        ] {
            assert_eq!(error.to_string(), text);
        }
    }
}
