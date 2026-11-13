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
    #[error(transparent)]
    Capacity(#[from] ployz_core::StorageCapacityError),
    /// Every other refusal and failure, written where it happens: what ZFS or the host
    /// answered, or the request that was invalid.
    #[error("{0}")]
    Message(String),
}

impl From<String> for PluginError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl From<ployzd::dataset_layout::InvalidVolumeName> for PluginError {
    fn from(error: ployzd::dataset_layout::InvalidVolumeName) -> Self {
        Self::Message(error.to_string())
    }
}

impl From<&str> for PluginError {
    fn from(message: &str) -> Self {
        Self::Message(message.to_owned())
    }
}

impl PluginError {
    /// The capacity refusal itself, or this error when it is anything else.
    pub(super) fn into_capacity(self) -> Result<ployz_core::StorageCapacityError, Self> {
        if let Self::Capacity(error) = self {
            Ok(error)
        } else {
            Err(self)
        }
    }

    pub(super) fn is_capacity(&self) -> bool {
        matches!(self, Self::Capacity(_))
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
