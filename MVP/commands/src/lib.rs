use std::collections::BTreeMap;
use std::fmt::{self, Debug, Display, Formatter};
use std::future::Future;
use std::hash::Hash;
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PhaseName(String);

impl PhaseName {
    pub fn parse(value: impl Into<String>) -> CommandResult<Self> {
        let value = value.into();
        validate_identifier("phase name", &value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for PhaseName {
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
    fn intent_fact(&self) -> CommandFact;
}

pub trait Phase:
    Serialize + DeserializeOwned + Clone + Debug + Eq + Hash + Send + Sync + 'static
{
}

impl<T> Phase for T where
    T: Serialize + DeserializeOwned + Clone + Debug + Eq + Hash + Send + Sync + 'static
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
        let name = command.name();
        let intent = command.intent_id();
        let Some(stored) = self.store.read_latest_phase(&name, &intent)? else {
            return Ok(None);
        };
        serde_json::from_slice(stored.payload.as_ref())
            .map(Some)
            .map_err(|error| CommandError::DeserializePhase {
                key: stored.key,
                message: error.to_string(),
            })
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
    cx.write_intent(command.intent_fact())?;
    let mut current = cx
        .read_phase::<C::Phase>(command)?
        .unwrap_or_else(|| command.initial_phase());
    let mut committed = Vec::new();

    loop {
        match command.step(cx, current.clone()).await? {
            PhaseTransition::Continue(next) => {
                cx.write_phase(command, &next)?;
                committed.push(next.clone());
                current = next;
            }
            PhaseTransition::Done(output) => return Ok(output),
        }
    }
}

pub async fn run_phased_with_compensation<C>(
    cx: &CommandContext,
    command: &C,
) -> Result<C::Output, C::Error>
where
    C: PhasedCommand,
{
    cx.write_intent(command.intent_fact())?;
    let mut current = cx
        .read_phase::<C::Phase>(command)?
        .unwrap_or_else(|| command.initial_phase());
    let mut committed = Vec::new();

    loop {
        match command.step(cx, current.clone()).await {
            Ok(PhaseTransition::Continue(next)) => {
                cx.write_phase(command, &next)?;
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

#[derive(Debug, Clone, Default)]
pub struct InMemoryCommandPhaseStore {
    inner: Arc<Mutex<InMemoryCommandPhaseStoreInner>>,
}

#[derive(Debug, Default)]
struct InMemoryCommandPhaseStoreInner {
    intents: BTreeMap<(CommandName, IntentId), CommandFact>,
    phases: BTreeMap<(CommandName, IntentId), BTreeMap<u64, StoredCommandPhase>>,
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
        Ok(inner
            .phases
            .get(&(command.clone(), intent.clone()))
            .and_then(|phases| phases.last_key_value().map(|(_index, phase)| phase.clone())))
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
        inner
            .phases
            .entry((command.clone(), intent.clone()))
            .or_default()
            .entry(index)
            .or_insert_with(|| StoredCommandPhase {
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
            .any(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'-' | b'_'))
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
    use std::sync::{Arc, Mutex};

    use serde::{Deserialize, Serialize};

    use crate::{
        Command, CommandContext, CommandError, CommandFact, CommandName, InMemoryCommandPhaseStore,
        IntentId, PhaseTransition, PhasedCommand, run_phased, run_phased_with_compensation,
    };

    #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
                seen,
                compensated,
            }
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

        fn intent_fact(&self) -> CommandFact {
            CommandFact::new(self.name(), self.intent_id())
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
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<PhaseTransition<Self::Phase, Self::Output>, Self::Error>,
                    > + Send
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
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), Self::Error>> + Send + 'a>>
        {
            Box::pin(async move {
                self.compensated
                    .lock()
                    .expect("compensated lock")
                    .push(phase);
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn run_phased_persists_each_continued_phase() {
        let store = Arc::new(InMemoryCommandPhaseStore::default());
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
        let store = Arc::new(InMemoryCommandPhaseStore::default());
        let cx = CommandContext::new(store);
        let initial_seen = Arc::new(Mutex::new(Vec::new()));
        let compensated = Arc::new(Mutex::new(Vec::new()));
        let initial = TestCommand::new(
            Some(TestPhase::Prepared),
            Arc::clone(&initial_seen),
            Arc::clone(&compensated),
        );

        let error = run_phased_with_compensation(&cx, &initial)
            .await
            .expect_err("prepared fails");
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
    async fn compensation_walks_committed_phases_in_reverse_without_failing_phase() {
        let store = Arc::new(InMemoryCommandPhaseStore::default());
        let cx = CommandContext::new(store);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let compensated = Arc::new(Mutex::new(Vec::new()));
        let command = TestCommand::new(Some(TestPhase::Committed), seen, Arc::clone(&compensated));

        let error = run_phased_with_compensation(&cx, &command)
            .await
            .expect_err("committed fails");

        assert!(matches!(error, TestError::Failed));
        assert_eq!(
            compensated.lock().expect("compensated lock").as_slice(),
            &[TestPhase::Committed, TestPhase::Prepared]
        );
    }
}
