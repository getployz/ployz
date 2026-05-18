use std::collections::BTreeMap;
use std::fmt::{self, Debug, Display, Formatter};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use mvp_bus::{BusError, FactKey, FactPayload};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CommandName(String);

impl CommandName {
    pub fn parse(value: impl Into<String>) -> CommandResult<Self> {
        let value = value.into();
        validate_identifier("command name", &value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for CommandName {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct IntentId(String);

impl IntentId {
    pub fn parse(value: impl Into<String>) -> CommandResult<Self> {
        let value = value.into();
        validate_identifier("intent id", &value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for IntentId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("{label} is invalid: {value:?}")]
    InvalidIdentifier { label: &'static str, value: String },
    #[error(transparent)]
    Bus(#[from] BusError),
    #[error("command phase serialization failed: {message}")]
    SerializePhase { message: String },
    #[error("command phase deserialization failed for {key}: {message}")]
    DeserializePhase { key: FactKey, message: String },
    #[error("command phase has conflicting candidates at {key}")]
    PhaseConflict { key: FactKey },
    #[error("command store failed: {message}")]
    Store { message: String },
    #[error("command phase index overflow for {command}/{intent}")]
    PhaseIndexOverflow {
        command: CommandName,
        intent: IntentId,
    },
}

pub type CommandResult<T> = Result<T, CommandError>;

pub trait Command {
    type Output: Send;
    type Error: Send + From<CommandError>;

    fn name(&self) -> CommandName;
    fn intent_id(&self) -> IntentId;
}

pub trait Phase: Serialize + DeserializeOwned + Clone + Debug + Eq + Send + Sync + 'static {}

impl<T> Phase for T where
    T: Serialize + DeserializeOwned + Clone + Debug + Eq + Send + Sync + 'static
{
}

pub trait PhasedCommand: Command {
    type Phase: Phase;

    fn initial_phase(&self) -> Self::Phase;

    fn step<'a>(
        &'a self,
        cx: &'a CommandContext,
        phase: Self::Phase,
    ) -> CommandStepFuture<'a, Self::Phase, <Self as Command>::Output, <Self as Command>::Error>;

    fn compensate<'a>(
        &'a self,
        cx: &'a CommandContext,
        phase: Self::Phase,
    ) -> CommandCompensationFuture<'a, <Self as Command>::Error>;
}

pub type CommandStepFuture<'a, P, O, E> =
    Pin<Box<dyn Future<Output = Result<PhaseTransition<P, O>, E>> + Send + 'a>>;

pub type CommandCompensationFuture<'a, E> =
    Pin<Box<dyn Future<Output = Result<(), E>> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhaseTransition<P, O> {
    Continue(P),
    Done(O),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandFact {
    pub command: CommandName,
    pub intent: IntentId,
}

impl CommandFact {
    #[must_use]
    pub fn new(command: CommandName, intent: IntentId) -> Self {
        Self { command, intent }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCommandPhase {
    pub key: FactKey,
    pub index: u64,
    pub payload: FactPayload,
}

pub trait CommandPhaseStore: Send + Sync {
    fn read_latest_phase(
        &self,
        command: &CommandName,
        intent: &IntentId,
    ) -> CommandResult<Option<StoredCommandPhase>>;

    fn write_intent(&self, fact: CommandFact) -> CommandResult<FactKey>;

    fn write_phase(
        &self,
        command: &CommandName,
        intent: &IntentId,
        index: u64,
        payload: FactPayload,
    ) -> CommandResult<FactKey>;
}

#[derive(Clone)]
pub struct CommandContext {
    store: Arc<dyn CommandPhaseStore>,
}

impl CommandContext {
    #[must_use]
    pub fn new(store: Arc<dyn CommandPhaseStore>) -> Self {
        Self { store }
    }

    pub fn write_intent(&self, fact: CommandFact) -> CommandResult<FactKey> {
        self.store.write_intent(fact)
    }

    pub fn read_phase<P: Phase>(&self, command: &impl Command) -> CommandResult<Option<P>> {
        let Some(stored) = self.read_stored_phase(command)? else {
            return Ok(None);
        };
        serde_json::from_slice(stored.payload.as_ref())
            .map(Some)
            .map_err(|error| CommandError::DeserializePhase {
                key: stored.key,
                message: error.to_string(),
            })
    }

    pub fn read_stored_phase(
        &self,
        command: &impl Command,
    ) -> CommandResult<Option<StoredCommandPhase>> {
        self.store
            .read_latest_phase(&command.name(), &command.intent_id())
    }

    pub fn write_phase<P: Phase>(
        &self,
        command: &impl Command,
        phase: &P,
    ) -> CommandResult<FactKey> {
        let name = command.name();
        let intent = command.intent_id();
        let next_index = match self.store.read_latest_phase(&name, &intent)? {
            Some(stored) => {
                stored
                    .index
                    .checked_add(1)
                    .ok_or_else(|| CommandError::PhaseIndexOverflow {
                        command: name.clone(),
                        intent: intent.clone(),
                    })?
            }
            None => 1,
        };
        let payload = serde_json::to_vec(phase)
            .map(FactPayload::from)
            .map_err(|error| CommandError::SerializePhase {
                message: error.to_string(),
            })?;
        self.store.write_phase(&name, &intent, next_index, payload)
    }
}

pub async fn run_phased<C>(cx: &CommandContext, command: &C) -> Result<C::Output, C::Error>
where
    C: PhasedCommand,
{
    cx.write_intent(CommandFact::new(command.name(), command.intent_id()))?;
    let stored = cx.read_stored_phase(command)?;
    let mut current = match &stored {
        Some(stored) => serde_json::from_slice(stored.payload.as_ref()).map_err(|error| {
            CommandError::DeserializePhase {
                key: stored.key.clone(),
                message: error.to_string(),
            }
        })?,
        None => command.initial_phase(),
    };
    let mut committed = match stored {
        Some(_) => vec![current.clone()],
        None => Vec::new(),
    };

    loop {
        match command.step(cx, current.clone()).await {
            Ok(PhaseTransition::Continue(next)) => {
                if let Err(error) = cx.write_phase(command, &next) {
                    let _ = command.compensate(cx, next).await;
                    for phase in committed.into_iter().rev() {
                        let _ = command.compensate(cx, phase).await;
                    }
                    return Err(error.into());
                }
                committed.push(next.clone());
                current = next;
            }
            Ok(PhaseTransition::Done(output)) => return Ok(output),
            Err(error) => {
                for phase in committed.into_iter().rev() {
                    let _ = command.compensate(cx, phase).await;
                }
                return Err(error);
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct InMemoryCommandPhaseStore {
    inner: Arc<Mutex<InMemoryCommandPhaseStoreInner>>,
}

impl InMemoryCommandPhaseStore {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            inner: Arc::new(Mutex::new(InMemoryCommandPhaseStoreInner::new())),
        }
    }

    #[cfg(test)]
    fn write_conflicting_phase_for_test(
        &self,
        command: &CommandName,
        intent: &IntentId,
        index: u64,
        payload: FactPayload,
    ) -> CommandResult<FactKey> {
        let key = command_phase_fact_key(command, intent, index)?;
        let mut inner = self.inner.lock().map_err(|_| CommandError::Store {
            message: "command phase store lock poisoned".to_string(),
        })?;
        inner
            .phases
            .entry((command.clone(), intent.clone()))
            .or_default()
            .entry(index)
            .or_default()
            .push(StoredCommandPhase {
                key: key.clone(),
                index,
                payload,
            });
        Ok(key)
    }
}

#[derive(Debug)]
struct InMemoryCommandPhaseStoreInner {
    intents: BTreeMap<(CommandName, IntentId), CommandFact>,
    phases: BTreeMap<(CommandName, IntentId), BTreeMap<u64, Vec<StoredCommandPhase>>>,
}

impl InMemoryCommandPhaseStoreInner {
    fn new() -> Self {
        Self {
            intents: BTreeMap::new(),
            phases: BTreeMap::new(),
        }
    }
}

impl CommandPhaseStore for InMemoryCommandPhaseStore {
    fn read_latest_phase(
        &self,
        command: &CommandName,
        intent: &IntentId,
    ) -> CommandResult<Option<StoredCommandPhase>> {
        let inner = self.inner.lock().map_err(|_| CommandError::Store {
            message: "command phase store lock poisoned".to_string(),
        })?;
        let Some((index, phases)) = inner
            .phases
            .get(&(command.clone(), intent.clone()))
            .and_then(|phases| phases.last_key_value())
        else {
            return Ok(None);
        };
        let [phase] = phases.as_slice() else {
            return Err(CommandError::PhaseConflict {
                key: command_phase_fact_key(command, intent, *index)?,
            });
        };
        Ok(Some(phase.clone()))
    }

    fn write_intent(&self, fact: CommandFact) -> CommandResult<FactKey> {
        let key = command_intent_fact_key(&fact.command, &fact.intent)?;
        let mut inner = self.inner.lock().map_err(|_| CommandError::Store {
            message: "command phase store lock poisoned".to_string(),
        })?;
        inner
            .intents
            .entry((fact.command.clone(), fact.intent.clone()))
            .or_insert(fact);
        Ok(key)
    }

    fn write_phase(
        &self,
        command: &CommandName,
        intent: &IntentId,
        index: u64,
        payload: FactPayload,
    ) -> CommandResult<FactKey> {
        let key = command_phase_fact_key(command, intent, index)?;
        let mut inner = self.inner.lock().map_err(|_| CommandError::Store {
            message: "command phase store lock poisoned".to_string(),
        })?;
        let candidates = inner
            .phases
            .entry((command.clone(), intent.clone()))
            .or_default()
            .entry(index)
            .or_default();
        if candidates.iter().any(|phase| phase.payload == payload) {
            return Ok(key);
        }
        if !candidates.is_empty() {
            return Err(CommandError::PhaseConflict { key });
        }
        candidates.push(StoredCommandPhase {
            key: key.clone(),
            index,
            payload,
        });
        Ok(key)
    }
}

pub fn command_intent_fact_key(command: &CommandName, intent: &IntentId) -> CommandResult<FactKey> {
    FactKey::parse(format!(
        "/facts/command/{}/{}/intent",
        command.as_str(),
        intent.as_str()
    ))
    .map_err(CommandError::from)
}

pub fn command_phase_fact_key(
    command: &CommandName,
    intent: &IntentId,
    index: u64,
) -> CommandResult<FactKey> {
    FactKey::parse(format!(
        "/facts/command/{}/{}/phase/{index}",
        command.as_str(),
        intent.as_str()
    ))
    .map_err(CommandError::from)
}

fn validate_identifier(label: &'static str, value: &str) -> CommandResult<()> {
    if value.is_empty()
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(CommandError::InvalidIdentifier {
            label,
            value: value.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    use mvp_bus::{FactKey, FactPayload};
    use serde::{Deserialize, Serialize};

    use crate::{
        Command, CommandContext, CommandError, CommandFact, CommandName, CommandPhaseStore,
        InMemoryCommandPhaseStore, IntentId, PhaseTransition, PhasedCommand, StoredCommandPhase,
        run_phased,
    };

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    enum TestPhase {
        Start,
        Prepared,
        Committed,
    }

    #[derive(Debug)]
    enum TestError {
        Command,
        Failed,
    }

    impl From<CommandError> for TestError {
        fn from(_error: CommandError) -> Self {
            Self::Command
        }
    }

    struct TestCommand {
        fail_at: Option<TestPhase>,
        fail_compensation: bool,
        seen: Arc<Mutex<Vec<TestPhase>>>,
        compensated: Arc<Mutex<Vec<TestPhase>>>,
    }

    impl TestCommand {
        fn new(
            fail_at: Option<TestPhase>,
            seen: Arc<Mutex<Vec<TestPhase>>>,
            compensated: Arc<Mutex<Vec<TestPhase>>>,
        ) -> Self {
            Self {
                fail_at,
                fail_compensation: false,
                seen,
                compensated,
            }
        }

        fn with_failing_compensation(mut self) -> Self {
            self.fail_compensation = true;
            self
        }
    }

    impl Command for TestCommand {
        type Output = &'static str;
        type Error = TestError;

        fn name(&self) -> CommandName {
            CommandName::parse("test-command").expect("valid command")
        }

        fn intent_id(&self) -> IntentId {
            IntentId::parse("intent-1").expect("valid intent")
        }
    }

    impl PhasedCommand for TestCommand {
        type Phase = TestPhase;

        fn initial_phase(&self) -> Self::Phase {
            TestPhase::Start
        }

        fn step<'a>(
            &'a self,
            _cx: &'a CommandContext,
            phase: Self::Phase,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<PhaseTransition<Self::Phase, Self::Output>, Self::Error>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                self.seen.lock().expect("seen lock").push(phase.clone());
                if self.fail_at.as_ref() == Some(&phase) {
                    return Err(TestError::Failed);
                }
                match phase {
                    TestPhase::Start => Ok(PhaseTransition::Continue(TestPhase::Prepared)),
                    TestPhase::Prepared => Ok(PhaseTransition::Continue(TestPhase::Committed)),
                    TestPhase::Committed => Ok(PhaseTransition::Done("done")),
                }
            })
        }

        fn compensate<'a>(
            &'a self,
            _cx: &'a CommandContext,
            phase: Self::Phase,
        ) -> Pin<Box<dyn Future<Output = Result<(), Self::Error>> + Send + 'a>> {
            Box::pin(async move {
                self.compensated
                    .lock()
                    .expect("compensated lock")
                    .push(phase);
                if self.fail_compensation {
                    return Err(TestError::Failed);
                }
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn run_phased_persists_each_continued_phase() {
        let store = Arc::new(InMemoryCommandPhaseStore::empty());
        let cx = CommandContext::new(store);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let compensated = Arc::new(Mutex::new(Vec::new()));
        let command = TestCommand::new(None, Arc::clone(&seen), compensated);

        let result = run_phased(&cx, &command).await.expect("command runs");

        assert_eq!(result, "done");
        assert_eq!(
            seen.lock().expect("seen lock").as_slice(),
            &[TestPhase::Start, TestPhase::Prepared, TestPhase::Committed]
        );
        assert_eq!(
            cx.read_phase::<TestPhase>(&command).expect("phase"),
            Some(TestPhase::Committed)
        );
    }

    #[tokio::test]
    async fn resumed_command_starts_at_latest_phase() {
        let store = Arc::new(InMemoryCommandPhaseStore::empty());
        let cx = CommandContext::new(store);
        let initial_seen = Arc::new(Mutex::new(Vec::new()));
        let compensated = Arc::new(Mutex::new(Vec::new()));
        let initial = TestCommand::new(
            Some(TestPhase::Prepared),
            Arc::clone(&initial_seen),
            Arc::clone(&compensated),
        );

        let error = run_phased(&cx, &initial).await.expect_err("prepared fails");
        assert!(matches!(error, TestError::Failed));
        assert_eq!(
            cx.read_phase::<TestPhase>(&initial).expect("phase"),
            Some(TestPhase::Prepared)
        );

        let resumed_seen = Arc::new(Mutex::new(Vec::new()));
        let resumed = TestCommand::new(None, Arc::clone(&resumed_seen), compensated);
        run_phased(&cx, &resumed).await.expect("resume");

        assert_eq!(
            resumed_seen.lock().expect("resumed seen").as_slice(),
            &[TestPhase::Prepared, TestPhase::Committed]
        );
    }

    #[tokio::test]
    async fn resumed_failure_compensates_persisted_phase() {
        let store = Arc::new(InMemoryCommandPhaseStore::empty());
        let cx = CommandContext::new(store);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let compensated = Arc::new(Mutex::new(Vec::new()));
        let command = TestCommand::new(
            Some(TestPhase::Prepared),
            Arc::clone(&seen),
            Arc::clone(&compensated),
        );
        cx.write_phase(&command, &TestPhase::Prepared)
            .expect("seed persisted phase");

        let error = run_phased(&cx, &command).await.expect_err("prepared fails");

        assert!(matches!(error, TestError::Failed));
        assert_eq!(
            seen.lock().expect("seen lock").as_slice(),
            &[TestPhase::Prepared]
        );
        assert_eq!(
            compensated.lock().expect("compensated lock").as_slice(),
            &[TestPhase::Prepared]
        );
    }

    #[tokio::test]
    async fn compensation_walks_committed_phases_in_reverse_without_failing_phase() {
        let store = Arc::new(InMemoryCommandPhaseStore::empty());
        let cx = CommandContext::new(store);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let compensated = Arc::new(Mutex::new(Vec::new()));
        let command = TestCommand::new(Some(TestPhase::Committed), seen, Arc::clone(&compensated));

        let error = run_phased(&cx, &command)
            .await
            .expect_err("committed fails");

        assert!(matches!(error, TestError::Failed));
        assert_eq!(
            compensated.lock().expect("compensated lock").as_slice(),
            &[TestPhase::Committed, TestPhase::Prepared]
        );
    }

    #[tokio::test]
    async fn compensation_failure_preserves_original_error() {
        let store = Arc::new(InMemoryCommandPhaseStore::empty());
        let cx = CommandContext::new(store);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let compensated = Arc::new(Mutex::new(Vec::new()));
        let command = TestCommand::new(Some(TestPhase::Committed), seen, Arc::clone(&compensated))
            .with_failing_compensation();

        let error = run_phased(&cx, &command)
            .await
            .expect_err("original command error returned");

        assert!(matches!(error, TestError::Failed));
        assert_eq!(
            compensated.lock().expect("compensated lock").as_slice(),
            &[TestPhase::Committed, TestPhase::Prepared]
        );
    }

    #[test]
    fn conflicting_phase_candidates_are_structured_errors() {
        let store = Arc::new(InMemoryCommandPhaseStore::empty());
        let cx = CommandContext::new(store.clone());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let compensated = Arc::new(Mutex::new(Vec::new()));
        let command = TestCommand::new(None, seen, compensated);
        let name = command.name();
        let intent = command.intent_id();
        store
            .write_conflicting_phase_for_test(
                &name,
                &intent,
                1,
                FactPayload::from(br#""Prepared""#.to_vec()),
            )
            .expect("write first candidate");
        let key = store
            .write_conflicting_phase_for_test(
                &name,
                &intent,
                1,
                FactPayload::from(br#""Committed""#.to_vec()),
            )
            .expect("write second candidate");

        let error = cx
            .read_phase::<TestPhase>(&command)
            .expect_err("conflict surfaces");

        assert!(
            matches!(error, CommandError::PhaseConflict { key: error_key } if error_key == key)
        );
    }

    #[test]
    fn same_index_phase_write_conflict_is_immediate() {
        let store = Arc::new(InMemoryCommandPhaseStore::empty());
        let cx = CommandContext::new(store);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let compensated = Arc::new(Mutex::new(Vec::new()));
        let command = TestCommand::new(None, seen, compensated);
        cx.write_phase(&command, &TestPhase::Prepared)
            .expect("write first phase");
        let name = command.name();
        let intent = command.intent_id();
        let key = crate::command_phase_fact_key(&name, &intent, 1).expect("phase key");

        let error = cx
            .store
            .write_phase(
                &name,
                &intent,
                1,
                FactPayload::from(br#""Committed""#.to_vec()),
            )
            .expect_err("same index conflict");

        assert!(
            matches!(error, CommandError::PhaseConflict { key: error_key } if error_key == key)
        );
    }

    #[tokio::test]
    async fn phase_write_failure_compensates_next_and_prior_phases() {
        let store = Arc::new(FailingPhaseWriteStore);
        let cx = CommandContext::new(store);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let compensated = Arc::new(Mutex::new(Vec::new()));
        let command = TestCommand::new(None, Arc::clone(&seen), Arc::clone(&compensated));

        let error = run_phased(&cx, &command)
            .await
            .expect_err("phase write fails");

        assert!(matches!(error, TestError::Command));
        assert_eq!(
            seen.lock().expect("seen lock").as_slice(),
            &[TestPhase::Start]
        );
        assert_eq!(
            compensated.lock().expect("compensated lock").as_slice(),
            &[TestPhase::Prepared]
        );
    }

    #[test]
    fn invalid_command_identifiers_are_rejected() {
        assert!(matches!(
            CommandName::parse("deploy.submit"),
            Ok(command) if command.as_str() == "deploy.submit"
        ));
        assert!(matches!(
            IntentId::parse("bad/intent"),
            Err(CommandError::InvalidIdentifier { .. })
        ));
    }

    struct FailingPhaseWriteStore;

    impl CommandPhaseStore for FailingPhaseWriteStore {
        fn read_latest_phase(
            &self,
            _command: &CommandName,
            _intent: &IntentId,
        ) -> crate::CommandResult<Option<StoredCommandPhase>> {
            Ok(None)
        }

        fn write_intent(&self, fact: CommandFact) -> crate::CommandResult<FactKey> {
            crate::command_intent_fact_key(&fact.command, &fact.intent)
        }

        fn write_phase(
            &self,
            command: &CommandName,
            intent: &IntentId,
            _index: u64,
            _payload: FactPayload,
        ) -> crate::CommandResult<FactKey> {
            Err(CommandError::PhaseConflict {
                key: crate::command_phase_fact_key(command, intent, 1)?,
            })
        }
    }
}
