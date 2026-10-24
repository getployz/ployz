//! The one-time conversion of secrets the Store sealed itself into values sealed
//! for the Cluster, which only its Machines open. The Store is the only holder
//! of the key that opens them, so the conversion runs here.

use ployz_core::RpcError;
use ployz_core::config::{
    LegacyCiphertext, SavedEnvironmentIntent, SavedVariableValue, SealedCell, SealedSecret,
    SecretCiphertext,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::client_sealed;
use crate::id::Revision;
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
    /// Values left in place: those the Store's key doesn't open, PORT, and those
    /// of a Service without an ID.
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
/// doesn't open, PORT, and a Service without an ID stay as they are and are counted.
pub(crate) fn reseal(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    recipient: &SealingRecipient,
    sealing: &SealingKey,
) -> Result<Resealed, RpcError> {
    crate::scope::lock_all(tx, [environment.clone()])?;
    let rows = tx.query(
        "SELECT organization_id, working, working_revision FROM config_environment WHERE id = ?1",
        &[environment.as_str().into()],
    )?;
    let Some(row) = rows.first() else {
        return Err(error::not_found(
            "Environment not found",
            json!({ "environment": environment.as_str() }),
        ));
    };
    let organization = OrganizationId::parse(row.text(0)?)?;
    let mut resealing = Resealing {
        organization: &organization,
        environment,
        recipient,
        sealing,
        sealed: Vec::new(),
        tally: Resealed::default(),
    };
    let mut working = row.intent(1, "Working State")?;
    if resealing.intent(&mut working)? {
        let revision = Revision(row.number(2, "revision")?).next();
        tx.execute(
            "UPDATE config_environment SET working_revision = ?1, working = ?2 WHERE id = ?3",
            &[
                crate::scope::revision_param(revision)?.into(),
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
        if resealing.intent(&mut intent)? {
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
    Ok(resealing.tally)
}

/// One Environment's reseal across its Working State and Saved head.
struct Resealing<'a> {
    organization: &'a OrganizationId,
    environment: &'a EnvironmentId,
    recipient: &'a SealingRecipient,
    sealing: &'a SealingKey,
    /// Each value sealed so far, by Service ID, key and the value it replaced: the
    /// same value in Working State and the Saved head gets the same sealed value, so
    /// Review shows nothing staged.
    sealed: Vec<(String, String, LegacyCiphertext, SealedSecret)>,
    tally: Resealed,
}

impl Resealing<'_> {
    /// Reseal `intent` in place; whether anything in it changed.
    fn intent(&mut self, intent: &mut SavedEnvironmentIntent) -> Result<bool, RpcError> {
        let mut changed = false;
        for service in &mut intent.services {
            let identity =
                client_sealed::identity(self.organization, self.environment, &service.id);
            for variable in &mut service.variables {
                let SavedVariableValue::Secret {
                    encrypted_value: Some(SecretCiphertext::Legacy(legacy)),
                } = &variable.value
                else {
                    continue;
                };
                // Ployz reads PORT to route traffic, and a sealed value binds to its
                // Service's UUID: neither can hold one.
                let Some(identity) = identity.as_ref().filter(|_| variable.key != "PORT") else {
                    self.tally.remaining += 1;
                    continue;
                };
                let earlier = self.sealed.iter().find(|(id, key, was, _)| {
                    *id == service.id && *key == variable.key && was == legacy
                });
                let sealed = match earlier {
                    Some((.., sealed)) => sealed.clone(),
                    None => {
                        let Ok(plaintext) = self.sealing.open(legacy) else {
                            self.tally.remaining += 1;
                            continue;
                        };
                        let sealed = client_sealed::seal_to(
                            self.recipient,
                            identity,
                            &variable.key,
                            &plaintext,
                        )?;
                        self.sealed.push((
                            service.id.clone(),
                            variable.key.clone(),
                            legacy.clone(),
                            sealed.clone(),
                        ));
                        sealed
                    }
                };
                variable.value_fingerprint = sealed.fingerprint;
                variable.value = SavedVariableValue::Secret {
                    encrypted_value: Some(sealed.value),
                };
                self.tally.resealed += 1;
                changed = true;
            }
        }
        Ok(changed)
    }
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
        Actor, Change, ConfigStore, CreateProject, CreateService, Edit, EnvironmentRef, ProjectId,
        ProjectName, ServiceLineageId, SettingPath,
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
        shop_at("sqlite::memory:")
    }

    fn shop_at(url: &str) -> (ConfigStore, Actor) {
        let store = ConfigStore::open(url, SealingKey::new(b"test").unwrap()).unwrap();
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

    fn revision(store: &ConfigStore) -> i64 {
        store
            .storage
            .read(|tx| {
                tx.query(
                    "SELECT working_revision FROM config_environment WHERE id = ?1",
                    &[environment().as_str().into()],
                )?
                .first()
                .unwrap()
                .int(0)
            })
            .unwrap()
    }

    fn set_mode(store: &ConfigStore, who: &Actor) {
        store
            .write(
                who,
                &Edit {
                    environment: EnvironmentRef::default(),
                    expect: None,
                    changes: vec![Change::Set {
                        path: SettingPath::parse("web.env.MODE").unwrap(),
                        value: json!("fast"),
                    }],
                },
            )
            .unwrap();
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

    #[test]
    fn a_reseal_moves_the_revision_once_and_a_later_save_keeps_both_changes() {
        let (store, who) = shop();
        let keypair = ployz_core::Keypair::generate();
        plant_working(
            &store,
            &who,
            vec![secret("TOKEN", legacy(&store.sealing, "hunter2"))],
        );
        let before = revision(&store);

        store.reseal(&environment(), &recipient(&keypair)).unwrap();
        assert_eq!(revision(&store), before + 1);
        store.reseal(&environment(), &recipient(&keypair)).unwrap();
        assert_eq!(revision(&store), before + 1);

        set_mode(&store, &who);
        let after = working(&store);
        client_sealed(variable(&after, "TOKEN"));
        assert_eq!(
            variable(&after, "MODE").value,
            SavedVariableValue::Literal {
                value: "fast".into()
            }
        );
    }

    #[test]
    fn a_save_committed_while_a_reseal_waits_is_kept() {
        // SQLite runs one write at a time, so only Postgres can interleave them.
        let Ok(server) = std::env::var("PLOYZ_STORE_TEST_POSTGRES") else {
            return;
        };
        let database = format!("reseal_{}", Uuid::new_v4().simple());
        postgres::Client::connect(&server, postgres::NoTls)
            .unwrap()
            .batch_execute(&format!("CREATE DATABASE {database}"))
            .unwrap();
        let url = format!("{server}/{database}");
        let (store, who) = shop_at(&url);
        plant_working(
            &store,
            &who,
            vec![secret("TOKEN", legacy(&store.sealing, "hunter2"))],
        );
        let mut saved = working(&store);
        saved
            .services
            .first_mut()
            .unwrap()
            .variables
            .push(SavedVariableIntent {
                id: Uuid::new_v4().to_string(),
                key: "MODE".into(),
                description: None,
                exported: false,
                value_fingerprint: "p1:fast".into(),
                value: SavedVariableValue::Literal {
                    value: "fast".into(),
                },
            });
        let mut save = postgres::Client::connect(&url, postgres::NoTls).unwrap();
        let mut save = save.transaction().unwrap();
        save.execute(
            "UPDATE config_environment SET working = $1, working_revision = working_revision + 1 WHERE id = $2",
            &[&serde_json::to_string(&saved).unwrap(), &environment().as_str()],
        )
        .unwrap();

        let keypair = ployz_core::Keypair::generate();
        std::thread::scope(|scope| {
            let reseal = scope.spawn(|| store.reseal(&environment(), &recipient(&keypair)));
            std::thread::sleep(std::time::Duration::from_millis(300));
            save.commit().unwrap();
            reseal.join().unwrap().unwrap();
        });

        let after = working(&store);
        client_sealed(variable(&after, "TOKEN"));
        assert_eq!(variable(&after, "MODE").value_fingerprint, "p1:fast");
    }

    #[test]
    fn one_legacy_value_in_working_state_and_the_saved_head_gets_one_sealed_value() {
        let (store, who) = shop();
        plant_working(
            &store,
            &who,
            vec![
                secret("TOKEN", legacy(&store.sealing, "hunter2")),
                secret("OTHER", legacy(&store.sealing, "swordfish")),
            ],
        );
        plant_saved(&store, 1, |intent| {
            for variable in &mut intent.services.first_mut().unwrap().variables {
                if variable.key == "OTHER" {
                    variable.value = SavedVariableValue::Secret {
                        encrypted_value: Some(legacy(&store.sealing, "swordfish")),
                    };
                }
            }
        });

        let keypair = ployz_core::Keypair::generate();
        store.reseal(&environment(), &recipient(&keypair)).unwrap();
        let (working, head) = (working(&store), saved(&store, 1));
        assert_eq!(variable(&working, "TOKEN"), variable(&head, "TOKEN"));
        assert_ne!(
            client_sealed(variable(&working, "OTHER")),
            client_sealed(variable(&head, "OTHER"))
        );
        assert_ne!(
            variable(&working, "OTHER").value_fingerprint,
            variable(&head, "OTHER").value_fingerprint
        );
        let unsaved: Vec<String> = store
            .read(&who, &crate::DiffQuery::default())
            .unwrap()
            .draft_changes
            .into_iter()
            .flat_map(|change| change.settings)
            .map(|row| row.path)
            .collect();
        assert_eq!(unsaved, ["web.env.OTHER"]);
    }

    #[test]
    fn a_legacy_port_stays_and_is_counted_as_remaining() {
        let (store, who) = shop();
        plant_working(
            &store,
            &who,
            vec![
                secret("PORT", legacy(&store.sealing, "8080")),
                secret("TOKEN", legacy(&store.sealing, "hunter2")),
            ],
        );
        let port = variable(&working(&store), "PORT").clone();

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
        assert_eq!(variable(&after, "PORT"), &port);
        client_sealed(variable(&after, "TOKEN"));
        set_mode(&store, &who);
    }
}
