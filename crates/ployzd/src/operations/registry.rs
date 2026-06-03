use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use ployz::operation::{OperationKind, OperationStage, OperationSubmission, OperationTerminal};

pub(crate) trait OperationImplementation: Send + Sync {
    fn script(&self, submission: &OperationSubmission) -> OperationScript;
}

#[derive(Clone, Default)]
pub(crate) struct OperationRegistry {
    implementations: BTreeMap<OperationKind, Arc<dyn OperationImplementation>>,
}

impl OperationRegistry {
    #[must_use]
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub(crate) fn with_operation<I>(mut self, kind: OperationKind, implementation: I) -> Self
    where
        I: OperationImplementation + 'static,
    {
        self.implementations.insert(kind, Arc::new(implementation));
        self
    }

    #[must_use]
    pub(crate) fn get(&self, kind: &OperationKind) -> Option<Arc<dyn OperationImplementation>> {
        self.implementations.get(kind).cloned()
    }

    #[must_use]
    pub(crate) fn contains(&self, kind: &OperationKind) -> bool {
        self.implementations.contains_key(kind)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OperationScript {
    steps: Vec<OperationScriptStep>,
    terminal: OperationTerminal,
}

impl OperationScript {
    #[cfg(test)]
    #[must_use]
    pub(crate) fn new(steps: Vec<OperationScriptStep>, terminal: OperationTerminal) -> Self {
        Self { steps, terminal }
    }

    #[must_use]
    pub(crate) fn steps(&self) -> &[OperationScriptStep] {
        &self.steps
    }

    #[must_use]
    pub(crate) fn terminal(&self) -> &OperationTerminal {
        &self.terminal
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "operation runner mechanics compile before the first real operation primitive constructs steps"
    )
)]
pub(crate) enum OperationScriptStep {
    Stage(OperationStage),
    Delay(Duration),
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct FakeOperation {
    script: OperationScript,
}

#[cfg(test)]
impl FakeOperation {
    #[must_use]
    pub(crate) fn new(script: OperationScript) -> Self {
        Self { script }
    }
}

#[cfg(test)]
impl OperationImplementation for FakeOperation {
    fn script(&self, _submission: &OperationSubmission) -> OperationScript {
        self.script.clone()
    }
}
