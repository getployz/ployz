use std::collections::BTreeMap;
use std::sync::Arc;

use mvp_bus::{BusSession, FactKey, FactKeyPattern, FactPayload};
use mvp_p2panda_facts::{PandaFactAuthor, PandaFactWriteOutcome, SharedPandaFactStore};
use mvp_projection::{CandidateStatus, FactCandidate, FactSource};

use crate::{
    CommandError, CommandFact, CommandName, CommandPhaseStore, CommandResult, IntentId,
    StoredCommandPhase, command_intent_fact_key, command_phase_fact_key,
};

pub struct PandaCommandPhaseStore {
    store: SharedPandaFactStore,
    session: BusSession,
    author: Arc<PandaFactAuthor>,
}

impl PandaCommandPhaseStore {
    #[must_use]
    pub fn new(
        store: SharedPandaFactStore,
        session: BusSession,
        author: Arc<PandaFactAuthor>,
    ) -> Self {
        Self {
            store,
            session,
            author,
        }
    }
}

impl CommandPhaseStore for PandaCommandPhaseStore {
    fn read_phase_history(
        &self,
        command: &CommandName,
        intent: &IntentId,
    ) -> CommandResult<Vec<StoredCommandPhase>> {
        let pattern = FactKeyPattern::parse(format!(
            "/facts/command/{}/{}/phase/>",
            command.as_str(),
            intent.as_str()
        ))?;
        let candidates = self
            .store
            .list_candidates(self.session.island(), &pattern, &self.session)
            .map_err(store_error)?;
        command_phase_history(&self.store, &self.session, command, intent, candidates)
    }

    fn write_intent(&self, fact: CommandFact) -> CommandResult<FactKey> {
        let key = command_intent_fact_key(&fact.command, &fact.intent)?;
        let payload = serde_json::to_vec(&fact)
            .map(FactPayload::from)
            .map_err(store_error)?;
        write_command_fact_payload(&self.store, &self.session, &self.author, key, payload)
    }

    fn append_phase(
        &self,
        command: &CommandName,
        intent: &IntentId,
        expected_previous: Option<&StoredCommandPhase>,
        index: u64,
        payload: FactPayload,
    ) -> CommandResult<FactKey> {
        let latest = self.read_latest_phase(command, intent)?;
        let key = command_phase_fact_key(command, intent, index)?;
        if latest
            .as_ref()
            .is_some_and(|stored| stored.index == index && stored.payload == payload)
        {
            return Ok(key);
        }
        match latest.as_ref() {
            Some(stored) if expected_previous != Some(stored) => {
                return Err(CommandError::PhaseAdvanced {
                    key: stored.key.clone(),
                });
            }
            None if expected_previous.is_some() => {
                return Err(CommandError::PhaseAdvanced {
                    key: expected_previous
                        .expect("expected previous checked above")
                        .key
                        .clone(),
                });
            }
            Some(_) | None => {}
        }
        write_command_fact_payload(&self.store, &self.session, &self.author, key, payload)
    }
}

fn write_command_fact_payload(
    store: &SharedPandaFactStore,
    session: &BusSession,
    author: &Arc<PandaFactAuthor>,
    key: FactKey,
    payload: FactPayload,
) -> CommandResult<FactKey> {
    let outcome = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(store.write_fact_payload(
            session,
            author,
            key.clone(),
            payload,
        ))
    })
    .map_err(store_error)?;
    match outcome {
        PandaFactWriteOutcome::Inserted(_) | PandaFactWriteOutcome::AlreadyPresent(_) => Ok(key),
        PandaFactWriteOutcome::Conflict(_) => Err(CommandError::PhaseConflict { key }),
    }
}

fn command_phase_history(
    store: &SharedPandaFactStore,
    session: &BusSession,
    command: &CommandName,
    intent: &IntentId,
    candidates: Vec<FactCandidate>,
) -> CommandResult<Vec<StoredCommandPhase>> {
    let mut candidates_by_index = BTreeMap::<u64, Vec<FactCandidate>>::new();
    for candidate in candidates {
        if let Some(index) = command_phase_index(candidate.key(), command, intent) {
            candidates_by_index
                .entry(index)
                .or_default()
                .push(candidate);
        }
    }

    let mut selected = Vec::with_capacity(candidates_by_index.len());
    for (expected_index, (index, candidates)) in (1..).zip(candidates_by_index) {
        if index != expected_index {
            return Err(CommandError::PhaseHistoryGap {
                command: command.clone(),
                intent: intent.clone(),
                expected_index,
                actual_index: index,
            });
        }
        let key = command_phase_fact_key(command, intent, index)?;
        let [candidate] = candidates.as_slice() else {
            return Err(CommandError::PhaseConflict { key });
        };
        if candidate.status() == CandidateStatus::Conflict {
            return Err(CommandError::PhaseConflict { key });
        }
        selected.push(candidate.clone());
    }

    let payloads = store
        .read_payloads(session.island(), &selected, session)
        .map_err(store_error)?;
    selected
        .into_iter()
        .map(|candidate| {
            let index = command_phase_index(candidate.key(), command, intent).ok_or_else(|| {
                CommandError::Store {
                    message: format!(
                        "candidate key does not match command phase pattern: {}",
                        candidate.key()
                    ),
                }
            })?;
            let payload = payloads
                .get(candidate.content_hash())
                .cloned()
                .ok_or_else(|| CommandError::Store {
                    message: format!("command phase payload missing for {}", candidate.key()),
                })?;
            Ok(StoredCommandPhase {
                key: candidate.key().clone(),
                index,
                payload,
            })
        })
        .collect()
}

fn command_phase_index(key: &FactKey, command: &CommandName, intent: &IntentId) -> Option<u64> {
    let segments = key.segments().collect::<Vec<_>>();
    let [
        "facts",
        "command",
        command_segment,
        intent_segment,
        "phase",
        index,
    ] = segments.as_slice()
    else {
        return None;
    };
    if *command_segment != command.as_str() || *intent_segment != intent.as_str() {
        return None;
    }
    index.parse().ok()
}

fn store_error(error: impl std::error::Error) -> CommandError {
    CommandError::Store {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{
        CommandError, CommandFact, CommandName, CommandPhaseStore, IntentId, command_phase_fact_key,
    };
    use mvp_bus::{FactPayload, Grant, IslandId, PrincipalId};
    use mvp_p2panda_facts::{PandaFactAuthor, PandaFactStore, SharedPandaFactStore};

    use super::PandaCommandPhaseStore;

    #[tokio::test(flavor = "multi_thread")]
    async fn read_phase_history_returns_ordered_phases() {
        let fixture = Fixture::new().await;
        let command = command();
        let intent = intent();
        fixture
            .store
            .write_intent(CommandFact::new(command.clone(), intent.clone()))
            .expect("write intent");
        fixture
            .store
            .append_phase(
                &command,
                &intent,
                None,
                1,
                FactPayload::from(br#""prepared""#.to_vec()),
            )
            .expect("append first phase");
        let first = fixture
            .store
            .read_latest_phase(&command, &intent)
            .expect("read latest")
            .expect("first phase exists");
        fixture
            .store
            .append_phase(
                &command,
                &intent,
                Some(&first),
                2,
                FactPayload::from(br#""committed""#.to_vec()),
            )
            .expect("append second phase");

        let history = fixture
            .store
            .read_phase_history(&command, &intent)
            .expect("read history");

        assert_eq!(history.len(), 2);
        assert_eq!(history[0].index, 1);
        assert_eq!(history[1].index, 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn append_same_payload_is_idempotent() {
        let fixture = Fixture::new().await;
        let command = command();
        let intent = intent();
        let payload = FactPayload::from(br#""prepared""#.to_vec());
        let first = fixture
            .store
            .append_phase(&command, &intent, None, 1, payload.clone())
            .expect("first append");
        let repeated = fixture
            .store
            .append_phase(&command, &intent, None, 1, payload)
            .expect("repeat append");

        assert_eq!(first, repeated);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn append_stale_previous_reports_phase_advanced() {
        let fixture = Fixture::new().await;
        let command = command();
        let intent = intent();
        fixture
            .store
            .append_phase(
                &command,
                &intent,
                None,
                1,
                FactPayload::from(br#""prepared""#.to_vec()),
            )
            .expect("append first");
        let error = fixture
            .store
            .append_phase(
                &command,
                &intent,
                None,
                2,
                FactPayload::from(br#""committed""#.to_vec()),
            )
            .expect_err("stale append rejected");

        assert!(matches!(error, CommandError::PhaseAdvanced { .. }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_phase_history_rejects_gaps() {
        let fixture = Fixture::new().await;
        let command = command();
        let intent = intent();
        fixture
            .write_raw_phase(
                &command,
                &intent,
                2,
                FactPayload::from(br#""committed""#.to_vec()),
            )
            .await;

        let error = fixture
            .store
            .read_phase_history(&command, &intent)
            .expect_err("gap rejected");

        assert!(matches!(
            error,
            CommandError::PhaseHistoryGap {
                expected_index: 1,
                actual_index: 2,
                ..
            }
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_phase_history_rejects_conflicting_candidates() {
        let fixture = Fixture::new().await;
        let command = command();
        let intent = intent();
        fixture
            .write_raw_phase(
                &command,
                &intent,
                1,
                FactPayload::from(br#""prepared-a""#.to_vec()),
            )
            .await;
        fixture
            .write_raw_phase(
                &command,
                &intent,
                1,
                FactPayload::from(br#""prepared-b""#.to_vec()),
            )
            .await;

        let error = fixture
            .store
            .read_phase_history(&command, &intent)
            .expect_err("conflict rejected");

        assert!(matches!(error, CommandError::PhaseConflict { .. }));
    }

    struct Fixture {
        store: PandaCommandPhaseStore,
        raw_store: SharedPandaFactStore,
        session: mvp_bus::BusSession,
        author: Arc<PandaFactAuthor>,
    }

    impl Fixture {
        async fn new() -> Self {
            let (_, authority, bus) = mvp_bus::harness::actor_with_authority();
            let island = IslandId::new("prod");
            let session = authority.grant_in(
                island.clone(),
                PrincipalId::new("writer"),
                Grant::empty()
                    .with_fact_write(pattern("/facts/command/>"))
                    .with_fact_read(pattern("/facts/command/>")),
            );
            let author = Arc::new(PandaFactAuthor::new(session.principal().clone()));
            let raw_store = SharedPandaFactStore::new(PandaFactStore::new(Arc::new(bus)));
            raw_store
                .trust_author_key(&island, session.principal().clone(), author.author_key())
                .await
                .expect("trust author");
            let store =
                PandaCommandPhaseStore::new(raw_store.clone(), session.clone(), author.clone());
            Self {
                store,
                raw_store,
                session,
                author,
            }
        }

        async fn write_raw_phase(
            &self,
            command: &CommandName,
            intent: &IntentId,
            index: u64,
            payload: FactPayload,
        ) {
            let key = command_phase_fact_key(command, intent, index).expect("phase key");
            self.raw_store
                .write_fact_payload(&self.session, &self.author, key, payload)
                .await
                .expect("raw phase write");
        }
    }

    fn command() -> CommandName {
        CommandName::parse("test-command").expect("command name")
    }

    fn intent() -> IntentId {
        IntentId::parse("intent-1").expect("intent")
    }

    fn pattern(value: &str) -> mvp_bus::FactKeyPattern {
        mvp_bus::FactKeyPattern::parse(value).expect("fact pattern")
    }
}
