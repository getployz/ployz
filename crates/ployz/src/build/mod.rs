//! Image build command and runtime.

pub mod command;
pub(crate) mod embedded_executor;
pub mod enrollment;
pub mod executor_context;
pub(crate) mod external_runtime;
mod external_service;
pub(crate) mod github_actions;
pub(crate) mod runtime;
mod watch_lifecycle;

pub(crate) fn deserialize_credential_expiry<'de, D>(
    deserializer: D,
) -> Result<ployz_core::nats_config::BuildExecutorCredentialExpiresAt, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <String as serde::Deserialize>::deserialize(deserializer)?;
    let timestamp =
        time::OffsetDateTime::parse(&value, &time::format_description::well_known::Rfc3339)
            .map_err(serde::de::Error::custom)?
            .unix_timestamp();
    let timestamp = u64::try_from(timestamp).map_err(serde::de::Error::custom)?;
    ployz_core::nats_config::BuildExecutorCredentialExpiresAt::try_new(timestamp)
        .map_err(serde::de::Error::custom)
}
