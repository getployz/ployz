use super::{AsyncNatsCoreStateStore, CoreStateStoreError};
use crate::kv::{list_current, with_io_timeout};
use ployz_core::ids::{NamespaceId, ServiceId};
use ployz_core::state::{ACTIVE_SERVICE_STATE_PREFIX, ActiveServiceState, ActiveServiceStateKey};

impl AsyncNatsCoreStateStore {
    pub async fn replace_active_service(
        &self,
        state: &ActiveServiceState,
    ) -> Result<(), CoreStateStoreError> {
        let key =
            ActiveServiceStateKey::from_namespace_service(&state.namespace_id, &state.service_id);
        let payload = serde_json::to_vec(state).map_err(CoreStateStoreError::Encode)?;
        with_io_timeout(
            "active service state replace",
            self.bucket.put(key.as_str(), payload.into()),
        )
        .await?
        .map_err(|error| CoreStateStoreError::Put {
            key: key.as_str().to_owned(),
            message: error.to_string(),
        })?;

        Ok(())
    }

    pub async fn active_service(
        &self,
        namespace_id: &NamespaceId,
        service_id: &ServiceId,
    ) -> Result<Option<ActiveServiceState>, CoreStateStoreError> {
        let key = ActiveServiceStateKey::from_namespace_service(namespace_id, service_id);
        let Some(payload) =
            with_io_timeout("active service state get", self.bucket.get(key.as_str()))
                .await?
                .map_err(|error| CoreStateStoreError::Get {
                    key: key.as_str().to_owned(),
                    message: error.to_string(),
                })?
        else {
            return Ok(None);
        };

        decode_active_service_state(namespace_id, service_id, &key, &payload).map(Some)
    }

    pub async fn remove_active_service(
        &self,
        namespace_id: &NamespaceId,
        service_id: &ServiceId,
    ) -> Result<(), CoreStateStoreError> {
        if self
            .active_service(namespace_id, service_id)
            .await?
            .is_none()
        {
            return Ok(());
        }

        let key = ActiveServiceStateKey::from_namespace_service(namespace_id, service_id);
        with_io_timeout(
            "active service state delete",
            self.bucket.delete(key.as_str()),
        )
        .await?
        .map_err(|error| CoreStateStoreError::Delete {
            key: key.as_str().to_owned(),
            message: error.to_string(),
        })?;

        Ok(())
    }

    pub async fn active_services(&self) -> Result<Vec<ActiveServiceState>, CoreStateStoreError> {
        list_current(
            &self.bucket,
            &format!("{ACTIVE_SERVICE_STATE_PREFIX}."),
            |state: &ActiveServiceState| {
                ActiveServiceStateKey::from_namespace_service(
                    &state.namespace_id,
                    &state.service_id,
                )
                .as_str()
                .to_owned()
            },
            |state| state.service_id.clone(),
        )
        .await
        .map_err(CoreStateStoreError::from)
    }
}

fn decode_active_service_state(
    expected_namespace_id: &NamespaceId,
    expected_service_id: &ServiceId,
    key: &ActiveServiceStateKey,
    payload: &[u8],
) -> Result<ActiveServiceState, CoreStateStoreError> {
    let state: ActiveServiceState =
        serde_json::from_slice(payload).map_err(CoreStateStoreError::Decode)?;
    if state.namespace_id != *expected_namespace_id {
        return Err(CoreStateStoreError::CorruptActiveServiceState {
            key: key.as_str().to_owned(),
            expected_service_id: expected_service_id.clone(),
            actual_service_id: state.service_id,
        });
    }
    if state.service_id != *expected_service_id {
        return Err(CoreStateStoreError::CorruptActiveServiceState {
            key: key.as_str().to_owned(),
            expected_service_id: expected_service_id.clone(),
            actual_service_id: state.service_id,
        });
    }

    Ok(state)
}
