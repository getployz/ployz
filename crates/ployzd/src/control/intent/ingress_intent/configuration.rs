pub use ployz_core::ingress::IngressConfiguration;
use rusqlite::Connection;

use crate::control::store::{CoreStore, CoreStoreError, query_json, to_json};

const SINGLETON_ID: &str = "1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressConfigurationWrite {
    Stored,
    Unchanged,
}

#[derive(Debug, Clone)]
pub struct IngressIntentStore {
    store: CoreStore,
}

impl IngressIntentStore {
    #[must_use]
    pub fn new(store: CoreStore) -> Self {
        Self { store }
    }

    pub async fn load(&self) -> Result<Option<IngressConfiguration>, IngressIntentStoreError> {
        self.store
            .call(|conn| load_configuration(conn))
            .await
            .map_err(IngressIntentStoreError::Store)
    }

    pub async fn replace(
        &self,
        configuration: IngressConfiguration,
    ) -> Result<IngressConfigurationWrite, IngressIntentStoreError> {
        let outcome = self
            .store
            .call(move |conn| replace_configuration(conn, &configuration))
            .await
            .map_err(IngressIntentStoreError::Store)?;
        match outcome {
            ReplaceConfigurationOutcome::Written(write) => Ok(write),
            ReplaceConfigurationOutcome::Invalid { message } => {
                Err(IngressIntentStoreError::InvalidConfiguration { message })
            }
        }
    }

    pub async fn validate_replace(
        &self,
        configuration: &IngressConfiguration,
    ) -> Result<(), IngressIntentStoreError> {
        let configuration = configuration.clone();
        let rejection = self
            .store
            .call(move |conn| replace_rejection(conn, &configuration))
            .await
            .map_err(IngressIntentStoreError::Store)?;
        match rejection {
            Some(message) => Err(IngressIntentStoreError::InvalidConfiguration { message }),
            None => Ok(()),
        }
    }
}

enum ReplaceConfigurationOutcome {
    Written(IngressConfigurationWrite),
    Invalid { message: String },
}

fn replace_configuration(
    conn: &mut Connection,
    configuration: &IngressConfiguration,
) -> Result<ReplaceConfigurationOutcome, rusqlite::Error> {
    if let Some(message) = replace_rejection(conn, configuration)? {
        return Ok(ReplaceConfigurationOutcome::Invalid { message });
    }
    let transaction = conn.transaction()?;
    let current = load_configuration(&transaction)?;
    if current.as_ref() == Some(configuration) {
        return Ok(ReplaceConfigurationOutcome::Written(
            IngressConfigurationWrite::Unchanged,
        ));
    }
    transaction.execute(
        "INSERT INTO automatic_hostname_intent (id, json) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET json = excluded.json",
        [to_json(configuration.automatic_hostnames())?],
    )?;
    transaction.execute(
        "INSERT INTO ployz_dns_target_intent (id, json) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET json = excluded.json",
        [to_json(&configuration.ployz_dns_target())?],
    )?;
    transaction.commit()?;
    Ok(ReplaceConfigurationOutcome::Written(
        IngressConfigurationWrite::Stored,
    ))
}

fn replace_rejection(
    conn: &Connection,
    configuration: &IngressConfiguration,
) -> Result<Option<String>, rusqlite::Error> {
    let current = load_configuration(conn)?;
    if current
        .as_ref()
        .is_some_and(|current| current.automatic_hostnames() != configuration.automatic_hostnames())
        && has_automatic_route_bindings(conn)?
    {
        return Ok(Some("automatic route bindings exist".to_owned()));
    }
    Ok(None)
}

fn load_configuration(conn: &Connection) -> Result<Option<IngressConfiguration>, rusqlite::Error> {
    let automatic_hostnames = query_json(
        conn,
        "SELECT json FROM automatic_hostname_intent WHERE id = ?1",
        SINGLETON_ID,
    )?;
    let ployz_dns_target = query_json(
        conn,
        "SELECT json FROM ployz_dns_target_intent WHERE id = ?1",
        SINGLETON_ID,
    )?;
    match (automatic_hostnames, ployz_dns_target) {
        (None, None) => Ok(None),
        (Some(automatic_hostnames), Some(ployz_dns_target)) => {
            IngressConfiguration::try_new(automatic_hostnames, ployz_dns_target)
                .map(Some)
                .map_err(|_| rusqlite::Error::InvalidQuery)
        }
        (Some(_), None) | (None, Some(_)) => Err(rusqlite::Error::InvalidQuery),
    }
}

fn has_automatic_route_bindings(conn: &Connection) -> Result<bool, rusqlite::Error> {
    conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM route_bindings
            WHERE json_extract(json, '$.origin') = 'automatic'
        )",
        [],
        |row| row.get(0),
    )
}

#[derive(Debug, thiserror::Error)]
pub enum IngressIntentStoreError {
    #[error("ingress intent store: {0}")]
    Store(CoreStoreError),
    #[error("invalid ingress configuration: {message}")]
    InvalidConfiguration { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::ingress::{AutomaticHostnameConfiguration, PloyzDnsTargetIntent};

    fn configuration(
        automatic_hostnames: AutomaticHostnameConfiguration,
        ployz_dns_target: PloyzDnsTargetIntent,
    ) -> IngressConfiguration {
        IngressConfiguration::try_new(automatic_hostnames, ployz_dns_target)
            .expect("valid ingress configuration")
    }

    #[tokio::test]
    async fn configuration_write_commits_both_decisions_atomically() {
        let store = IngressIntentStore::new(CoreStore::open_in_memory().await.expect("store"));
        let expected = configuration(
            AutomaticHostnameConfiguration::Ployz,
            PloyzDnsTargetIntent::Enabled,
        );

        store.replace(expected.clone()).await.expect("store config");

        assert_eq!(store.load().await.expect("load config"), Some(expected));
    }

    #[tokio::test]
    async fn configuration_write_preserves_namespace_used_by_automatic_bindings() {
        let core = CoreStore::open_in_memory().await.expect("store");
        let store = IngressIntentStore::new(core.clone());
        store
            .replace(configuration(
                AutomaticHostnameConfiguration::Ployz,
                PloyzDnsTargetIntent::Enabled,
            ))
            .await
            .expect("store config");
        core.call(|conn| {
            conn.execute(
                "INSERT INTO route_bindings (hostname, route_binding_id, json)
                 VALUES ('api.example.com', 'route_1', '{\"origin\":\"automatic\"}')",
                [],
            )?;
            Ok(())
        })
        .await
        .expect("store binding");

        let error = store
            .replace(configuration(
                AutomaticHostnameConfiguration::Disabled,
                PloyzDnsTargetIntent::Enabled,
            ))
            .await
            .expect_err("namespace remains in use");

        assert!(matches!(
            error,
            IngressIntentStoreError::InvalidConfiguration { .. }
        ));
    }

    #[tokio::test]
    async fn configuration_preflight_rejects_namespace_used_by_automatic_bindings() {
        let core = CoreStore::open_in_memory().await.expect("store");
        let store = IngressIntentStore::new(core.clone());
        store
            .replace(configuration(
                AutomaticHostnameConfiguration::Ployz,
                PloyzDnsTargetIntent::Enabled,
            ))
            .await
            .expect("store config");
        core.call(|conn| {
            conn.execute(
                "INSERT INTO route_bindings (hostname, route_binding_id, json)
                 VALUES ('api.example.com', 'route_1', '{\"origin\":\"automatic\"}')",
                [],
            )?;
            Ok(())
        })
        .await
        .expect("store binding");

        let error = store
            .validate_replace(&configuration(
                AutomaticHostnameConfiguration::Disabled,
                PloyzDnsTargetIntent::Enabled,
            ))
            .await
            .expect_err("namespace remains in use");

        assert!(matches!(
            error,
            IngressIntentStoreError::InvalidConfiguration { .. }
        ));
        assert_eq!(
            store.load().await.expect("load config"),
            Some(configuration(
                AutomaticHostnameConfiguration::Ployz,
                PloyzDnsTargetIntent::Enabled,
            ))
        );
    }
}
