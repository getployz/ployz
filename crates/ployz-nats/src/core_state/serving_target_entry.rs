use super::{AsyncNatsCoreStateStore, CoreStateStoreError};
use crate::kv::{list_current, with_io_timeout};
use ployz_core::ids::ServiceId;
use ployz_core::state::{SERVING_TARGET_ENTRY_PREFIX, ServingTargetEntry, ActiveServiceStateKey};

impl AsyncNatsCoreStateStore {
    pub async fn replace_serving_target_entry(
        &self,
        state: &ServingTargetEntry,
    ) -> Result<(), CoreStateStoreError> {
        let key = ActiveServiceStateKey::from_service_id(&state.service_id);
        let payload = serde_json::to_vec(state).map_err(CoreStateStoreError::Encode)?;
        with_io_timeout(
            "serving target entry state replace",
            self.bucket.put(key.as_str(), payload.into()),
        )
        .await?
        .map_err(|error| CoreStateStoreError::Put {
            key: key.as_str().to_owned(),
            message: error.to_string(),
        })?;

        Ok(())
    }

    pub async fn serving_target_entry(
        &self,
        service_id: &ServiceId,
    ) -> Result<Option<ServingTargetEntry>, CoreStateStoreError> {
        let key = ActiveServiceStateKey::from_service_id(service_id);
        let Some(payload) =
            with_io_timeout("serving target entry state get", self.bucket.get(key.as_str()))
                .await?
                .map_err(|error| CoreStateStoreError::Get {
                    key: key.as_str().to_owned(),
                    message: error.to_string(),
                })?
        else {
            return Ok(None);
        };

        decode_active_service_state(service_id, &key, &payload).map(Some)
    }

    pub async fn remove_serving_target_entry(
        &self,
        service_id: &ServiceId,
    ) -> Result<(), CoreStateStoreError> {
        if self.serving_target_entry(service_id).await?.is_none() {
            return Ok(());
        }

        let key = ActiveServiceStateKey::from_service_id(service_id);
        with_io_timeout(
            "serving target entry state delete",
            self.bucket.delete(key.as_str()),
        )
        .await?
        .map_err(|error| CoreStateStoreError::Delete {
            key: key.as_str().to_owned(),
            message: error.to_string(),
        })?;

        Ok(())
    }

    pub async fn serving_target_entries(&self) -> Result<Vec<ServingTargetEntry>, CoreStateStoreError> {
        list_current(
            &self.bucket,
            &format!("{SERVING_TARGET_ENTRY_PREFIX}."),
            |state: &ServingTargetEntry| {
                ActiveServiceStateKey::from_service_id(&state.service_id)
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
    expected_service_id: &ServiceId,
    key: &ActiveServiceStateKey,
    payload: &[u8],
) -> Result<ServingTargetEntry, CoreStateStoreError> {
    let state: ServingTargetEntry =
        serde_json::from_slice(payload).map_err(CoreStateStoreError::Decode)?;
    if state.service_id != *expected_service_id {
        return Err(CoreStateStoreError::CorruptServingTargetEntry {
            key: key.as_str().to_owned(),
            expected_service_id: expected_service_id.clone(),
            actual_service_id: state.service_id,
        });
    }

    Ok(state)
}
