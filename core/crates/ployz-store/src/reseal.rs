//! The one-time conversion of secrets the Store sealed itself into values sealed
//! for the Cluster, which only its Machines open. The Store is the only holder
//! of the key that opens them, so the conversion runs here.

use ployz_core::RpcError;
use ployz_core::config::{
    SavedEnvironmentIntent, SavedVariableValue, SealedCell, SecretCiphertext,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::client_sealed::Sealer;
use crate::sealing::SealingKey;
use crate::storage::Tx;
use crate::{EnvironmentId, OrganizationId, SealingRecipient, error};

/// How many secrets the Store sealed itself an Organization still holds.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct LegacyCount {
    /// In Working State and the Saved head of every Environment: what a reseal
    /// converts.
    pub current: u32,
    /// In older Saved revisions, pending arrivals and merge values: kept as they
    /// are, and Not set should one of them ever apply.
    pub history: u32,
}

/// What one Environment's reseal did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Resealed {
    /// Values sealed for the Cluster.
    pub resealed: u32,
    /// Values the Store's key doesn't open, left in place.
    pub remaining: u32,
}

/// Organizations with a secret the Store sealed itself in some Working State or
/// Saved head.
pub(crate) fn legacy_organizations(tx: &mut dyn Tx) -> Result<Vec<OrganizationId>, RpcError> {
    let mut organizations = Vec::new();
    for row in tx.query(
        "SELECT organization_id FROM config_environment GROUP BY organization_id ORDER BY organization_id",
        &[],
    )? {
        let organization = OrganizationId::parse(row.text(0)?)?;
        if current_by_environment(tx, &organization)?
            .iter()
            .any(|(_, current)| *current > 0)
        {
            organizations.push(organization);
        }
    }
    Ok(organizations)
}

/// How many secrets the Store sealed itself `organization` holds, by whether a
/// reseal converts them.
pub(crate) fn legacy_count(
    tx: &mut dyn Tx,
    organization: &OrganizationId,
) -> Result<LegacyCount, RpcError> {
    let mut count = LegacyCount::default();
    for (_, current) in current_by_environment(tx, organization)? {
        count.current += current;
    }
    for row in tx.query(
        "SELECT intent FROM config_saved s WHERE organization_id = ?1 \
         AND revision < (SELECT MAX(l.revision) FROM config_saved l WHERE l.environment_id = s.environment_id)",
        &[organization.as_str().into()],
    )? {
        count.history += legacy_in(&row.intent(0, "Saved revision")?);
    }
    for (table, column) in [
        ("config_sync_arrival", "was"),
        ("config_merge_value", "cell"),
    ] {
        for row in tx.query(
            &format!(
                "SELECT {column} FROM {table} WHERE organization_id = ?1 AND {column} IS NOT NULL"
            ),
            &[organization.as_str().into()],
        )? {
            let cell: SealedCell = row.json(0, "a sealed cell")?;
            if matches!(cell, SealedCell::Secret(secret) if is_legacy(&secret.value)) {
                count.history += 1;
            }
        }
    }
    Ok(count)
}

/// The Environments of `organization` a reseal has work in.
pub(crate) fn reseal_environments(
    tx: &mut dyn Tx,
    organization: &OrganizationId,
) -> Result<Vec<EnvironmentId>, RpcError> {
    Ok(current_by_environment(tx, organization)?
        .into_iter()
        .filter(|(_, current)| *current > 0)
        .map(|(environment, _)| environment)
        .collect())
}

/// Each Environment of `organization` with how many secrets the Store sealed
/// itself its Working State and Saved head hold.
fn current_by_environment(
    tx: &mut dyn Tx,
    organization: &OrganizationId,
) -> Result<Vec<(EnvironmentId, u32)>, RpcError> {
    let mut environments = Vec::new();
    for row in tx.query(
        "SELECT e.id, e.working, \
         (SELECT s.intent FROM config_saved s WHERE s.environment_id = e.id ORDER BY s.revision DESC LIMIT 1) \
         FROM config_environment e WHERE e.organization_id = ?1 ORDER BY e.id",
        &[organization.as_str().into()],
    )? {
        let mut current = legacy_in(&row.intent(1, "Working State")?);
        if row.optional_text(2)?.is_some() {
            current += legacy_in(&row.intent(2, "Saved revision")?);
        }
        environments.push((EnvironmentId::parse(row.text(0)?)?, current));
    }
    Ok(environments)
}

/// Seal every secret of `environment` the Store sealed itself for `recipient`
/// instead, in its Working State and Saved head. Values already sealed for the
/// Cluster are left alone, so running it again changes nothing. A value `sealing`
/// doesn't open stays as it is and is counted.
pub(crate) fn reseal(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    recipient: &SealingRecipient,
    sealing: &SealingKey,
) -> Result<Resealed, RpcError> {
    let rows = tx.query(
        "SELECT organization_id, working FROM config_environment WHERE id = ?1",
        &[environment.as_str().into()],
    )?;
    let Some(row) = rows.first() else {
        return Err(error::not_found(
            "Environment not found",
            json!({ "environment": environment.as_str() }),
        ));
    };
    let organization = OrganizationId::parse(row.text(0)?)?;
    let sealer = Sealer {
        organization: &organization,
        recipient: Some(recipient),
    };
    let mut tally = Resealed::default();
    let mut working = row.intent(1, "Working State")?;
    if reseal_intent(&mut working, environment, sealer, sealing, &mut tally)? {
        tx.execute(
            "UPDATE config_environment SET working = ?1 WHERE id = ?2",
            &[
                serde_json::to_string(&working)
                    .map_err(|_| error::internal("Working State did not serialize"))?
                    .as_str()
                    .into(),
                environment.as_str().into(),
            ],
        )?;
    }
    let head = tx.query(
        "SELECT revision, intent FROM config_saved WHERE environment_id = ?1 ORDER BY revision DESC LIMIT 1",
        &[environment.as_str().into()],
    )?;
    if let Some(row) = head.first() {
        let mut intent = row.intent(1, "Saved revision")?;
        if reseal_intent(&mut intent, environment, sealer, sealing, &mut tally)? {
            tx.execute(
                "UPDATE config_saved SET intent = ?3 WHERE environment_id = ?1 AND revision = ?2",
                &[
                    environment.as_str().into(),
                    row.int(0)?.into(),
                    serde_json::to_string(&intent)
                        .map_err(|_| error::internal("Saved revision did not serialize"))?
                        .as_str()
                        .into(),
                ],
            )?;
        }
    }
    Ok(tally)
}

/// Reseal `intent` in place; whether anything in it changed.
fn reseal_intent(
    intent: &mut SavedEnvironmentIntent,
    environment: &EnvironmentId,
    sealer: Sealer<'_>,
    sealing: &SealingKey,
    tally: &mut Resealed,
) -> Result<bool, RpcError> {
    let mut changed = false;
    for service in &mut intent.services {
        let service_id = service.id.clone();
        for variable in &mut service.variables {
            let SavedVariableValue::Secret {
                encrypted_value: Some(SecretCiphertext::Legacy(legacy)),
            } = &variable.value
            else {
                continue;
            };
            let Ok(plaintext) = sealing.open(legacy) else {
                tally.remaining += 1;
                continue;
            };
            let sealed = sealer.seal(environment, &service_id, &variable.key, &plaintext)?;
            variable.value_fingerprint = sealed.fingerprint;
            variable.value = SavedVariableValue::Secret {
                encrypted_value: Some(sealed.value),
            };
            tally.resealed += 1;
            changed = true;
        }
    }
    Ok(changed)
}

fn legacy_in(intent: &SavedEnvironmentIntent) -> u32 {
    let count = intent
        .services
        .iter()
        .flat_map(|service| &service.variables)
        .filter(|variable| {
            matches!(
                &variable.value,
                SavedVariableValue::Secret { encrypted_value: Some(value) } if is_legacy(value)
            )
        })
        .count();
    u32::try_from(count).expect("an Environment holds fewer than u32::MAX variables")
}

const fn is_legacy(value: &SecretCiphertext) -> bool {
    matches!(value, SecretCiphertext::Legacy(_))
}

#[cfg(test)]
mod tests {
    use ployz_core::config::{
        Cell, SavedVariableIntent, SavedVariableValue, SealedCell, SealedSecret, SecretCiphertext,
    };
    use ployz_core::{SealedIdentity, ServiceName};
    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::{
        Actor, ConfigStore, CreateProject, CreateService, EnvironmentRef, ProjectId, ProjectName,
        ServiceLineageId,
    };

    const ORGANIZATION: &str = "org_reseal";

    fn uuid(n: u8) -> String {
        format!("00000000-0000-4000-8000-0000000000{n:02}")
    }

    fn environment() -> EnvironmentId {
        EnvironmentId::parse(uuid(2)).unwrap()
    }

    /// `org_reseal` with Service `web` in its one Environment.
    fn shop() -> (ConfigStore, Actor) {
        let store =
            ConfigStore::open("sqlite::memory:", SealingKey::new(b"test").unwrap()).unwrap();
        let who = Actor::system(OrganizationId::parse(ORGANIZATION).unwrap());
        store
            .write(
                &who,
                &CreateProject {
                    id: ProjectId::parse(uuid(1)).unwrap(),
                    name: ProjectName::parse("shop").unwrap(),
                    default_environment: environment(),
                },
            )
            .unwrap();
        store
            .write(
                &who,
                &CreateService {
                    id: ServiceLineageId::parse(uuid(3)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse("web").unwrap(),
                    image: Some("nginx:1".into()),
                    template: None,
                },
            )
            .unwrap();
        (store, who)
    }

    fn secret(key: &str, value: SecretCiphertext) -> SavedVariableIntent {
        SavedVariableIntent {
            id: Uuid::new_v4().to_string(),
            key: key.into(),
            description: None,
            exported: false,
            value_fingerprint: "s1:old".into(),
            value: SavedVariableValue::Secret {
                encrypted_value: Some(value),
            },
        }
    }

    fn legacy(sealing: &SealingKey, plaintext: &str) -> SecretCiphertext {
        SecretCiphertext::Legacy(sealing.seal(plaintext))
    }

    /// Plant `variables` on `web` in Working State.
    fn plant_working(store: &ConfigStore, who: &Actor, variables: Vec<SavedVariableIntent>) {
        crate::rules::tests::seed(store, who, |working| {
            working
                .services
                .first_mut()
                .unwrap()
                .variables
                .extend(variables);
        });
    }

    /// Plant Working State as Saved `revision`.
    fn plant_saved(
        store: &ConfigStore,
        revision: i64,
        change: impl FnOnce(&mut SavedEnvironmentIntent),
    ) {
        store
            .storage
            .write(|tx| {
                let rows = tx.query(
                    "SELECT working FROM config_environment WHERE id = ?1",
                    &[environment().as_str().into()],
                )?;
                let mut intent = rows.first().unwrap().intent(0, "Working State")?;
                change(&mut intent);
                tx.execute(
                    "INSERT INTO config_saved (environment_id, revision, organization_id, intent) VALUES (?1, ?2, ?3, ?4)",
                    &[
                        environment().as_str().into(),
                        revision.into(),
                        ORGANIZATION.into(),
                        serde_json::to_string(&intent).unwrap().as_str().into(),
                    ],
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn working(store: &ConfigStore) -> SavedEnvironmentIntent {
        store
            .storage
            .read(|tx| {
                tx.query(
                    "SELECT working FROM config_environment WHERE id = ?1",
                    &[environment().as_str().into()],
                )?
                .first()
                .unwrap()
                .intent(0, "Working State")
            })
            .unwrap()
    }

    fn saved(store: &ConfigStore, revision: i64) -> SavedEnvironmentIntent {
        store
            .storage
            .read(|tx| {
                tx.query(
                    "SELECT intent FROM config_saved WHERE environment_id = ?1 AND revision = ?2",
                    &[environment().as_str().into(), revision.into()],
                )?
                .first()
                .unwrap()
                .intent(0, "Saved revision")
            })
            .unwrap()
    }

    fn web(intent: &SavedEnvironmentIntent) -> &ployz_core::config::SavedServiceIntent {
        intent.services.first().unwrap()
    }

    fn variable<'a>(intent: &'a SavedEnvironmentIntent, key: &str) -> &'a SavedVariableIntent {
        web(intent)
            .variables
            .iter()
            .find(|variable| variable.key == key)
            .unwrap()
    }

    fn client_sealed(variable: &SavedVariableIntent) -> &ployz_core::config::ClientSealedValue {
        let SavedVariableValue::Secret {
            encrypted_value: Some(SecretCiphertext::ClientSealed(value)),
        } = &variable.value
        else {
            panic!(
                "{} is not client sealed: {:?}",
                variable.key, variable.value
            )
        };
        value
    }

    fn recipient(keypair: &ployz_core::Keypair) -> SealingRecipient {
        SealingRecipient {
            public_key: keypair.public(),
        }
    }

    #[test]
    fn a_reseal_seals_each_legacy_value_for_the_cluster_and_the_second_run_does_nothing() {
        let (store, who) = shop();
        let keypair = ployz_core::Keypair::generate();
        plant_working(
            &store,
            &who,
            vec![
                secret("TOKEN", legacy(&store.sealing, "hunter2")),
                secret("OTHER", legacy(&store.sealing, "swordfish")),
            ],
        );
        plant_saved(&store, 1, |_| {});
        let organization = who.organization.clone();
        assert_eq!(
            store.legacy_organizations().unwrap(),
            vec![organization.clone()]
        );
        assert_eq!(
            store.legacy_count(&organization).unwrap(),
            LegacyCount {
                current: 4,
                history: 0
            }
        );
        assert_eq!(
            store.reseal_environments(&organization).unwrap(),
            vec![environment()]
        );

        let first = store.reseal(&environment(), &recipient(&keypair)).unwrap();
        assert_eq!(
            first,
            Resealed {
                resealed: 4,
                remaining: 0
            }
        );
        let identity = SealedIdentity {
            organization: ORGANIZATION.into(),
            environment: Uuid::parse_str(&uuid(2)).unwrap(),
            service: Uuid::parse_str(&web(&working(&store)).id).unwrap(),
        };
        for intent in [working(&store), saved(&store, 1)] {
            for (key, plaintext) in [("TOKEN", "hunter2"), ("OTHER", "swordfish")] {
                let variable = variable(&intent, key);
                let sealed = client_sealed(variable);
                assert_eq!(keypair.open(&identity, sealed).as_deref(), Some(plaintext));
                assert_eq!(
                    variable.value_fingerprint,
                    crate::client_sealed::fingerprint(sealed)
                );
            }
        }
        assert!(store.legacy_organizations().unwrap().is_empty());
        assert_eq!(
            store.legacy_count(&organization).unwrap(),
            LegacyCount::default()
        );
        assert!(store.reseal_environments(&organization).unwrap().is_empty());

        let before = (working(&store), saved(&store, 1));
        let second = store.reseal(&environment(), &recipient(&keypair)).unwrap();
        assert_eq!(second, Resealed::default());
        assert_eq!((working(&store), saved(&store, 1)), before);
    }

    #[test]
    fn a_value_the_key_does_not_open_stays_and_is_counted_as_remaining() {
        let (store, who) = shop();
        let other_key = SealingKey::new(b"other").unwrap();
        plant_working(
            &store,
            &who,
            vec![
                secret("TOKEN", legacy(&store.sealing, "hunter2")),
                secret("LOST", legacy(&other_key, "gone")),
            ],
        );
        let lost_before = variable(&working(&store), "LOST").clone();

        let keypair = ployz_core::Keypair::generate();
        let done = store.reseal(&environment(), &recipient(&keypair)).unwrap();
        assert_eq!(
            done,
            Resealed {
                resealed: 1,
                remaining: 1
            }
        );
        let after = working(&store);
        assert_eq!(variable(&after, "LOST"), &lost_before);
        client_sealed(variable(&after, "TOKEN"));
        assert_eq!(
            store.legacy_count(&who.organization).unwrap(),
            LegacyCount {
                current: 1,
                history: 0
            }
        );
        assert_eq!(
            store.reseal_environments(&who.organization).unwrap(),
            vec![environment()]
        );
    }

    #[test]
    fn history_is_counted_and_never_touched() {
        let (store, who) = shop();
        plant_working(
            &store,
            &who,
            vec![secret("TOKEN", legacy(&store.sealing, "hunter2"))],
        );
        plant_saved(&store, 1, |intent| {
            intent
                .services
                .first_mut()
                .unwrap()
                .variables
                .push(secret("OLD", legacy(&store.sealing, "older")));
        });
        plant_saved(&store, 2, |_| {});
        let was = SealedCell::Secret(SealedSecret {
            fingerprint: "s1:was".into(),
            value: legacy(&store.sealing, "was"),
        });
        let plain = SealedCell::Cell(Cell::Value(json!("plain")));
        store
            .storage
            .write(|tx| {
                tx.execute(
                    "INSERT INTO config_sync_arrival (environment_id, other_id, lineage, at, organization_id, how, state, value, source, was) \
                     VALUES (?1, 'other', 'lineage', 'env.A', ?2, 'sync', 'pending', ?3, ?3, ?4)",
                    &[
                        environment().as_str().into(),
                        ORGANIZATION.into(),
                        serde_json::to_string(&Cell::Absent).unwrap().as_str().into(),
                        serde_json::to_string(&was).unwrap().as_str().into(),
                    ],
                )?;
                tx.execute(
                    "INSERT INTO config_merge_value (environment_id, destination_id, organization_id, lineage, at, cell) \
                     VALUES (?1, ?1, ?2, 'lineage', 'env.B', ?3)",
                    &[
                        environment().as_str().into(),
                        ORGANIZATION.into(),
                        serde_json::to_string(&was).unwrap().as_str().into(),
                    ],
                )?;
                tx.execute(
                    "INSERT INTO config_merge_value (environment_id, destination_id, organization_id, lineage, at, cell) \
                     VALUES (?1, ?1, ?2, 'lineage', 'env.C', ?3)",
                    &[
                        environment().as_str().into(),
                        ORGANIZATION.into(),
                        serde_json::to_string(&plain).unwrap().as_str().into(),
                    ],
                )?;
                Ok(())
            })
            .unwrap();
        let organization = who.organization.clone();
        assert_eq!(
            store.legacy_count(&organization).unwrap(),
            LegacyCount {
                current: 2,
                history: 4
            }
        );

        let older_before = saved(&store, 1);
        let keypair = ployz_core::Keypair::generate();
        let done = store.reseal(&environment(), &recipient(&keypair)).unwrap();
        assert_eq!(
            done,
            Resealed {
                resealed: 2,
                remaining: 0
            }
        );
        assert_eq!(saved(&store, 1), older_before);
        assert_eq!(
            store.legacy_count(&organization).unwrap(),
            LegacyCount {
                current: 0,
                history: 4
            }
        );
        assert!(store.legacy_organizations().unwrap().is_empty());
    }

    #[test]
    fn an_organization_with_only_client_sealed_values_has_no_legacy() {
        let (store, who) = shop();
        let keypair = ployz_core::Keypair::generate();
        let identity = SealedIdentity {
            organization: ORGANIZATION.into(),
            environment: Uuid::parse_str(&uuid(2)).unwrap(),
            service: Uuid::parse_str(&web(&working(&store)).id).unwrap(),
        };
        let sealed =
            ployz_core::seal_value(&keypair.public(), &identity, "TOKEN", "hunter2").unwrap();
        plant_working(
            &store,
            &who,
            vec![secret("TOKEN", SecretCiphertext::ClientSealed(sealed))],
        );
        assert!(store.legacy_organizations().unwrap().is_empty());
        assert_eq!(
            store.legacy_count(&who.organization).unwrap(),
            LegacyCount::default()
        );
        assert!(
            store
                .reseal_environments(&who.organization)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.reseal(&environment(), &recipient(&keypair)).unwrap(),
            Resealed::default()
        );
    }

    #[test]
    fn a_reseal_of_an_unknown_environment_is_not_found() {
        let (store, _) = shop();
        let keypair = ployz_core::Keypair::generate();
        let error = store
            .reseal(
                &EnvironmentId::parse(uuid(9)).unwrap(),
                &recipient(&keypair),
            )
            .unwrap_err();
        assert_eq!(error.code, ployz_core::RpcErrorCode::NotFound);
    }
}
